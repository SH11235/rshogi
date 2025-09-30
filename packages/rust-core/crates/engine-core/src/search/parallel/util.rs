//! Shared helpers for parallel search timing logic.

use crate::search::constants::{MAIN_NEAR_DEADLINE_WINDOW_MS, NEAR_HARD_FINALIZE_MS};

/// Default上限: finalize後の衛生待機で許可する最大待ち時間 (ms)
pub(crate) const HYGIENE_WAIT_MAX_MS: u64 = 50;
/// 残余時間が十分な場合の拡張上限 (ms)
pub(crate) const HYGIENE_WAIT_EXTENDED_MAX_MS: u64 = 250;
/// 待機ループのステップ幅 (ms)。5ms 刻みで sleep させる。
pub(crate) const HYGIENE_WAIT_STEP_MS: u64 = 5;
/// finalize後も確保しておきたい思考リード (ms)
pub(crate) const HYGIENE_LEAD_MS: u64 = 100;
/// finalize後に発生し得る入出力オーバーヘッド (ms)
pub(crate) const HYGIENE_IO_MS: u64 = 15;

const HYGIENE_RESERVED_MS: u64 = HYGIENE_LEAD_MS + HYGIENE_IO_MS;

/// Compute the base polling tick (ms) used by the time manager for a given reference window.
#[inline]
pub(crate) fn poll_tick_ms(window_ref_ms: u64) -> u64 {
    if window_ref_ms == u64::MAX {
        20
    } else if window_ref_ms <= 200 {
        5
    } else if window_ref_ms <= 800 {
        10
    } else {
        20
    }
}

/// Compute the near-finalization guard window (in ms) for a given absolute limit.
///
/// `total_limit_ms` は “総ハード／プラン済み締切” の時間であり、残り時間ではない。
/// 1000ms 以上では 500ms、500–999ms では 250ms、200–499ms では 120ms、それ未満は
/// ポーリング周期に応じた最低限のガード（10ms）を確保する。
#[inline]
pub(crate) fn compute_finalize_window_ms(total_limit_ms: u64) -> u64 {
    if total_limit_ms == 0 || total_limit_ms == u64::MAX {
        return 0;
    }

    let base = if total_limit_ms >= 1_000 {
        NEAR_HARD_FINALIZE_MS
    } else if total_limit_ms >= 500 {
        NEAR_HARD_FINALIZE_MS / 2
    } else if total_limit_ms >= 200 {
        120
    } else {
        0
    };

    let tick = poll_tick_ms(total_limit_ms);
    base.max(tick.saturating_mul(2))
}

/// Compute the hard-deadline guard window (ms) used by the main-thread near-guard logic.
#[inline]
pub(crate) fn compute_hard_guard_ms(total_hard_limit_ms: u64) -> u64 {
    if total_hard_limit_ms >= 1_000 {
        MAIN_NEAR_DEADLINE_WINDOW_MS
    } else if total_hard_limit_ms >= 500 {
        150
    } else if total_hard_limit_ms >= 200 {
        80
    } else {
        0
    }
}

/// 残余時間に基づいて衛生待機の動的上限を計算する。
///
/// 残余時間が十分にある場合（≥1000ms）は拡張上限（250ms）、
/// それ以外は通常上限（50ms）を返す。
#[inline]
pub(crate) fn compute_dynamic_hygiene_max(
    elapsed_ms: u64,
    hard_limit_ms: u64,
    planned_limit_ms: u64,
) -> u64 {
    let nearest_deadline = [hard_limit_ms, planned_limit_ms]
        .into_iter()
        .filter(|limit| *limit > 0 && *limit < u64::MAX)
        .min()
        .unwrap_or(u64::MAX);

    if nearest_deadline == u64::MAX {
        return HYGIENE_WAIT_MAX_MS;
    }

    let remaining = nearest_deadline.saturating_sub(elapsed_ms);

    // 残余時間が1000ms以上なら拡張上限、それ以外は通常上限
    if remaining >= 1000 {
        HYGIENE_WAIT_EXTENDED_MAX_MS
    } else {
        HYGIENE_WAIT_MAX_MS
    }
}

/// finalize 時の衛生待機に使う安全な上限時間を計算する。
///
/// - `elapsed_ms`: 直近探索で消費した時間
/// - `hard_limit_ms`: ハード締切 (無効なら 0 または `u64::MAX`)
/// - `planned_limit_ms`: 予定締切 (無効なら 0 または `u64::MAX`)
/// - `default_max_ms`: 通常の上限 (例: 50ms)
#[inline]
pub(crate) fn compute_hygiene_wait_budget(
    elapsed_ms: u64,
    hard_limit_ms: u64,
    planned_limit_ms: u64,
    default_max_ms: u64,
) -> u64 {
    let nearest_deadline = [hard_limit_ms, planned_limit_ms]
        .into_iter()
        .filter(|limit| *limit > 0 && *limit < u64::MAX)
        .min()
        .unwrap_or(u64::MAX);

    if nearest_deadline == u64::MAX {
        return default_max_ms;
    }

    let remaining = nearest_deadline.saturating_sub(elapsed_ms);
    let safe_budget = remaining.saturating_sub(HYGIENE_RESERVED_MS);

    default_max_ms.min(safe_budget)
}
