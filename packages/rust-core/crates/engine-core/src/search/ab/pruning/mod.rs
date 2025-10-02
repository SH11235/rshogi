use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::params as dynp;
use crate::search::params::{
    NMP_BASE_R, NMP_BONUS_DELTA_BETA, NMP_HAND_SUM_DISABLE, NMP_MIN_DEPTH,
};
use crate::search::tt::TTProbe;
use crate::search::types::SearchStack;
use crate::Position;

use super::driver::ClassicBackend;
use super::ordering::{EvalMoveGuard, EvalNullGuard, Heuristics};
use super::profile::PruneToggles;
use super::pvs::{ABArgs, SearchContext};

pub(super) struct NullMovePruneParams<'a, 'ctx> {
    pub toggles: &'a PruneToggles,
    pub depth: i32,
    pub pos: &'a Position,
    pub beta: i32,
    pub static_eval: i32,
    pub ply: u32,
    pub stack: &'a mut [SearchStack],
    pub heur: &'a mut Heuristics,
    pub tt_hits: &'a mut u64,
    pub beta_cuts: &'a mut u64,
    pub lmr_counter: &'a mut u64,
    pub ctx: &'a mut SearchContext<'ctx>,
}

pub(super) struct MaybeIidParams<'a, 'ctx> {
    pub toggles: &'a PruneToggles,
    pub depth: i32,
    pub pos: &'a Position,
    pub alpha: i32,
    pub beta: i32,
    pub ply: u32,
    pub stack: &'a mut [SearchStack],
    pub heur: &'a mut Heuristics,
    pub tt_hits: &'a mut u64,
    pub beta_cuts: &'a mut u64,
    pub lmr_counter: &'a mut u64,
    pub ctx: &'a mut SearchContext<'ctx>,
    pub tt_hint: &'a mut Option<crate::shogi::Move>,
    pub tt_depth_ok: bool,
}

pub(super) struct ProbcutParams<'a, 'ctx> {
    pub toggles: &'a PruneToggles,
    pub depth: i32,
    pub pos: &'a Position,
    pub beta: i32,
    pub ply: u32,
    pub stack: &'a mut [SearchStack],
    pub heur: &'a mut Heuristics,
    pub tt_hits: &'a mut u64,
    pub beta_cuts: &'a mut u64,
    pub lmr_counter: &'a mut u64,
    pub ctx: &'a mut SearchContext<'ctx>,
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(super) fn should_static_beta_prune(
        &self,
        toggles: &PruneToggles,
        depth: i32,
        pos: &Position,
        beta: i32,
        static_eval: i32,
    ) -> bool {
        toggles.enable_static_beta_pruning
            && dynp::static_beta_enabled()
            && depth <= 2
            && !pos.is_in_check()
            && {
                let margin = if depth == 1 {
                    dynp::sbp_margin_d1()
                } else {
                    dynp::sbp_margin_d2()
                };
                static_eval - margin >= beta
            }
    }

    pub(super) fn razor_prune(
        &self,
        toggles: &PruneToggles,
        depth: i32,
        pos: &Position,
        alpha: i32,
        ctx: &mut SearchContext,
        ply: u32,
    ) -> Option<i32> {
        if toggles.enable_razor && dynp::razor_enabled() && depth == 1 && !pos.is_in_check() {
            let r = self.qsearch(pos, alpha, alpha + 1, ctx, ply);
            if r <= alpha {
                return Some(r);
            }
        }
        None
    }

    pub(super) fn null_move_prune(&self, params: NullMovePruneParams<'_, '_>) -> Option<i32> {
        let NullMovePruneParams {
            toggles,
            depth,
            pos,
            beta,
            static_eval,
            ply,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
            ctx,
        } = params;
        if !toggles.enable_nmp || !dynp::nmp_enabled() {
            return None;
        }
        let prev_null = if ply > 0 {
            stack[(ply - 1) as usize].null_move
        } else {
            false
        };
        if depth < NMP_MIN_DEPTH || pos.is_in_check() || prev_null {
            return None;
        }
        let side = pos.side_to_move as usize;
        let hand_sum: i32 = pos.hands[side].iter().map(|&c| c as i32).sum();
        if hand_sum >= NMP_HAND_SUM_DISABLE {
            return None;
        }
        let bonus = if static_eval - beta > NMP_BONUS_DELTA_BETA {
            1
        } else {
            0
        };
        let mut r = NMP_BASE_R + (depth / 4) + bonus;
        r = r.min(depth - 1);
        let score = {
            let _guard = EvalNullGuard::new(self.evaluator.as_ref(), pos);
            let mut child = pos.clone();
            let undo_null = child.do_null_move();
            stack[ply as usize].null_move = true;
            let (sc, _) = self.alphabeta(
                ABArgs {
                    pos: &child,
                    depth: depth - 1 - r,
                    alpha: -(beta),
                    beta: -(beta - 1),
                    ply: ply + 1,
                    is_pv: false,
                    stack,
                    heur,
                    tt_hits,
                    beta_cuts,
                    lmr_counter,
                },
                ctx,
            );
            child.undo_null_move(undo_null);
            stack[ply as usize].null_move = false;
            -sc
        };
        if score >= beta {
            Some(score)
        } else {
            None
        }
    }

    pub(super) fn maybe_iid(&self, params: MaybeIidParams<'_, '_>) {
        let MaybeIidParams {
            toggles,
            depth,
            pos,
            alpha,
            beta,
            ply,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
            ctx,
            tt_hint,
            tt_depth_ok,
        } = params;
        if !(toggles.enable_iid
            && dynp::iid_enabled()
            && depth >= dynp::iid_min_depth()
            && !pos.is_in_check()
            && (!tt_depth_ok || tt_hint.is_none()))
        {
            return;
        }
        let iid_depth = depth - 2;
        let _ = self.alphabeta(
            ABArgs {
                pos,
                depth: iid_depth,
                alpha,
                beta,
                ply,
                is_pv: false,
                stack,
                heur,
                tt_hits,
                beta_cuts,
                lmr_counter,
            },
            ctx,
        );
        if let Some(tt) = &self.tt {
            if let Some(entry) = tt.probe(pos.zobrist_hash, pos.side_to_move) {
                *tt_hint = entry.get_move();
            }
        }
    }

    pub(super) fn probcut(
        &self,
        params: ProbcutParams<'_, '_>,
    ) -> Option<(i32, crate::shogi::Move)> {
        let ProbcutParams {
            toggles,
            depth,
            pos,
            beta,
            ply,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
            ctx,
        } = params;
        if !(toggles.enable_probcut && dynp::probcut_enabled() && depth >= 5 && !pos.is_in_check())
        {
            return None;
        }
        let threshold = beta + dynp::probcut_margin(depth);
        let mgp = MoveGenerator::new();
        if let Ok(caps) = mgp.generate_captures(pos) {
            for mv in caps.as_slice().iter().copied() {
                if pos.see(mv) < 0 {
                    continue;
                }
                let parent_sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    let mut child = pos.clone();
                    child.do_move(mv);
                    let (sc, _) = self.alphabeta(
                        ABArgs {
                            pos: &child,
                            depth: depth - 2,
                            alpha: -threshold,
                            beta: -threshold + 1,
                            ply: ply + 1,
                            is_pv: false,
                            stack,
                            heur,
                            tt_hits,
                            beta_cuts,
                            lmr_counter,
                        },
                        ctx,
                    );
                    -sc
                };
                if parent_sc >= threshold {
                    return Some((parent_sc, mv));
                }
            }
        }
        None
    }
}
