use crate::evaluation::evaluate::Evaluator;
use crate::search::mate_score;
use crate::search::params::{
    qs_check_prune_margin, qs_check_see_margin, qs_checks_enabled, QS_MAX_QUIET_CHECKS,
};
use crate::Position;

use super::driver::ClassicBackend;
use super::ordering::{EvalMoveGuard, Heuristics, MovePicker};
use super::pvs::SearchContext;
use crate::movegen::MoveGenerator;
use crate::search::common::{adjust_mate_score_for_tt, adjust_mate_score_from_tt};
use crate::search::history::ContinuationKey;
use crate::search::tt::TTProbe;
use crate::search::types::NodeType;
use crate::shogi::Move;

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

const CONT_HISTORY_QS_PRUNE_THRESHOLD: i32 = 5_868;
const DEPTH_QS_RECAPTURES: i32 = -5;

#[inline]
fn continuation_history_score(
    heur: &Heuristics,
    pos: &Position,
    mv: Move,
    prev_move: Option<Move>,
) -> i32 {
    if let (Some(prev_mv), Some(curr_piece)) = (prev_move, mv.piece_type()) {
        if let Some(prev_piece) = prev_mv.piece_type() {
            let key = ContinuationKey::new(
                pos.side_to_move,
                prev_piece as usize,
                prev_mv.to(),
                prev_mv.is_drop(),
                curr_piece as usize,
                mv.to(),
                mv.is_drop(),
            );
            return heur.continuation.get(key);
        }
    }
    0
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub(super) fn qsearch(
        &self,
        pos: &Position,
        window: SearchWindow,
        ctx: &mut SearchContext,
        frame: QSearchFrame,
        heur: &Heuristics,
        qcheck_budget: &mut i32,
    ) -> i32 {
        let mut alpha = window.alpha;
        let alpha_orig = alpha;
        let mut best_move: Option<crate::shogi::Move> = None;
        let beta = window.beta;
        let ply = frame.ply;
        let qdepth = frame.qdepth;
        let prev_move = frame.prev_move;
        ctx.tick(ply);

        if pos.is_in_check() {
            let mut picker = MovePicker::new_evasion(pos, None, None, None);
            let mut has_legal = false;
            let mut aborted = false;
            while let Some(mv) = picker.next(heur) {
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
                        heur,
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

        // --- TT probe for qsearch (YO方針の簡易版) ---
        let pos_hash = pos.zobrist_hash();
        if let Some(tt) = &self.tt {
            if let Some(entry) = tt.probe(pos_hash, pos.side_to_move) {
                // 取得スコアはroot相対 → 現在相対に戻す
                let tt_score = adjust_mate_score_from_tt(entry.score() as i32, ply as u8);
                match entry.node_type() {
                    NodeType::LowerBound if tt_score >= beta => {
                        return tt_score;
                    }
                    NodeType::UpperBound if tt_score <= alpha => {
                        // 早期fail-low扱い（上下界の信頼は限定的だが、浅層での無駄探索を抑える）
                        return tt_score.max(alpha);
                    }
                    _ => {}
                }
            }
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
            // Store lower bound to TT（qsearch）
            if let Some(tt) = &self.tt {
                let store = adjust_mate_score_for_tt(smoothed, ply as u8)
                    .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                let eval_i16 = stand_pat.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                let args = crate::search::tt::TTStoreArgs::new(
                    pos_hash,
                    None,
                    store,
                    eval_i16,
                    0, // depth tag for qsearch
                    NodeType::LowerBound,
                    pos.side_to_move,
                );
                tt.store(args);
            }
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
        let force_recapture_only = qdepth <= DEPTH_QS_RECAPTURES && recapture_sq.is_some();

        // --- 1手詰め検出（簡易）: 非チェック時のみ、合法手mで進めて相手方に合法手がなければmate-in-1 ---
        // コストを抑えるため、TT未命中時のみ実施し、検出時は即return。
        // 注意: 終盤の判定安定化が目的。間違って走るのを避けるため例外は握りつぶさない。
        if !pos.is_in_check() {
            let mg = MoveGenerator::new();
            if let Ok(list) = mg.generate_all(pos) {
                for &mv in list.as_slice() {
                    let mut child = pos.clone();
                    let undo = child.do_move(mv);
                    // 相手側に合法手がなければ詰み
                    let opp_mg = MoveGenerator::new();
                    let mate = match opp_mg.generate_all(&child) {
                        Ok(ml) => ml.as_slice().is_empty(),
                        Err(_) => false,
                    };
                    child.undo_move(mv, undo);
                    if mate {
                        let score = crate::search::mate_score(ply as u8 + 1, true);
                        if let Some(tt) = &self.tt {
                            let store = adjust_mate_score_for_tt(score, ply as u8)
                                .clamp(i16::MIN as i32, i16::MAX as i32)
                                as i16;
                            let eval_i16 = stand_pat.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                            let args = crate::search::tt::TTStoreArgs::new(
                                pos_hash,
                                Some(mv),
                                store,
                                eval_i16,
                                0,
                                NodeType::LowerBound,
                                pos.side_to_move,
                            );
                            tt.store(args);
                        }
                        return score;
                    }
                }
            }
        }

        let mut move_count: i32 = 0;
        while let Some(mv) = picker.next(heur) {
            if ctx.time_up_fast() {
                #[cfg(feature = "diagnostics")]
                record_qsearch_abort();
                return alpha.max(stand_pat);
            }
            let is_recapture = recapture_sq.is_some_and(|sq| sq == mv.to());
            if force_recapture_only && !is_recapture {
                continue;
            }
            if mv.is_capture_hint() {
                move_count += 1;
                // MoveCount-based pruning for captures (YO-aligned):
                // After the first two capture candidates, skip the rest in
                // typical non-forcing situations to prevent wide capture
                // branches from dominating qsearch. Do not apply this to
                // promotions, checking moves, or recaptures on the previous
                // move's destination.
                let gives_check = pos.gives_check(mv);
                if move_count > 2 && !gives_check && !mv.is_promote() && !is_recapture {
                    continue;
                }
                // 深い静止層では再捕獲以外のキャプチャを抑制（YO: DEPTH_QS_RECAPTURES=-5）
                if qdepth <= DEPTH_QS_RECAPTURES && !is_recapture {
                    continue;
                }

                let cont_score = continuation_history_score(heur, pos, mv, prev_move);
                let pawn_score = mv
                    .piece_type()
                    .map(|pt| heur.pawn_history.get(pos.side_to_move, pt, mv.to()))
                    .unwrap_or(0);
                if cont_score + pawn_score <= CONT_HISTORY_QS_PRUNE_THRESHOLD
                    && !gives_check
                    && !mv.is_promote()
                    && !is_recapture
                {
                    continue;
                }

                let see = pos.see(mv);
                if !gives_check && !is_recapture {
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
                        heur,
                        qcheck_budget,
                    )
                };
                if sc >= beta {
                    if let Some(tt) = &self.tt {
                        let store = adjust_mate_score_for_tt(sc, ply as u8)
                            .clamp(i16::MIN as i32, i16::MAX as i32)
                            as i16;
                        let eval_i16 = stand_pat.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                        let args = crate::search::tt::TTStoreArgs::new(
                            pos_hash,
                            Some(mv),
                            store,
                            eval_i16,
                            0,
                            NodeType::LowerBound,
                            pos.side_to_move,
                        );
                        tt.store(args);
                    }
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                    best_move = Some(mv);
                }
            } else if (qdepth == 0) && qs_checks_enabled() && pos.gives_check(mv) {
                if remaining_quiet_checks == 0 {
                    continue;
                }
                let cont_score = continuation_history_score(heur, pos, mv, prev_move);
                let pawn_score = mv
                    .piece_type()
                    .map(|pt| heur.pawn_history.get(pos.side_to_move, pt, mv.to()))
                    .unwrap_or(0);
                if cont_score + pawn_score <= CONT_HISTORY_QS_PRUNE_THRESHOLD {
                    continue;
                }
                // Require SEE >= margin for quiet checks (YO-aligned guard)
                if pos.see(mv) < check_see_margin {
                    continue;
                }
                if stand_pat + check_prune_margin <= alpha {
                    continue;
                }
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
                        heur,
                        qcheck_budget,
                    )
                };
                remaining_quiet_checks = remaining_quiet_checks.saturating_sub(1);
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                    best_move = Some(mv);
                }
            }
        }
        let result = alpha;
        if let Some(tt) = &self.tt {
            // qsearch のTT格納分類:
            // - fail-low: result <= alpha_orig → UpperBound
            // - fail-high: result >= beta → LowerBound（上で早期return済みだが安全のため条件化）
            // - 範囲内: alpha_orig < result < beta → Exact（後続の再利用で有効）
            let node_type = if result <= alpha_orig {
                NodeType::UpperBound
            } else if result >= beta {
                NodeType::LowerBound
            } else {
                NodeType::Exact
            };
            let store = adjust_mate_score_for_tt(result, ply as u8)
                .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let eval_i16 = stand_pat.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            let args = crate::search::tt::TTStoreArgs::new(
                pos_hash,
                best_move,
                store,
                eval_i16,
                0,
                node_type,
                pos.side_to_move,
            );
            tt.store(args);
        }
        result
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
