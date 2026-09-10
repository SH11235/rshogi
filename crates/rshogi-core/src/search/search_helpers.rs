//! 探索ヘルパー関数群
//!
//! NNUE操作、ContinuationHistory、中断チェック等の基本操作。

use std::ptr::NonNull;

use crate::eval::{EvalHash, eval_hash_enabled};
#[cfg(feature = "use-lazy-evaluate")]
use crate::nnue::ensure_accumulator_computed;
#[cfg(feature = "layerstack-arch")]
use crate::nnue::{AccumulatorStackVariant, update_and_evaluate_layer_stacks_cached};
use crate::nnue::{DirtyPiece, evaluate_dispatch};
use crate::position::Position;
use crate::prefetch::TtPrefetch;
use crate::search::PieceToHistory;
use crate::types::{Move, Piece, Square, Value};

use super::alpha_beta::{SearchContext, SearchState};
use super::history::CorrectionPieceToHistory;
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

    // 頻度制御：512回に1回だけ実際のチェックを行う
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

    // ponder 中は予算を使い切っても、GUI の通知まで探索を継続する。
    if limits.nodes > 0 && !time_manager.is_pondering() && st.nodes >= limits.nodes {
        #[cfg(debug_assertions)]
        eprintln!("check_abort: node limit reached nodes={} limit={}", st.nodes, limits.nodes);
        st.abort = true;
        return true;
    }

    // 時間制限チェック（main threadのみ）
    // 2フェーズロジック
    if ctx.thread_id == 0 {
        // ponderhit フラグをポーリングし、検知したら通常探索へ切り替える
        if time_manager.take_ponderhit() {
            time_manager.on_ponderhit();
        }

        let elapsed = time_manager.elapsed();
        let elapsed_effective = time_manager.elapsed_from_ponderhit();

        // フェーズ1: search_end 設定済み → 即座に停止
        if !time_manager.is_pondering()
            && time_manager.search_end() > 0
            && elapsed >= time_manager.search_end()
        {
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
        // ただし ponder 中は停止判定を行わない
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
///
/// `layerstack-arch` feature かつ実行中ネットワークが LayerStacks のときは
/// `evaluate_dispatch` をバイパスし、`network_ptr` から直接 LayerStacks 評価を呼ぶ。
/// これにより `get_network()` の RwLock::read + Arc::clone を完全回避する。
/// HalfKX 系ネットワークがロードされている場合は通常の `evaluate_dispatch` を使う。
#[inline]
pub(super) fn nnue_evaluate(st: &mut SearchState, pos: &Position) -> Value {
    #[cfg(feature = "layerstack-arch")]
    {
        let ptr = st.network_ptr;
        if !ptr.is_null()
            && let AccumulatorStackVariant::LayerStacks(ref mut s) = st.nnue_stack
        {
            // SAFETY: network_ptr は reset() で Arc::as_ptr() から設定。
            // Arc は NETWORK の RwLock 内に保持され、探索中に drop されない。
            // nnue_stack が LayerStacks variant のとき network も LayerStacks
            // (reset() で from_network により対応付けされる) と保証されているため、
            // as_layer_stacks() は panic しない。
            let network = unsafe { &*ptr };
            let net = network.as_layer_stacks();
            return update_and_evaluate_layer_stacks_cached(net, pos, s, &mut st.acc_cache);
        }
    }
    #[cfg(feature = "layerstack-arch")]
    let acc_cache = &mut st.acc_cache;
    #[cfg(not(feature = "layerstack-arch"))]
    let acc_cache = &mut None;
    evaluate_dispatch(pos, &mut st.nnue_stack, acc_cache)
}

/// EvalHash を介した NNUE 静的評価（YaneuraOu の `Eval::evaluate` 相当）
///
/// hit 時はアキュムレータを更新しない。後続ノードの `update_accumulator` は
/// 未計算の祖先を遡って差分適用 / refresh するため、skip しても整合は保たれる。
#[inline]
pub(super) fn nnue_evaluate_cached(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
    pos: &Position,
) -> Value {
    if !eval_hash_enabled() {
        return nnue_evaluate(st, pos);
    }
    let key = pos.key();
    if let Some(raw) = ctx.eval_hash.probe(key) {
        return Value::new(raw);
    }
    let value = nnue_evaluate(st, pos);
    ctx.eval_hash.store(key, value.raw());
    value
}

/// NNUE アキュムレータを計算済みにする（評価値の計算はしない）
///
/// `use-lazy-evaluate` 有効時のみ使用する。
/// TT eval を再利用する経路で、後続の差分更新の整合を保つために必要。
#[cfg(feature = "use-lazy-evaluate")]
#[inline]
pub(super) fn ensure_nnue_accumulator(st: &mut SearchState, pos: &Position) {
    #[cfg(feature = "layerstack-arch")]
    let acc_cache = &mut st.acc_cache;
    #[cfg(not(feature = "layerstack-arch"))]
    let acc_cache = &mut None;
    ensure_accumulator_computed(pos, &mut st.nnue_stack, acc_cache)
}

/// do_move + nodes++ + nnue_push をまとめたラッパー
///
/// YO では Worker::do_move() 内部で nodes++ と nnue push を行う。
/// rshogi でも同等の一括処理を提供する。
#[inline]
pub(super) fn do_move_and_push<P: TtPrefetch>(
    st: &mut SearchState,
    pos: &mut Position,
    mv: Move,
    gives_check: bool,
    prefetcher: &P,
    eval_hash: &EvalHash,
) {
    let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, prefetcher);
    if eval_hash_enabled() {
        eval_hash.prefetch(pos.key());
    }
    st.nodes += 1;
    st.nnue_stack.push(dirty_piece);
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
    // SAFETY: cont_history_ptrは常に有効なNonNullポインタを返す
    // - ply >= back の場合: st.stack[(ply-back)].cont_history_ptr から取得
    //   スタックエントリは SearchState のライフタイム 'a で有効
    // - ply < back の場合: ctx.cont_history_sentinel（静的に確保されたsentinel）
    // いずれの場合もポインタは 'a の間有効であり、参照への変換は安全
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
    // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない
    let (table, correction_table) = {
        let h = unsafe { ctx.history.as_ref_unchecked() };
        (
            NonNull::from(h.continuation_history[in_check_idx][capture_idx].get_table(piece, to)),
            NonNull::<CorrectionPieceToHistory>::from(
                h.correction_history.continuation_table(piece, to),
            ),
        )
    };
    // SAFETY: ply < MAX_PLY < STACK_SIZE は上の debug_assert で検証済み。
    let ss = unsafe { st.stack.get_unchecked_mut(ply as usize) };
    ss.cont_history_ptr = table;
    ss.cont_correction_ptr = correction_table;
    ss.cont_hist_key = Some(ContHistKey::new(in_check, capture, piece, to));
}

/// ContinuationHistory をクリア（null move用）
/// sentinelテーブル(NO_PIECE, SQ_ZERO)を参照するようにする。
/// cont_hist_keyをNoneにするとcorrection historyのcontinuation更新がスキップされてしまうため、
/// sentinel keyを設定してYOと同じsentinelテーブルへの読み書きが行われるようにする。
#[inline]
pub(super) fn clear_cont_history_for_null(st: &mut SearchState, ctx: &SearchContext<'_>, ply: i32) {
    let correction_sentinel = {
        // SAFETY: 単一スレッド内で使用、可変参照と同時保持しない。
        let h = unsafe { ctx.history.as_ref_unchecked() };
        NonNull::from(h.correction_history.continuation_table(Piece::NONE, Square::SQ_11))
    };
    // SAFETY: ply < MAX_PLY < STACK_SIZE。
    let ss = unsafe { st.stack.get_unchecked_mut(ply as usize) };
    ss.cont_history_ptr = ctx.cont_history_sentinel;
    ss.cont_correction_ptr = correction_sentinel;
    ss.cont_hist_key = Some(ContHistKey::null_sentinel());
}

// =============================================================================
// その他のヘルパー
// =============================================================================

/// 親ノードのreductionを取得してクリア
#[inline]
pub(super) fn take_prior_reduction(st: &mut SearchState, ply: i32) -> i32 {
    if ply >= 1 {
        let parent_idx = (ply - 1) as usize;
        // SAFETY: ply >= 1 なので parent_idx >= 0。ply < MAX_PLY < STACK_SIZE。
        let ss = unsafe { st.stack.get_unchecked_mut(parent_idx) };
        let pr = ss.reduction;
        ss.reduction = 0;
        pr
    } else {
        0
    }
}
