//! Centralized tuning parameters for ClassicAB pruning/ordering.
//!
//! 既定値は定数として定義しつつ、USIの`setoption`から動的に変更できるように
//! ランタイム値を原子的に保持する。`get_*()`系の関数は常にランタイム値
//! （未変更時は既定値）を返す。

// LMR
pub const LMR_K_COEFF: f32 = 1.7; // 既定: r = floor(ln(depth)*ln(moveno)/LMR_K_COEFF)

// LMP (late quiet skip) thresholds per depth (depth<=3 only)
pub const LMP_LIMIT_D1: usize = 6;
pub const LMP_LIMIT_D2: usize = 12;
pub const LMP_LIMIT_D3: usize = 18;

// History Pruning threshold (skip quiet if history < HP_THRESHOLD at shallow depth)
pub const HP_THRESHOLD: i32 = -2000;

// Static Beta Pruning margins (cp)
pub const SBP_MARGIN_D1: i32 = 200;
pub const SBP_MARGIN_D2: i32 = 300;

// Razor: enabled depth==1 (no explicit margin here; we use qsearch(alpha, alpha+1))
pub const RAZOR_ENABLED: bool = true;

// ProbCut margins (cp)
pub const PROBCUT_MARGIN_D5: i32 = 250;
pub const PROBCUT_MARGIN_D6P: i32 = 300;

// Null Move Pruning (NMP)
pub const NMP_MIN_DEPTH: i32 = 3;
pub const NMP_BASE_R: i32 = 2; // R = BASE + depth/4 + bonus
pub const NMP_BONUS_DELTA_BETA: i32 = 150; // if static_eval - beta > this, R += 1
pub const NMP_HAND_SUM_DISABLE: i32 = 6; // disable when hand pieces sum >= this

// QSearch parameters
pub const QS_MARGIN_CAPTURE: i32 = 100; // cp, delta pruning margin for captures
pub const QS_PROMOTE_BONUS: i32 = 50; // cp, small promote bonus in delta estimate
pub const QS_MAX_QUIET_CHECKS: usize = 16; // cap quiet-check searches to bound qsearch

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};

// ランタイム値（USI setoptionで変更可能）
static RUNTIME_LMR_K_X100: AtomicU32 = AtomicU32::new((LMR_K_COEFF * 100.0) as u32);
static RUNTIME_LMP_D1: AtomicUsize = AtomicUsize::new(LMP_LIMIT_D1);
static RUNTIME_LMP_D2: AtomicUsize = AtomicUsize::new(LMP_LIMIT_D2);
static RUNTIME_LMP_D3: AtomicUsize = AtomicUsize::new(LMP_LIMIT_D3);
static RUNTIME_HP_THRESHOLD: AtomicI32 = AtomicI32::new(HP_THRESHOLD);
static RUNTIME_SBP_D1: AtomicI32 = AtomicI32::new(SBP_MARGIN_D1);
static RUNTIME_SBP_D2: AtomicI32 = AtomicI32::new(SBP_MARGIN_D2);
static RUNTIME_PROBCUT_D5: AtomicI32 = AtomicI32::new(PROBCUT_MARGIN_D5);
static RUNTIME_PROBCUT_D6P: AtomicI32 = AtomicI32::new(PROBCUT_MARGIN_D6P);
static RUNTIME_RAZOR: AtomicBool = AtomicBool::new(RAZOR_ENABLED);
static RUNTIME_IID_MIN_DEPTH: AtomicI32 = AtomicI32::new(6); // 既定: 6ply

// Getter API（探索側からはこちらを使用）
#[inline]
pub fn lmr_k_coeff() -> f32 {
    (RUNTIME_LMR_K_X100.load(Ordering::Relaxed) as f32) / 100.0
}

#[inline]
pub fn lmp_limit_for_depth(depth: i32) -> usize {
    match depth {
        d if d <= 1 => RUNTIME_LMP_D1.load(Ordering::Relaxed),
        2 => RUNTIME_LMP_D2.load(Ordering::Relaxed),
        _ => RUNTIME_LMP_D3.load(Ordering::Relaxed),
    }
}

#[inline]
pub fn hp_threshold() -> i32 {
    RUNTIME_HP_THRESHOLD.load(Ordering::Relaxed)
}

#[inline]
pub fn sbp_margin_d1() -> i32 {
    RUNTIME_SBP_D1.load(Ordering::Relaxed)
}
#[inline]
pub fn sbp_margin_d2() -> i32 {
    RUNTIME_SBP_D2.load(Ordering::Relaxed)
}

#[inline]
pub fn probcut_margin(depth: i32) -> i32 {
    if depth >= 6 {
        RUNTIME_PROBCUT_D6P.load(Ordering::Relaxed)
    } else {
        RUNTIME_PROBCUT_D5.load(Ordering::Relaxed)
    }
}

#[inline]
pub fn razor_enabled() -> bool {
    RUNTIME_RAZOR.load(Ordering::Relaxed)
}

#[inline]
pub fn iid_min_depth() -> i32 {
    RUNTIME_IID_MIN_DEPTH.load(Ordering::Relaxed)
}

// Setter API（USI側から更新）
pub fn set_lmr_k_x100(v: u32) {
    RUNTIME_LMR_K_X100.store(v.max(1), Ordering::Relaxed);
}
pub fn set_lmp_d1(v: usize) {
    RUNTIME_LMP_D1.store(v, Ordering::Relaxed);
}
pub fn set_lmp_d2(v: usize) {
    RUNTIME_LMP_D2.store(v, Ordering::Relaxed);
}
pub fn set_lmp_d3(v: usize) {
    RUNTIME_LMP_D3.store(v, Ordering::Relaxed);
}
pub fn set_hp_threshold(v: i32) {
    RUNTIME_HP_THRESHOLD.store(v, Ordering::Relaxed);
}
pub fn set_sbp_d1(v: i32) {
    RUNTIME_SBP_D1.store(v, Ordering::Relaxed);
}
pub fn set_sbp_d2(v: i32) {
    RUNTIME_SBP_D2.store(v, Ordering::Relaxed);
}
pub fn set_probcut_d5(v: i32) {
    RUNTIME_PROBCUT_D5.store(v, Ordering::Relaxed);
}
pub fn set_probcut_d6p(v: i32) {
    RUNTIME_PROBCUT_D6P.store(v, Ordering::Relaxed);
}
pub fn set_razor_enabled(b: bool) {
    RUNTIME_RAZOR.store(b, Ordering::Relaxed);
}
pub fn set_iid_min_depth(v: i32) {
    RUNTIME_IID_MIN_DEPTH.store(v.max(0), Ordering::Relaxed);
}
