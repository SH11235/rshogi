//! Global search configuration toggles

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};

// Mate early stop (distance-based) toggle
static MATE_EARLY_STOP_ENABLED: AtomicBool = AtomicBool::new(true);
static MATE_EARLY_STOP_MAX_DISTANCE: AtomicU8 = AtomicU8::new(1);

// Root guard rails (global, set by USI layer; default OFF)

static POST_VERIFY_ENABLED: AtomicBool = AtomicBool::new(false);
static POST_VERIFY_YDROP_CP: AtomicI32 = AtomicI32::new(300);

static PROMOTE_VERIFY_ENABLED: AtomicBool = AtomicBool::new(false);
static PROMOTE_BIAS_CP: AtomicI32 = AtomicI32::new(20);

// Root verification (drop-aware SEE gate + shallow re-search)
static ROOT_VERIFY_ENABLED: AtomicBool = AtomicBool::new(true);
static ROOT_VERIFY_MAX_MS: AtomicU64 = AtomicU64::new(8);
static ROOT_VERIFY_MAX_NODES: AtomicU64 = AtomicU64::new(150_000);
static ROOT_VERIFY_CHECK_DEPTH: AtomicU32 = AtomicU32::new(3);
static ROOT_VERIFY_OPP_SEE_MIN_CP: AtomicI32 = AtomicI32::new(0);
static ROOT_VERIFY_MAJOR_LOSS_PENALTY_CP: AtomicI32 = AtomicI32::new(1_200);
static ROOT_VERIFY_REQUIRE_PASS: AtomicBool = AtomicBool::new(true);
static ROOT_VERIFY_MAX_CANDIDATES: AtomicU32 = AtomicU32::new(4);
static ROOT_VERIFY_MAX_CANDIDATES_THREAT: AtomicU32 = AtomicU32::new(12);
static ROOT_VERIFY_MAX_DEFENSE_SEEDS: AtomicU32 = AtomicU32::new(4);
static ROOT_VERIFY_MAX_DEFENSE_SEEDS_THREAT: AtomicU32 = AtomicU32::new(12);

// Win-Protect (victory-state guard rails)
static WIN_PROTECT_ENABLED: AtomicBool = AtomicBool::new(true);
static WIN_PROTECT_THRESHOLD_CP: AtomicI32 = AtomicI32::new(1_200);

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

// ---- Root SEE Gate (revived)
// やねうら王系のルート近傍ガードに相当する軽量ゲート。
// ここではフラグと閾値のみを保持し、実際の適用は上位層（USI/検索部）に委ねる。
static ROOT_SEE_GATE_ENABLED: AtomicBool = AtomicBool::new(false);
/// 拡張SEE（XSEE）のしきい値（cp相当）。0 で無効。
static ROOT_SEE_GATE_XSEE_CP: AtomicI32 = AtomicI32::new(0);

pub fn set_root_see_gate_enabled(on: bool) {
    ROOT_SEE_GATE_ENABLED.store(on, Ordering::Release);
}

#[inline]
pub fn root_see_gate_enabled() -> bool {
    ROOT_SEE_GATE_ENABLED.load(Ordering::Acquire)
}

pub fn set_root_see_gate_xsee_cp(v: i32) {
    // 実用域: 0〜1000cp 程度にクランプ
    let clamped = v.clamp(0, 5000);
    ROOT_SEE_GATE_XSEE_CP.store(clamped, Ordering::Release);
}

#[inline]
pub fn root_see_gate_xsee_cp() -> i32 {
    ROOT_SEE_GATE_XSEE_CP.load(Ordering::Acquire)
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

// ---- Root Verify (drop-aware post-check)
pub fn set_root_verify_enabled(on: bool) {
    ROOT_VERIFY_ENABLED.store(on, Ordering::Release);
}
#[inline]
pub fn root_verify_enabled() -> bool {
    ROOT_VERIFY_ENABLED.load(Ordering::Acquire)
}
pub fn set_root_verify_max_ms(ms: u64) {
    ROOT_VERIFY_MAX_MS.store(ms.clamp(0, 50), Ordering::Release);
}
#[inline]
pub fn root_verify_max_ms() -> u64 {
    ROOT_VERIFY_MAX_MS.load(Ordering::Acquire)
}
pub fn set_root_verify_max_nodes(nodes: u64) {
    let clamped = nodes.clamp(0, 5_000_000);
    ROOT_VERIFY_MAX_NODES.store(clamped, Ordering::Release);
}
#[inline]
pub fn root_verify_max_nodes() -> u64 {
    ROOT_VERIFY_MAX_NODES.load(Ordering::Acquire)
}
pub fn set_root_verify_check_depth(depth: u32) {
    let d = depth.clamp(1, 5);
    ROOT_VERIFY_CHECK_DEPTH.store(d, Ordering::Release);
}
#[inline]
pub fn root_verify_check_depth() -> u32 {
    ROOT_VERIFY_CHECK_DEPTH.load(Ordering::Acquire)
}
pub fn set_root_verify_opp_see_min_cp(cp: i32) {
    ROOT_VERIFY_OPP_SEE_MIN_CP.store(cp.clamp(-200, 300), Ordering::Release);
}
#[inline]
pub fn root_verify_opp_see_min_cp() -> i32 {
    ROOT_VERIFY_OPP_SEE_MIN_CP.load(Ordering::Acquire)
}
pub fn set_root_verify_major_loss_penalty_cp(cp: i32) {
    ROOT_VERIFY_MAJOR_LOSS_PENALTY_CP.store(cp.clamp(200, 3000), Ordering::Release);
}
#[inline]
pub fn root_verify_major_loss_penalty_cp() -> i32 {
    ROOT_VERIFY_MAJOR_LOSS_PENALTY_CP.load(Ordering::Acquire)
}
pub fn set_root_verify_require_pass(on: bool) {
    ROOT_VERIFY_REQUIRE_PASS.store(on, Ordering::Release);
}
#[inline]
pub fn root_verify_require_pass() -> bool {
    ROOT_VERIFY_REQUIRE_PASS.load(Ordering::Acquire)
}
pub fn set_root_verify_max_candidates(count: u32) {
    ROOT_VERIFY_MAX_CANDIDATES.store(count.clamp(1, 32), Ordering::Release);
}
#[inline]
pub fn root_verify_max_candidates() -> u32 {
    ROOT_VERIFY_MAX_CANDIDATES.load(Ordering::Acquire)
}
pub fn set_root_verify_max_candidates_threat(count: u32) {
    ROOT_VERIFY_MAX_CANDIDATES_THREAT.store(count.clamp(1, 32), Ordering::Release);
}
#[inline]
pub fn root_verify_max_candidates_threat() -> u32 {
    ROOT_VERIFY_MAX_CANDIDATES_THREAT.load(Ordering::Acquire)
}
pub fn set_root_verify_max_defense_seeds(count: u32) {
    ROOT_VERIFY_MAX_DEFENSE_SEEDS.store(count.clamp(0, 32), Ordering::Release);
}
#[inline]
pub fn root_verify_max_defense_seeds() -> u32 {
    ROOT_VERIFY_MAX_DEFENSE_SEEDS.load(Ordering::Acquire)
}
pub fn set_root_verify_max_defense_seeds_threat(count: u32) {
    ROOT_VERIFY_MAX_DEFENSE_SEEDS_THREAT.store(count.clamp(0, 32), Ordering::Release);
}
#[inline]
pub fn root_verify_max_defense_seeds_threat() -> u32 {
    ROOT_VERIFY_MAX_DEFENSE_SEEDS_THREAT.load(Ordering::Acquire)
}

// ---- Win-Protect guard
pub fn set_win_protect_enabled(on: bool) {
    WIN_PROTECT_ENABLED.store(on, Ordering::Release);
}
#[inline]
pub fn win_protect_enabled() -> bool {
    WIN_PROTECT_ENABLED.load(Ordering::Acquire)
}
pub fn set_win_protect_threshold_cp(cp: i32) {
    WIN_PROTECT_THRESHOLD_CP.store(cp.clamp(600, 3000), Ordering::Release);
}
#[inline]
pub fn win_protect_threshold_cp() -> i32 {
    WIN_PROTECT_THRESHOLD_CP.load(Ordering::Acquire)
}
