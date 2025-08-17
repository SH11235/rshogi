//! Pruning techniques for unified searcher
//!
//! Implements various pruning methods to reduce search tree size

use crate::shogi::{Move, Position};

// Pruning constants based on empirical testing and modern engine practices

/// Razoring margin - aggressive pruning at very low depths
/// Adjusted for more aggressive pruning in tactical positions
const RAZORING_BASE_MARGIN: i32 = 350; // Reduced from 400 for more aggressive razoring

/// Static null move pruning depth factor
/// Controls how aggressively we prune based on static evaluation
/// Lower values = more aggressive pruning
const STATIC_NULL_MOVE_DEPTH_FACTOR: i32 = 100; // Reduced from 120 for more aggressive pruning

/// Delta pruning margin for quiescence search
/// Conservative to avoid missing important captures
const DELTA_PRUNING_MARGIN: i32 = 150; // Reduced from 200 for slightly more aggressive delta pruning

/// Razoring margins by depth - optimized for shogi
/// More aggressive at shallow depths where tactical threats are common
const RAZORING_MARGIN_DEPTH_1: i32 = 150; // Reduced from 200
const RAZORING_MARGIN_DEPTH_2: i32 = 300; // Reduced from 400

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
            // Optimized futility margins for shogi
            // More aggressive at shallow depths, gradual increase for deeper searches
            futility_margins: [0, 80, 160, 250, 350, 450, 550, 650],
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

/// Get futility margin for given depth - optimized values
pub fn futility_margin(depth: u8) -> i32 {
    match depth {
        0 => 0,
        1 => 80,  // Reduced from 100
        2 => 160, // Reduced from 200
        3 => 250, // Reduced from 300
        4 => 350, // Reduced from 400
        5 => 450, // Reduced from 500
        6 => 550, // Reduced from 600
        _ => 650, // Reduced from 700
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

/// Calculate LMR reduction - optimized for shogi's tactical nature
/// More aggressive reduction table tuned for shogi games
pub fn lmr_reduction(depth: u8, moves_searched: u32) -> u8 {
    // No reduction for first few moves or shallow depths
    if depth < 3 || moves_searched < 4 {
        return 0;
    }

    // Optimized reduction table for shogi
    // Slightly more conservative than chess due to drops and sudden tactics
    match (depth, moves_searched) {
        // Very deep searches with many moves - maximum reduction
        (d, m) if d >= 12 && m >= 24 => 6, // Cap at 6 for very deep searches
        (d, m) if d >= 10 && m >= 20 => 5,
        (d, m) if d >= 9 && m >= 16 => 5,
        (d, m) if d >= 8 && m >= 14 => 4,

        // Deep searches - significant reduction
        (d, m) if d >= 7 && m >= 12 => 4,
        (d, m) if d >= 6 && m >= 10 => 3,
        (d, m) if d >= 6 && m >= 7 => 3,

        // Medium depth - moderate reduction
        (d, m) if d >= 5 && m >= 8 => 3,
        (d, m) if d >= 5 && m >= 5 => 2,
        (d, m) if d >= 4 && m >= 6 => 2,
        (d, m) if d >= 4 && m >= 4 => 2,

        // Shallow depth - conservative reduction
        (d, m) if d >= 3 && m >= 5 => 1,
        (d, m) if d >= 3 && m >= 4 => 1,

        _ => 0,
    }
}

/// Calculate LMR reduction with logarithmic formula (alternative implementation)
/// Optimized formula for shogi with adjusted parameters
/// Can be enabled by switching the function call in node.rs for A/B testing
#[allow(dead_code)]
pub fn lmr_reduction_formula(depth: u8, moves_searched: u32) -> u8 {
    if depth < 3 || moves_searched < 4 {
        return 0;
    }

    // Optimized formula for shogi: log(depth) * log(moves) / 1.75
    // The divisor is set to 1.75 (between conservative 2.0 and aggressive 1.5)
    let depth_factor = (depth as f32).ln();
    let moves_factor = (moves_searched as f32).ln();
    let reduction = (depth_factor * moves_factor / 1.75) as u8;

    // Cap the reduction more conservatively for shogi
    // Allow reduction up to depth-2 to preserve some tactical depth
    reduction.min(depth.saturating_sub(2).min(6)) // Also cap at 6 maximum
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
    depth <= 7 // Extended from 6 to 7 for slightly more aggressive static null move
        && can_prune_beta(in_check, beta)
        && static_eval - STATIC_NULL_MOVE_DEPTH_FACTOR * depth as i32 >= beta
}

/// Lightweight pre-filter to check if a move might give check
/// This is much cheaper than full gives_check() calculation
fn likely_could_give_check(pos: &Position, mv: Move) -> bool {
    use crate::shogi::PieceType;

    // Get opponent king position
    let opponent = pos.side_to_move.opposite();
    let opp_king_sq = match pos.board.king_square(opponent) {
        Some(sq) => sq,
        None => return false,
    };

    let to = mv.to();
    let dr = opp_king_sq.rank() as i8 - to.rank() as i8;
    let dc = opp_king_sq.file() as i8 - to.file() as i8;
    let dr_abs = dr.abs();
    let dc_abs = dc.abs();

    // Get piece type and check if it's promoted or will promote
    let (piece_type, will_be_promoted) = if mv.is_drop() {
        (mv.drop_piece_type(), false)
    } else {
        // For normal moves, get piece type from the board
        let from = match mv.from() {
            Some(f) => f,
            None => return false,
        };
        match pos.board.piece_on(from) {
            Some(piece) => (piece.piece_type, piece.promoted || mv.is_promote()),
            None => return false,
        }
    };

    // 1. Direct check - piece moves to attack the king
    let direct_check = match piece_type {
        // Sliding pieces - check if on same line
        PieceType::Rook => dr == 0 || dc == 0,
        PieceType::Bishop => dr_abs == dc_abs,
        PieceType::Lance => {
            if will_be_promoted {
                // Promoted lance moves like gold
                if pos.side_to_move == crate::shogi::Color::Black {
                    (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                } else {
                    (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                }
            } else {
                // Normal lance
                if pos.side_to_move == crate::shogi::Color::Black {
                    dc == 0 && dr < 0
                } else {
                    dc == 0 && dr > 0
                }
            }
        }
        // Close range pieces - check if within range
        PieceType::Knight => {
            if will_be_promoted {
                // Promoted knight moves like gold
                if pos.side_to_move == crate::shogi::Color::Black {
                    (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                } else {
                    (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                }
            } else {
                // Normal knight
                if pos.side_to_move == crate::shogi::Color::Black {
                    dr == -2 && dc_abs == 1
                } else {
                    dr == 2 && dc_abs == 1
                }
            }
        }
        PieceType::Pawn => {
            if will_be_promoted {
                // Tokin moves like gold
                if pos.side_to_move == crate::shogi::Color::Black {
                    (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                } else {
                    (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                }
            } else {
                // Normal pawn
                if pos.side_to_move == crate::shogi::Color::Black {
                    dr == -1 && dc == 0
                } else {
                    dr == 1 && dc == 0
                }
            }
        }
        PieceType::Silver => {
            if will_be_promoted {
                // Promoted silver moves like gold
                if pos.side_to_move == crate::shogi::Color::Black {
                    (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                } else {
                    (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                }
            } else {
                // Normal silver
                if pos.side_to_move == crate::shogi::Color::Black {
                    (dr == -1 && dc_abs <= 1) || (dr == 1 && dc_abs == 1)
                } else {
                    (dr == 1 && dc_abs <= 1) || (dr == -1 && dc_abs == 1)
                }
            }
        }
        PieceType::Gold => {
            // Gold movement pattern
            if pos.side_to_move == crate::shogi::Color::Black {
                (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
            } else {
                (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
            }
        }
        PieceType::King => false, // King can't give check
    };

    if direct_check {
        // For sliding pieces, do a quick obstruction check
        match piece_type {
            PieceType::Rook | PieceType::Bishop | PieceType::Lance => {
                // Simple path check between to and king
                let step_r = if dr != 0 { dr / dr_abs } else { 0 };
                let step_c = if dc != 0 { dc / dc_abs } else { 0 };

                // Check at most 7 squares (max distance on shogi board)
                for i in 1..dr_abs.max(dc_abs) {
                    let check_rank = to.rank() as i8 + i * step_r;
                    let check_file = to.file() as i8 + i * step_c;

                    if let Some(sq) =
                        crate::shogi::Square::new_safe(check_file as u8, check_rank as u8)
                    {
                        if pos.board.piece_on(sq).is_some() {
                            // Path is blocked
                            return false;
                        }
                    }
                }
            }
            _ => {}
        }
        return true;
    }

    // 2. Discovered check - moving piece uncovers attack from behind
    // Only check for non-drop moves (drops can't cause discovered check)
    if !mv.is_drop() {
        let from = match mv.from() {
            Some(f) => f,
            None => return false,
        };

        // Check if 'from' square is on a line with the opponent king
        let dr_from = opp_king_sq.rank() as i8 - from.rank() as i8;
        let dc_from = opp_king_sq.file() as i8 - from.file() as i8;
        let dr_from_abs = dr_from.abs();
        let dc_from_abs = dc_from.abs();

        // Is 'from' on same rank/file/diagonal as king?
        let on_rank = dr_from == 0;
        let on_file = dc_from == 0;
        let on_diagonal = dr_from_abs == dc_from_abs;

        if on_rank || on_file || on_diagonal {
            // Quick check: look for a sliding piece behind 'from' that could attack the king
            let step_r = if dr_from != 0 {
                -dr_from / dr_from_abs
            } else {
                0
            };
            let step_c = if dc_from != 0 {
                -dc_from / dc_from_abs
            } else {
                0
            };

            // Check up to 8 squares behind 'from'
            for i in 1..=8 {
                let check_rank = from.rank() as i8 + i * step_r;
                let check_file = from.file() as i8 + i * step_c;

                // Check bounds
                if !(0..=8).contains(&check_rank) || !(0..=8).contains(&check_file) {
                    break;
                }

                if let Some(sq) = crate::shogi::Square::new_safe(check_file as u8, check_rank as u8)
                {
                    if let Some(piece) = pos.board.piece_on(sq) {
                        // Found a piece - check if it's our sliding piece that could attack
                        if piece.color == pos.side_to_move {
                            match piece.piece_type {
                                PieceType::Rook if on_rank || on_file => return true,
                                PieceType::Bishop if on_diagonal => return true,
                                PieceType::Lance if on_file => {
                                    // Check lance direction
                                    if pos.side_to_move == crate::shogi::Color::Black {
                                        if dr_from < 0 {
                                            return true;
                                        }
                                    } else if dr_from > 0 {
                                        return true;
                                    }
                                }
                                _ => {}
                            }
                        }
                        // Any piece blocks further discovery
                        break;
                    }
                }
            }
        }
    }

    false
}

/// Check if a move should skip SEE pruning (for shogi-specific moves)
/// Returns true if the move should NOT be pruned by SEE
pub fn should_skip_see_pruning(pos: &Position, mv: Move) -> bool {
    // Drop moves are excluded from SEE pruning
    // (drops have tactical value that SEE cannot evaluate properly)
    if mv.is_drop() {
        return true;
    }

    // King moves in check must be allowed (evasion moves)
    if pos.is_in_check() {
        return true;
    }

    // Promotion moves might be worth considering even with bad SEE
    // (especially pawn promotions to tokin)
    if mv.is_promote() && !mv.is_drop() {
        if let Some(from) = mv.from() {
            if let Some(piece) = pos.board.piece_on(from) {
                if piece.piece_type == crate::shogi::PieceType::Pawn {
                    return true; // Pawn promotion is SEE excluded
                }
            }
        }
    }

    // Moves that give check are excluded - but use lightweight pre-filter first
    // (checks often have tactical value beyond material exchange)
    let likely_check = likely_could_give_check(pos, mv);
    if likely_check {
        let actual_check = pos.gives_check(mv);
        if actual_check {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use crate::usi::parse_usi_square;

    use super::*;

    #[test]
    fn test_mate_score_detection() {
        assert!(is_mate_score(30001));
        assert!(is_mate_score(-30001));
        assert!(is_mate_score(40000));
        assert!(!is_mate_score(30000));
        assert!(!is_mate_score(-30000));
        assert!(!is_mate_score(0));
        assert!(!is_mate_score(100));
    }

    #[test]
    fn test_common_pruning_helpers() {
        // Test can_prune
        assert!(can_prune(false, 100, 200));
        assert!(!can_prune(true, 100, 200)); // in check
        assert!(!can_prune(false, 31000, 200)); // mate score alpha
        assert!(!can_prune(false, 100, -31000)); // mate score beta

        // Test can_prune_alpha
        assert!(can_prune_alpha(false, 100));
        assert!(!can_prune_alpha(true, 100)); // in check
        assert!(!can_prune_alpha(false, 31000)); // mate score

        // Test can_prune_beta
        assert!(can_prune_beta(false, 200));
        assert!(!can_prune_beta(true, 200)); // in check
        assert!(!can_prune_beta(false, -31000)); // mate score
    }

    #[test]
    fn test_null_move_pruning_conditions() {
        // Valid conditions
        let pos = Position::startpos();
        assert!(can_do_null_move(&pos, 3, false, 100, 50));
        assert!(can_do_null_move(&pos, 5, false, 100, 50));

        // Invalid: in check
        assert!(!can_do_null_move(&pos, 3, true, 100, 50));

        // Invalid: too shallow
        assert!(!can_do_null_move(&pos, 2, false, 100, 50));

        // Test reduction calculation
        assert_eq!(null_move_reduction(3), 2);
        assert_eq!(null_move_reduction(4), 3);
        assert_eq!(null_move_reduction(8), 4);
        assert_eq!(null_move_reduction(12), 5);
    }

    #[test]
    fn test_futility_pruning_boundary_conditions() {
        // Valid conditions
        assert!(can_do_futility_pruning(3, false, 100, 200, 150));
        assert!(can_do_futility_pruning(7, false, 100, 200, 150));

        // Invalid: too deep
        assert!(!can_do_futility_pruning(8, false, 100, 200, 150));

        // Invalid: in check
        assert!(!can_do_futility_pruning(3, true, 100, 200, 150));

        // Invalid: mate scores
        assert!(!can_do_futility_pruning(3, false, 31000, 200, 150));
        assert!(!can_do_futility_pruning(3, false, 100, -31000, 150));

        // Test optimized margin values
        assert_eq!(futility_margin(0), 0);
        assert_eq!(futility_margin(1), 80);
        assert_eq!(futility_margin(2), 160);
        assert_eq!(futility_margin(7), 650);
        assert_eq!(futility_margin(10), 650); // capped at 650
    }

    #[test]
    fn test_razoring_boundary_conditions() {
        // Valid razoring at depth 1
        assert!(can_do_razoring(1, false, 0, -400)); // static_eval + 350 < alpha
        assert!(can_do_razoring(2, false, 100, -300)); // static_eval + 350 < alpha

        // Invalid: static eval too high
        assert!(!can_do_razoring(1, false, 0, -300)); // -300 + 350 = 50 >= 0

        // Invalid: too deep
        assert!(!can_do_razoring(3, false, 0, -500));

        // Invalid: in check
        assert!(!can_do_razoring(1, true, 0, -500));

        // Invalid: mate score
        assert!(!can_do_razoring(1, false, 31000, -500));

        // Test margin values
        assert_eq!(razoring_margin(1), RAZORING_MARGIN_DEPTH_1);
        assert_eq!(razoring_margin(2), RAZORING_MARGIN_DEPTH_2);
        assert_eq!(razoring_margin(3), 0);
    }

    #[test]
    fn test_lmr_conditions_and_reductions() {
        use crate::shogi::Move;

        // Create test moves
        let normal_move =
            Move::normal(parse_usi_square("2h").unwrap(), parse_usi_square("2g").unwrap(), false);
        let capture_move = Move::normal_with_piece(
            parse_usi_square("2h").unwrap(),
            parse_usi_square("2g").unwrap(),
            false,
            crate::shogi::PieceType::Pawn,
            Some(crate::shogi::PieceType::Pawn),
        );
        let promote_move =
            Move::normal(parse_usi_square("2h").unwrap(), parse_usi_square("2d").unwrap(), true);

        // Valid LMR conditions
        assert!(can_do_lmr(3, 4, false, false, normal_move));
        assert!(can_do_lmr(5, 10, false, false, normal_move));

        // Invalid: too few moves searched
        assert!(!can_do_lmr(3, 3, false, false, normal_move));

        // Invalid: too shallow
        assert!(!can_do_lmr(2, 5, false, false, normal_move));

        // Invalid: in check or gives check
        assert!(!can_do_lmr(3, 5, true, false, normal_move));
        assert!(!can_do_lmr(3, 5, false, true, normal_move));

        // Invalid: capture or promotion
        assert!(!can_do_lmr(3, 5, false, false, capture_move));
        assert!(!can_do_lmr(3, 5, false, false, promote_move));
    }

    #[test]
    fn test_lmr_reduction_table_optimized() {
        // Test the optimized reduction table for shogi

        // No reduction for early moves or shallow depths
        assert_eq!(lmr_reduction(2, 5), 0);
        assert_eq!(lmr_reduction(3, 3), 0);

        // Shallow depth reductions - more conservative
        assert_eq!(lmr_reduction(3, 4), 1);
        assert_eq!(lmr_reduction(3, 5), 1);

        // Medium depth reductions
        assert_eq!(lmr_reduction(4, 4), 2);
        assert_eq!(lmr_reduction(4, 6), 2);
        assert_eq!(lmr_reduction(5, 5), 2);
        assert_eq!(lmr_reduction(5, 8), 3);

        // Deep reductions
        assert_eq!(lmr_reduction(6, 7), 3);
        assert_eq!(lmr_reduction(6, 10), 3);
        assert_eq!(lmr_reduction(7, 12), 4);
        assert_eq!(lmr_reduction(8, 14), 4);

        // Very deep reductions
        assert_eq!(lmr_reduction(9, 16), 5);
        assert_eq!(lmr_reduction(10, 20), 5);
        assert_eq!(lmr_reduction(12, 24), 6); // max reduction
    }

    #[test]
    fn test_lmr_reduction_formula_optimized() {
        // Test optimized logarithmic formula
        assert_eq!(lmr_reduction_formula(2, 5), 0);
        assert_eq!(lmr_reduction_formula(3, 3), 0);

        // Should produce reasonable reductions
        let r1 = lmr_reduction_formula(5, 10);
        assert!(r1 > 0 && r1 <= 3);

        let r2 = lmr_reduction_formula(8, 20);
        assert!(r2 > r1); // deeper/later moves get more reduction
        assert!(r2 <= 6); // capped at 6

        let r3 = lmr_reduction_formula(10, 30);
        assert!(r3 <= 6); // capped at 6 maximum
    }

    #[test]
    fn test_static_null_move_pruning_optimized() {
        // Valid conditions with optimized parameters
        assert!(can_do_static_null_move(3, false, 100, 500)); // 500 - 100*3 = 200 >= 100
        assert!(can_do_static_null_move(2, false, 0, 250)); // 250 - 100*2 = 50 >= 0

        // Invalid: static eval too low
        assert!(!can_do_static_null_move(3, false, 100, 350)); // 350 - 100*3 = 50 < 100

        // Now valid at depth 7
        assert!(can_do_static_null_move(7, false, 100, 800)); // 800 - 100*7 = 100 >= 100

        // Invalid: too deep
        assert!(!can_do_static_null_move(8, false, 100, 1000));

        // Invalid: in check
        assert!(!can_do_static_null_move(3, true, 100, 500));

        // Invalid: mate score
        assert!(!can_do_static_null_move(3, false, 31000, 500));

        // Boundary test at exact threshold
        assert!(can_do_static_null_move(1, false, 100, 200)); // 200 - 100*1 = 100 >= 100
        assert!(!can_do_static_null_move(1, false, 100, 199)); // 199 - 100*1 = 99 < 100
    }

    #[test]
    fn test_delta_pruning_margin_optimized() {
        assert_eq!(delta_pruning_margin(), DELTA_PRUNING_MARGIN);
        assert_eq!(delta_pruning_margin(), 150); // verify optimized value
    }
}
