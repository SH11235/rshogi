use crate::evaluation::evaluate::Evaluator;
use crate::search::mate_score;
use crate::search::params::{
    qs_checks_enabled, QS_BAD_CAPTURE_MIN, QS_CHECK_PRUNE_MARGIN, QS_MARGIN_CAPTURE,
    QS_MAX_QUIET_CHECKS, QS_PROMOTE_BONUS,
};
use crate::Position;

use std::sync::OnceLock;

use super::driver::ClassicBackend;
use super::ordering::{EvalMoveGuard, Heuristics, MovePicker};
use super::pvs::SearchContext;

#[cfg(feature = "diagnostics")]
thread_local! {
    static QSEARCH_DEEP_LOGGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(crate) fn qsearch(
        &self,
        pos: &Position,
        mut alpha: i32,
        beta: i32,
        ctx: &mut SearchContext,
        ply: u32,
    ) -> i32 {
        if (ply as u16) >= crate::search::constants::MAX_QUIESCE_DEPTH {
            return alpha;
        }
        if ctx.time_up_fast() || ctx.time_up() || Self::should_stop(ctx.limits) {
            return alpha;
        }
        ctx.tick(ply);
        if ctx.register_qnode() {
            return alpha;
        }

        static HEUR_STUB: OnceLock<Heuristics> = OnceLock::new();
        let heur_stub = HEUR_STUB.get_or_init(Heuristics::default);

        if pos.is_in_check() {
            let mut picker = MovePicker::new_evasion(pos, None, None, None);
            let mut has_legal = false;
            while let Some(mv) = picker.next(heur_stub) {
                has_legal = true;
                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    -self.qsearch(&child, -beta, -alpha, ctx, ply + 1)
                };
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
            }
            if !has_legal {
                return mate_score(ply as u8, false);
            }
            return alpha;
        }

        let stand_pat = self.evaluator.evaluate(pos);

        #[cfg(feature = "diagnostics")]
        {
            if ply >= 12 {
                let should_log = QSEARCH_DEEP_LOGGED.with(|flag| {
                    if !flag.get() {
                        flag.set(true);
                        true
                    } else {
                        false
                    }
                });
                if should_log {
                    if let Some(cb) = ctx.limits.info_string_callback.as_ref() {
                        cb(&format!(
                            "qsearch_deep ply={} nodes={} stand={} alpha={} beta={} side={:?}",
                            ply, *ctx.nodes, stand_pat, alpha, beta, pos.side_to_move
                        ));
                    }
                }
            }
        }
        if stand_pat >= beta {
            return stand_pat;
        }
        if stand_pat > alpha {
            alpha = stand_pat;
        }

        let quiet_limit = if qs_checks_enabled() {
            QS_MAX_QUIET_CHECKS
        } else {
            0
        };
        let mut picker = MovePicker::new_qsearch(pos, None, None, None, quiet_limit);

        while let Some(mv) = picker.next(heur_stub) {
            if ctx.time_up_fast() {
                return alpha;
            }
            if mv.is_capture_hint() {
                let see = pos.see(mv);
                if see >= 0 {
                    let captured_val = mv
                        .captured_piece_type()
                        .map(|pt| crate::shogi::piece_constants::SEE_PIECE_VALUES[0][pt as usize])
                        .unwrap_or(0);
                    let best_gain = stand_pat + captured_val + QS_PROMOTE_BONUS + QS_MARGIN_CAPTURE;
                    if best_gain <= alpha {
                        continue;
                    }
                } else {
                    let captured_val = mv
                        .captured_piece_type()
                        .map(|pt| crate::shogi::piece_constants::SEE_PIECE_VALUES[0][pt as usize])
                        .unwrap_or(0);
                    if captured_val < QS_BAD_CAPTURE_MIN && !pos.gives_check(mv) {
                        continue;
                    }
                    let best_gain = stand_pat + captured_val + QS_MARGIN_CAPTURE;
                    if best_gain <= alpha {
                        continue;
                    }
                }

                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    -self.qsearch(&child, -beta, -alpha, ctx, ply + 1)
                };
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
            } else if qs_checks_enabled() && pos.gives_check(mv) {
                if stand_pat + QS_CHECK_PRUNE_MARGIN <= alpha {
                    continue;
                }
                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    -self.qsearch(&child, -beta, -alpha, ctx, ply + 1)
                };
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
            }
        }

        alpha
    }
}

#[cfg(feature = "diagnostics")]
pub(crate) fn reset_qsearch_diagnostics() {
    QSEARCH_DEEP_LOGGED.with(|flag| flag.set(false));
}
