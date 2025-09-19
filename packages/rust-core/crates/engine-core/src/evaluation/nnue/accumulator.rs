//! Accumulator for incremental feature updates
//!
//! Manages transformed features for both perspectives with differential updates

use super::error::{NNUEError, NNUEResult};
use super::features::{
    extract_features, oriented_board_feature_index, oriented_hand_feature_index, FeatureTransformer,
};
use super::simd::SimdDispatcher;
use super::CLASSIC_ACC_DIM;
use crate::shogi::{piece_type_to_hand_index, Move};
use crate::{Color, Piece, PieceType, Position, Square};
use smallvec::SmallVec;

/// Accumulator for storing transformed features
#[derive(Clone)]
pub struct Accumulator {
    /// Black perspective features \[acc_dim\]
    pub black: Vec<i16>,
    /// White perspective features \[acc_dim\]
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
        Self::new_with_dim(CLASSIC_ACC_DIM)
    }

    /// Create new empty accumulator with specified dimension
    pub fn new_with_dim(dim: usize) -> Self {
        Accumulator {
            black: vec![0; dim],
            white: vec![0; dim],
            computed_black: false,
            computed_white: false,
        }
    }

    #[inline]
    fn ensure_dim(&mut self, dim: usize) {
        if self.black.len() != dim {
            self.black.resize(dim, 0);
        }
        if self.white.len() != dim {
            self.white.resize(dim, 0);
        }
    }

    /// Refresh accumulator from position (full calculation)
    pub fn refresh(&mut self, pos: &Position, transformer: &FeatureTransformer) {
        self.computed_black = false;
        self.computed_white = false;

        // Ensure accumulator arrays match transformer's dimension
        self.ensure_dim(transformer.acc_dim());

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

        // Initialize with biases（acc_dim 可変）
        let dim = transformer.acc_dim();
        accumulator
            .iter_mut()
            .zip(transformer.biases.iter())
            .take(dim)
            .for_each(|(dst, &b)| {
                let clamped = b.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                *dst = clamped;
            });

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
        SimdDispatcher::update_accumulator(
            accumulator,
            &transformer.weights,
            features,
            true,
            transformer.acc_dim(),
        );
    }

    /// Update accumulator with differential changes
    #[inline]
    pub fn update(
        &mut self,
        delta: &AccumulatorDelta,
        perspective: Color,
        transformer: &FeatureTransformer,
    ) {
        let (accumulator, removed_src, added_src) = match perspective {
            Color::Black => (&mut self.black, &delta.removed_b, &delta.added_b),
            Color::White => (&mut self.white, &delta.removed_w, &delta.added_w),
        };

        debug_assert_eq!(
            accumulator.len(),
            transformer.acc_dim(),
            "accumulator length and transformer acc_dim must match"
        );

        // Bridge u32 indices -> usize (SmallVec; typically no heap allocation)
        if !removed_src.is_empty() {
            let removed_us = bridge_u32_to_usize(removed_src);
            SimdDispatcher::update_accumulator(
                accumulator,
                &transformer.weights,
                &removed_us,
                false,
                transformer.acc_dim(),
            );
        }
        if !added_src.is_empty() {
            let added_us = bridge_u32_to_usize(added_src);
            SimdDispatcher::update_accumulator(
                accumulator,
                &transformer.weights,
                &added_us,
                true,
                transformer.acc_dim(),
            );
        }
    }
}

#[inline]
fn bridge_u32_to_usize(xs: &[u32]) -> SmallVec<[usize; 12]> {
    let mut out: SmallVec<[usize; 12]> = SmallVec::with_capacity(xs.len());
    for &i in xs {
        let iu = i as usize;
        debug_assert_eq!(iu as u32, i);
        out.push(iu);
    }
    out
}

/// 視点別（黒/白）に分割した差分集合
#[derive(Debug)]
pub struct AccumulatorDelta {
    /// Black-perspective features to remove
    pub removed_b: SmallVec<[u32; 12]>,
    /// Black-perspective features to add
    pub added_b: SmallVec<[u32; 12]>,
    /// White-perspective features to remove
    pub removed_w: SmallVec<[u32; 12]>,
    /// White-perspective features to add
    pub added_w: SmallVec<[u32; 12]>,
}

impl Default for AccumulatorDelta {
    fn default() -> Self {
        Self {
            removed_b: SmallVec::new(),
            added_b: SmallVec::new(),
            removed_w: SmallVec::new(),
            added_w: SmallVec::new(),
        }
    }
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
        if let Some(idx) = oriented_board_feature_index(Color::Black, black_king, piece, to) {
            debug_assert!(idx <= u32::MAX as usize);
            out.added_b.push(idx as u32);
        }

        // White perspective
        if let Some(idx) = oriented_board_feature_index(Color::White, white_king_flipped, piece, to)
        {
            debug_assert!(idx <= u32::MAX as usize);
            out.added_w.push(idx as u32);
        }

        // Remove from hand
        let color = pos.side_to_move;
        let hand_idx =
            piece_type_to_hand_index(piece_type).expect("Drop piece type must be valid hand piece");
        let count = pos.hands[color as usize][hand_idx];

        // Remove old hand count
        match oriented_hand_feature_index(Color::Black, black_king, piece_type, color, count) {
            Ok(idx) => {
                debug_assert!(idx <= u32::MAX as usize);
                out.removed_b.push(idx as u32)
            }
            Err(_e) => {
                #[cfg(debug_assertions)]
                log::error!("[NNUE] Error creating BonaPiece from hand: {_e}");
            }
        }

        match oriented_hand_feature_index(
            Color::White,
            white_king_flipped,
            piece_type,
            color,
            count,
        ) {
            Ok(idx) => {
                debug_assert!(idx <= u32::MAX as usize);
                out.removed_w.push(idx as u32)
            }
            Err(_e) => {
                #[cfg(debug_assertions)]
                log::error!("[NNUE] Error creating BonaPiece from hand: {_e}");
            }
        }

        // Add new hand count (if not zero)
        if count > 1 {
            match oriented_hand_feature_index(
                Color::Black,
                black_king,
                piece_type,
                color,
                count - 1,
            ) {
                Ok(idx) => {
                    debug_assert!(idx <= u32::MAX as usize);
                    out.added_b.push(idx as u32)
                }
                Err(_e) => {
                    #[cfg(debug_assertions)]
                    log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                }
            }

            match oriented_hand_feature_index(
                Color::White,
                white_king_flipped,
                piece_type,
                color,
                count - 1,
            ) {
                Ok(idx) => {
                    debug_assert!(idx <= u32::MAX as usize);
                    out.added_w.push(idx as u32)
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
        if let Some(idx) =
            oriented_board_feature_index(Color::Black, black_king, moving_piece, from)
        {
            debug_assert!(idx <= u32::MAX as usize);
            out.removed_b.push(idx as u32);
        }

        if let Some(idx) =
            oriented_board_feature_index(Color::White, white_king_flipped, moving_piece, from)
        {
            debug_assert!(idx <= u32::MAX as usize);
            out.removed_w.push(idx as u32);
        }

        // Add piece to destination (possibly promoted)
        let dest_piece = if mv.is_promote() {
            moving_piece.promote()
        } else {
            moving_piece
        };

        if let Some(idx) = oriented_board_feature_index(Color::Black, black_king, dest_piece, to) {
            debug_assert!(idx <= u32::MAX as usize);
            out.added_b.push(idx as u32);
        }

        if let Some(idx) =
            oriented_board_feature_index(Color::White, white_king_flipped, dest_piece, to)
        {
            debug_assert!(idx <= u32::MAX as usize);
            out.added_w.push(idx as u32);
        }

        // Handle capture
        if let Some(captured) = pos.piece_at(to) {
            // Remove captured piece
            if let Some(idx) = oriented_board_feature_index(Color::Black, black_king, captured, to)
            {
                debug_assert!(idx <= u32::MAX as usize);
                out.removed_b.push(idx as u32);
            }

            if let Some(idx) =
                oriented_board_feature_index(Color::White, white_king_flipped, captured, to)
            {
                debug_assert!(idx <= u32::MAX as usize);
                out.removed_w.push(idx as u32);
            }

            // Add to hand
            // `Piece` は基底種 (`piece_type`) と成りフラグを分離保持するため、ここでは基底種で扱える。
            let hand_type = captured.piece_type;

            let hand_idx = piece_type_to_hand_index(hand_type)
                .expect("Captured piece type must be valid hand piece");

            let hand_color = pos.side_to_move;
            let new_count = pos.hands[hand_color as usize][hand_idx] + 1;

            match oriented_hand_feature_index(
                Color::Black,
                black_king,
                hand_type,
                hand_color,
                new_count,
            ) {
                Ok(idx) => {
                    debug_assert!(idx <= u32::MAX as usize);
                    out.added_b.push(idx as u32)
                }
                Err(_e) => {
                    #[cfg(debug_assertions)]
                    log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                }
            }

            match oriented_hand_feature_index(
                Color::White,
                white_king_flipped,
                hand_type,
                hand_color,
                new_count,
            ) {
                Ok(idx) => {
                    debug_assert!(idx <= u32::MAX as usize);
                    out.added_w.push(idx as u32)
                }
                Err(_e) => {
                    #[cfg(debug_assertions)]
                    log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                }
            }

            // Remove old hand count if it existed
            if new_count > 1 {
                match oriented_hand_feature_index(
                    Color::Black,
                    black_king,
                    hand_type,
                    hand_color,
                    new_count - 1,
                ) {
                    Ok(idx) => {
                        debug_assert!(idx <= u32::MAX as usize);
                        out.removed_b.push(idx as u32)
                    }
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                    }
                }

                match oriented_hand_feature_index(
                    Color::White,
                    white_king_flipped,
                    hand_type,
                    hand_color,
                    new_count - 1,
                ) {
                    Ok(idx) => {
                        debug_assert!(idx <= u32::MAX as usize);
                        out.removed_w.push(idx as u32)
                    }
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        log::warn!("[NNUE] Error creating BonaPiece from hand: {}", _e);
                    }
                }
            }
        }
    }

    // Telemetry: record total delta length（最大値を観測）
    #[cfg(feature = "nnue_telemetry")]
    {
        use std::sync::atomic::{AtomicUsize, Ordering::*};
        static MAX_DELTA_LEN: AtomicUsize = AtomicUsize::new(0);
        let cur = out.removed_b.len() + out.added_b.len() + out.removed_w.len() + out.added_w.len();
        let old = MAX_DELTA_LEN.load(Relaxed);
        if cur > old {
            let _ = MAX_DELTA_LEN.compare_exchange(old, cur, Relaxed, Relaxed);
        }
    }

    Ok(UpdateOp::Delta)
}

#[cfg(test)]
mod tests {
    use crate::usi::parse_usi_square;

    use super::*;

    #[test]
    fn test_classic_random_chain_matches_refresh() {
        use crate::movegen::MoveGenerator;
        use rand::{RngCore, SeedableRng};

        let mut pos = Position::startpos();
        let gen = MoveGenerator::new();
        let mut rng = rand_xoshiro::Xoshiro128Plus::seed_from_u64(0xC1A55E);

        // Transformer starts with zeros; we will fill rows lazily for encountered features
        let mut transformer = FeatureTransformer::zero();

        // helper to ensure row is non-zero to catch mistakes
        fn fill_row(transformer: &mut FeatureTransformer, feat: usize, val: i16) {
            let dim = transformer.acc_dim();
            for o in 0..dim {
                *transformer.weight_mut(feat, o) = val;
            }
        }

        let mut acc = Accumulator::new();

        let mut delta = AccumulatorDelta::default();
        // apply ~20 moves
        for step in 0..20 {
            let legal = gen.generate_all(&pos).unwrap_or_default();
            if legal.is_empty() {
                break;
            }
            let mv = legal[(rng.next_u32() as usize) % legal.len()];
            if calculate_update_into(&mut delta, &pos, mv).unwrap() == UpdateOp::Delta {
                // lazily fill rows for any unseen features (distinct values per perspective)
                // Update Black perspective using only black deltas
                for &f in delta.removed_b.iter() {
                    fill_row(&mut transformer, f as usize, 1);
                }
                for &f in delta.added_b.iter() {
                    fill_row(&mut transformer, f as usize, 1);
                }
                // Refresh pre-accumulator after assigning rows so removed features exist in acc
                acc.refresh(&pos, &transformer);
                acc.update(&delta, Color::Black, &transformer);

                // Note: This test focuses on Black perspective equivalence only to
                // avoid cross-perspective row collisions when using synthetic rows.
            } else {
                // Full refresh path
                let _ = pos.do_move(mv);
                acc.refresh(&pos, &transformer);
                // continue with next step
                continue;
            }

            // advance position and compare against full refresh
            let _u = pos.do_move(mv);
            let mut full = Accumulator::new();
            full.refresh(&pos, &transformer);
            assert_eq!(acc.black, full.black, "black acc mismatch at step {}", step);
        }
    }

    #[test]
    fn test_accumulator_new() {
        let acc = Accumulator::new();
        assert!(!acc.computed_black);
        assert!(!acc.computed_white);
        assert_eq!(acc.black.len(), CLASSIC_ACC_DIM);
        assert_eq!(acc.white.len(), CLASSIC_ACC_DIM);
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
        for i in 0..CLASSIC_ACC_DIM {
            assert_eq!(acc.black[i], 0);
            assert_eq!(acc.white[i], 0);
        }
    }

    #[test]
    fn test_calculate_update_normal_move() {
        let pos = Position::startpos();
        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap()); // 3g3f
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
        let mut transformer = FeatureTransformer::zero();

        let mut delta = AccumulatorDelta {
            removed_b: SmallVec::new(),
            added_b: SmallVec::new(),
            removed_w: SmallVec::new(),
            added_w: SmallVec::new(),
        };
        let op = calculate_update_into(&mut delta, &pos, mv).expect("delta or refresh");
        assert_eq!(op, UpdateOp::Delta);

        // Helper: fill one feature row with a constant
        fn fill_row(transformer: &mut FeatureTransformer, feat: usize, val: i16) {
            let dim = transformer.acc_dim();
            for o in 0..dim {
                *transformer.weight_mut(feat, o) = val;
            }
        }

        // Assign distinct values for black perspective only for this test
        // Use different constants so net delta is non-zero
        for &f in delta.removed_b.iter() {
            fill_row(&mut transformer, f as usize, 2);
        }
        for &f in delta.added_b.iter() {
            fill_row(&mut transformer, f as usize, 3);
        }
        for &f in delta.removed_w.iter() {
            fill_row(&mut transformer, f as usize, 4);
        }
        for &f in delta.added_w.iter() {
            fill_row(&mut transformer, f as usize, 5);
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
        let mut transformer = FeatureTransformer::zero();
        fn fill_row(transformer: &mut FeatureTransformer, feat: usize, val: i16) {
            let dim = transformer.acc_dim();
            for o in 0..dim {
                *transformer.weight_mut(feat, o) = val;
            }
        }
        for &f in delta.removed_b.iter() {
            fill_row(&mut transformer, f as usize, 2);
        }
        for &f in delta.added_b.iter() {
            fill_row(&mut transformer, f as usize, 3);
        }

        // Acc at pre position (refresh after assigning black rows)
        let mut acc_pre = Accumulator::new();
        acc_pre.refresh(&pos, &transformer);

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

    #[test]
    fn test_classic_capture_promoted_piece_hand_unpromoted() {
        use crate::usi::parse_usi_square;

        // Kings
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        // Black pawn at 3g, White promoted silver (成銀) at 3f
        pos.board
            .put_piece(parse_usi_square("3g").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
        let ws = Piece::new(PieceType::Silver, Color::White).promote();
        pos.board.put_piece(parse_usi_square("3f").unwrap(), ws);
        pos.side_to_move = Color::Black;

        // Build transformer with zero weights; we will fill rows lazily for delta indices
        let mut transformer = FeatureTransformer::zero();
        fn fill_row(transformer: &mut FeatureTransformer, feat: usize, val: i16) {
            let dim = transformer.acc_dim();
            for o in 0..dim {
                *transformer.weight_mut(feat, o) = val;
            }
        }

        let mv =
            Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap());
        let mut delta = AccumulatorDelta::default();
        let op = calculate_update_into(&mut delta, &pos, mv).expect("delta or refresh");
        assert_eq!(op, UpdateOp::Delta);

        // Lazily assign non-zero rows per perspective, then apply
        for &f in delta.removed_b.iter() {
            fill_row(&mut transformer, f as usize, 1);
        }
        for &f in delta.added_b.iter() {
            fill_row(&mut transformer, f as usize, 1);
        }
        for &f in delta.removed_w.iter() {
            fill_row(&mut transformer, f as usize, 2);
        }
        for &f in delta.added_w.iter() {
            fill_row(&mut transformer, f as usize, 2);
        }
        // Apply delta then compare to full refresh after making the move
        let mut acc_pre = Accumulator::new();
        // Refresh after assigning black rows so removed features exist in acc
        acc_pre.refresh(&pos, &transformer);
        let mut acc_inc = acc_pre.clone();
        acc_inc.update(&delta, Color::Black, &transformer);
        acc_inc.update(&delta, Color::White, &transformer);

        let _u = pos.do_move(mv);
        let mut acc_full = Accumulator::new();
        acc_full.refresh(&pos, &transformer);
        assert_eq!(acc_inc.black, acc_full.black);
        assert_eq!(acc_inc.white, acc_full.white);
    }

    #[test]
    fn test_bias_clamp_to_i16() {
        let mut ft = FeatureTransformer::zero_with_dim(4);
        // 32767 を超える値を入れてみる
        ft.biases = vec![i32::MAX, 40000, -40000, 123];

        let pos = Position::startpos();
        let mut acc = Accumulator::new_with_dim(4);
        acc.refresh(&pos, &ft);

        assert_eq!(acc.black[0], i16::MAX);
        assert_eq!(acc.black[1], i16::MAX);
        assert_eq!(acc.black[2], i16::MIN);
        assert_eq!(acc.black[3], 123);
        // white も同じ
        assert_eq!(acc.white[0], i16::MAX);
        assert_eq!(acc.white[1], i16::MAX);
        assert_eq!(acc.white[2], i16::MIN);
        assert_eq!(acc.white[3], 123);
    }
}
