//! Shared helpers for parallel search timing logic.

use crate::search::constants::NEAR_HARD_FINALIZE_MS;

/// Compute the near-finalization guard window (in ms) for a given absolute limit.
///
/// `total_limit_ms` は “総ハード／プラン済み締切” の時間であり、残り時間ではない。
/// 1000ms 以上では 500ms、500–999ms では 250ms、200–499ms では 120ms、それ未満は 0ms を返す。
#[inline]
pub(crate) fn compute_finalize_window_ms(total_limit_ms: u64) -> u64 {
    if total_limit_ms == 0 || total_limit_ms == u64::MAX {
        0
    } else if total_limit_ms >= 1_000 {
        NEAR_HARD_FINALIZE_MS
    } else if total_limit_ms >= 500 {
        NEAR_HARD_FINALIZE_MS / 2
    } else if total_limit_ms >= 200 {
        120
    } else {
        0
    }
}
