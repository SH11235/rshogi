//! Global search configuration toggles

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

// Mate early stop (distance-based) toggle
static MATE_EARLY_STOP_ENABLED: AtomicBool = AtomicBool::new(true);
static MATE_EARLY_STOP_MAX_DISTANCE: AtomicU8 = AtomicU8::new(1);

/// Enable or disable mate early stop globally
pub fn set_mate_early_stop_enabled(enabled: bool) {
    MATE_EARLY_STOP_ENABLED.store(enabled, Ordering::Release);
}

/// Check if mate early stop is enabled
#[inline]
pub fn mate_early_stop_enabled() -> bool {
    MATE_EARLY_STOP_ENABLED.load(Ordering::Acquire)
}

/// Set maximum mate distance (plies) for early stop trigger.
/// Valid range is clamped to [1, 5]. Default = 1 (mate in 1).
pub fn set_mate_early_stop_max_distance(distance: u8) {
    let d = distance.clamp(1, 5);
    MATE_EARLY_STOP_MAX_DISTANCE.store(d, Ordering::Release);
}

/// Get maximum mate distance for early stop trigger (plies).
#[inline]
pub fn mate_early_stop_max_distance() -> u8 {
    MATE_EARLY_STOP_MAX_DISTANCE.load(Ordering::Acquire)
}
