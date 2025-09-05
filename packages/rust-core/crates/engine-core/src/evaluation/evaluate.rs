//! Evaluation function for shogi
//!
//! Simple material-based evaluation

use crate::shogi::board::NUM_PIECE_TYPES;
use crate::{
    shogi::{ALL_PIECE_TYPES, NUM_HAND_PIECE_TYPES},
    Color, PieceType, Position, Square,
};

/// Trait for position evaluation
pub trait Evaluator {
    /// Evaluate position from side to move perspective
    fn evaluate(&self, pos: &Position) -> i32;
}

/// Implement Evaluator for Arc<T> where T: Evaluator
impl<T: Evaluator + ?Sized> Evaluator for std::sync::Arc<T> {
    fn evaluate(&self, pos: &Position) -> i32 {
        (**self).evaluate(pos)
    }
}

/// Piece values in centipawns
const PIECE_VALUES: [i32; NUM_PIECE_TYPES] = [
    0,    // King (infinite value, but we use 0 here)
    1000, // Rook
    800,  // Bishop
    450,  // Gold
    400,  // Silver
    350,  // Knight
    300,  // Lance
    100,  // Pawn
];

/// Promoted piece bonus
const PROMOTION_BONUS: [i32; NUM_PIECE_TYPES] = [
    0,   // King cannot promote
    200, // Dragon (promoted rook)
    200, // Horse (promoted bishop)
    0,   // Gold cannot promote
    50,  // Promoted silver
    100, // Promoted knight
    100, // Promoted lance
    300, // Tokin (promoted pawn)
];

/// Evaluate position from side to move perspective
pub fn evaluate(pos: &Position) -> i32 {
    let us = pos.side_to_move;
    let them = us.opposite();

    let mut score = 0;

    // Material on board
    for &pt in &ALL_PIECE_TYPES {
        let piece_type = pt as usize;

        // Count pieces
        let our_pieces = pos.board.piece_bb[us as usize][piece_type];
        let their_pieces = pos.board.piece_bb[them as usize][piece_type];

        let our_count = our_pieces.count_ones() as i32;
        let their_count = their_pieces.count_ones() as i32;

        score += PIECE_VALUES[piece_type] * (our_count - their_count);

        // Promotion bonus
        if pt != PieceType::King && pt != PieceType::Gold {
            let our_promoted = our_pieces & pos.board.promoted_bb;
            let their_promoted = their_pieces & pos.board.promoted_bb;

            let our_promoted_count = our_promoted.count_ones() as i32;
            let their_promoted_count = their_promoted.count_ones() as i32;

            score += PROMOTION_BONUS[piece_type] * (our_promoted_count - their_promoted_count);
        }
    }

    // Material in hand
    for piece_idx in 0..NUM_HAND_PIECE_TYPES {
        let our_hand = pos.hands[us as usize][piece_idx] as i32;
        let their_hand = pos.hands[them as usize][piece_idx] as i32;

        // Map piece index to piece type value
        let value = match piece_idx {
            0 => PIECE_VALUES[1], // Rook
            1 => PIECE_VALUES[2], // Bishop
            2 => PIECE_VALUES[3], // Gold
            3 => PIECE_VALUES[4], // Silver
            4 => PIECE_VALUES[5], // Knight
            5 => PIECE_VALUES[6], // Lance
            6 => PIECE_VALUES[7], // Pawn
            _ => unreachable!(),
        };

        score += value * (our_hand - their_hand);
    }

    // Add small positional bonus (placeholder for future improvements)
    score += evaluate_position(pos);

    score
}

/// Simple positional evaluation
fn evaluate_position(pos: &Position) -> i32 {
    let mut score = 0;
    let us = pos.side_to_move;
    let them = us.opposite();

    // Enhanced king safety evaluation
    score += evaluate_king_safety(pos, us) - evaluate_king_safety(pos, them);

    // Piece activity
    score += evaluate_piece_activity(pos, us) - evaluate_piece_activity(pos, them);

    // Pawn structure
    score += evaluate_pawn_structure(pos, us) - evaluate_pawn_structure(pos, them);

    // Castle formation
    score += evaluate_castle_formation(pos, us) - evaluate_castle_formation(pos, them);

    score
}

/// Enhanced king safety evaluation
fn evaluate_king_safety(pos: &Position, color: Color) -> i32 {
    let mut score = 0;

    if let Some(king_sq) = pos.board.king_square(color) {
        let king_file = king_sq.file();
        let king_rank = king_sq.rank();

        // Check if king has moved from initial position
        let initial_king_sq = match color {
            Color::Black => Square::new(4, 8), // 5i
            Color::White => Square::new(4, 0), // 5a
        };

        let king_has_moved = king_sq != initial_king_sq;

        // In the opening, penalize king moves
        // Use game phase detection to determine opening
        use crate::game_phase::{detect_game_phase, GamePhase, Profile};
        let phase = detect_game_phase(pos, pos.ply as u32, Profile::Search);

        if king_has_moved && phase == GamePhase::Opening {
            // Strong penalty for moving king in opening
            score -= 200;
        }

        // Base position bonus - prefer king on back ranks
        match color {
            Color::Black => {
                // Black king prefers rank 6-8 (own territory)
                if king_rank >= 6 {
                    score += 100;
                    // Reduced corner bonus to discourage unnecessary moves
                    if (king_file <= 2 || king_file >= 6) && king_rank >= 7 {
                        score += 20; // Reduced from 50
                    }
                } else {
                    // Penalty for exposed king
                    let exposure = (6i32 - king_rank as i32).max(0);
                    score -= exposure * 30;
                }
            }
            Color::White => {
                // White king prefers rank 0-2 (own territory)
                if king_rank <= 2 {
                    score += 100;
                    // Reduced corner bonus to discourage unnecessary moves
                    if (king_file <= 2 || king_file >= 6) && king_rank <= 1 {
                        score += 20; // Reduced from 50
                    }
                } else {
                    // Penalty for exposed king
                    let exposure = (king_rank as i32 - 2).max(0);
                    score -= exposure * 30;
                }
            }
        }

        // Check enemy attacks near king
        let enemy_color = color.opposite();
        let directions: [(i8, i8); 8] = [
            (-1, -1),
            (-1, 0),
            (-1, 1),
            (0, -1),
            (0, 1),
            (1, -1),
            (1, 0),
            (1, 1),
        ];

        let mut enemy_attackers = 0;
        let mut friendly_defenders = 0;

        // Check squares around king
        for &(df, dr) in &directions {
            let new_file = king_file as i8 + df;
            let new_rank = king_rank as i8 + dr;

            if (0..9).contains(&new_file) && (0..9).contains(&new_rank) {
                let sq = Square::new(new_file as u8, new_rank as u8);
                if let Some(piece) = pos.board.squares[sq.index()] {
                    if piece.color == enemy_color {
                        // Enemy piece near king
                        enemy_attackers += match piece.piece_type {
                            PieceType::Rook => 5,
                            PieceType::Bishop => 4,
                            PieceType::Gold => 3,
                            PieceType::Silver => 3,
                            _ => 2,
                        };
                    } else {
                        // Friendly piece defending
                        friendly_defenders += match piece.piece_type {
                            PieceType::Gold => 3,
                            PieceType::Silver => 2,
                            _ => 1,
                        };
                    }
                }
            }
        }

        // Apply attacker/defender balance
        score -= enemy_attackers * 20;
        score += friendly_defenders * 10;

        // Penalty for king in center files during middle/endgame
        if (3..=5).contains(&king_file) {
            score -= 15; // Reduced penalty for central king placement
        }
    }

    score
}

/// Evaluate piece activity (mobility and central control)
fn evaluate_piece_activity(pos: &Position, color: Color) -> i32 {
    let mut score = 0;

    // Central squares (files 3-5, ranks 3-5)
    const CENTRAL_SQUARES: [(u8, u8); 9] = [
        (3, 3),
        (3, 4),
        (3, 5),
        (4, 3),
        (4, 4),
        (4, 5),
        (5, 3),
        (5, 4),
        (5, 5),
    ];

    // Bonus for pieces in central squares
    for &(file, rank) in &CENTRAL_SQUARES {
        let sq = Square::new(file, rank);
        if let Some(piece) = pos.board.squares[sq.index()] {
            if piece.color == color && piece.piece_type != PieceType::King {
                let bonus = match piece.piece_type {
                    PieceType::Knight => 20, // Knights are good in center
                    PieceType::Silver => 15, // Silvers too
                    PieceType::Gold => 10,   // Golds are defensive
                    PieceType::Bishop => 15, // Bishops control diagonals
                    PieceType::Rook => 10,   // Rooks prefer files
                    _ => 5,
                };
                score += bonus;
                if piece.promoted {
                    score += 5; // Extra bonus for promoted pieces in center
                }
            }
        }
    }

    // Bonus for advanced pieces (in enemy territory)
    let advanced_ranks = match color {
        Color::Black => 0..=3, // Black advances upward (ranks 0-3 are enemy territory)
        Color::White => 5..=8, // White advances downward (ranks 5-8 are enemy territory)
    };

    for rank in advanced_ranks {
        for file in 0..9 {
            let sq = Square::new(file, rank);
            if let Some(piece) = pos.board.squares[sq.index()] {
                if piece.color == color && piece.piece_type != PieceType::King {
                    let bonus = match piece.piece_type {
                        PieceType::Pawn => 10,   // Advanced pawns are strong
                        PieceType::Silver => 15, // Advanced silvers threaten
                        PieceType::Knight => 12, // Knights in enemy camp
                        _ => 8,
                    };
                    score += bonus;
                }
            }
        }
    }

    score
}

/// Evaluate pawn structure
fn evaluate_pawn_structure(pos: &Position, color: Color) -> i32 {
    let mut score = 0;
    let pawn_bb = pos.board.piece_bb[color as usize][PieceType::Pawn as usize];

    // Note: Doubled pawns (nifu) are illegal in shogi, so no penalty needed
    // Note: Shogi pawns cannot protect each other (no diagonal movement), so no connected pawn bonus

    // Bonus for advanced pawns (closer to promotion)
    let mut pawn_copy = pawn_bb;
    while let Some(sq) = pawn_copy.pop_lsb() {
        let advancement = match color {
            Color::Black => 8 - sq.rank(), // Black promotes at rank 0-2
            Color::White => sq.rank(),     // White promotes at rank 6-8
        };
        if advancement >= 5 {
            score += 5 * (advancement as i32 - 4);
        }
    }

    score
}

/// Evaluate castle formation (king safety with surrounding pieces)
fn evaluate_castle_formation(pos: &Position, color: Color) -> i32 {
    let mut score = 0;

    if let Some(king_sq) = pos.board.king_square(color) {
        let king_file = king_sq.file();
        let king_rank = king_sq.rank();

        // Check for castle formation based on king position
        let is_castled = match color {
            Color::Black => king_rank >= 6 && (king_file <= 2 || king_file >= 6), // Black's back rank
            Color::White => king_rank <= 2 && (king_file <= 2 || king_file >= 6), // White's back rank
        };

        if is_castled {
            score += 30; // Bonus for castled king

            // Note: Detailed castle evaluation (defenders, holes) removed to avoid
            // double-counting with king_safety which already evaluates nearby pieces
        }
    }

    score
}

/// Simple material evaluator implementing Evaluator trait
#[derive(Clone, Copy, Debug)]
pub struct MaterialEvaluator;

impl Evaluator for MaterialEvaluator {
    fn evaluate(&self, pos: &Position) -> i32 {
        evaluate(pos)
    }
}

#[cfg(test)]
mod tests {
    use crate::{usi::parse_usi_square, Piece};

    use super::*;

    #[test]
    fn test_evaluate_startpos() {
        let pos = Position::startpos();
        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Starting position should be roughly equal
        assert!(score.abs() < 100);
    }

    #[test]
    fn test_evaluate_material() {
        let mut pos = Position::empty();

        // Black has rook, White has bishop
        // Place kings in safe back rank positions to minimize king safety effects
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black)); // Black's safe corner
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White)); // White's safe corner
        pos.board
            .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Rook, Color::Black));
        pos.board.put_piece(
            parse_usi_square("8h").unwrap(),
            Piece::new(PieceType::Bishop, Color::White),
        );

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Black should be ahead by material difference (rook=1000 - bishop=800 = 200)
        // Both kings are safe in corners, so king safety bonus should be similar
        // Expected: 200 (material) + small king safety difference
        assert!((180..=220).contains(&score), "Score was {score}, expected around 200");
    }

    #[test]
    fn test_evaluate_promoted() {
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;

        // Both have promoted pawns
        // Place kings in safe back rank positions to minimize king safety effects
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black)); // Black's safe corner
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White)); // White's safe corner

        let mut tokin_black = Piece::new(PieceType::Pawn, Color::Black);
        tokin_black.promoted = true;
        pos.board.put_piece(parse_usi_square("5e").unwrap(), tokin_black);

        let mut tokin_white = Piece::new(PieceType::Pawn, Color::White);
        tokin_white.promoted = true;
        pos.board.put_piece(parse_usi_square("5f").unwrap(), tokin_white);

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Both have tokin worth 100+300=400, kings in similar safe positions
        // Score should be approximately equal
        assert!(score.abs() < 50, "Score was {score}, expected near 0");
    }

    #[test]
    fn test_piece_activity() {
        let mut pos = Position::empty();

        // Place kings in their proper territories
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black)); // Black's territory
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White)); // White's territory

        // Black has a knight in center, white has a knight on edge
        pos.board.put_piece(
            parse_usi_square("5e").unwrap(),
            Piece::new(PieceType::Knight, Color::Black),
        );
        pos.board.put_piece(
            parse_usi_square("9i").unwrap(),
            Piece::new(PieceType::Knight, Color::White),
        );

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Black should have advantage from central knight
        assert!(score > 0);
    }

    #[test]
    fn test_pawn_structure() {
        let mut pos = Position::empty();

        // Place kings in their proper territories
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black)); // Black's territory
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White)); // White's territory

        // Place some pawns (doubled pawns and connected pawns are not evaluated in shogi)
        pos.board
            .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Pawn, Color::Black)); // Nifu would be illegal in real game

        pos.board
            .put_piece(parse_usi_square("7f").unwrap(), Piece::new(PieceType::Pawn, Color::White));
        pos.board
            .put_piece(parse_usi_square("6e").unwrap(), Piece::new(PieceType::Pawn, Color::White));

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Score should be roughly equal (only advancement bonus matters)
        assert!(score.abs() <= 50);
    }

    #[test]
    fn test_castle_formation() {
        let mut pos = Position::empty();

        // Black has castled king with defenders (in correct back rank)
        pos.board
            .put_piece(parse_usi_square("8i").unwrap(), Piece::new(PieceType::King, Color::Black)); // Correct back rank
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::Lance, Color::Black));
        pos.board
            .put_piece(parse_usi_square("7i").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board.put_piece(
            parse_usi_square("8h").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        // White king is exposed in center
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::White));

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Black should have significant advantage from castle safety
        assert!(score > 100);
    }

    #[test]
    fn test_advanced_pawns() {
        let mut pos = Position::empty();

        // Place kings in their proper territories
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black)); // Black's territory
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White)); // White's territory

        // Black has advanced pawn
        pos.board
            .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

        // White has backward pawn
        pos.board
            .put_piece(parse_usi_square("4c").unwrap(), Piece::new(PieceType::Pawn, Color::White));

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Black should have advantage from advanced pawn
        assert!(score > 0);
    }

    #[test]
    fn test_king_safety_coordinate_bug() {
        // Test that demonstrates the coordinate bug in king safety evaluation

        // Create position with Black king in actual safe position (8i)
        let mut pos1 = Position::empty();
        pos1.board.put_piece(
            parse_usi_square("8i").unwrap(), // rank 8 - actual safe position for Black
            Piece::new(PieceType::King, Color::Black),
        );
        let black_safe_score = evaluate_king_safety(&pos1, Color::Black);

        // Create position with Black king in unsafe position (8a)
        let mut pos2 = Position::empty();
        pos2.board.put_piece(
            parse_usi_square("8a").unwrap(), // rank 0 - enemy territory for Black
            Piece::new(PieceType::King, Color::Black),
        );
        let black_unsafe_score = evaluate_king_safety(&pos2, Color::Black);

        // Fixed: 8i (rank 8) gives bonus and 8a (rank 0) gives penalty
        eprintln!("Black king at 8i (safe): {}", black_safe_score);
        eprintln!("Black king at 8a (unsafe): {}", black_unsafe_score);

        // Correct behavior after fix
        assert!(
            black_safe_score > black_unsafe_score,
            "Black king should be safer at 8i ({}) than 8a ({})",
            black_safe_score,
            black_unsafe_score
        );
    }

    #[test]
    fn test_white_king_safety_coordinate_bug() {
        // Test White king safety

        // Create position with White king in actual safe position (2a)
        let mut pos1 = Position::empty();
        pos1.board.put_piece(
            parse_usi_square("2a").unwrap(), // rank 0 - actual safe position for White
            Piece::new(PieceType::King, Color::White),
        );
        let white_safe_score = evaluate_king_safety(&pos1, Color::White);

        // Create position with White king in unsafe position (2i)
        let mut pos2 = Position::empty();
        pos2.board.put_piece(
            parse_usi_square("2i").unwrap(), // rank 8 - enemy territory for White
            Piece::new(PieceType::King, Color::White),
        );
        let white_unsafe_score = evaluate_king_safety(&pos2, Color::White);

        eprintln!("White king at 2a (safe): {}", white_safe_score);
        eprintln!("White king at 2i (unsafe): {}", white_unsafe_score);

        // Correct behavior after fix
        assert!(
            white_safe_score > white_unsafe_score,
            "White king should be safer at 2a ({}) than 2i ({})",
            white_safe_score,
            white_unsafe_score
        );
    }

    #[test]
    fn test_piece_activity_coordinate_bug() {
        // Test piece advancement bonus

        // Black pawn in enemy territory (should get bonus)
        let mut pos1 = Position::empty();
        pos1.board.put_piece(
            parse_usi_square("5c").unwrap(), // rank 2 - enemy territory for Black
            Piece::new(PieceType::Pawn, Color::Black),
        );
        let black_advanced_score = evaluate_piece_activity(&pos1, Color::Black);

        // Black pawn in own territory (should not get bonus)
        let mut pos2 = Position::empty();
        pos2.board.put_piece(
            parse_usi_square("5g").unwrap(), // rank 6 - own territory for Black
            Piece::new(PieceType::Pawn, Color::Black),
        );
        let black_home_score = evaluate_piece_activity(&pos2, Color::Black);

        eprintln!("Black pawn at 5c (enemy territory): {}", black_advanced_score);
        eprintln!("Black pawn at 5g (own territory): {}", black_home_score);

        // Correct behavior after fix
        assert!(
            black_advanced_score > black_home_score,
            "Black pawn in enemy territory ({}) should have bonus over own territory ({})",
            black_advanced_score,
            black_home_score
        );
    }

    #[test]
    fn test_coordinate_fixes_comprehensive() {
        // Comprehensive test to verify all coordinate fixes work together
        let mut pos = Position::empty();

        // Set up a position with kings in their proper territories
        pos.board.put_piece(
            parse_usi_square("7i").unwrap(), // Black king in safe corner (rank 8)
            Piece::new(PieceType::King, Color::Black),
        );
        pos.board.put_piece(
            parse_usi_square("3a").unwrap(), // White king in safe corner (rank 0)
            Piece::new(PieceType::King, Color::White),
        );

        // Add some defenders for black
        pos.board
            .put_piece(parse_usi_square("8i").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board.put_piece(
            parse_usi_square("6i").unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        // Add advanced pawns
        pos.board.put_piece(
            parse_usi_square("7c").unwrap(), // Black pawn in enemy territory (rank 2)
            Piece::new(PieceType::Pawn, Color::Black),
        );
        pos.board.put_piece(
            parse_usi_square("3g").unwrap(), // White pawn in enemy territory (rank 6)
            Piece::new(PieceType::Pawn, Color::White),
        );

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        eprintln!("Comprehensive test score: {}", score);

        // Black should have advantage:
        // - Extra defenders (Gold + Silver vs none)
        // - Both have safe kings
        // - Both have advanced pawns
        assert!(score > 200, "Black should have significant advantage from extra pieces");
    }
}
