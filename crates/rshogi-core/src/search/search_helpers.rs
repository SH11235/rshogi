//! 探索ヘルパー関数群
//!
//! NNUE操作、ContinuationHistory、中断チェック等の基本操作。

use std::ptr::NonNull;

use crate::nnue::{evaluate_dispatch, DirtyPiece};
use crate::position::Position;
use crate::search::PieceToHistory;
use crate::types::{Piece, Square, Value};

use super::alpha_beta::{SearchContext, SearchState};
use super::types::{ContHistKey, STACK_SIZE};
use super::{LimitsType, TimeManagement};

// =============================================================================
// 中断チェック
// =============================================================================

/// 中断チェック
#[inline]
pub(super) fn check_abort(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
) -> bool {
    // すでにabortフラグが立っている場合は即座に返す
    if st.abort {
        #[cfg(debug_assertions)]
        eprintln!("check_abort: abort flag already set");
        return true;
    }

    // 頻度制御：512回に1回だけ実際のチェックを行う（YaneuraOu準拠）
    st.calls_cnt -= 1;
    if st.calls_cnt > 0 {
        return false;
    }
    // カウンターをリセット
    st.calls_cnt = if limits.nodes > 0 {
        std::cmp::min(512, (limits.nodes / 1024) as i32).max(1)
    } else {
        512
    };

    // 外部からの停止要求
    if time_manager.stop_requested() {
        #[cfg(debug_assertions)]
        eprintln!("check_abort: stop requested");
        st.abort = true;
        return true;
    }

    // ノード数制限チェック
    if limits.nodes > 0 && st.nodes >= limits.nodes {
        #[cfg(debug_assertions)]
        eprintln!("check_abort: node limit reached nodes={} limit={}", st.nodes, limits.nodes);
        st.abort = true;
        return true;
    }

    // 時間制限チェック（main threadのみ）
    // YaneuraOu準拠の2フェーズロジック
    if ctx.thread_id == 0 {
        // ponderhit フラグをポーリングし、検知したら通常探索へ切り替える
        if time_manager.take_ponderhit() {
            time_manager.on_ponderhit();
        }

        let elapsed = time_manager.elapsed();
        let elapsed_effective = time_manager.elapsed_from_ponderhit();

        // フェーズ1: search_end 設定済み → 即座に停止
        if time_manager.search_end() > 0 && elapsed >= time_manager.search_end() {
            #[cfg(debug_assertions)]
            eprintln!(
                "check_abort: search_end reached elapsed={} search_end={}",
                elapsed,
                time_manager.search_end()
            );
            st.abort = true;
            return true;
        }

        // フェーズ2: search_end 未設定 → maximum超過 or stop_on_ponderhit で設定
        // ただし ponder 中は停止判定を行わない（YO準拠）
        if !time_manager.is_pondering()
            && time_manager.search_end() == 0
            && limits.use_time_management()
            && (elapsed_effective > time_manager.maximum() || time_manager.stop_on_ponderhit())
        {
            time_manager.set_search_end(elapsed);
            // 注: ここでは停止せず、次のチェックで秒境界で停止
        }
    }

    false
}

// =============================================================================
// NNUE操作
// =============================================================================

/// NNUE 評価
#[inline]
pub(super) fn nnue_evaluate(st: &mut SearchState, pos: &Position) -> Value {
    evaluate_dispatch(pos, &mut st.nnue_stack)
}

/// NNUE push
#[inline]
pub(super) fn nnue_push(st: &mut SearchState, dirty_piece: DirtyPiece) {
    st.nnue_stack.push(dirty_piece);
}

/// NNUE pop
#[inline]
pub(super) fn nnue_pop(st: &mut SearchState) {
    st.nnue_stack.pop();
}

// =============================================================================
// ContinuationHistory 操作
// =============================================================================

/// ContinuationHistory ポインタを取得
#[inline]
pub(super) fn cont_history_ptr(
    st: &SearchState,
    ctx: &SearchContext<'_>,
    ply: i32,
    back: i32,
) -> NonNull<PieceToHistory> {
    debug_assert!(ply >= 0 && (ply as usize) < STACK_SIZE, "ply out of bounds: {ply}");
    debug_assert!(back >= 0, "back must be non-negative: {back}");
    if ply >= back {
        st.stack[(ply - back) as usize].cont_history_ptr
    } else {
        ctx.cont_history_sentinel
    }
}

/// ContinuationHistory 参照を取得
#[inline]
pub(super) fn cont_history_ref<'a>(
    st: &'a SearchState,
    ctx: &SearchContext<'_>,
    ply: i32,
    back: i32,
) -> &'a PieceToHistory {
    let ptr = cont_history_ptr(st, ctx, ply, back);
    unsafe { ptr.as_ref() }
}

/// ContinuationHistory テーブル配列を取得
#[inline]
pub(super) fn cont_history_tables<'a>(
    st: &'a SearchState,
    ctx: &SearchContext<'_>,
    ply: i32,
) -> [&'a PieceToHistory; 6] {
    [
        cont_history_ref(st, ctx, ply, 1),
        cont_history_ref(st, ctx, ply, 2),
        cont_history_ref(st, ctx, ply, 3),
        cont_history_ref(st, ctx, ply, 4),
        cont_history_ref(st, ctx, ply, 5),
        cont_history_ref(st, ctx, ply, 6),
    ]
}

/// ContinuationHistory を設定
#[inline]
pub(super) fn set_cont_history_for_move(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
    ply: i32,
    in_check: bool,
    capture: bool,
    piece: Piece,
    to: Square,
) {
    debug_assert!(ply >= 0 && (ply as usize) < STACK_SIZE, "ply out of bounds: {ply}");
    let in_check_idx = in_check as usize;
    let capture_idx = capture as usize;
    let table = ctx.history.with_read(|h| {
        NonNull::from(h.continuation_history[in_check_idx][capture_idx].get_table(piece, to))
    });
    st.stack[ply as usize].cont_history_ptr = table;
    st.stack[ply as usize].cont_hist_key = Some(ContHistKey::new(in_check, capture, piece, to));
}

/// ContinuationHistory をクリア（null move用）
#[inline]
pub(super) fn clear_cont_history_for_null(st: &mut SearchState, ctx: &SearchContext<'_>, ply: i32) {
    st.stack[ply as usize].cont_history_ptr = ctx.cont_history_sentinel;
    st.stack[ply as usize].cont_hist_key = None;
}

// =============================================================================
// その他のヘルパー
// =============================================================================

/// 親ノードのreductionを取得してクリア
#[inline]
pub(super) fn take_prior_reduction(st: &mut SearchState, ply: i32) -> i32 {
    if ply >= 1 {
        let parent_idx = (ply - 1) as usize;
        let pr = st.stack[parent_idx].reduction;
        st.stack[parent_idx].reduction = 0;
        pr
    } else {
        0
    }
}
