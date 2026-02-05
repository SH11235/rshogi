//! 評価・補正ヘルパー関数群
//!
//! 補正履歴、静的評価コンテキスト、置換表プローブ等。

use crate::eval::evaluate_pass_rights;
use crate::position::Position;
use crate::types::{Bound, Color, Depth, Move, Value, MAX_PLY};

use super::alpha_beta::{
    to_corrected_static_eval, EvalContext, ProbeOutcome, SearchContext, SearchState, TTContext,
};
use super::history::CORRECTION_HISTORY_SIZE;
use super::search_helpers::{ensure_nnue_accumulator, nnue_evaluate};
use super::stats::inc_stat_by_depth;
use super::types::{value_from_tt, NodeType};

// =============================================================================
// 補正履歴
// =============================================================================

/// 補正履歴から静的評価の補正値を算出
#[inline]
pub(super) fn correction_value(
    st: &SearchState,
    ctx: &SearchContext<'_>,
    pos: &Position,
    ply: i32,
) -> i32 {
    let us = pos.side_to_move();
    let pawn_idx = (pos.pawn_key() as usize) & (CORRECTION_HISTORY_SIZE - 1);
    let minor_idx = (pos.minor_piece_key() as usize) & (CORRECTION_HISTORY_SIZE - 1);
    let non_pawn_idx_w = (pos.non_pawn_key(Color::White) as usize) & (CORRECTION_HISTORY_SIZE - 1);
    let non_pawn_idx_b = (pos.non_pawn_key(Color::Black) as usize) & (CORRECTION_HISTORY_SIZE - 1);

    // continuation_value 用の事前計算
    let cont_params = if ply >= 2 {
        let prev_move = st.stack[(ply - 1) as usize].current_move;
        if prev_move.is_normal() {
            st.stack[(ply - 2) as usize].cont_hist_key.map(|prev2_key| {
                let pc = pos.piece_on(prev_move.to());
                (prev2_key.piece, prev2_key.to, pc, prev_move.to())
            })
        } else {
            None
        }
    } else {
        None
    };

    ctx.history.with_read(|h| {
        let pcv = h.correction_history.pawn_value(pawn_idx, us) as i32;
        let micv = h.correction_history.minor_value(minor_idx, us) as i32;
        let wnpcv = h.correction_history.non_pawn_value(non_pawn_idx_w, Color::White, us) as i32;
        let bnpcv = h.correction_history.non_pawn_value(non_pawn_idx_b, Color::Black, us) as i32;

        let cntcv = cont_params
            .map(|(piece, to, pc, prev_to)| {
                h.correction_history.continuation_value(piece, to, pc, prev_to) as i32
            })
            .unwrap_or(0);

        8867 * pcv + 8136 * micv + 10_757 * (wnpcv + bnpcv) + 7232 * cntcv
    })
}

/// 補正履歴の更新
#[inline]
pub(super) fn update_correction_history(
    st: &SearchState,
    ctx: &SearchContext<'_>,
    pos: &Position,
    ply: i32,
    bonus: i32,
) {
    let us = pos.side_to_move();
    let pawn_idx = (pos.pawn_key() as usize) & (CORRECTION_HISTORY_SIZE - 1);
    let minor_idx = (pos.minor_piece_key() as usize) & (CORRECTION_HISTORY_SIZE - 1);
    let non_pawn_idx_w = (pos.non_pawn_key(Color::White) as usize) & (CORRECTION_HISTORY_SIZE - 1);
    let non_pawn_idx_b = (pos.non_pawn_key(Color::Black) as usize) & (CORRECTION_HISTORY_SIZE - 1);

    // continuation_update 用の事前計算
    let cont_params = if ply >= 2 {
        let prev_move = st.stack[(ply - 1) as usize].current_move;
        if prev_move.is_normal() {
            st.stack[(ply - 2) as usize].cont_hist_key.map(|prev2_key| {
                let pc = pos.piece_on(prev_move.to());
                (prev2_key.piece, prev2_key.to, pc, prev_move.to())
            })
        } else {
            None
        }
    } else {
        None
    };

    const NON_PAWN_WEIGHT: i32 = 165;

    ctx.history.with_write(|h| {
        h.correction_history.update_pawn(pawn_idx, us, bonus);
        h.correction_history.update_minor(minor_idx, us, bonus * 153 / 128);
        h.correction_history.update_non_pawn(
            non_pawn_idx_w,
            Color::White,
            us,
            bonus * NON_PAWN_WEIGHT / 128,
        );
        h.correction_history.update_non_pawn(
            non_pawn_idx_b,
            Color::Black,
            us,
            bonus * NON_PAWN_WEIGHT / 128,
        );

        if let Some((piece, to, pc, prev_to)) = cont_params {
            h.correction_history
                .update_continuation(piece, to, pc, prev_to, bonus * 153 / 128);
        }
    });
}

// =============================================================================
// 置換表プローブ
// =============================================================================

/// 置換表プローブ
#[allow(clippy::too_many_arguments)]
pub(super) fn probe_transposition<const NT: u8>(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
    pos: &mut Position,
    depth: Depth,
    beta: Value,
    ply: i32,
    pv_node: bool,
    in_check: bool,
    excluded_move: Move,
) -> ProbeOutcome {
    let key = pos.key();
    let tt_result = ctx.tt.probe(key, pos);
    let tt_hit = tt_result.found;
    let tt_data = tt_result.data;

    st.stack[ply as usize].tt_hit = tt_hit;
    // excludedMoveがある場合は前回のttPvを維持（YaneuraOu準拠）
    st.stack[ply as usize].tt_pv = if excluded_move.is_some() {
        st.stack[ply as usize].tt_pv
    } else {
        pv_node || (tt_hit && tt_data.is_pv)
    };

    let tt_move = if tt_hit { tt_data.mv } else { Move::NONE };
    let tt_value = if tt_hit {
        value_from_tt(tt_data.value, ply)
    } else {
        Value::NONE
    };
    let tt_capture = tt_move.is_some() && pos.is_capture(tt_move);

    // TT統計収集
    inc_stat_by_depth!(st, tt_probe_by_depth, depth);
    if tt_hit {
        inc_stat_by_depth!(st, tt_hit_by_depth, depth);
    }

    // excludedMoveがある場合はカットオフしない（YaneuraOu準拠）
    if !pv_node
        && excluded_move.is_none()
        && tt_hit
        && tt_data.depth >= depth
        && tt_value != Value::NONE
        && tt_data.bound.can_cutoff(tt_value, beta)
    {
        return ProbeOutcome::Cutoff(tt_value);
    }

    // TTカットオフ失敗理由の統計
    #[cfg(feature = "search-stats")]
    if !pv_node && excluded_move.is_none() && tt_hit && tt_value != Value::NONE {
        if tt_data.depth < depth {
            inc_stat_by_depth!(st, tt_fail_depth_by_depth, depth);
        } else if !tt_data.bound.can_cutoff(tt_value, beta) {
            inc_stat_by_depth!(st, tt_fail_bound_by_depth, depth);
        }
    }

    // 1手詰め判定（置換表未ヒット時のみ、Rootでは実施しない）
    // excludedMoveがある場合も実施しない（詰みがあればsingular前にbeta cutするため）
    if NT != NodeType::Root as u8 && !in_check && !tt_hit && excluded_move.is_none() {
        let mate_move = pos.mate_1ply();
        if mate_move.is_some() {
            let value = Value::mate_in(ply + 1);
            let stored_depth = (depth + 6).min(MAX_PLY - 1);
            tt_result.write(
                key,
                value,
                st.stack[ply as usize].tt_pv,
                Bound::Exact,
                stored_depth,
                mate_move,
                Value::NONE,
                ctx.tt.generation(),
            );
            inc_stat_by_depth!(st, tt_write_by_depth, stored_depth);
            return ProbeOutcome::Cutoff(value);
        }
    }

    ProbeOutcome::Continue(TTContext {
        key,
        result: tt_result,
        data: tt_data,
        hit: tt_hit,
        mv: tt_move,
        value: tt_value,
        capture: tt_capture,
    })
}

// =============================================================================
// 静的評価コンテキスト
// =============================================================================

/// 静的評価と補正値の計算
///
/// # 引数
/// - `pv_node`: PVノードかどうか。PVノードでは必ずNNUE評価を実行する（YaneuraOu準拠）
#[allow(clippy::too_many_arguments)]
pub(super) fn compute_eval_context(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
    pos: &mut Position,
    ply: i32,
    in_check: bool,
    pv_node: bool,
    tt_ctx: &TTContext,
    excluded_move: Move,
) -> EvalContext {
    let corr_value = correction_value(st, ctx, pos, ply);

    // excludedMoveがある場合は、前回のstatic_evalをそのまま使用（YaneuraOu準拠）
    if excluded_move.is_some() {
        let static_eval = st.stack[ply as usize].static_eval;
        let improving = if ply >= 2 && !in_check && static_eval != Value::NONE {
            static_eval > st.stack[(ply - 2) as usize].static_eval
        } else {
            false
        };
        let opponent_worsening = if ply >= 1 && static_eval != Value::NONE {
            let prev_eval = st.stack[(ply - 1) as usize].static_eval;
            prev_eval != Value::NONE && static_eval > -prev_eval
        } else {
            false
        };
        return EvalContext {
            static_eval,
            unadjusted_static_eval: static_eval, // excludedMove時は未補正値も同じ
            correction_value: corr_value,
            improving,
            opponent_worsening,
        };
    }

    let mut unadjusted_static_eval = Value::NONE;
    // YaneuraOu準拠: TTからのeval取得 + PvNodeでは必ずevaluate()
    // yaneuraou-search.cpp:2680-2706 参照
    // 「🌈 これ書かないとR70ぐらい弱くなる。」

    // デバッグ: TTヒット時のeval状態を確認
    #[cfg(feature = "search-stats")]
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static TT_EVAL_VALID: AtomicU64 = AtomicU64::new(0);
        static TT_EVAL_NONE: AtomicU64 = AtomicU64::new(0);
        static TT_MISS: AtomicU64 = AtomicU64::new(0);
        static TT_PV_NODE: AtomicU64 = AtomicU64::new(0);

        if !in_check {
            if tt_ctx.hit {
                if tt_ctx.data.eval != Value::NONE {
                    if !pv_node {
                        TT_EVAL_VALID.fetch_add(1, Ordering::Relaxed);
                    } else {
                        TT_PV_NODE.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    TT_EVAL_NONE.fetch_add(1, Ordering::Relaxed);
                }
            } else {
                TT_MISS.fetch_add(1, Ordering::Relaxed);
            }
        }

        // 一定間隔でログ出力
        let total = TT_EVAL_VALID.load(Ordering::Relaxed)
            + TT_EVAL_NONE.load(Ordering::Relaxed)
            + TT_MISS.load(Ordering::Relaxed)
            + TT_PV_NODE.load(Ordering::Relaxed);
        if total > 0 && total.is_multiple_of(100000) {
            eprintln!(
                "[TT-EVAL-DEBUG] valid={}, none={}, miss={}, pv={}",
                TT_EVAL_VALID.load(Ordering::Relaxed),
                TT_EVAL_NONE.load(Ordering::Relaxed),
                TT_MISS.load(Ordering::Relaxed),
                TT_PV_NODE.load(Ordering::Relaxed),
            );
        }
    }

    let mut static_eval = if in_check {
        Value::NONE
    } else if tt_ctx.hit && tt_ctx.data.eval != Value::NONE && !pv_node {
        // TTヒット && eval有効 && 非PVノード → TTからevalを取得
        ensure_nnue_accumulator(st, pos);
        unadjusted_static_eval = tt_ctx.data.eval;

        // デバッグ: TTから取得したevalとNNUE評価を比較
        #[cfg(feature = "search-stats")]
        {
            use std::sync::atomic::{AtomicU64, Ordering};
            static EVAL_MATCH: AtomicU64 = AtomicU64::new(0);
            static EVAL_MISMATCH: AtomicU64 = AtomicU64::new(0);

            let nnue_eval = nnue_evaluate(st, pos);
            if unadjusted_static_eval == nnue_eval {
                EVAL_MATCH.fetch_add(1, Ordering::Relaxed);
            } else {
                EVAL_MISMATCH.fetch_add(1, Ordering::Relaxed);
                // 不一致時の差分を出力（最初の10回のみ）
                static MISMATCH_LOG_COUNT: AtomicU64 = AtomicU64::new(0);
                let log_count = MISMATCH_LOG_COUNT.fetch_add(1, Ordering::Relaxed);
                if log_count < 10 {
                    eprintln!(
                        "[EVAL-MISMATCH] tt_eval={}, nnue_eval={}, diff={}",
                        unadjusted_static_eval.raw(),
                        nnue_eval.raw(),
                        (unadjusted_static_eval.raw() - nnue_eval.raw()).abs()
                    );
                }
            }

            let m = EVAL_MATCH.load(Ordering::Relaxed);
            let mm = EVAL_MISMATCH.load(Ordering::Relaxed);
            let total = m + mm;
            // 毎回出力（デバッグ用）
            if total == 1
                || total == 100
                || total == 1000
                || total == 5000
                || total == 10000
                || total == 18000
            {
                eprintln!(
                    "[EVAL-COMPARE] match={}, mismatch={} (mismatch rate: {:.2}%)",
                    m,
                    mm,
                    if total > 0 {
                        mm as f64 / total as f64 * 100.0
                    } else {
                        0.0
                    },
                );
            }
        }

        unadjusted_static_eval
    } else {
        // PVノード または TTミス/eval無効 → 常にNNUE評価
        unadjusted_static_eval = nnue_evaluate(st, pos);
        unadjusted_static_eval
    };

    if !in_check && unadjusted_static_eval != Value::NONE {
        static_eval = to_corrected_static_eval(unadjusted_static_eval, corr_value);
        // パス権評価を動的に追加（TTには保存されないので手数依存でもOK）
        static_eval += evaluate_pass_rights(pos, pos.game_ply() as u16);
    }

    if !in_check
        && tt_ctx.hit
        && tt_ctx.value != Value::NONE
        && !tt_ctx.value.is_mate_score()
        && ((tt_ctx.value > static_eval && tt_ctx.data.bound == Bound::Lower)
            || (tt_ctx.value < static_eval && tt_ctx.data.bound == Bound::Upper))
    {
        static_eval = tt_ctx.value;
    }

    st.stack[ply as usize].static_eval = static_eval;

    let improving = if ply >= 2 && !in_check {
        static_eval > st.stack[(ply - 2) as usize].static_eval
    } else {
        false
    };
    let opponent_worsening = if ply >= 1 && static_eval != Value::NONE {
        let prev_eval = st.stack[(ply - 1) as usize].static_eval;
        prev_eval != Value::NONE && static_eval > -prev_eval
    } else {
        false
    };

    EvalContext {
        static_eval,
        unadjusted_static_eval,
        correction_value: corr_value,
        improving,
        opponent_worsening,
    }
}
