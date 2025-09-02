//! Global search configuration toggles

use std::sync::atomic::{AtomicBool, Ordering};

// Mate early stop (distance-based) toggle
static MATE_EARLY_STOP_ENABLED: AtomicBool = AtomicBool::new(true);

/// Enable or disable mate early stop globally
pub fn set_mate_early_stop_enabled(enabled: bool) {
    MATE_EARLY_STOP_ENABLED.store(enabled, Ordering::Release);
}

/// Check if mate early stop is enabled
#[inline]
pub fn mate_early_stop_enabled() -> bool {
    MATE_EARLY_STOP_ENABLED.load(Ordering::Acquire)
}
