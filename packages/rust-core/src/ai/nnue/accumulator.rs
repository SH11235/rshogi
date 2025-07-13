//! Accumulator for incremental feature updates
//!
//! Manages transformed features for both perspectives with differential updates

use super::features::{extract_features, halfkp_index, BonaPiece, FeatureTransformer, FE_END};
use crate::ai::board::{Color, Piece, PieceType, Position, Square};
use crate::ai::moves::Move;

/// Accumulator for storing transformed features
#[derive(Clone)]
pub struct Accumulator {
    /// Black perspective features [256]
    pub black: Vec<i16>,
    /// White perspective features [256]
    pub white: Vec<i16>,
    /// Whether black features are computed
    pub computed_black: bool,
    /// Whether white features are computed
    pub computed_white: bool,
}

impl Default for Accumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Accumulator {
    /// Create new empty accumulator
    pub fn new() -> Self {
        Accumulator {
            black: vec![0; 256],
            white: vec![0; 256],
            computed_black: false,
            computed_white: false,
        }
    }

    /// Refresh accumulator from position (full calculation)
    pub fn refresh(&mut self, pos: &Position, transformer: &FeatureTransformer) {
        self.computed_black = false;
        self.computed_white = false;

        // Black perspective
        if let Some(king_sq) = pos.king_square(Color::Black) {
            self.refresh_side(pos, king_sq, Color::Black, transformer);
            self.computed_black = true;
        }

        // White perspective (flipped)
        if let Some(king_sq) = pos.king_square(Color::White) {
            let king_sq_flipped = king_sq.flip();
            self.refresh_side(pos, king_sq_flipped, Color::White, transformer);
            self.computed_white = true;
        }
    }

    /// Refresh one side's features
    fn refresh_side(
        &mut self,
        pos: &Position,
        king_sq: Square,
        perspective: Color,
        transformer: &FeatureTransformer,
    ) {
        let accumulator = if perspective == Color::Black {
            &mut self.black
        } else {
            &mut self.white
        };

        // Initialize with biases
        for (i, acc) in accumulator.iter_mut().enumerate().take(256) {
            *acc = transformer.biases[i] as i16;
        }

        // Get active features
        let features = extract_features(pos, king_sq, perspective);

        // Apply features
        Self::apply_features(accumulator, &features, transformer);
    }

    /// Apply feature weights to accumulator
    fn apply_features(
        accumulator: &mut [i16],
        features: &[usize],
        transformer: &FeatureTransformer,
    ) {
        for &feature_idx in features {
            debug_assert!(feature_idx < FE_END * 81);

            for (i, acc) in accumulator.iter_mut().enumerate().take(256) {
                *acc += transformer.weight(feature_idx, i);
            }
        }
    }

    /// Update accumulator with differential changes
    pub fn update(
        &mut self,
        update: &AccumulatorUpdate,
        perspective: Color,
        transformer: &FeatureTransformer,
    ) {
        let accumulator = if perspective == Color::Black {
            &mut self.black
        } else {
            &mut self.white
        };

        // Remove features
        for &feature_idx in &update.removed {
            for (i, acc) in accumulator.iter_mut().enumerate().take(256) {
                *acc -= transformer.weight(feature_idx, i);
            }
        }

        // Add features
        for &feature_idx in &update.added {
            for (i, acc) in accumulator.iter_mut().enumerate().take(256) {
                *acc += transformer.weight(feature_idx, i);
            }
        }
    }
}

/// Update information for differential calculation
#[derive(Debug)]
pub struct AccumulatorUpdate {
    /// Features to remove
    pub removed: Vec<usize>,
    /// Features to add
    pub added: Vec<usize>,
}

/// Calculate differential update from move
pub fn calculate_update(pos: &Position, mv: Move) -> AccumulatorUpdate {
    let mut removed = Vec::new();
    let mut added = Vec::new();

    // Get king positions
    let black_king = pos.king_square(Color::Black).unwrap();
    let white_king = pos.king_square(Color::White).unwrap();
    let white_king_flipped = white_king.flip();

    if mv.is_drop() {
        // Drop move
        let to = mv.to();
        let piece_type = mv.drop_piece_type();
        let piece = Piece::new(piece_type, pos.side_to_move);

        // Add new piece for both perspectives
        // Black perspective
        let bona_black = BonaPiece::from_board(piece, to);
        added.push(halfkp_index(black_king, bona_black));

        // White perspective
        let piece_white = piece.flip_color();
        let to_white = to.flip();
        let bona_white = BonaPiece::from_board(piece_white, to_white);
        added.push(halfkp_index(white_king_flipped, bona_white));

        // Remove from hand
        let color = pos.side_to_move;
        let hand_idx = match piece_type {
            PieceType::Rook => 0,
            PieceType::Bishop => 1,
            PieceType::Gold => 2,
            PieceType::Silver => 3,
            PieceType::Knight => 4,
            PieceType::Lance => 5,
            PieceType::Pawn => 6,
            _ => unreachable!(),
        };
        let count = pos.hands[color as usize][hand_idx];

        // Remove old hand count
        let bona_hand_black = BonaPiece::from_hand(piece_type, color, count);
        removed.push(halfkp_index(black_king, bona_hand_black));

        let color_white = color.flip();
        let bona_hand_white = BonaPiece::from_hand(piece_type, color_white, count);
        removed.push(halfkp_index(white_king_flipped, bona_hand_white));

        // Add new hand count (if not zero)
        if count > 1 {
            let bona_hand_black_new = BonaPiece::from_hand(piece_type, color, count - 1);
            added.push(halfkp_index(black_king, bona_hand_black_new));

            let bona_hand_white_new = BonaPiece::from_hand(piece_type, color_white, count - 1);
            added.push(halfkp_index(white_king_flipped, bona_hand_white_new));
        }
    } else {
        // Normal move
        let from = mv.from().unwrap(); // Safe because not a drop
        let to = mv.to();

        // Get moving piece
        let moving_piece = pos.piece_at(from).unwrap();

        // Remove piece from source
        let bona_from_black = BonaPiece::from_board(moving_piece, from);
        removed.push(halfkp_index(black_king, bona_from_black));

        let moving_piece_white = moving_piece.flip_color();
        let from_white = from.flip();
        let bona_from_white = BonaPiece::from_board(moving_piece_white, from_white);
        removed.push(halfkp_index(white_king_flipped, bona_from_white));

        // Add piece to destination (possibly promoted)
        let dest_piece = if mv.is_promote() {
            moving_piece.promote()
        } else {
            moving_piece
        };

        let bona_to_black = BonaPiece::from_board(dest_piece, to);
        added.push(halfkp_index(black_king, bona_to_black));

        let dest_piece_white = dest_piece.flip_color();
        let to_white = to.flip();
        let bona_to_white = BonaPiece::from_board(dest_piece_white, to_white);
        added.push(halfkp_index(white_king_flipped, bona_to_white));

        // Handle capture
        if let Some(captured) = pos.piece_at(to) {
            // Remove captured piece
            let bona_cap_black = BonaPiece::from_board(captured, to);
            removed.push(halfkp_index(black_king, bona_cap_black));

            let captured_white = captured.flip_color();
            let bona_cap_white = BonaPiece::from_board(captured_white, to_white);
            removed.push(halfkp_index(white_king_flipped, bona_cap_white));

            // Add to hand
            let hand_type = captured.piece_type; // Already unpromoted by board logic

            let hand_idx = match hand_type {
                PieceType::Rook => 0,
                PieceType::Bishop => 1,
                PieceType::Gold => 2,
                PieceType::Silver => 3,
                PieceType::Knight => 4,
                PieceType::Lance => 5,
                PieceType::Pawn => 6,
                _ => unreachable!(),
            };

            let hand_color = pos.side_to_move;
            let new_count = pos.hands[hand_color as usize][hand_idx] + 1;

            let bona_hand_black = BonaPiece::from_hand(hand_type, hand_color, new_count);
            added.push(halfkp_index(black_king, bona_hand_black));

            let hand_color_white = hand_color.flip();
            let bona_hand_white = BonaPiece::from_hand(hand_type, hand_color_white, new_count);
            added.push(halfkp_index(white_king_flipped, bona_hand_white));

            // Remove old hand count if it existed
            if new_count > 1 {
                let bona_hand_old_black =
                    BonaPiece::from_hand(hand_type, hand_color, new_count - 1);
                removed.push(halfkp_index(black_king, bona_hand_old_black));

                let bona_hand_old_white =
                    BonaPiece::from_hand(hand_type, hand_color_white, new_count - 1);
                removed.push(halfkp_index(white_king_flipped, bona_hand_old_white));
            }
        }

        // Special case: king move requires full refresh
        if moving_piece.piece_type == PieceType::King {
            // This is handled by caller - just mark for full refresh
            // For now, we'll handle partial updates only
        }
    }

    AccumulatorUpdate { removed, added }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accumulator_new() {
        let acc = Accumulator::new();
        assert!(!acc.computed_black);
        assert!(!acc.computed_white);
        assert_eq!(acc.black.len(), 256);
        assert_eq!(acc.white.len(), 256);
    }

    #[test]
    fn test_accumulator_refresh() {
        let pos = Position::startpos();
        let transformer = FeatureTransformer::zero();
        let mut acc = Accumulator::new();

        acc.refresh(&pos, &transformer);

        assert!(acc.computed_black);
        assert!(acc.computed_white);

        // With zero transformer, should have zero values
        for i in 0..256 {
            assert_eq!(acc.black[i], 0);
            assert_eq!(acc.white[i], 0);
        }
    }

    #[test]
    fn test_calculate_update_normal_move() {
        let pos = Position::startpos();
        let mv = Move::make_normal(Square::new(6, 6), Square::new(6, 5)); // 7g7f

        let update = calculate_update(&pos, mv);

        // Should remove pawn from 7g and add to 7f
        assert_eq!(update.removed.len(), 2); // Black and white perspectives
        assert_eq!(update.added.len(), 2);
    }

    #[test]
    fn test_calculate_update_drop() {
        let mut pos = Position::startpos();
        pos.hands[Color::Black as usize][6] = 1; // Pawn is index 6

        let mv = Move::make_drop(PieceType::Pawn, Square::new(4, 4)); // P*5e

        let update = calculate_update(&pos, mv);

        // Should add pawn to board and update hand count
        assert!(update.added.len() >= 2); // New piece on board
        assert!(update.removed.len() >= 2); // Hand count changed
    }
}
