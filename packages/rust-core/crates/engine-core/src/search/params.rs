//! Centralized tuning parameters for ClassicAB pruning/ordering.

// LMR
pub const LMR_K_COEFF: f32 = 1.7; // r = floor(ln(depth)*ln(moveno)/LMR_K_COEFF)

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
