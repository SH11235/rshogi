use crate::evaluation::evaluate::Evaluator;
use crate::search::mate_score;
use crate::search::params::{
    qs_check_prune_margin, qs_check_see_margin, qs_checks_enabled, QS_MAX_QUIET_CHECKS,
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

/// 検索窓（alpha/beta）
#[derive(Clone, Copy, Debug)]
pub(super) struct SearchWindow {
    pub alpha: i32,
    pub beta: i32,
}

/// qsearch 呼び出しフレームのメタ情報
/// - `ply`: 現在の手数（root からの距離）
/// - `qdepth`: qsearch 内部の静止探索深さ（入口は0）
/// - `prev_move`: 直前手（再捕獲などの判定に利用）
#[derive(Clone, Copy, Debug)]
pub(super) struct QSearchFrame {
    pub ply: u32,
    pub qdepth: i32,
    pub prev_move: Option<crate::shogi::Move>,
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(super) fn qsearch(
        &self,
        pos: &Position,
        window: SearchWindow,
        ctx: &mut SearchContext,
        frame: QSearchFrame,
        qcheck_budget: &mut i32,
    ) -> i32 {
        let mut alpha = window.alpha;
        let beta = window.beta;
        let ply = frame.ply;
        let qdepth = frame.qdepth;
        let prev_move = frame.prev_move;
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
                    -self.qsearch(
                        &child,
                        SearchWindow {
                            alpha: -beta,
                            beta: -alpha,
                        },
                        ctx,
                        QSearchFrame {
                            ply: ply + 1,
                            qdepth: qdepth - 1,
                            prev_move: Some(mv),
                        },
                        qcheck_budget,
                    )
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

        // 非チェック時の繰り返し（千日手）を早期検出して即時帰り（YO準拠の方針）。
        // in-check の場合は回避手生成に委ねるためここでは判定しない。
        if !pos.is_in_check() && pos.is_draw() {
            return crate::search::constants::DRAW_SCORE;
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
            // YO系のスムージングに合わせ、βへ少し寄せて返す。
            // （TT汚染抑制・情報の安定化目的。決定的スコアではないため単純平均で十分。）
            let smoothed = (stand_pat + beta) / 2;
            return smoothed;
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

        // 静かチェックは qsearch 侵入直後（qdepth==0）のみ生成を許可する。
        // それ以外の再帰では quiet checks を生成しない（quiet_limit=0）。
        // 将棋では手駒による連続王手で組合せが急増しやすいため、
        // 生成位置を浅層に限定して探索の安定性を確保する目的。
        let mut quiet_limit = if qdepth == 0 && qs_checks_enabled() {
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
        let own_king_sq = pos.board.king_square(pos.side_to_move);
        let recapture_sq = prev_move.map(|mv| mv.to());

        let mut move_count: i32 = 0;
        while let Some(mv) = picker.next(heur_stub) {
            if ctx.time_up_fast() {
                #[cfg(feature = "diagnostics")]
                record_qsearch_abort();
                return alpha.max(stand_pat);
            }
            if mv.is_capture_hint() {
                move_count += 1;
                // MoveCount-based pruning for captures (YO-aligned):
                // After the first two capture candidates, skip the rest in
                // typical non-forcing situations to prevent wide capture
                // branches from dominating qsearch. Do not apply this to
                // promotions, checking moves, or recaptures on the previous
                // move's destination.
                let is_recapture = recapture_sq.is_some_and(|sq| sq == mv.to());
                if move_count > 2 && !pos.gives_check(mv) && !mv.is_promote() && !is_recapture {
                    continue;
                }
                let see = pos.see(mv);
                // 取る駒の価値（成りを考慮）
                let captured_val_prom_aware = {
                    let to = mv.to();
                    if let Some(piece) = pos.board.squares[to.index()] {
                        crate::shogi::piece_constants::SEE_PIECE_VALUES[piece.promoted as usize]
                            [piece.piece_type as usize]
                    } else {
                        0
                    }
                };

                // YO準拠: futility + SEE による枝刈り
                const QS_FUTILITY_BASE_MARGIN: i32 = 352; // cp
                let futility_base = stand_pat.saturating_add(QS_FUTILITY_BASE_MARGIN);

                // 1) futility: 静的評価 + 捕獲駒価値 が alpha を超えないならスキップ
                let futility_value = futility_base.saturating_add(captured_val_prom_aware);
                if futility_value <= alpha {
                    continue;
                }

                // 2) SEE が十分でないならスキップ（alpha - futility_base）
                if see < alpha.saturating_sub(futility_base) {
                    continue;
                }

                // 3) SEE の絶対下限（歩損未満は通さない）
                if see < -74 {
                    continue;
                }

                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    -self.qsearch(
                        &child,
                        SearchWindow {
                            alpha: -beta,
                            beta: -alpha,
                        },
                        ctx,
                        QSearchFrame {
                            ply: ply + 1,
                            qdepth: qdepth - 1,
                            prev_move: Some(mv),
                        },
                        qcheck_budget,
                    )
                };
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
            } else if (qdepth == 0) && qs_checks_enabled() && pos.gives_check(mv) {
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
                let is_recapture = recapture_sq.is_some_and(|sq| sq == mv.to());
                let king_adjacent = own_king_sq.is_some_and(|king| {
                    u8::abs_diff(king.file(), mv.to().file()) <= 1
                        && u8::abs_diff(king.rank(), mv.to().rank()) <= 1
                });
                let history_favored = is_recapture || king_adjacent;
                if *qcheck_budget <= 0 && !history_favored {
                    continue;
                }
                #[cfg(feature = "diagnostics")]
                record_quiet_check_generated();
                let mut child = pos.clone();
                let sc = {
                    let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                    child.do_move(mv);
                    if !history_favored {
                        *qcheck_budget -= 1;
                    }
                    -self.qsearch(
                        &child,
                        SearchWindow {
                            alpha: -beta,
                            beta: -alpha,
                        },
                        ctx,
                        QSearchFrame {
                            ply: ply + 1,
                            qdepth: qdepth - 1,
                            prev_move: Some(mv),
                        },
                        qcheck_budget,
                    )
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
