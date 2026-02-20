//! 評価・補正ヘルパー関数群
//!
//! 補正履歴、静的評価コンテキスト、置換表プローブ等。

#[cfg(not(feature = "search-no-pass-rules"))]
use crate::eval::evaluate_pass_rights;
use crate::position::Position;
use crate::types::{Bound, Color, DEPTH_UNSEARCHED, Depth, MAX_PLY, Move, Value};

use super::alpha_beta::{
    EvalContext, ProbeOutcome, SearchContext, SearchState, TTContext, to_corrected_static_eval,
};
use super::history::CORRECTION_HISTORY_SIZE;
#[cfg(feature = "use-lazy-evaluate")]
use super::search_helpers::ensure_nnue_accumulator;
use super::search_helpers::nnue_evaluate;
use super::stats::inc_stat_by_depth;
#[cfg(feature = "tt-trace")]
use super::tt_sanity::{
    InvalidTtLog, TtCutoffTrace, TtProbeTrace, TtWriteTrace, helper_tt_write_enabled_for_depth,
    maybe_log_invalid_tt_data, maybe_trace_tt_cutoff, maybe_trace_tt_probe, maybe_trace_tt_write,
};
use super::tt_sanity::{is_valid_tt_eval, is_valid_tt_stored_value};
use super::types::{ContHistKey, NodeType, value_from_tt};

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

    // YO準拠: (ss-1)->currentMove を使って continuation correction を参照
    let prev_move = if ply >= 1 {
        st.stack[(ply - 1) as usize].current_move
    } else {
        Move::NONE
    };
    let move_ok = prev_move.is_normal();

    // continuation correction 用キー: (ss-2) と (ss-4) の2段階（YO準拠）
    // YO準拠: cont_hist_key が未設定(None)でも sentinel にフォールバックして参照する。
    let sentinel_key = ContHistKey::null_sentinel();
    let cont_key_2 = if move_ok {
        if ply >= 2 {
            match st.stack[(ply - 2) as usize].cont_hist_key {
                Some(key) => key,
                None => sentinel_key,
            }
        } else {
            sentinel_key
        }
    } else {
        sentinel_key
    };
    let cont_key_4 = if move_ok {
        if ply >= 4 {
            match st.stack[(ply - 4) as usize].cont_hist_key {
                Some(key) => key,
                None => sentinel_key,
            }
        } else {
            sentinel_key
        }
    } else {
        sentinel_key
    };
    // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
    let h = unsafe { ctx.history.as_ref_unchecked() };
    let pcv = h.correction_history.pawn_value(pawn_idx, us) as i32;
    let micv = h.correction_history.minor_value(minor_idx, us) as i32;
    let wnpcv = h.correction_history.non_pawn_value(non_pawn_idx_w, Color::White, us) as i32;
    let bnpcv = h.correction_history.non_pawn_value(non_pawn_idx_b, Color::Black, us) as i32;

    // YO準拠: move無効の場合はcntcv全体が8（個別デフォルトの合計ではない）
    let cntcv = if move_ok {
        let pc = pos.piece_on(prev_move.to());
        let cv2 = h.correction_history.continuation_value(
            cont_key_2.piece,
            cont_key_2.to,
            pc,
            prev_move.to(),
        ) as i32;
        let cv4 = h.correction_history.continuation_value(
            cont_key_4.piece,
            cont_key_4.to,
            pc,
            prev_move.to(),
        ) as i32;
        cv2 + cv4
    } else {
        8
    };

    9536 * pcv + 8494 * micv + 10_132 * (wnpcv + bnpcv) + 7156 * cntcv
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

    // YO準拠: (ss-1)->currentMove を使って continuation correction を更新
    let prev_move = if ply >= 1 {
        st.stack[(ply - 1) as usize].current_move
    } else {
        Move::NONE
    };
    let move_ok = prev_move.is_normal();

    // (ss-2)/(ss-4) context — YO準拠: cont_hist_key が未設定(None)でも sentinel へフォールバック
    let sentinel_key = ContHistKey::null_sentinel();
    let cont_key_2 = if ply >= 2 {
        match st.stack[(ply - 2) as usize].cont_hist_key {
            Some(key) => key,
            None => sentinel_key,
        }
    } else {
        sentinel_key
    };
    let cont_key_4 = if ply >= 4 {
        match st.stack[(ply - 4) as usize].cont_hist_key {
            Some(key) => key,
            None => sentinel_key,
        }
    } else {
        sentinel_key
    };

    const NON_PAWN_WEIGHT: i32 = 165;

    // SAFETY: 単一スレッド内で使用、他の参照と同時保持しない
    let h = unsafe { ctx.history.as_mut_unchecked() };
    h.correction_history.update_pawn(pawn_idx, us, bonus);
    h.correction_history.update_minor(minor_idx, us, bonus * 156 / 128);
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

    // YO準拠: continuation(ss-2) 重み 137/128, (ss-4) 重み 64/128
    // YO準拠: m.is_ok()のときはcontinuation correctionを常に更新（sentinelテーブル含む）
    if move_ok {
        let pc = pos.piece_on(prev_move.to());
        let prev_to = prev_move.to();
        h.correction_history.update_continuation(
            cont_key_2.piece,
            cont_key_2.to,
            pc,
            prev_to,
            bonus * 137 / 128,
        );
        h.correction_history.update_continuation(
            cont_key_4.piece,
            cont_key_4.to,
            pc,
            prev_to,
            bonus * 64 / 128,
        );
    }
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
    cut_node: bool,
) -> ProbeOutcome {
    let key = pos.key();
    let tt_result = ctx.tt.probe(key, pos);
    let tt_hit = tt_result.found;
    let mut tt_data = tt_result.data;

    st.stack[ply as usize].tt_hit = tt_hit;
    // excludedMoveがある場合は前回のttPvを維持（YaneuraOu準拠）
    st.stack[ply as usize].tt_pv = if excluded_move.is_some() {
        st.stack[ply as usize].tt_pv
    } else {
        pv_node || (tt_hit && tt_data.is_pv)
    };

    // YaneuraOu準拠: alpha-beta では ttHit で ttMove を潰さない。
    // probe() 側で to_move 変換に失敗した手は除外済み。
    let tt_move = tt_data.mv;
    let mut tt_value = if tt_hit {
        value_from_tt(tt_data.value, ply)
    } else {
        Value::NONE
    };
    if tt_hit && !is_valid_tt_stored_value(tt_data.value) {
        #[cfg(feature = "tt-trace")]
        maybe_log_invalid_tt_data(InvalidTtLog {
            reason: "invalid_value",
            stage: "ab_probe",
            thread_id: ctx.thread_id,
            ply,
            key,
            depth: tt_data.depth,
            bound: tt_data.bound,
            tt_move,
            stored_value: tt_data.value,
            converted_value: tt_value,
            eval: tt_data.eval,
        });
        tt_value = Value::NONE;
    }
    if tt_hit && !is_valid_tt_eval(tt_data.eval) {
        #[cfg(feature = "tt-trace")]
        maybe_log_invalid_tt_data(InvalidTtLog {
            reason: "invalid_eval",
            stage: "ab_probe",
            thread_id: ctx.thread_id,
            ply,
            key,
            depth: tt_data.depth,
            bound: tt_data.bound,
            tt_move,
            stored_value: tt_data.value,
            converted_value: tt_value,
            eval: tt_data.eval,
        });
        tt_data.eval = Value::NONE;
    }
    #[cfg(feature = "tt-trace")]
    maybe_trace_tt_probe(TtProbeTrace {
        stage: "ab_probe",
        thread_id: ctx.thread_id,
        ply,
        key,
        hit: tt_hit,
        depth: tt_data.depth,
        bound: tt_data.bound,
        tt_move,
        stored_value: tt_data.value,
        converted_value: tt_value,
        eval: tt_data.eval,
        root_move: if ply >= 1 {
            st.stack[0].current_move
        } else {
            Move::NONE
        },
    });
    let tt_capture = tt_move.is_some() && pos.capture_stage(tt_move);

    // TT統計収集
    inc_stat_by_depth!(st, tt_probe_by_depth, depth);
    if tt_hit {
        inc_stat_by_depth!(st, tt_hit_by_depth, depth);
    }

    // YaneuraOu準拠のTTカットオフ条件（yaneuraou-search.cpp:2331-2337）
    // - fail-high時は tt_data.depth > depth を要求（fail-low時は >= depth）
    // - depth<=5ではcutNodeとTT値の方向が一致する場合のみカットオフ許可
    let tt_value_lte_beta = tt_value != Value::NONE && tt_value.raw() <= beta.raw();
    if !pv_node
        && excluded_move.is_none()
        && tt_hit
        && tt_data.depth > depth - tt_value_lte_beta as i32
        && tt_value != Value::NONE
        && tt_data.bound.can_cutoff(tt_value, beta)
        && (cut_node == (tt_value.raw() >= beta.raw()) || depth > 5)
    {
        #[cfg(feature = "tt-trace")]
        maybe_trace_tt_cutoff(TtCutoffTrace {
            stage: "ab_probe_cutoff",
            thread_id: ctx.thread_id,
            ply,
            key,
            search_depth: depth,
            depth: tt_data.depth,
            bound: tt_data.bound,
            value: tt_value,
            beta,
            root_move: if ply >= 1 {
                st.stack[0].current_move
            } else {
                Move::NONE
            },
        });
        return ProbeOutcome::Cutoff {
            value: tt_value,
            tt_move,
            tt_capture,
        };
    }

    // TTカットオフ失敗理由の統計
    #[cfg(feature = "search-stats")]
    if !pv_node && excluded_move.is_none() && tt_hit && tt_value != Value::NONE {
        if tt_data.depth <= depth - tt_value_lte_beta as i32 {
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
            #[cfg(feature = "tt-trace")]
            let allow_write = ctx.allow_tt_write
                && helper_tt_write_enabled_for_depth(ctx.thread_id, Bound::Exact, stored_depth);
            #[cfg(not(feature = "tt-trace"))]
            let allow_write = ctx.allow_tt_write;
            if allow_write {
                #[cfg(feature = "tt-trace")]
                maybe_trace_tt_write(TtWriteTrace {
                    stage: "ab_mate1_store",
                    thread_id: ctx.thread_id,
                    ply,
                    key,
                    depth: stored_depth,
                    bound: Bound::Exact,
                    is_pv: st.stack[ply as usize].tt_pv,
                    tt_move: mate_move,
                    stored_value: value,
                    eval: Value::NONE,
                    root_move: if ply >= 1 {
                        st.stack[0].current_move
                    } else {
                        Move::NONE
                    },
                });
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
            }
            // 1手詰めカットオフではヒストリ更新不要（mate_moveは特殊）
            return ProbeOutcome::Cutoff {
                value,
                tt_move: Move::NONE,
                tt_capture: false,
            };
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
        // YaneuraOu準拠: improving/opponentWorsening は VALUE_NONE を含めた生比較で算出する。
        let prev2_eval = if ply >= 2 {
            st.stack[(ply - 2) as usize].static_eval
        } else {
            Value::NONE
        };
        let prev_eval = if ply >= 1 {
            st.stack[(ply - 1) as usize].static_eval
        } else {
            Value::NONE
        };
        let improving = static_eval > prev2_eval;
        let opponent_worsening = static_eval > -prev_eval;
        return EvalContext {
            eval: static_eval,
            static_eval,
            unadjusted_static_eval: static_eval, // excludedMove時は未補正値も同じ
            correction_value: corr_value,
            improving,
            opponent_worsening,
        };
    }

    let mut unadjusted_static_eval = Value::NONE;

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

    // YaneuraOu準拠: TTからのeval取得 + PvNodeでは必ずevaluate()
    // yaneuraou-search.cpp:2680-2706 参照
    // 「🌈 これ書かないとR70ぐらい弱くなる。」
    let mut static_eval = if in_check {
        // YaneuraOu準拠: in-check では (ss-2)->staticEval を継承する。
        // root直下 (ply < 2) は参照先がないため VALUE_NONE を使う。
        if ply >= 2 {
            st.stack[(ply - 2) as usize].static_eval
        } else {
            Value::NONE
        }
    } else if tt_ctx.hit && tt_ctx.data.eval != Value::NONE && !pv_node {
        // TTヒット && eval有効 && 非PVノード
        #[cfg(feature = "use-lazy-evaluate")]
        {
            // USE_LAZY_EVALUATE相当: TT eval を再利用する。
            // 後続の差分更新に備え、アキュムレータだけは計算済みにしておく。
            ensure_nnue_accumulator(st, pos);
            unadjusted_static_eval = tt_ctx.data.eval;
        }
        #[cfg(not(feature = "use-lazy-evaluate"))]
        {
            // YO現行ビルド整合モード: 常に NNUE 再評価する。
            // TT eval 再利用による type-1 collision 伝播を避ける。
            unadjusted_static_eval = nnue_evaluate(st, pos);
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
        let pass_rights_eval = {
            #[cfg(feature = "search-no-pass-rules")]
            {
                Value::ZERO
            }
            #[cfg(not(feature = "search-no-pass-rules"))]
            {
                evaluate_pass_rights(pos, pos.game_ply() as u16)
            }
        };
        static_eval += pass_rights_eval;
    }

    // YO準拠: TTミス時は eval のみを BOUND_NONE/DEPTH_UNSEARCHED で保存する。
    #[cfg(feature = "tt-trace")]
    let eval_allow_write = !in_check
        && !tt_ctx.hit
        && ctx.allow_tt_write
        && helper_tt_write_enabled_for_depth(ctx.thread_id, Bound::None, DEPTH_UNSEARCHED);
    #[cfg(not(feature = "tt-trace"))]
    let eval_allow_write = !in_check && !tt_ctx.hit && ctx.allow_tt_write;
    if eval_allow_write {
        #[cfg(feature = "tt-trace")]
        maybe_trace_tt_write(TtWriteTrace {
            stage: "ab_eval_store_none",
            thread_id: ctx.thread_id,
            ply,
            key: tt_ctx.key,
            depth: DEPTH_UNSEARCHED,
            bound: Bound::None,
            is_pv: st.stack[ply as usize].tt_pv,
            tt_move: Move::NONE,
            stored_value: Value::NONE,
            eval: unadjusted_static_eval,
            root_move: if ply >= 1 {
                st.stack[0].current_move
            } else {
                Move::NONE
            },
        });
        tt_ctx.result.write(
            tt_ctx.key,
            Value::NONE,
            st.stack[ply as usize].tt_pv,
            Bound::None,
            DEPTH_UNSEARCHED,
            Move::NONE,
            unadjusted_static_eval,
            ctx.tt.generation(),
        );
        inc_stat_by_depth!(st, tt_write_by_depth, 0);
    }

    // YOの `eval` 相当: static_eval をベースに、TT境界値で補正する。
    let mut eval = static_eval;
    if !in_check && tt_ctx.hit && tt_ctx.value != Value::NONE && {
        if tt_ctx.value > eval {
            tt_ctx.data.bound.is_lower_or_exact()
        } else {
            matches!(tt_ctx.data.bound, Bound::Upper | Bound::Exact)
        }
    } {
        eval = tt_ctx.value;
    }

    // YO準拠: improving / opponentWorsening は ss->staticEval ベースで
    // VALUE_NONE を含めた生比較で計算する。
    st.stack[ply as usize].static_eval = static_eval;

    let prev2_eval = if ply >= 2 {
        st.stack[(ply - 2) as usize].static_eval
    } else {
        Value::NONE
    };
    let prev_eval = if ply >= 1 {
        st.stack[(ply - 1) as usize].static_eval
    } else {
        Value::NONE
    };
    let improving = static_eval > prev2_eval;
    let opponent_worsening = static_eval > -prev_eval;

    EvalContext {
        eval,
        static_eval,
        unadjusted_static_eval,
        correction_value: corr_value,
        improving,
        opponent_worsening,
    }
}
