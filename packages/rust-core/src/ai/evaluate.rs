//! Evaluation function for shogi
//!
//! Simple material-based evaluation

use super::board::{Color, PieceType, Position};

/// Trait for position evaluation
pub trait Evaluator {
    /// Evaluate position from side to move perspective
    fn evaluate(&self, pos: &Position) -> i32;
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
    for piece_type in 0..8 {
        let pt = match piece_type {
            0 => PieceType::King,
            1 => PieceType::Rook,
            2 => PieceType::Bishop,
            3 => PieceType::Gold,
            4 => PieceType::Silver,
            5 => PieceType::Knight,
            6 => PieceType::Lance,
            7 => PieceType::Pawn,
            _ => unreachable!(),
        };

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

    // King safety bonus - prefer king on back rank
    if let Some(king_sq) = pos.board.king_square(us) {
        let king_rank = king_sq.rank();
        match us {
            Color::Black => {
                // Black king prefers rank 0-2
                if king_rank <= 2 {
                    score += 50;
                }
            }
            Color::White => {
                // White king prefers rank 6-8
                if king_rank >= 6 {
                    score += 50;
                }
            }
        }
    }

    // TODO: Add more positional factors
    // - Piece activity
    // - Pawn structure
    // - Castle formation

    score
}

/// Simple material evaluator implementing Evaluator trait
pub struct MaterialEvaluator;

impl Evaluator for MaterialEvaluator {
    fn evaluate(&self, pos: &Position) -> i32 {
        evaluate(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::board::{Piece, Square};

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
        // Place kings in neutral positions to avoid positional bonus
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 5), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::Rook, Color::Black));
        pos.board
            .put_piece(Square::new(8, 8), Piece::new(PieceType::Bishop, Color::White));

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Black (to move) should be ahead by 200 (rook=1000 - bishop=800)
        assert_eq!(score, 200);
    }

    #[test]
    fn test_evaluate_promoted() {
        let mut pos = Position::empty();

        // Both have promoted pawns
        // Place kings in neutral positions to avoid positional bonus
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 5), Piece::new(PieceType::King, Color::White));

        let mut tokin_black = Piece::new(PieceType::Pawn, Color::Black);
        tokin_black.promoted = true;
        pos.board.put_piece(Square::new(0, 0), tokin_black);

        let mut tokin_white = Piece::new(PieceType::Pawn, Color::White);
        tokin_white.promoted = true;
        pos.board.put_piece(Square::new(8, 8), tokin_white);

        let evaluator = MaterialEvaluator;
        let score = evaluator.evaluate(&pos);

        // Should be equal (both have tokin worth 100+300=400)
        assert_eq!(score, 0);
    }
}
