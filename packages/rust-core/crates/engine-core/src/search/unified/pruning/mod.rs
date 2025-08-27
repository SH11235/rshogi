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
///
/// Null move pruning is disabled in endgame positions to avoid zugzwang issues
pub fn can_do_null_move(
    pos: &Position,
    depth: u8,
    in_check: bool,
    _beta: i32,
    _static_eval: i32,
) -> bool {
    !in_check && depth >= 3 && !pos.is_endgame() // Avoid null move in endgame due to zugzwang
}

/// Check if null move pruning is applicable (with PV node support)
pub fn can_do_null_move_with_pv(
    pos: &Position,
    depth: u8,
    in_check: bool,
    beta: i32,
    static_eval: i32,
    is_pv: bool,
) -> bool {
    !is_pv && // Never do null move in PV nodes
    can_do_null_move(pos, depth, in_check, beta, static_eval)
}

/// Calculate null move reduction
pub fn null_move_reduction(depth: u8) -> u8 {
    2 + depth / 4
}

/// Check if futility pruning is applicable
///
/// Note: The caller must ensure that the move is not:
/// - A capture
/// - A check
/// - A promotion  
/// - A drop
///
/// These tactical moves should never be pruned by futility.
pub fn can_do_futility_pruning(
    depth: u8,
    in_check: bool,
    alpha: i32,
    beta: i32,
    _static_eval: i32,
) -> bool {
    depth <= 7 && can_prune(in_check, alpha, beta)
}

/// Check if futility pruning is applicable (with move awareness)
pub fn can_do_futility_pruning_for_move(
    depth: u8,
    in_check: bool,
    alpha: i32,
    beta: i32,
    static_eval: i32,
    mv: Move,
    gives_check: bool,
) -> bool {
    can_do_futility_pruning(depth, in_check, alpha, beta, static_eval)
        && !mv.is_capture_hint()
        && !gives_check
        && !mv.is_promote()
        && !mv.is_drop()
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
        && !mv.is_drop() // Drop moves are tactically sharp in shogi
}

/// Check if we can do razoring (extreme futility pruning at low depths)
///
/// Note: Should not be applied in PV nodes or when tactical moves are possible.
/// The caller should ensure proper conditions before applying razoring.
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
#[cfg(test)]
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

/// Check if static null move pruning is applicable (with position awareness)
pub fn can_do_static_null_move_with_pos(
    pos: &Position,
    depth: u8,
    in_check: bool,
    beta: i32,
    static_eval: i32,
) -> bool {
    !pos.is_endgame() && // Avoid in endgame positions
    can_do_static_null_move(depth, in_check, beta, static_eval)
}

/// Lightweight pre-filter to check if a move might give check
/// This is much cheaper than full gives_check() calculation
///
/// IMPORTANT: This function must be called with the position BEFORE the move is made.
/// The function analyzes whether 'mv' would give check when played from 'pos'.
///
/// Note: King cannot give check to opponent king (illegal by shogi rules), so we don't handle direct King attacks.
/// However, King moves can still cause discovered checks, which are handled in the discovered check section.
#[inline]
pub fn likely_could_give_check(pos: &Position, mv: Move) -> bool {
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
        PieceType::Rook => {
            (dr == 0 || dc == 0) || (will_be_promoted && dr_abs == 1 && dc_abs == 1)
            // Dragon can move 1 square diagonally
        }
        PieceType::Bishop => {
            (dr_abs == dc_abs && dr_abs > 0)
                || (will_be_promoted && ((dr_abs == 1 && dc == 0) || (dc_abs == 1 && dr == 0)))
            // Horse can move 1 square orthogonally
        }
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
                            // Check if this is a sliding piece that could attack through the line
                            let piece_found = match piece.piece_type {
                                PieceType::Rook => {
                                    // Unpromoted rook: rank/file only
                                    // Promoted rook (dragon): rank/file + one square diagonally
                                    on_rank || on_file
                                }
                                PieceType::Bishop => {
                                    // Unpromoted bishop: diagonal only
                                    // Promoted bishop (horse): diagonal + one square orthogonally
                                    on_diagonal
                                }
                                PieceType::Lance if !piece.promoted => {
                                    // Unpromoted lance: forward file only
                                    on_file
                                        && ((pos.side_to_move == crate::shogi::Color::Black
                                            && dr_from < 0)
                                            || (pos.side_to_move == crate::shogi::Color::White
                                                && dr_from > 0))
                                }
                                _ => false,
                            };

                            if piece_found {
                                // Check if 'to' is on the same line and between king and the sliding piece
                                if is_collinear(opp_king_sq, from, to)
                                    && is_between(opp_king_sq, to, sq)
                                {
                                    // The destination still blocks the line, so no discovered check
                                    return false;
                                }

                                // Check if there's already a blocker between 'from' and king
                                // This prevents false positives where the line is already blocked
                                let step_r_fwd = if dr_from != 0 {
                                    dr_from / dr_from_abs
                                } else {
                                    0
                                };
                                let step_c_fwd = if dc_from != 0 {
                                    dc_from / dc_from_abs
                                } else {
                                    0
                                };

                                for j in 1..dr_from_abs.max(dc_from_abs) {
                                    let check_rank = from.rank() as i8 + j * step_r_fwd;
                                    let check_file = from.file() as i8 + j * step_c_fwd;

                                    if let Some(sq_mid) = crate::shogi::Square::new_safe(
                                        check_file as u8,
                                        check_rank as u8,
                                    ) {
                                        if pos.board.piece_on(sq_mid).is_some() {
                                            // There's already a blocker between from and king
                                            return false;
                                        }
                                    }
                                }

                                return true;
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

/// Helper function to check if three squares are collinear
fn is_collinear(a: crate::shogi::Square, b: crate::shogi::Square, c: crate::shogi::Square) -> bool {
    let a_rank = a.rank() as i32;
    let a_file = a.file() as i32;
    let b_rank = b.rank() as i32;
    let b_file = b.file() as i32;
    let c_rank = c.rank() as i32;
    let c_file = c.file() as i32;

    // Check if the cross product is zero (points are collinear)
    (b_file - a_file) * (c_rank - a_rank) == (b_rank - a_rank) * (c_file - a_file)
}

/// Helper function to check if b is between a and c on a line
fn is_between(a: crate::shogi::Square, b: crate::shogi::Square, c: crate::shogi::Square) -> bool {
    let a_rank = a.rank() as i32;
    let a_file = a.file() as i32;
    let b_rank = b.rank() as i32;
    let b_file = b.file() as i32;
    let c_rank = c.rank() as i32;
    let c_file = c.file() as i32;

    // Check if b is within the bounding box of a and c
    let min_rank = a_rank.min(c_rank);
    let max_rank = a_rank.max(c_rank);
    let min_file = a_file.min(c_file);
    let max_file = a_file.max(c_file);

    b_rank >= min_rank && b_rank <= max_rank && b_file >= min_file && b_file <= max_file
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

    #[test]
    fn test_null_move_endgame_suppression() {
        use crate::shogi::board::{Color, Piece, PieceType, Square};
        use crate::shogi::Position;

        // Create an endgame position (only kings and a rook)
        let mut endgame_pos = Position::empty();
        endgame_pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        endgame_pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        endgame_pos.board.put_piece(
            Square::from_usi_chars('1', 'i').unwrap(),
            Piece::new(PieceType::Rook, Color::Black),
        );
        endgame_pos.board.rebuild_occupancy_bitboards();

        // Verify it's detected as endgame
        assert!(endgame_pos.is_endgame(), "Position should be detected as endgame");

        // Null move should be suppressed in endgame
        assert!(
            !can_do_null_move(&endgame_pos, 5, false, 100, 50),
            "Null move should be suppressed in endgame"
        );

        // Create an opening position
        let opening_pos = Position::startpos();

        // Verify it's not endgame
        assert!(!opening_pos.is_endgame(), "Starting position should not be endgame");
        assert!(opening_pos.is_opening(), "Starting position should be opening");

        // Null move should be allowed in opening
        assert!(
            can_do_null_move(&opening_pos, 5, false, 100, 50),
            "Null move should be allowed in opening"
        );

        // Test with PV node
        assert!(
            !can_do_null_move_with_pv(&opening_pos, 5, false, 100, 50, true),
            "Null move should not be allowed in PV nodes"
        );
        assert!(
            can_do_null_move_with_pv(&opening_pos, 5, false, 100, 50, false),
            "Null move should be allowed in non-PV nodes"
        );
    }

    #[test]
    fn test_lmr_reduction_monotonicity() {
        // Test that LMR reduction increases monotonically with depth and moves searched

        // Test monotonicity with respect to depth (fixed moves)
        for moves in [4, 10, 20, 30] {
            let mut prev_reduction = 0;
            for depth in 3..=12 {
                let reduction = lmr_reduction(depth, moves);
                assert!(
                    reduction >= prev_reduction,
                    "LMR reduction should be monotonic in depth: lmr({depth}, {moves}) = {reduction} < {prev_reduction}"
                );
                prev_reduction = reduction;
            }
        }

        // Test monotonicity with respect to moves searched (fixed depth)
        for depth in [3, 5, 8, 10] {
            let mut prev_reduction = 0;
            for moves in 4..=30 {
                let reduction = lmr_reduction(depth, moves);
                assert!(
                    reduction >= prev_reduction,
                    "LMR reduction should be monotonic in moves: lmr({depth}, {moves}) = {reduction} < {prev_reduction}"
                );
                prev_reduction = reduction;
            }
        }

        // Test the formula version as well
        for depth in 3..=12 {
            let mut prev_reduction = 0;
            for moves in 4..=30 {
                let reduction = lmr_reduction_formula(depth, moves);
                assert!(
                    reduction >= prev_reduction,
                    "LMR formula should be monotonic: lmr_formula({depth}, {moves}) = {reduction} < {prev_reduction}"
                );
                prev_reduction = reduction;
            }
        }
    }

    #[test]
    fn test_promoted_lance_discovered_check() {
        use crate::shogi::{Color, Move, Piece, PieceType, Position, Square};

        // Test 1: Promoted lance should NOT be detected as giving discovered check
        // Setup: Black promoted lance behind a piece, with White king in line
        let mut pos = Position::empty();
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(Square::new(4, 6), Piece::promoted(PieceType::Lance, Color::Black));
        pos.board
            .put_piece(Square::new(4, 5), Piece::new(PieceType::Pawn, Color::Black));
        pos.side_to_move = Color::Black;

        // Move the pawn sideways - should NOT create discovered check from promoted lance
        let mv = Move::normal(Square::new(4, 5), Square::new(3, 5), false);
        assert!(
            !likely_could_give_check(&pos, mv),
            "Promoted lance should not give discovered check (moves like gold)"
        );

        // Test 2: Unpromoted lance SHOULD be detected as giving discovered check
        pos.board
            .put_piece(Square::new(4, 6), Piece::new(PieceType::Lance, Color::Black));
        assert!(
            likely_could_give_check(&pos, mv),
            "Unpromoted lance should give discovered check when piece moves off the file"
        );

        // Test 3: Promoted rook (dragon) should still give discovered check on rank/file
        pos.board
            .put_piece(Square::new(4, 6), Piece::promoted(PieceType::Rook, Color::Black));
        assert!(
            likely_could_give_check(&pos, mv),
            "Promoted rook should give discovered check on rank/file"
        );

        // Test 4: Promoted bishop (horse) should give discovered check on diagonal
        let mut pos2 = Position::empty();
        pos2.board
            .put_piece(Square::new(2, 2), Piece::new(PieceType::King, Color::White));
        pos2.board
            .put_piece(Square::new(4, 4), Piece::promoted(PieceType::Bishop, Color::Black));
        pos2.board
            .put_piece(Square::new(3, 3), Piece::new(PieceType::Pawn, Color::Black));
        pos2.side_to_move = Color::Black;

        // Move pawn off diagonal
        let mv2 = Move::normal(Square::new(3, 3), Square::new(3, 2), false);
        assert!(
            likely_could_give_check(&pos2, mv2),
            "Promoted bishop should give discovered check on diagonal"
        );
    }

    #[test]
    fn test_gives_check_pre_move_position() {
        use crate::shogi::board::{Color, Piece, PieceType, Square};
        use crate::shogi::Position;

        // Test that gives_check functions work on pre-move position
        // Setup: Black rook on 2h, white king on 2a
        // Move rook to 2b gives check
        let mut pos = Position::empty();
        pos.board.put_piece(
            Square::from_usi_chars('2', 'h').unwrap(),
            Piece::new(PieceType::Rook, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('2', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );
        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        // Move that gives check
        let mv = Move::normal(
            Square::from_usi_chars('2', 'h').unwrap(),
            Square::from_usi_chars('2', 'b').unwrap(),
            false,
        );

        // Test lightweight check works on pre-move position
        assert!(
            likely_could_give_check(&pos, mv),
            "Rook to 2b should likely give check to king on 2a"
        );

        // Test actual gives_check on pre-move position
        assert!(pos.gives_check(mv), "Rook to 2b should give check to king on 2a");

        // Make the move and verify post-move state
        let undo_info = pos.do_move(mv);
        assert!(pos.is_in_check(), "White should be in check after rook to 2b");

        // After move, side_to_move has changed
        assert_eq!(pos.side_to_move, Color::White);

        // Undo and verify
        pos.undo_move(mv, undo_info);
        assert_eq!(pos.side_to_move, Color::Black);
    }

    #[test]
    fn test_lmr_does_not_reduce_checking_moves() {
        use crate::shogi::Move;

        // Test that moves giving check are not reduced by LMR
        let normal_move =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);

        // Verify LMR conditions with gives_check = true
        assert!(
            !can_do_lmr(5, 10, false, true, normal_move),
            "LMR should not reduce moves that give check"
        );

        // Verify LMR conditions with gives_check = false
        assert!(
            can_do_lmr(5, 10, false, false, normal_move),
            "LMR should be applicable for non-checking moves"
        );
    }

    #[test]
    fn test_likely_could_give_check_no_false_negatives() {
        use crate::{
            movegen::MoveGenerator,
            shogi::Position,
        };

        // Test that likely_could_give_check never returns false when gives_check is true
        // This ensures our lightweight filter doesn't miss any actual checks

        // Test various positions
        let test_positions = vec![
            // Starting position
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            // Middle game position with pieces in hand
            "ln1g1g1nl/1r2k2b1/p1pppp1pp/1p4p2/9/2P4P1/PP1PPPP1P/1B5R1/LN1GKGSNL b Ss 20",
            // Endgame position with kings
            "8k/9/9/9/9/9/9/9/8K b 2r2b4g4s4n4l14p 1",
            // Position with promoted pieces
            "+R2g1g1nl/2s1k1sb1/p1pppp1pp/1p4p2/9/2P4P1/PP1PPPP1P/1+B5R1/LN1GKG1NL w - 30",
        ];

        for sfen in &test_positions {
            let pos = match Position::from_sfen(sfen) {
                Ok(p) => p,
                Err(_) => continue, // Skip invalid SFEN
            };

            let move_gen = MoveGenerator::new();
            let moves = move_gen.generate_all(&pos).unwrap();

            let mut false_negatives = 0;
            let mut total_checks = 0;

            for &mv in moves.as_slice() {
                // Skip if move is invalid
                if !pos.is_pseudo_legal(mv) {
                    continue;
                }

                let gives_check = pos.gives_check(mv);
                let likely_check = likely_could_give_check(&pos, mv);

                if gives_check {
                    total_checks += 1;
                    if !likely_check {
                        false_negatives += 1;
                        #[cfg(debug_assertions)]
                        {
                            eprintln!(
                                "False negative detected! Move {} gives check but likely_could_give_check returned false",
                                crate::usi::move_to_usi(&mv)
                            );
                        }
                    }
                }
            }

            assert_eq!(
                false_negatives, 0,
                "Found {false_negatives} false negatives out of {total_checks} checking moves in position: {sfen}"
            );
        }
    }

    #[test]
    fn test_likely_could_give_check_property_random_positions() {
        use crate::{
            movegen::MoveGenerator,
            shogi::Position,
        };
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        // Property-based test with semi-random positions
        // For any position and pseudo-legal move:
        // gives_check(m) == true => likely_could_give_check(m) == true

        // Generate deterministic "random" positions for reproducibility
        let mut hasher = DefaultHasher::new();
        let base_positions = vec![
            Position::startpos(),
            // Add more base positions as needed
        ];

        for (idx, mut pos) in base_positions.into_iter().enumerate() {
            // Apply some semi-random moves to create varied positions
            let move_gen = MoveGenerator::new();

            // Make 5-15 moves from starting position
            let num_moves = 5 + (idx % 11);
            for i in 0..num_moves {
                let moves = move_gen.generate_all(&pos).unwrap();
                if moves.is_empty() {
                    break;
                }

                // Pick a move based on hash
                i.hash(&mut hasher);
                let hash = hasher.finish();
                let move_idx = (hash as usize) % moves.len();
                let mv = moves.as_slice()[move_idx];

                if pos.is_pseudo_legal(mv) {
                    pos.do_move(mv);
                }
            }

            // Now test all moves from this position
            let moves = move_gen.generate_all(&pos).unwrap();

            for &mv in moves.as_slice() {
                if !pos.is_pseudo_legal(mv) {
                    continue;
                }

                let gives_check = pos.gives_check(mv);
                let likely_check = likely_could_give_check(&pos, mv);

                // Property: gives_check implies likely_could_give_check
                if gives_check {
                    assert!(
                        likely_check,
                        "Property violated: gives_check is true but likely_could_give_check is false for move {}",
                        crate::usi::move_to_usi(&mv)
                    );
                }
            }
        }
    }

    #[test]
    fn test_likely_could_give_check_discovered_checks() {
        use crate::{
            movegen::MoveGenerator,
            shogi::Position,
        };

        // Specific test for discovered checks
        // Position where moving a piece uncovers an attack from behind

        // White king on 5d (rank 5), Black pawn on 5f (rank 3), Black rook on 5h (rank 1), Black king on 1i
        // Moving the pawn from 5f to 5e discovers check from the rook
        let sfen = "9/9/9/4k4/9/4P4/9/3R5/K8 b - 1";
        let pos = Position::from_sfen(sfen).unwrap();

        // Find the pawn move
        let from = parse_usi_square("5f").unwrap();
        let to = parse_usi_square("5e").unwrap();

        let move_gen = MoveGenerator::new();
        let moves = move_gen.generate_all(&pos).unwrap();

        let pawn_move = moves
            .as_slice()
            .iter()
            .find(|&&mv| !mv.is_drop() && mv.from() == Some(from) && mv.to() == to)
            .copied();

        assert!(pawn_move.is_some(), "Pawn move from 5f to 5e should exist");
        let mv = pawn_move.unwrap();

        assert!(pos.gives_check(mv), "Pawn move should give discovered check");
        assert!(
            likely_could_give_check(&pos, mv),
            "likely_could_give_check should detect discovered check"
        );
    }
}
