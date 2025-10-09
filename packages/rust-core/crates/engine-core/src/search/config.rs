//! Global search configuration toggles

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};

// Mate early stop (distance-based) toggle
static MATE_EARLY_STOP_ENABLED: AtomicBool = AtomicBool::new(true);
static MATE_EARLY_STOP_MAX_DISTANCE: AtomicU8 = AtomicU8::new(1);

// Root guard rails (global, set by USI layer; default OFF)
static ROOT_SEE_GATE_ENABLED: AtomicBool = AtomicBool::new(false);
static ROOT_SEE_X_CP: AtomicI32 = AtomicI32::new(100);

static POST_VERIFY_ENABLED: AtomicBool = AtomicBool::new(false);
static POST_VERIFY_YDROP_CP: AtomicI32 = AtomicI32::new(300);

static PROMOTE_VERIFY_ENABLED: AtomicBool = AtomicBool::new(false);
static PROMOTE_BIAS_CP: AtomicI32 = AtomicI32::new(20);

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

// ---- Root SEE Gate
pub fn set_root_see_gate_enabled(on: bool) {
    ROOT_SEE_GATE_ENABLED.store(on, Ordering::Release);
}
#[inline]
pub fn root_see_gate_enabled() -> bool {
    ROOT_SEE_GATE_ENABLED.load(Ordering::Acquire)
}
pub fn set_root_see_x_cp(x: i32) {
    ROOT_SEE_X_CP.store(x, Ordering::Release);
}
#[inline]
pub fn root_see_x_cp() -> i32 {
    ROOT_SEE_X_CP.load(Ordering::Acquire)
}

// ---- Post-bestmove Verify
pub fn set_post_verify_enabled(on: bool) {
    POST_VERIFY_ENABLED.store(on, Ordering::Release);
}
#[inline]
pub fn post_verify_enabled() -> bool {
    POST_VERIFY_ENABLED.load(Ordering::Acquire)
}
pub fn set_post_verify_ydrop_cp(y: i32) {
    POST_VERIFY_YDROP_CP.store(y, Ordering::Release);
}
#[inline]
pub fn post_verify_ydrop_cp() -> i32 {
    POST_VERIFY_YDROP_CP.load(Ordering::Acquire)
}

// ---- Promote verify
pub fn set_promote_verify_enabled(on: bool) {
    PROMOTE_VERIFY_ENABLED.store(on, Ordering::Release);
}
#[inline]
pub fn promote_verify_enabled() -> bool {
    PROMOTE_VERIFY_ENABLED.load(Ordering::Acquire)
}
pub fn set_promote_bias_cp(bias: i32) {
    PROMOTE_BIAS_CP.store(bias, Ordering::Release);
}
#[inline]
pub fn promote_bias_cp() -> i32 {
    PROMOTE_BIAS_CP.load(Ordering::Acquire)
}
