//! Accumulator for incremental feature updates
//!
//! Manages transformed features for both perspectives with differential updates

use super::error::{NNUEError, NNUEResult};
use super::features::{extract_features, halfkp_index, BonaPiece, FeatureTransformer};
use super::simd::SimdDispatcher;
use crate::shogi::{piece_type_to_hand_index, Move};
use crate::{Color, Piece, PieceType, Position, Square};
use smallvec::SmallVec;

#[cfg(debug_assertions)]
use log::error;

/// Accumulator for storing transformed features
#[derive(Clone)]
pub struct Accumulator {
    /// Black perspective features \[256\]
    pub black: Vec<i16>,
    /// White perspective features \[256\]
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
        Self::apply_features(accumulator, features.as_slice(), transformer);
    }

    /// Apply feature weights to accumulator
    fn apply_features(
        accumulator: &mut [i16],
        features: &[usize],
        transformer: &FeatureTransformer,
    ) {
        SimdDispatcher::update_accumulator(accumulator, &transformer.weights, features, true);
    }

    /// Update accumulator with differential changes
    pub fn update(
        &mut self,
        delta: &AccumulatorDelta,
        perspective: Color,
        transformer: &FeatureTransformer,
    ) {
        let (accumulator, removed, added) = match perspective {
            Color::Black => (&mut self.black, &delta.removed_b, &delta.added_b),
            Color::White => (&mut self.white, &delta.removed_w, &delta.added_w),
        };

        if !removed.is_empty() {
            SimdDispatcher::update_accumulator(accumulator, &transformer.weights, removed, false);
        }
        if !added.is_empty() {
            SimdDispatcher::update_accumulator(accumulator, &transformer.weights, added, true);
        }
    }
}

/// 視点別（黒/白）に分割した差分集合
#[derive(Debug)]
pub struct AccumulatorDelta {
    /// Black-perspective features to remove
    pub removed_b: SmallVec<[usize; 12]>,
    /// Black-perspective features to add
    pub added_b: SmallVec<[usize; 12]>,
    /// White-perspective features to remove
    pub removed_w: SmallVec<[usize; 12]>,
    /// White-perspective features to add
    pub added_w: SmallVec<[usize; 12]>,
}

/// 差分の有無（王移動など安全側でのフル再構築を含む）
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum UpdateOp {
    Delta,
    FullRefresh,
}

impl AccumulatorDelta {
    #[inline]
    pub fn clear(&mut self) {
        self.removed_b.clear();
        self.added_b.clear();
        self.removed_w.clear();
        self.added_w.clear();
    }
}

/// Calculate differential update from move into given buffer (no heap, reusable)
#[inline]
pub fn calculate_update_into(
    out: &mut AccumulatorDelta,
    pos: &Position,
    mv: Move,
) -> NNUEResult<UpdateOp> {
    out.clear();

    // Get king positions
    let black_king = pos.king_square(Color::Black).ok_or(NNUEError::KingNotFound(Color::Black))?;
    let white_king = pos.king_square(Color::White).ok_or(NNUEError::KingNotFound(Color::White))?;
    let white_king_flipped = white_king.flip();

    if mv.is_drop() {
        // Drop move
        let to = mv.to();
        let piece_type = mv.drop_piece_type();
        let piece = Piece::new(piece_type, pos.side_to_move);

        // Add new piece for both perspectives
        // Black perspective
        if let Some(bona_black) = BonaPiece::from_board(piece, to) {
            out.added_b.push(halfkp_index(black_king, bona_black));
        }

        // White perspective
        let piece_white = piece.flip_color();
        let to_white = to.flip();
        if let Some(bona_white) = BonaPiece::from_board(piece_white, to_white) {
            out.added_w.push(halfkp_index(white_king_flipped, bona_white));
        }

        // Remove from hand
        let color = pos.side_to_move;
        let hand_idx =
            piece_type_to_hand_index(piece_type).expect("Drop piece type must be valid hand piece");
        let count = pos.hands[color as usize][hand_idx];

        // Remove old hand count
        match BonaPiece::from_hand(piece_type, color, count) {
            Ok(bona_hand_black) => out.removed_b.push(halfkp_index(black_king, bona_hand_black)),
            Err(_e) => {
                #[cfg(debug_assertions)]
                error!("[NNUE] Error creating BonaPiece from hand: {_e}");
            }
        }

        let color_white = color.flip();
        match BonaPiece::from_hand(piece_type, color_white, count) {
            Ok(bona_hand_white) => {
                out.removed_w.push(halfkp_index(white_king_flipped, bona_hand_white))
            }
            Err(_e) => {
                #[cfg(debug_assertions)]
                error!("[NNUE] Error creating BonaPiece from hand: {_e}");
            }
        }

        // Add new hand count (if not zero)
        if count > 1 {
            match BonaPiece::from_hand(piece_type, color, count - 1) {
                Ok(bona_hand_black_new) => {
                    out.added_b.push(halfkp_index(black_king, bona_hand_black_new))
                }
                Err(_e) => {
                    #[cfg(debug_assertions)]
                    log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                }
            }

            match BonaPiece::from_hand(piece_type, color_white, count - 1) {
                Ok(bona_hand_white_new) => {
                    out.added_w.push(halfkp_index(white_king_flipped, bona_hand_white_new))
                }
                Err(_e) => {
                    #[cfg(debug_assertions)]
                    log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                }
            }
        }
    } else {
        // Normal move
        let from = mv.from().ok_or_else(|| {
            NNUEError::InvalidMove("Non-drop move without source square".to_string())
        })?;
        let to = mv.to();

        // Get moving piece
        let moving_piece = pos.piece_at(from).ok_or(NNUEError::InvalidPiece(from))?;

        // King move → FullRefresh（安全側）
        if moving_piece.piece_type == PieceType::King {
            return Ok(UpdateOp::FullRefresh);
        }

        // Remove piece from source
        if let Some(bona_from_black) = BonaPiece::from_board(moving_piece, from) {
            out.removed_b.push(halfkp_index(black_king, bona_from_black));
        }

        let moving_piece_white = moving_piece.flip_color();
        let from_white = from.flip();
        if let Some(bona_from_white) = BonaPiece::from_board(moving_piece_white, from_white) {
            out.removed_w.push(halfkp_index(white_king_flipped, bona_from_white));
        }

        // Add piece to destination (possibly promoted)
        let dest_piece = if mv.is_promote() {
            moving_piece.promote()
        } else {
            moving_piece
        };

        if let Some(bona_to_black) = BonaPiece::from_board(dest_piece, to) {
            out.added_b.push(halfkp_index(black_king, bona_to_black));
        }

        let dest_piece_white = dest_piece.flip_color();
        let to_white = to.flip();
        if let Some(bona_to_white) = BonaPiece::from_board(dest_piece_white, to_white) {
            out.added_w.push(halfkp_index(white_king_flipped, bona_to_white));
        }

        // Handle capture
        if let Some(captured) = pos.piece_at(to) {
            // Remove captured piece
            if let Some(bona_cap_black) = BonaPiece::from_board(captured, to) {
                out.removed_b.push(halfkp_index(black_king, bona_cap_black));
            }

            let captured_white = captured.flip_color();
            if let Some(bona_cap_white) = BonaPiece::from_board(captured_white, to_white) {
                out.removed_w.push(halfkp_index(white_king_flipped, bona_cap_white));
            }

            // Add to hand
            let hand_type = captured.piece_type; // Already unpromoted by board logic

            let hand_idx = piece_type_to_hand_index(hand_type)
                .expect("Captured piece type must be valid hand piece");

            let hand_color = pos.side_to_move;
            let new_count = pos.hands[hand_color as usize][hand_idx] + 1;

            match BonaPiece::from_hand(hand_type, hand_color, new_count) {
                Ok(bona_hand_black) => out.added_b.push(halfkp_index(black_king, bona_hand_black)),
                Err(_e) => {
                    #[cfg(debug_assertions)]
                    log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                }
            }

            let hand_color_white = hand_color.flip();
            match BonaPiece::from_hand(hand_type, hand_color_white, new_count) {
                Ok(bona_hand_white) => {
                    out.added_w.push(halfkp_index(white_king_flipped, bona_hand_white))
                }
                Err(_e) => {
                    #[cfg(debug_assertions)]
                    log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                }
            }

            // Remove old hand count if it existed
            if new_count > 1 {
                match BonaPiece::from_hand(hand_type, hand_color, new_count - 1) {
                    Ok(bona_hand_old_black) => {
                        out.removed_b.push(halfkp_index(black_king, bona_hand_old_black))
                    }
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                    }
                }

                match BonaPiece::from_hand(hand_type, hand_color_white, new_count - 1) {
                    Ok(bona_hand_old_white) => {
                        out.removed_w.push(halfkp_index(white_king_flipped, bona_hand_old_white))
                    }
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                    }
                }
            }
        }
    }

    Ok(UpdateOp::Delta)
}

#[cfg(test)]
mod tests {
    use crate::usi::parse_usi_square;

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
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap()); // 7g7f
        let mut d = AccumulatorDelta {
            removed_b: SmallVec::new(),
            added_b: SmallVec::new(),
            removed_w: SmallVec::new(),
            added_w: SmallVec::new(),
        };
        let op = calculate_update_into(&mut d, &pos, mv).unwrap();
        assert_eq!(op, UpdateOp::Delta);
        assert_eq!(d.removed_b.len(), 1);
        assert_eq!(d.removed_w.len(), 1);
        assert_eq!(d.added_b.len(), 1);
        assert_eq!(d.added_w.len(), 1);
    }

    #[test]
    fn test_calculate_update_drop() {
        let mut pos = Position::startpos();
        pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // Pawn is index 6

        let mv = Move::make_drop(PieceType::Pawn, parse_usi_square("5e").unwrap()); // P*5e
        let mut d = AccumulatorDelta {
            removed_b: SmallVec::new(),
            added_b: SmallVec::new(),
            removed_w: SmallVec::new(),
            added_w: SmallVec::new(),
        };
        let op = calculate_update_into(&mut d, &pos, mv).unwrap();
        assert_eq!(op, UpdateOp::Delta);
        assert!(d.added_b.len() + d.added_w.len() >= 2);
        assert!(d.removed_b.len() + d.removed_w.len() >= 2);
    }

    #[test]
    fn test_calculate_update_no_king() {
        let mut pos = Position::empty();
        // Add some pieces but no kings
        pos.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

        let mv =
            Move::make_normal(parse_usi_square("5e").unwrap(), parse_usi_square("5d").unwrap());

        let mut d = AccumulatorDelta {
            removed_b: SmallVec::new(),
            added_b: SmallVec::new(),
            removed_w: SmallVec::new(),
            added_w: SmallVec::new(),
        };
        let result = calculate_update_into(&mut d, &pos, mv);
        assert!(result.is_err());
        match result {
            Err(NNUEError::KingNotFound(_)) => (),
            _ => panic!("Expected KingNotFound error"),
        }
    }

    #[test]
    fn test_calculate_update_king_move_fullrefresh() {
        // Empty position with only kings so we can move king safely for the test
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        // Black king move 5i -> 5h
        let mv =
            Move::make_normal(parse_usi_square("5i").unwrap(), parse_usi_square("5h").unwrap());
        let mut d = AccumulatorDelta {
            removed_b: SmallVec::new(),
            added_b: SmallVec::new(),
            removed_w: SmallVec::new(),
            added_w: SmallVec::new(),
        };
        let op = calculate_update_into(&mut d, &pos, mv).expect("update available");
        assert_eq!(op, UpdateOp::FullRefresh);
    }

    #[test]
    fn test_classic_update_one_side_no_cross_contamination() {
        // Start position with both kings; use a simple pawn move 3g->3f
        let pos = Position::startpos();
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());

        // Build transformer with zeros then assign non-zero rows only for delta indices
        let mut transformer = FeatureTransformer {
            weights: vec![
                0;
                crate::shogi::SHOGI_BOARD_SIZE
                    * crate::evaluation::nnue::features::FE_END
                    * 256
            ],
            biases: vec![0; 256],
        };

        let mut delta = AccumulatorDelta {
            removed_b: SmallVec::new(),
            added_b: SmallVec::new(),
            removed_w: SmallVec::new(),
            added_w: SmallVec::new(),
        };
        let op = calculate_update_into(&mut delta, &pos, mv).expect("delta or refresh");
        assert_eq!(op, UpdateOp::Delta);

        // Helper: fill one feature row with a constant
        let mut fill_row = |feat: usize, val: i16| {
            for o in 0..256 {
                *transformer.weight_mut(feat, o) = val;
            }
        };

        // Assign distinct values so remove/add don't cancel out
        for &f in delta.removed_b.iter() {
            fill_row(f, 2);
        }
        for &f in delta.added_b.iter() {
            fill_row(f, 3);
        }
        for &f in delta.removed_w.iter() {
            fill_row(f, 4);
        }
        for &f in delta.added_w.iter() {
            fill_row(f, 5);
        }

        let mut acc0 = Accumulator::new();
        acc0.refresh(&pos, &transformer);

        // Apply only Black perspective delta
        let mut acc_b_only = acc0.clone();
        acc_b_only.update(&delta, Color::Black, &transformer);

        // White side must be unchanged
        assert_eq!(acc_b_only.white, acc0.white, "white acc should remain unchanged");
        // Black side should change (distinct row values ensure net delta != 0)
        assert_ne!(acc_b_only.black, acc0.black, "black acc should change after black-only update");
    }

    #[test]
    fn test_classic_delta_matches_full_refresh_both_sides() {
        // Start position and a regular pawn move 3g->3f
        let mut pos = Position::startpos();
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());

        // Prepare transformer with rows for delta indices
        let mut delta = AccumulatorDelta {
            removed_b: SmallVec::new(),
            added_b: SmallVec::new(),
            removed_w: SmallVec::new(),
            added_w: SmallVec::new(),
        };
        let op = calculate_update_into(&mut delta, &pos, mv).expect("delta or refresh");
        assert_eq!(op, UpdateOp::Delta);
        let mut transformer = FeatureTransformer {
            weights: vec![
                0;
                crate::shogi::SHOGI_BOARD_SIZE
                    * crate::evaluation::nnue::features::FE_END
                    * 256
            ],
            biases: vec![0; 256],
        };
        let mut fill_row = |feat: usize, val: i16| {
            for o in 0..256 {
                *transformer.weight_mut(feat, o) = val;
            }
        };
        for &f in delta.removed_b.iter() {
            fill_row(f, 2);
        }
        for &f in delta.added_b.iter() {
            fill_row(f, 3);
        }
        for &f in delta.removed_w.iter() {
            fill_row(f, 4);
        }
        for &f in delta.added_w.iter() {
            fill_row(f, 5);
        }

        // Acc at pre position
        let mut acc_pre = Accumulator::new();
        acc_pre.refresh(&pos, &transformer);

        // Apply delta on both perspectives
        let mut acc_inc = acc_pre.clone();
        acc_inc.update(&delta, Color::Black, &transformer);
        acc_inc.update(&delta, Color::White, &transformer);

        // Move position and full refresh
        let _u = pos.do_move(mv);
        let mut acc_full = Accumulator::new();
        acc_full.refresh(&pos, &transformer);

        assert_eq!(acc_inc.black, acc_full.black, "black acc must match full refresh");
        assert_eq!(acc_inc.white, acc_full.white, "white acc must match full refresh");
    }
}
