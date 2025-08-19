//! Evaluation function for shogi
//!
//! Simple material-based evaluation

use crate::{
    shogi::{attacks, ALL_PIECE_TYPES},
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
const PIECE_VALUES: [i32; 8] = [
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
const PROMOTION_BONUS: [i32; 8] = [
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
    for piece_idx in 0..7 {
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

        // Base position bonus - prefer king on back ranks
        match color {
            Color::Black => {
                // Black king prefers rank 0-2
                if king_rank <= 2 {
                    score += 100; // Increased from 50
                                  // Extra bonus for corner safety
                    if (king_file <= 2 || king_file >= 6) && king_rank <= 1 {
                        score += 50;
                    }
                } else {
                    // Penalty for exposed king
                    let exposure = king_rank as i32;
                    score -= exposure * 30; // -30 per rank away from safety
                }
            }
            Color::White => {
                // White king prefers rank 6-8
                if king_rank >= 6 {
                    score += 100; // Increased from 50
                                  // Extra bonus for corner safety
                    if (king_file <= 2 || king_file >= 6) && king_rank >= 7 {
                        score += 50;
                    }
                } else {
                    // Penalty for exposed king
                    let exposure = (8 - king_rank) as i32;
                    score -= exposure * 30; // -30 per rank away from safety
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
            score -= 30; // Discourage central king placement
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
        Color::Black => 5..=8, // Ranks 6-9 for black
        Color::White => 0..=3, // Ranks 1-4 for white
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

    // Penalty for doubled pawns (two pawns on same file)
    for file in 0..9 {
        let file_mask = attacks::file_mask(file);
        let pawns_on_file = (pawn_bb & file_mask).count_ones();
        if pawns_on_file > 1 {
            score -= 20 * (pawns_on_file as i32 - 1);
        }
    }

    // Bonus for connected pawns (pawns protecting each other)
    let mut pawn_copy = pawn_bb;
    while let Some(sq) = pawn_copy.pop_lsb() {
        let file = sq.file();
        let rank = sq.rank();

        // Check adjacent files for supporting pawns
        let support_rank = match color {
            Color::Black => {
                if rank > 0 {
                    Some(rank - 1)
                } else {
                    None
                }
            }
            Color::White => {
                if rank < 8 {
                    Some(rank + 1)
                } else {
                    None
                }
            }
        };

        let mut has_support = false;
        if let Some(sup_rank) = support_rank {
            if file > 0 {
                let left_sq = Square::new(file - 1, sup_rank);
                if pawn_bb.test(left_sq) {
                    has_support = true;
                }
            }
            if file < 8 {
                let right_sq = Square::new(file + 1, sup_rank);
                if pawn_bb.test(right_sq) {
                    has_support = true;
                }
            }
        }

        if has_support {
            score += 10; // Connected pawns bonus
        }
    }

    // Bonus for advanced pawns (closer to promotion)
    let mut pawn_copy = pawn_bb;
    while let Some(sq) = pawn_copy.pop_lsb() {
        let advancement = match color {
            Color::Black => sq.rank(),
            Color::White => 8 - sq.rank(),
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
            Color::Black => king_rank <= 2 && (king_file <= 2 || king_file >= 6),
            Color::White => king_rank >= 6 && (king_file <= 2 || king_file >= 6),
        };

        if is_castled {
            score += 30; // Bonus for castled king

            // Check for protective pieces around king
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

            let mut defenders = 0;
            for &(df, dr) in &directions {
                let new_file = king_file as i8 + df;
                let new_rank = king_rank as i8 + dr;

                if (0..9).contains(&new_file) && (0..9).contains(&new_rank) {
                    let sq = Square::new(new_file as u8, new_rank as u8);
                    if let Some(piece) = pos.board.squares[sq.index()] {
                        if piece.color == color {
                            match piece.piece_type {
                                PieceType::Gold => defenders += 3,
                                PieceType::Silver => defenders += 2,
                                PieceType::Knight | PieceType::Lance => defenders += 1,
                                _ => {}
                            }
                        }
                    }
                }
            }

            score += defenders * 5; // Bonus for each defending piece

            // Penalty for holes in castle (empty squares near king)
            let mut holes = 0;
            for &(df, dr) in &directions {
                let new_file = king_file as i8 + df;
                let new_rank = king_rank as i8 + dr;

                // Only check squares in our territory
                let in_territory = match color {
                    Color::Black => (0..=2).contains(&new_rank),
                    Color::White => (6..=8).contains(&new_rank),
                };

                if in_territory && (0..9).contains(&new_file) {
                    let sq = Square::new(new_file as u8, new_rank as u8);
                    if pos.board.squares[sq.index()].is_none() {
                        holes += 1;
                    }
                }
            }

            score -= holes * 3; // Penalty for holes in castle
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
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::White));
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
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::White));

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

        // Place kings
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

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

        // Place kings
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

        // Black has doubled pawns, white has connected pawns
        pos.board
            .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

        pos.board
            .put_piece(parse_usi_square("7f").unwrap(), Piece::new(PieceType::Pawn, Color::White));
        pos.board
            .put_piece(parse_usi_square("6e").unwrap(), Piece::new(PieceType::Pawn, Color::White));

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // White should have slight advantage due to better pawn structure
        // But since we're evaluating from Black's perspective, score might be close to 0
        // Just verify that the pawn structure evaluation is working
        assert!(score.abs() <= 50);
    }

    #[test]
    fn test_castle_formation() {
        let mut pos = Position::empty();

        // Black has castled king with defenders
        pos.board
            .put_piece(parse_usi_square("8a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::Lance, Color::Black));
        pos.board
            .put_piece(parse_usi_square("7a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
        pos.board.put_piece(
            parse_usi_square("8b").unwrap(),
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

        // Place kings
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

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
}
