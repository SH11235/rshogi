use crate::evaluation::evaluate::Evaluator;
use crate::search::mate_score;
use crate::search::params::{
    qs_bad_capture_min, qs_check_prune_margin, qs_check_see_margin, qs_checks_enabled,
    qs_margin_capture, QS_MAX_QUIET_CHECKS, QS_PROMOTE_BONUS,
};
use crate::Position;

use std::sync::OnceLock;

use super::driver::ClassicBackend;
use super::ordering::{EvalMoveGuard, Heuristics, MovePicker};
use super::pvs::SearchContext;

#[cfg(feature = "diagnostics")]
use crate::search::types::InfoStringCallback;

#[cfg(feature = "diagnostics")]
thread_local! {
    static QSEARCH_DEEP_LOGGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(feature = "diagnostics")]
thread_local! {
    static QSEARCH_ABORTED_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static QSEARCH_QUIET_CHECKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static QSEARCH_QNODES_PEAK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static QSEARCH_LAST_LIMIT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(feature = "diagnostics")]
#[inline]
pub(crate) fn record_qsearch_abort() {
    QSEARCH_ABORTED_COUNT.with(|cnt| cnt.set(cnt.get().saturating_add(1)));
}

#[cfg(feature = "diagnostics")]
#[inline]
pub(crate) fn record_quiet_check_generated() {
    QSEARCH_QUIET_CHECKS.with(|cnt| cnt.set(cnt.get().saturating_add(1)));
}

#[cfg(feature = "diagnostics")]
#[inline]
pub(crate) fn record_qnodes_peak(current: u64, limit: u64) {
    QSEARCH_QNODES_PEAK.with(|peak| {
        if current > peak.get() {
            peak.set(current);
        }
    });
    QSEARCH_LAST_LIMIT.with(|cell| cell.set(limit));
}

#[cfg(feature = "diagnostics")]
pub(crate) fn publish_qsearch_diagnostics(depth: i32, cb: Option<&InfoStringCallback>) {
    let (aborted, quiet_checks, peak, limit) = QSEARCH_ABORTED_COUNT.with(|ab| {
        let aborted = ab.get();
        let quiet = QSEARCH_QUIET_CHECKS.with(|qc| qc.get());
        let peak = QSEARCH_QNODES_PEAK.with(|pk| pk.get());
        let limit = QSEARCH_LAST_LIMIT.with(|lm| lm.get());
        (aborted, quiet, peak, limit)
    });
    if let Some(cb) = cb {
        cb(&format!(
            "qsearch_diag depth={} aborted={} quiet_checks={} qnodes_peak={} limit={}",
            depth, aborted, quiet_checks, peak, limit
        ));
    }
    QSEARCH_ABORTED_COUNT.with(|cnt| cnt.set(0));
    QSEARCH_QUIET_CHECKS.with(|cnt| cnt.set(0));
    QSEARCH_QNODES_PEAK.with(|cnt| cnt.set(0));
    QSEARCH_LAST_LIMIT.with(|cnt| cnt.set(0));
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(crate) fn qsearch(
        &self,
        pos: &Position,
        mut alpha: i32,
        beta: i32,
        ctx: &mut SearchContext,
        ply: u32,
        qcheck_budget: &mut i32,
    ) -> i32 {
        ctx.tick(ply);

        static HEUR_STUB: OnceLock<Heuristics> = OnceLock::new();
        let heur_stub = HEUR_STUB.get_or_init(Heuristics::default);

        if pos.is_in_check() {
            let mut picker = MovePicker::new_evasion(pos, None, None, None);
            let mut has_legal = false;
            let mut aborted = false;
            while let Some(mv) = picker.next(heur_stub) {
                has_legal = true;
                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    -self.qsearch(&child, -beta, -alpha, ctx, ply + 1, qcheck_budget)
                };
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
                if ctx.qnodes_limit_reached() || ctx.time_up_fast() {
                    aborted = true;
                    break;
                }
            }
            if !has_legal {
                return mate_score(ply as u8, false);
            }
            let should_stop_now =
                aborted || ctx.time_up_fast() || ctx.time_up() || Self::should_stop(ctx.limits);
            #[cfg(feature = "diagnostics")]
            if should_stop_now {
                record_qsearch_abort();
            }
            if should_stop_now {
                return alpha;
            }
            if ctx.register_qnode() {
                #[cfg(feature = "diagnostics")]
                record_qsearch_abort();
                return alpha;
            }
            return alpha;
        }

        let stand_pat = self.evaluator.evaluate(pos);

        if (ply as u16) >= crate::search::constants::MAX_QUIESCE_DEPTH {
            return stand_pat.max(alpha);
        }

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

        let should_stop_now = ctx.qnodes_limit_reached()
            || ctx.time_up_fast()
            || ctx.time_up()
            || Self::should_stop(ctx.limits);
        #[cfg(feature = "diagnostics")]
        if should_stop_now {
            record_qsearch_abort();
        }
        if should_stop_now {
            return alpha.max(stand_pat);
        }
        if ctx.register_qnode() {
            #[cfg(feature = "diagnostics")]
            record_qsearch_abort();
            return alpha.max(stand_pat);
        }

        let mut quiet_limit = if qs_checks_enabled() {
            QS_MAX_QUIET_CHECKS
        } else {
            0
        };
        if quiet_limit > 0 {
            if let Some(tm) = ctx.limits.time_manager.as_ref() {
                let soft = tm.soft_limit_ms();
                if soft > 0 && soft != u64::MAX {
                    let elapsed = tm.elapsed_ms();
                    let first_threshold = soft.saturating_mul(85).saturating_div(100);
                    let second_threshold = soft.saturating_mul(92).saturating_div(100);
                    let final_threshold = soft.saturating_mul(97).saturating_div(100);
                    if elapsed >= final_threshold {
                        quiet_limit = 0;
                    } else if elapsed >= second_threshold {
                        quiet_limit = quiet_limit.min(1);
                    } else if elapsed >= first_threshold {
                        quiet_limit = quiet_limit.min(2);
                    }
                }
                if tm.is_in_byoyomi() {
                    quiet_limit = quiet_limit.min(1);
                }
            }
            if matches!(
                ctx.limits.time_control,
                crate::time_management::TimeControl::Byoyomi { .. }
            ) {
                quiet_limit = quiet_limit.min(1);
            }
        }
        let margin_capture = qs_margin_capture();
        let bad_capture_min = qs_bad_capture_min();
        let check_prune_margin = qs_check_prune_margin();
        let mut check_see_margin = qs_check_see_margin();
        // tighten SEE in pure byoyomi to curb long quiet-check chains
        let in_byoyomi = if let Some(tm) = ctx.limits.time_manager.as_ref() {
            tm.is_in_byoyomi()
        } else {
            matches!(ctx.limits.time_control, crate::time_management::TimeControl::Byoyomi { .. })
        };
        if in_byoyomi && check_see_margin < -30 {
            check_see_margin = -30;
        }
        let mut picker = MovePicker::new_qsearch(pos, None, None, None, quiet_limit);
        let mut remaining_quiet_checks = quiet_limit;

        while let Some(mv) = picker.next(heur_stub) {
            if ctx.time_up_fast() {
                #[cfg(feature = "diagnostics")]
                record_qsearch_abort();
                return alpha.max(stand_pat);
            }
            if mv.is_capture_hint() {
                let see = pos.see(mv);
                // Use promoted-aware captured piece value for pre-filtering to keep
                // qsearch gating consistent with SEE/Material. This reduces the risk
                // of underestimating recaptures on promoted pieces (e.g., Dragon/Horse).
                let captured_val_prom_aware = {
                    let to = mv.to();
                    if let Some(piece) = pos.board.squares[to.index()] {
                        crate::shogi::piece_constants::SEE_PIECE_VALUES[piece.promoted as usize]
                            [piece.piece_type as usize]
                    } else {
                        0
                    }
                };

                if see >= 0 {
                    let best_gain =
                        stand_pat + captured_val_prom_aware + QS_PROMOTE_BONUS + margin_capture;
                    if best_gain <= alpha {
                        continue;
                    }
                } else {
                    if captured_val_prom_aware < bad_capture_min && !pos.gives_check(mv) {
                        continue;
                    }
                    let best_gain = stand_pat + captured_val_prom_aware + margin_capture;
                    if best_gain <= alpha {
                        continue;
                    }
                }

                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    -self.qsearch(&child, -beta, -alpha, ctx, ply + 1, qcheck_budget)
                };
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
            } else if qs_checks_enabled() && pos.gives_check(mv) {
                if remaining_quiet_checks == 0 {
                    continue;
                }
                // Require SEE >= margin for quiet checks (YO-aligned guard)
                if pos.see(mv) < check_see_margin {
                    continue;
                }
                if stand_pat + check_prune_margin <= alpha {
                    continue;
                }
                if *qcheck_budget <= 0 {
                    continue;
                }
                #[cfg(feature = "diagnostics")]
                record_quiet_check_generated();
                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    *qcheck_budget -= 1;
                    -self.qsearch(&child, -beta, -alpha, ctx, ply + 1, qcheck_budget)
                };
                remaining_quiet_checks = remaining_quiet_checks.saturating_sub(1);
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
    #[cfg(feature = "diagnostics")]
    {
        QSEARCH_ABORTED_COUNT.with(|cnt| cnt.set(0));
        QSEARCH_QUIET_CHECKS.with(|cnt| cnt.set(0));
        QSEARCH_QNODES_PEAK.with(|cnt| cnt.set(0));
        QSEARCH_LAST_LIMIT.with(|cnt| cnt.set(0));
    }
}

/// Compute initial quiet-check budget for one qsearch invocation.
/// Byoyomi では 1、それ以外は 2 を返す（保守的）。
pub(crate) fn initial_quiet_check_budget<'a>(ctx: &SearchContext<'a>) -> i32 {
    if let Some(tm) = ctx.limits.time_manager.as_ref() {
        if tm.is_in_byoyomi() {
            return 1;
        }
    }
    match ctx.limits.time_control {
        crate::time_management::TimeControl::Byoyomi { .. } => 1,
        _ => 2,
    }
}
