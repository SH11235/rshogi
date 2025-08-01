//! Pruning techniques for unified searcher
//!
//! Implements various pruning methods to reduce search tree size

use crate::shogi::{Move, Position};

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
    !in_check && depth <= 7 && !is_mate_score(alpha) && !is_mate_score(beta)
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
fn is_mate_score(score: i32) -> bool {
    score.abs() > 30000
}
