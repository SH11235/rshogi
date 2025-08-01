//! Pruning techniques for unified searcher
//!
//! Implements various pruning methods to reduce search tree size

use crate::shogi::{Move, Position};

// Pruning constants based on empirical testing

/// Razoring margin - aggressive pruning at very low depths
/// This value prevents missing shallow tactics while allowing meaningful reductions
const RAZORING_BASE_MARGIN: i32 = 400;

/// Static null move pruning depth factor
/// Controls how aggressively we prune based on static evaluation
/// Higher values = more conservative pruning
const STATIC_NULL_MOVE_DEPTH_FACTOR: i32 = 120;

/// Delta pruning margin for quiescence search
/// Conservative to avoid missing captures that change evaluation significantly
const DELTA_PRUNING_MARGIN: i32 = 200;

/// Razoring margins by depth
/// Lower depths get smaller margins for more aggressive pruning
const RAZORING_MARGIN_DEPTH_1: i32 = 200;
const RAZORING_MARGIN_DEPTH_2: i32 = 400;

/// Pruning parameters
pub struct PruningParams {
    /// Enable null move pruning
    pub null_move: bool,

    /// Enable futility pruning
    pub futility: bool,

    /// Enable late move reductions
    pub lmr: bool,

    /// Futility margins by depth
    pub futility_margins: [i32; 8],
}

impl Default for PruningParams {
    fn default() -> Self {
        Self {
            null_move: true,
            futility: true,
            lmr: true,
            futility_margins: [0, 100, 200, 300, 400, 500, 600, 700],
        }
    }
}

/// Check if null move pruning is applicable
pub fn can_do_null_move(
    _pos: &Position,
    depth: u8,
    in_check: bool,
    _beta: i32,
    _static_eval: i32,
) -> bool {
    !in_check && depth >= 3
    // Note: static_eval check removed as it's not used in simplified version
}

/// Calculate null move reduction
pub fn null_move_reduction(depth: u8) -> u8 {
    2 + depth / 4
}

/// Check if futility pruning is applicable
pub fn can_do_futility_pruning(
    depth: u8,
    in_check: bool,
    alpha: i32,
    beta: i32,
    _static_eval: i32,
) -> bool {
    depth <= 7 && can_prune(in_check, alpha, beta)
}

/// Get futility margin for given depth
pub fn futility_margin(depth: u8) -> i32 {
    match depth {
        0 => 0,
        1 => 100,
        2 => 200,
        3 => 300,
        4 => 400,
        5 => 500,
        6 => 600,
        _ => 700,
    }
}

/// Check if late move reduction is applicable
pub fn can_do_lmr(
    depth: u8,
    moves_searched: u32,
    in_check: bool,
    gives_check: bool,
    mv: Move,
) -> bool {
    depth >= 3
        && moves_searched >= 4
        && !in_check
        && !gives_check
        && !mv.is_capture_hint()
        && !mv.is_promote()
}

/// Check if we can do razoring (extreme futility pruning at low depths)
pub fn can_do_razoring(depth: u8, in_check: bool, alpha: i32, static_eval: i32) -> bool {
    depth <= 2 && can_prune_alpha(in_check, alpha) && static_eval + RAZORING_BASE_MARGIN < alpha
}

/// Get razoring margin
pub fn razoring_margin(depth: u8) -> i32 {
    match depth {
        1 => RAZORING_MARGIN_DEPTH_1,
        2 => RAZORING_MARGIN_DEPTH_2,
        _ => 0,
    }
}

/// Calculate LMR reduction
pub fn lmr_reduction(depth: u8, moves_searched: u32) -> u8 {
    // More sophisticated reduction table based on depth and move count
    match (depth, moves_searched) {
        (d, m) if d >= 8 && m >= 16 => 4,
        (d, m) if d >= 6 && m >= 12 => 3,
        (d, m) if d >= 5 && m >= 8 => 2,
        (d, m) if d >= 4 && m >= 4 => 2,
        (d, m) if d >= 3 && m >= 4 => 1,
        _ => 0,
    }
}

/// Calculate LMR reduction with logarithmic formula (alternative implementation)
/// This provides smoother reduction based on depth and move count
#[allow(dead_code)]
pub fn lmr_reduction_formula(depth: u8, moves_searched: u32) -> u8 {
    if depth < 3 || moves_searched < 4 {
        return 0;
    }

    // Formula similar to Stockfish: log(depth) * log(moves)
    let depth_factor = (depth as f32).ln();
    let moves_factor = (moves_searched as f32).ln();
    let reduction = (depth_factor * moves_factor / 2.0) as u8;

    // Cap the reduction to avoid over-reducing
    reduction.min(depth.saturating_sub(2))
}

/// Check if score is a mate score
pub fn is_mate_score(score: i32) -> bool {
    score.abs() > 30000
}

/// Common helper to check if pruning techniques can be applied
/// Returns true if pruning is allowed based on common preconditions
pub fn can_prune(in_check: bool, alpha: i32, beta: i32) -> bool {
    !in_check && !is_mate_score(alpha) && !is_mate_score(beta)
}

/// Check if pruning is allowed for techniques that only check one bound
pub fn can_prune_alpha(in_check: bool, alpha: i32) -> bool {
    !in_check && !is_mate_score(alpha)
}

/// Check if pruning is allowed for techniques that only check beta
pub fn can_prune_beta(in_check: bool, beta: i32) -> bool {
    !in_check && !is_mate_score(beta)
}

/// Get delta pruning margin for quiescence search
pub fn delta_pruning_margin() -> i32 {
    DELTA_PRUNING_MARGIN
}

/// Check if static null move (reverse futility) pruning is applicable
pub fn can_do_static_null_move(depth: u8, in_check: bool, beta: i32, static_eval: i32) -> bool {
    depth <= 6
        && can_prune_beta(in_check, beta)
        && static_eval - STATIC_NULL_MOVE_DEPTH_FACTOR * depth as i32 >= beta
}
