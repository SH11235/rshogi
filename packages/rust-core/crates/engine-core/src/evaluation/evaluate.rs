//! Evaluation function for shogi
//!
//! Simple material-based evaluation

use crate::shogi::piece_constants::{APERY_PIECE_VALUES, APERY_PROMOTED_PIECE_VALUES};
use crate::{
    shogi::{ALL_PIECE_TYPES, NUM_HAND_PIECE_TYPES},
    PieceType, Position,
};

/// Trait for position evaluation
///
/// Contract:
/// - Returns a score in centipawns from the side-to-move perspective.
/// - Positive values favor the side to move; negative values favor the opponent.
/// - Flipping `side_to_move` on the same board position should approximately flip the sign
///   (exact symmetry is not guaranteed if the evaluator incorporates tempo or king safety asymmetries).
///
/// Implementations should keep this polarity contract to ensure search components
/// (e.g., repetition penalties, pruning rules) behave consistently.
pub trait Evaluator {
    /// Evaluate position from side to move perspective
    fn evaluate(&self, pos: &Position) -> i32;

    /// Notify evaluator that search is starting at this position (root reset)
    /// Default: no-op
    fn on_set_position(&self, _pos: &Position) {}

    /// Notify evaluator before making a real move (called with pre-move position)
    /// Default: no-op
    fn on_do_move(&self, _pre_pos: &Position, _mv: crate::shogi::Move) {}

    /// Notify evaluator after undoing the last real move
    /// Default: no-op
    fn on_undo_move(&self) {}

    /// Notify evaluator before doing a null move (side-to-move flip)
    /// Default: no-op
    fn on_do_null_move(&self, _pre_pos: &Position) {}

    /// Notify evaluator after undoing the last null move
    /// Default: no-op
    fn on_undo_null_move(&self) {}
}

/// Implement Evaluator for Arc<T> where T: Evaluator
impl<T: Evaluator + ?Sized> Evaluator for std::sync::Arc<T> {
    fn evaluate(&self, pos: &Position) -> i32 {
        (**self).evaluate(pos)
    }
    fn on_set_position(&self, pos: &Position) {
        (**self).on_set_position(pos)
    }
    fn on_do_move(&self, pre_pos: &Position, mv: crate::shogi::Move) {
        (**self).on_do_move(pre_pos, mv)
    }
    fn on_undo_move(&self) {
        (**self).on_undo_move()
    }
    fn on_do_null_move(&self, pre_pos: &Position) {
        (**self).on_do_null_move(pre_pos)
    }
    fn on_undo_null_move(&self) {
        (**self).on_undo_null_move()
    }
}

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

        let base_value = APERY_PIECE_VALUES[piece_type];
        score += base_value * (our_count - their_count);

        let promoted_delta = APERY_PROMOTED_PIECE_VALUES[piece_type] - base_value;
        if promoted_delta != 0 {
            let our_promoted = our_pieces & pos.board.promoted_bb;
            let their_promoted = their_pieces & pos.board.promoted_bb;

            let our_promoted_count = our_promoted.count_ones() as i32;
            let their_promoted_count = their_promoted.count_ones() as i32;

            score += promoted_delta * (our_promoted_count - their_promoted_count);
        }
    }

    // Material in hand
    for piece_idx in 0..NUM_HAND_PIECE_TYPES {
        let our_hand = pos.hands[us as usize][piece_idx] as i32;
        let their_hand = pos.hands[them as usize][piece_idx] as i32;

        let piece_type = PieceType::from_hand_index(piece_idx).expect("invalid hand index");
        let value = APERY_PIECE_VALUES[piece_type as usize];

        score += value * (our_hand - their_hand);
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
    use crate::{usi::parse_usi_square, Color, Piece};

    use super::*;

    fn place_kings(pos: &mut Position) {
        pos.board
            .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    }

    #[test]
    fn test_evaluate_startpos_is_zero() {
        let pos = Position::startpos();
        let evaluator = MaterialEvaluator;
        assert_eq!(evaluator.evaluate(&pos), 0);
    }

    #[test]
    fn test_evaluate_material_apery_values() {
        let mut pos = Position::empty();
        place_kings(&mut pos);
        pos.board
            .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Rook, Color::Black));
        pos.board.put_piece(
            parse_usi_square("8h").unwrap(),
            Piece::new(PieceType::Bishop, Color::White),
        );

        let score = MaterialEvaluator.evaluate(&pos);
        // 990 (R) - 855 (B) = 135
        assert_eq!(score, 135);
    }

    #[test]
    fn test_promoted_piece_values_match_gold() {
        let mut pos = Position::empty();
        place_kings(&mut pos);

        let mut tokin_black = Piece::new(PieceType::Pawn, Color::Black);
        tokin_black.promoted = true;
        pos.board.put_piece(parse_usi_square("5e").unwrap(), tokin_black);

        let mut tokin_white = Piece::new(PieceType::Pawn, Color::White);
        tokin_white.promoted = true;
        pos.board.put_piece(parse_usi_square("5f").unwrap(), tokin_white);

        let score = MaterialEvaluator.evaluate(&pos);
        assert_eq!(score, 0, "Tokin vs Tokin should cancel out");
    }

    #[test]
    fn test_hand_material_consistency() {
        let mut pos = Position::empty();
        place_kings(&mut pos);
        pos.side_to_move = Color::Black;

        // Black has 1 rook in hand, White has 2 pawns in hand
        pos.hands[Color::Black as usize][0] = 1; // Rook
        pos.hands[Color::White as usize][6] = 2; // Pawns

        let score = MaterialEvaluator.evaluate(&pos);
        assert_eq!(score, 990 - 2 * 90);
    }
}
