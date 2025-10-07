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
// Dynamic SBP/Futility margins (Phase3) — base/slope (safeモードで使用)
pub const SBP_MARGIN_BASE: i32 = 120;
pub const SBP_MARGIN_SLOPE: i32 = 60; // per depth (clamped <=12)
pub const FUT_MARGIN_BASE: i32 = 100;
pub const FUT_MARGIN_SLOPE: i32 = 80; // per depth (clamped <=8)

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
pub const QS_MARGIN_CAPTURE: i32 = 150; // cp, delta pruning margin for captures
pub const QS_PROMOTE_BONUS: i32 = 50; // cp, small promote bonus in delta estimate
pub const QS_MAX_QUIET_CHECKS: usize = 4; // cap quiet-check searches to bound qsearch
pub const QS_BAD_CAPTURE_MIN: i32 = 450; // cp, SEE<0 captures below this are pruned unless checking
pub const QS_CHECK_PRUNE_MARGIN: i32 = 150; // cp, require stand_pat improvement before exploring quiet check

// Move ordering weights (exported for tuning)
pub const QUIET_HISTORY_WEIGHT: i32 = 4;
pub const CONTINUATION_HISTORY_WEIGHT: i32 = 2;
pub const CAPTURE_HISTORY_WEIGHT: i32 = 2;

pub const ROOT_BASE_KEY: i32 = 2_000_000;
pub const ROOT_TT_BONUS: i32 = 1_500_000;
pub const ROOT_PREV_SCORE_SCALE: i32 = 200;
pub const ROOT_PREV_SCORE_CLAMP: i32 = 300;
pub const ROOT_MULTIPV_BONUS_1: i32 = 50_000;
pub const ROOT_MULTIPV_BONUS_2: i32 = 25_000;

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering};
use std::sync::OnceLock;

// ランタイム値（USI setoptionで変更可能）
static RUNTIME_LMR_K_X100: AtomicU32 = AtomicU32::new((LMR_K_COEFF * 100.0) as u32);
static RUNTIME_LMP_D1: AtomicUsize = AtomicUsize::new(LMP_LIMIT_D1);
static RUNTIME_LMP_D2: AtomicUsize = AtomicUsize::new(LMP_LIMIT_D2);
static RUNTIME_LMP_D3: AtomicUsize = AtomicUsize::new(LMP_LIMIT_D3);
static RUNTIME_HP_THRESHOLD: AtomicI32 = AtomicI32::new(HP_THRESHOLD);
/// HP の深さ比例スケール（safeモード時のみ使用）。
/// 例: history < -HP_DEPTH_SCALE * depth
static RUNTIME_HP_DEPTH_SCALE: AtomicI32 = AtomicI32::new(4361);
static RUNTIME_SBP_D1: AtomicI32 = AtomicI32::new(SBP_MARGIN_D1);
static RUNTIME_SBP_D2: AtomicI32 = AtomicI32::new(SBP_MARGIN_D2);
static RUNTIME_SBP_BASE: AtomicI32 = AtomicI32::new(SBP_MARGIN_BASE);
static RUNTIME_SBP_SLOPE: AtomicI32 = AtomicI32::new(SBP_MARGIN_SLOPE);
static RUNTIME_FUT_BASE: AtomicI32 = AtomicI32::new(FUT_MARGIN_BASE);
static RUNTIME_FUT_SLOPE: AtomicI32 = AtomicI32::new(FUT_MARGIN_SLOPE);
static RUNTIME_PROBCUT_D5: AtomicI32 = AtomicI32::new(PROBCUT_MARGIN_D5);
static RUNTIME_PROBCUT_D6P: AtomicI32 = AtomicI32::new(PROBCUT_MARGIN_D6P);
static RUNTIME_ENABLE_NMP: AtomicBool = AtomicBool::new(true);
static RUNTIME_ENABLE_IID: AtomicBool = AtomicBool::new(true);
static RUNTIME_ENABLE_PROBCUT: AtomicBool = AtomicBool::new(true);
static RUNTIME_ENABLE_STATIC_BETA: AtomicBool = AtomicBool::new(true);
static RUNTIME_ENABLE_SBP_DYNAMIC: AtomicBool = AtomicBool::new(true);
static RUNTIME_ENABLE_FUT_DYNAMIC: AtomicBool = AtomicBool::new(true);
static RUNTIME_QS_CHECKS: AtomicBool = AtomicBool::new(true);
static RUNTIME_RAZOR: AtomicBool = AtomicBool::new(RAZOR_ENABLED);
static RUNTIME_IID_MIN_DEPTH: AtomicI32 = AtomicI32::new(6); // 既定: 6ply
/// YO安全側ガードの有効/無効（既定ON）。
static RUNTIME_PRUNING_SAFE_MODE: AtomicBool = AtomicBool::new(true);
/// ProbCut: 浅層(depth<4)で検証探索を行わず（無効化）にするオプション（既定OFF）。
static RUNTIME_PROBCUT_SKIP_VERIFY_LT4: AtomicBool = AtomicBool::new(false);
static PREFETCH_ENABLED: OnceLock<AtomicBool> = OnceLock::new();
static RUNTIME_QUIET_HISTORY_WEIGHT: AtomicI32 = AtomicI32::new(QUIET_HISTORY_WEIGHT);
static RUNTIME_CONT_HISTORY_WEIGHT: AtomicI32 = AtomicI32::new(CONTINUATION_HISTORY_WEIGHT);
static RUNTIME_CAP_HISTORY_WEIGHT: AtomicI32 = AtomicI32::new(CAPTURE_HISTORY_WEIGHT);
static RUNTIME_ROOT_TT_BONUS: AtomicI32 = AtomicI32::new(ROOT_TT_BONUS);
static RUNTIME_ROOT_PREV_SCORE_SCALE: AtomicI32 = AtomicI32::new(ROOT_PREV_SCORE_SCALE);
static RUNTIME_ROOT_MULTIPV_1: AtomicI32 = AtomicI32::new(ROOT_MULTIPV_BONUS_1);
static RUNTIME_ROOT_MULTIPV_2: AtomicI32 = AtomicI32::new(ROOT_MULTIPV_BONUS_2);
static RUNTIME_QS_MARGIN_CAPTURE: AtomicI32 = AtomicI32::new(QS_MARGIN_CAPTURE);
static RUNTIME_QS_BAD_CAPTURE_MIN: AtomicI32 = AtomicI32::new(QS_BAD_CAPTURE_MIN);
static RUNTIME_QS_CHECK_PRUNE_MARGIN: AtomicI32 = AtomicI32::new(QS_CHECK_PRUNE_MARGIN);

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

/// HPの深さ比例しきい値（safeモード時）
#[inline]
pub fn hp_depth_scale() -> i32 {
    RUNTIME_HP_DEPTH_SCALE.load(Ordering::Relaxed)
}

/// 現在の設定に応じたHPしきい値を返す。
/// safeモード時は depth 係数付きの厳しめ（=より負側）のしきい値を返す。
#[inline]
pub fn hp_threshold_for_depth(depth: i32) -> i32 {
    if pruning_safe_mode() {
        -(hp_depth_scale()).saturating_mul(depth.max(1))
    } else {
        hp_threshold()
    }
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
pub fn sbp_margin_base() -> i32 {
    RUNTIME_SBP_BASE.load(Ordering::Relaxed)
}
#[inline]
pub fn sbp_margin_slope() -> i32 {
    RUNTIME_SBP_SLOPE.load(Ordering::Relaxed)
}
#[inline]
pub fn fut_margin_base() -> i32 {
    RUNTIME_FUT_BASE.load(Ordering::Relaxed)
}
#[inline]
pub fn fut_margin_slope() -> i32 {
    RUNTIME_FUT_SLOPE.load(Ordering::Relaxed)
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
pub fn nmp_enabled() -> bool {
    RUNTIME_ENABLE_NMP.load(Ordering::Relaxed)
}

#[inline]
pub fn iid_enabled() -> bool {
    RUNTIME_ENABLE_IID.load(Ordering::Relaxed)
}

#[inline]
pub fn probcut_enabled() -> bool {
    RUNTIME_ENABLE_PROBCUT.load(Ordering::Relaxed)
}

#[inline]
pub fn static_beta_enabled() -> bool {
    RUNTIME_ENABLE_STATIC_BETA.load(Ordering::Relaxed)
}

#[inline]
pub fn sbp_dynamic_enabled() -> bool {
    RUNTIME_ENABLE_SBP_DYNAMIC.load(Ordering::Relaxed)
}
#[inline]
pub fn fut_dynamic_enabled() -> bool {
    RUNTIME_ENABLE_FUT_DYNAMIC.load(Ordering::Relaxed)
}

#[inline]
pub fn qs_checks_enabled() -> bool {
    RUNTIME_QS_CHECKS.load(Ordering::Relaxed)
}

#[inline]
pub fn qs_margin_capture() -> i32 {
    RUNTIME_QS_MARGIN_CAPTURE.load(Ordering::Relaxed)
}

#[inline]
pub fn qs_bad_capture_min() -> i32 {
    RUNTIME_QS_BAD_CAPTURE_MIN.load(Ordering::Relaxed)
}

#[inline]
pub fn qs_check_prune_margin() -> i32 {
    RUNTIME_QS_CHECK_PRUNE_MARGIN.load(Ordering::Relaxed)
}

#[inline]
pub fn tt_prefetch_enabled() -> bool {
    PREFETCH_ENABLED
        .get_or_init(|| AtomicBool::new(default_prefetch_value()))
        .load(Ordering::Relaxed)
}

fn default_prefetch_value() -> bool {
    match std::env::var("SHOGI_TT_PREFETCH") {
        Ok(val) => matches!(val.to_ascii_lowercase().as_str(), "1" | "true" | "on" | "yes"),
        Err(_) => true,
    }
}

#[inline]
pub fn quiet_history_weight() -> i32 {
    RUNTIME_QUIET_HISTORY_WEIGHT.load(Ordering::Relaxed)
}

#[inline]
pub fn continuation_history_weight() -> i32 {
    RUNTIME_CONT_HISTORY_WEIGHT.load(Ordering::Relaxed)
}

#[inline]
pub fn capture_history_weight() -> i32 {
    RUNTIME_CAP_HISTORY_WEIGHT.load(Ordering::Relaxed)
}

#[inline]
pub fn root_tt_bonus() -> i32 {
    RUNTIME_ROOT_TT_BONUS.load(Ordering::Relaxed)
}

#[inline]
pub fn root_prev_score_scale() -> i32 {
    RUNTIME_ROOT_PREV_SCORE_SCALE.load(Ordering::Relaxed)
}

#[inline]
pub fn root_multipv_bonus(rank: u8) -> i32 {
    match rank {
        1 => RUNTIME_ROOT_MULTIPV_1.load(Ordering::Relaxed),
        2 => RUNTIME_ROOT_MULTIPV_2.load(Ordering::Relaxed),
        _ => 0,
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

#[inline]
pub fn pruning_safe_mode() -> bool {
    RUNTIME_PRUNING_SAFE_MODE.load(Ordering::Relaxed)
}

#[inline]
pub fn probcut_skip_verify_lt4() -> bool {
    RUNTIME_PROBCUT_SKIP_VERIFY_LT4.load(Ordering::Relaxed)
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
pub fn set_hp_depth_scale(v: i32) {
    RUNTIME_HP_DEPTH_SCALE.store(v.max(0), Ordering::Relaxed);
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
pub fn set_nmp_enabled(b: bool) {
    RUNTIME_ENABLE_NMP.store(b, Ordering::Relaxed);
}
pub fn set_iid_enabled(b: bool) {
    RUNTIME_ENABLE_IID.store(b, Ordering::Relaxed);
}
pub fn set_probcut_enabled(b: bool) {
    RUNTIME_ENABLE_PROBCUT.store(b, Ordering::Relaxed);
}
pub fn set_static_beta_enabled(b: bool) {
    RUNTIME_ENABLE_STATIC_BETA.store(b, Ordering::Relaxed);
}
pub fn set_sbp_dynamic_enabled(b: bool) {
    RUNTIME_ENABLE_SBP_DYNAMIC.store(b, Ordering::Relaxed);
}
pub fn set_fut_dynamic_enabled(b: bool) {
    RUNTIME_ENABLE_FUT_DYNAMIC.store(b, Ordering::Relaxed);
}
pub fn set_qs_checks_enabled(b: bool) {
    RUNTIME_QS_CHECKS.store(b, Ordering::Relaxed);
}

pub fn set_qs_margin_capture(v: i32) {
    let clamped = v.clamp(0, 5000);
    RUNTIME_QS_MARGIN_CAPTURE.store(clamped, Ordering::Relaxed);
}

pub fn set_qs_bad_capture_min(v: i32) {
    let clamped = v.clamp(0, 5000);
    RUNTIME_QS_BAD_CAPTURE_MIN.store(clamped, Ordering::Relaxed);
}

pub fn set_qs_check_prune_margin(v: i32) {
    let clamped = v.clamp(0, 5000);
    RUNTIME_QS_CHECK_PRUNE_MARGIN.store(clamped, Ordering::Relaxed);
}

pub fn set_sbp_base(v: i32) {
    RUNTIME_SBP_BASE.store(v, Ordering::Relaxed);
}
pub fn set_sbp_slope(v: i32) {
    RUNTIME_SBP_SLOPE.store(v, Ordering::Relaxed);
}
pub fn set_fut_base(v: i32) {
    RUNTIME_FUT_BASE.store(v, Ordering::Relaxed);
}
pub fn set_fut_slope(v: i32) {
    RUNTIME_FUT_SLOPE.store(v, Ordering::Relaxed);
}

pub fn set_quiet_history_weight(v: i32) {
    RUNTIME_QUIET_HISTORY_WEIGHT.store(v, Ordering::Relaxed);
}

pub fn set_continuation_history_weight(v: i32) {
    RUNTIME_CONT_HISTORY_WEIGHT.store(v, Ordering::Relaxed);
}

pub fn set_capture_history_weight(v: i32) {
    RUNTIME_CAP_HISTORY_WEIGHT.store(v, Ordering::Relaxed);
}

pub fn set_tt_prefetch_enabled_runtime(on: bool) {
    PREFETCH_ENABLED
        .get_or_init(|| AtomicBool::new(default_prefetch_value()))
        .store(on, Ordering::Relaxed);
}

pub fn set_root_tt_bonus(v: i32) {
    RUNTIME_ROOT_TT_BONUS.store(v, Ordering::Relaxed);
}

pub fn set_root_prev_score_scale(v: i32) {
    RUNTIME_ROOT_PREV_SCORE_SCALE.store(v, Ordering::Relaxed);
}

pub fn set_root_multipv_bonus(rank: u8, value: i32) {
    match rank {
        1 => RUNTIME_ROOT_MULTIPV_1.store(value, Ordering::Relaxed),
        2 => RUNTIME_ROOT_MULTIPV_2.store(value, Ordering::Relaxed),
        _ => {}
    }
}
pub fn set_razor_enabled(b: bool) {
    RUNTIME_RAZOR.store(b, Ordering::Relaxed);
}
pub fn set_iid_min_depth(v: i32) {
    RUNTIME_IID_MIN_DEPTH.store(v.max(0), Ordering::Relaxed);
}
pub fn set_pruning_safe_mode(on: bool) {
    RUNTIME_PRUNING_SAFE_MODE.store(on, Ordering::Relaxed);
}

pub fn set_probcut_skip_verify_lt4(on: bool) {
    RUNTIME_PROBCUT_SKIP_VERIFY_LT4.store(on, Ordering::Relaxed);
}

#[cfg(test)]
pub fn __test_override_tt_prefetch_enabled(on: bool) {
    set_tt_prefetch_enabled_runtime(on);
}

#[cfg(test)]
pub fn __test_reset_tt_prefetch_to_default() {
    set_tt_prefetch_enabled_runtime(default_prefetch_value());
}

#[cfg(test)]
pub fn __test_reset_runtime_values() {
    set_lmr_k_x100((LMR_K_COEFF * 100.0) as u32);
    set_lmp_d1(LMP_LIMIT_D1);
    set_lmp_d2(LMP_LIMIT_D2);
    set_lmp_d3(LMP_LIMIT_D3);
    set_hp_threshold(HP_THRESHOLD);
    set_hp_depth_scale(4361);
    set_sbp_d1(SBP_MARGIN_D1);
    set_sbp_d2(SBP_MARGIN_D2);
    set_probcut_d5(PROBCUT_MARGIN_D5);
    set_probcut_d6p(PROBCUT_MARGIN_D6P);
    set_nmp_enabled(true);
    set_iid_enabled(true);
    set_probcut_enabled(true);
    set_static_beta_enabled(true);
    set_sbp_dynamic_enabled(true);
    set_fut_dynamic_enabled(true);
    set_qs_checks_enabled(true);
    set_quiet_history_weight(QUIET_HISTORY_WEIGHT);
    set_continuation_history_weight(CONTINUATION_HISTORY_WEIGHT);
    set_capture_history_weight(CAPTURE_HISTORY_WEIGHT);
    set_root_tt_bonus(ROOT_TT_BONUS);
    set_root_prev_score_scale(ROOT_PREV_SCORE_SCALE);
    set_root_multipv_bonus(1, ROOT_MULTIPV_BONUS_1);
    set_root_multipv_bonus(2, ROOT_MULTIPV_BONUS_2);
    set_razor_enabled(RAZOR_ENABLED);
    set_iid_min_depth(6);
    set_pruning_safe_mode(true);
    set_probcut_skip_verify_lt4(false);
    set_tt_prefetch_enabled_runtime(default_prefetch_value());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    #[test]
    fn quiet_history_weight_updates_and_restores() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let original = quiet_history_weight();
        let new_value = original + 5;
        set_quiet_history_weight(new_value);
        assert_eq!(quiet_history_weight(), new_value);
        set_quiet_history_weight(original);
    }

    #[test]
    fn tt_prefetch_override_takes_effect() {
        let _guard = TEST_MUTEX.lock().unwrap();
        let default = default_prefetch_value();
        set_tt_prefetch_enabled_runtime(true);
        assert!(tt_prefetch_enabled());
        set_tt_prefetch_enabled_runtime(false);
        assert!(!tt_prefetch_enabled());
        set_tt_prefetch_enabled_runtime(default);
        assert_eq!(tt_prefetch_enabled(), default);
    }
}
