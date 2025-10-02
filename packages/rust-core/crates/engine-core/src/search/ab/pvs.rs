use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::params as dynp;
use crate::search::tt::TTProbe;
use crate::search::types::SearchStack;
use crate::search::SearchLimits;
use crate::Position;

use super::driver::ClassicBackend;
use super::ordering::{self, EvalMoveGuard, Heuristics, LateMoveReductionParams};
use super::pruning::{MaybeIidParams, NullMovePruneParams, ProbcutParams};
use crate::search::types::NodeType;

pub(crate) struct SearchContext<'a> {
    pub(crate) limits: &'a SearchLimits,
    pub(crate) start_time: &'a std::time::Instant,
    pub(crate) nodes: &'a mut u64,
    pub(crate) seldepth: &'a mut u32,
}

impl<'a> SearchContext<'a> {
    #[inline]
    pub(crate) fn tick(&mut self, ply: u32) {
        *self.nodes += 1;
        if ply > *self.seldepth {
            *self.seldepth = ply;
        }
    }

    #[inline]
    pub(crate) fn time_up(&self) -> bool {
        self.limits.time_limit().is_some_and(|limit| self.start_time.elapsed() >= limit)
    }
}

pub(crate) struct ABArgs<'a> {
    pub(crate) pos: &'a Position,
    pub(crate) depth: i32,
    pub(crate) alpha: i32,
    pub(crate) beta: i32,
    pub(crate) ply: u32,
    pub(crate) is_pv: bool,
    pub(crate) stack: &'a mut [SearchStack],
    pub(crate) heur: &'a mut Heuristics,
    pub(crate) tt_hits: &'a mut u64,
    pub(crate) beta_cuts: &'a mut u64,
    pub(crate) lmr_counter: &'a mut u64,
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(crate) fn alphabeta(
        &self,
        args: ABArgs,
        ctx: &mut SearchContext,
    ) -> (i32, Option<crate::shogi::Move>) {
        let ABArgs {
            pos,
            depth,
            mut alpha,
            beta,
            ply,
            is_pv,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
        } = args;
        if (ply as usize) >= crate::search::constants::MAX_PLY {
            let eval = self.evaluator.evaluate(pos);
            return (eval, None);
        }
        if ctx.time_up() {
            let eval = self.evaluator.evaluate(pos);
            return (eval, None);
        }
        if Self::should_stop(ctx.limits) {
            return (0, None);
        }
        ctx.tick(ply);
        if depth <= 0 {
            let qs = self.qsearch(pos, alpha, beta, ctx, ply);
            return (qs, None);
        }

        let orig_alpha = alpha;
        let orig_beta = beta;
        let static_eval = self.evaluator.evaluate(pos);
        stack[ply as usize].static_eval = Some(static_eval);

        let mut a_md = alpha;
        let mut b_md = beta;
        if crate::search::mate_distance_pruning(&mut a_md, &mut b_md, ply as u8) {
            return (a_md, None);
        }
        alpha = a_md;
        let beta = b_md;

        if self.should_static_beta_prune(&self.profile.prune, depth, pos, beta, static_eval) {
            return (static_eval, None);
        }

        if let Some(r) = self.razor_prune(&self.profile.prune, depth, pos, alpha, ctx, ply) {
            return (r, None);
        }

        if let Some(score) = self.null_move_prune(NullMovePruneParams {
            toggles: &self.profile.prune,
            depth,
            pos,
            beta,
            static_eval,
            ply,
            stack: &mut *stack,
            heur: &mut *heur,
            tt_hits: &mut *tt_hits,
            beta_cuts: &mut *beta_cuts,
            lmr_counter: &mut *lmr_counter,
            ctx,
        }) {
            return (score, None);
        }

        let mut tt_hint: Option<crate::shogi::Move> = None;
        let mut tt_depth_ok = false;
        if let Some(tt) = &self.tt {
            if depth >= 3 {
                tt.prefetch_l2(pos.zobrist_hash, pos.side_to_move);
            }
            if let Some(entry) = tt.probe(pos.zobrist_hash, pos.side_to_move) {
                *tt_hits += 1;
                let stored = entry.score() as i32;
                let score = crate::search::common::adjust_mate_score_from_tt(stored, ply as u8);
                let sufficient = entry.depth() as i32 >= depth;
                tt_depth_ok = entry.depth() as i32 >= depth - 2;
                match entry.node_type() {
                    NodeType::LowerBound if sufficient && score >= beta => {
                        return (score, entry.get_move());
                    }
                    NodeType::UpperBound if sufficient && score <= alpha => {
                        return (score, entry.get_move());
                    }
                    NodeType::Exact if sufficient => {
                        return (score, entry.get_move());
                    }
                    _ => {
                        tt_hint = entry.get_move();
                    }
                }
            }
        }

        self.maybe_iid(MaybeIidParams {
            toggles: &self.profile.prune,
            depth,
            pos,
            alpha,
            beta,
            ply,
            stack: &mut *stack,
            heur: &mut *heur,
            tt_hits: &mut *tt_hits,
            beta_cuts: &mut *beta_cuts,
            lmr_counter: &mut *lmr_counter,
            ctx,
            tt_hint: &mut tt_hint,
            tt_depth_ok,
        });

        if let Some((score, mv)) = self.probcut(ProbcutParams {
            toggles: &self.profile.prune,
            depth,
            pos,
            beta,
            ply,
            stack: &mut *stack,
            heur: &mut *heur,
            tt_hits: &mut *tt_hits,
            beta_cuts: &mut *beta_cuts,
            lmr_counter: &mut *lmr_counter,
            ctx,
        }) {
            return (score, Some(mv));
        }

        let mg = MoveGenerator::new();
        let Ok(list) = mg.generate_all(pos) else {
            let qs = self.qsearch(pos, alpha, beta, ctx, ply);
            return (qs, None);
        };
        let mut moves: Vec<(crate::shogi::Move, i32)> = list
            .as_slice()
            .iter()
            .copied()
            .map(|m| {
                let is_check = pos.gives_check(m) as i32;
                let is_capture = m.is_capture_hint();
                let see = if is_capture { pos.see(m) } else { 0 };
                let promo = m.is_promote() as i32;
                let stage = if is_check == 1 {
                    3
                } else if is_capture && see >= 0 {
                    2
                } else if is_capture && see < 0 {
                    0
                } else {
                    1
                };
                let mut key = stage * 100_000 + is_check * 10_000 + see * 10 + promo;
                if stack[ply as usize].is_killer(m) {
                    key += 50_000;
                }
                if ply > 0 {
                    if let Some(prev_mv) = stack[(ply - 1) as usize].current_move {
                        if let Some(cm) = heur.counter.get(pos.side_to_move, prev_mv) {
                            if cm.equals_without_piece_type(&m) {
                                key += 60_000;
                            }
                        }
                    }
                }
                key += heur.history.get(pos.side_to_move, m);
                (m, key)
            })
            .collect();
        if let Some(ttm) = tt_hint {
            for (m, key) in &mut moves {
                if m.equals_without_piece_type(&ttm) {
                    *key += 1_000_000;
                }
            }
        }
        moves.sort_unstable_by(|a, b| b.1.cmp(&a.1));

        stack[ply as usize].clear_for_new_node();
        let mut best_mv = None;
        let mut best = i32::MIN / 2;
        let mut moveno: usize = 0;
        let mut first_move_done = false;
        for (mv, _key) in moves.into_iter() {
            if !pos.is_legal_move(mv) {
                continue;
            }
            moveno += 1;
            stack[ply as usize].current_move = Some(mv);
            let gives_check = pos.gives_check(mv);
            let is_capture = mv.is_capture_hint();
            let see = if is_capture { pos.see(mv) } else { 0 };
            let is_good_capture = is_capture && see >= 0;
            let is_quiet = !is_capture && !gives_check;

            if depth <= 3 && is_quiet {
                let h = heur.history.get(pos.side_to_move, mv);
                let mut is_counter = false;
                if ply > 0 {
                    if let Some(prev_mv) = stack[(ply - 1) as usize].current_move {
                        if let Some(cm) = heur.counter.get(pos.side_to_move, prev_mv) {
                            if cm.equals_without_piece_type(&mv) {
                                is_counter = true;
                            }
                        }
                    }
                }
                if h < dynp::hp_threshold() && !stack[ply as usize].is_killer(mv) && !is_counter {
                    continue;
                }
            }

            if depth <= 3 && is_quiet {
                let limit = dynp::lmp_limit_for_depth(depth);
                if moveno > limit {
                    continue;
                }
            }
            let mut next_depth = depth - 1;
            let reduction = ordering::late_move_reduction(LateMoveReductionParams {
                heur: &mut *heur,
                depth,
                moveno,
                is_quiet,
                is_good_capture,
                is_pv,
                gives_check,
                static_eval,
                ply,
                stack: &*stack,
            });
            if reduction > 0 {
                next_depth -= reduction;
                *lmr_counter += 1;
            }
            let pv_move = !first_move_done;
            let score = {
                let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                let mut child = pos.clone();
                child.do_move(mv);
                if pv_move {
                    let (sc, _) = self.alphabeta(
                        ABArgs {
                            pos: &child,
                            depth: next_depth,
                            alpha: -beta,
                            beta: -alpha,
                            ply: ply + 1,
                            is_pv: true,
                            stack,
                            heur,
                            tt_hits,
                            beta_cuts,
                            lmr_counter,
                        },
                        ctx,
                    );
                    -sc
                } else {
                    let (sc_nw, _) = self.alphabeta(
                        ABArgs {
                            pos: &child,
                            depth: next_depth,
                            alpha: -(alpha + 1),
                            beta: -alpha,
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
                    let mut s = -sc_nw;
                    if s > alpha && s < beta {
                        let (sc_fw, _) = self.alphabeta(
                            ABArgs {
                                pos: &child,
                                depth: next_depth,
                                alpha: -beta,
                                beta: -alpha,
                                ply: ply + 1,
                                is_pv: true,
                                stack,
                                heur,
                                tt_hits,
                                beta_cuts,
                                lmr_counter,
                            },
                            ctx,
                        );
                        s = -sc_fw;
                    }
                    s
                }
            };
            if pv_move {
                first_move_done = true;
            }
            if score > best {
                best = score;
                best_mv = Some(mv);
            }
            if score > alpha {
                alpha = score;
            }
            if alpha >= beta {
                *beta_cuts += 1;
                if is_quiet {
                    stack[ply as usize].update_killers(mv);
                    heur.history.update_good(pos.side_to_move, mv, depth);
                    if ply > 0 {
                        if let Some(prev_mv) = stack[(ply - 1) as usize].current_move {
                            heur.counter.update(pos.side_to_move, prev_mv, mv);
                        }
                    }
                }
                break;
            }
            if is_quiet {
                stack[ply as usize].quiet_moves.push(mv);
            }
        }
        if best == i32::MIN / 2 {
            let qs = self.qsearch(pos, alpha, beta, ctx, ply);
            (qs, None)
        } else {
            if let Some(tt) = &self.tt {
                let node_type = if best <= orig_alpha {
                    NodeType::UpperBound
                } else if best >= orig_beta {
                    NodeType::LowerBound
                } else {
                    NodeType::Exact
                };
                let store_score = crate::search::common::adjust_mate_score_for_tt(best, ply as u8);
                let mut args = crate::search::tt::TTStoreArgs::new(
                    pos.zobrist_hash,
                    best_mv,
                    store_score as i16,
                    static_eval as i16,
                    depth as u8,
                    node_type,
                    pos.side_to_move,
                );
                args.is_pv = is_pv;
                tt.store(args);
            }
            for &qmv in &stack[ply as usize].quiet_moves {
                if Some(qmv) != best_mv {
                    heur.history.update_bad(pos.side_to_move, qmv, depth);
                }
            }
            (best, best_mv)
        }
    }
}
