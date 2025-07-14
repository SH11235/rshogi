//! HalfKP feature extraction for NNUE
//!
//! HalfKP uses the king position and all other pieces as features

use crate::ai::board::{Color, Piece, PieceType, Position, Square};

/// Maximum pieces in hand for each type (indexed as in hands array)
const MAX_HAND_PIECES: [u8; 7] = [
    2,  // Rook (index 0)
    2,  // Bishop (index 1)
    4,  // Gold (index 2)
    4,  // Silver (index 3)
    4,  // Knight (index 4)
    4,  // Lance (index 5)
    18, // Pawn (index 6)
];

/// Piece representation for feature indexing
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BonaPiece(u16);

impl BonaPiece {
    /// Create BonaPiece from board piece
    pub fn from_board(piece: Piece, sq: Square) -> Self {
        let piece_offset = match (piece.piece_type, piece.is_promoted()) {
            (PieceType::Pawn, false) => 0,
            (PieceType::Lance, false) => 81,
            (PieceType::Knight, false) => 162,
            (PieceType::Silver, false) => 243,
            (PieceType::Gold, false) => 324,
            (PieceType::Bishop, false) => 405,
            (PieceType::Rook, false) => 486,
            (PieceType::Pawn, true) => 567,   // Tokin
            (PieceType::Lance, true) => 648,  // Promoted Lance
            (PieceType::Knight, true) => 729, // Promoted Knight
            (PieceType::Silver, true) => 810, // Promoted Silver
            (PieceType::Bishop, true) => 891, // Horse
            (PieceType::Rook, true) => 972,   // Dragon
            (PieceType::King, _) => unreachable!("King should not be included in features"),
            (PieceType::Gold, true) => unreachable!("Gold cannot be promoted"),
        };

        let color_offset = if piece.color == Color::White { 1053 } else { 0 };
        let index = piece_offset + sq.index() as u16 + color_offset;

        BonaPiece(index)
    }

    /// Create BonaPiece from hand piece
    pub fn from_hand(piece_type: PieceType, color: Color, count: u8) -> Self {
        debug_assert!(count > 0);

        // Map piece type to hand array index
        let hand_idx = match piece_type {
            PieceType::Rook => 0,
            PieceType::Bishop => 1,
            PieceType::Gold => 2,
            PieceType::Silver => 3,
            PieceType::Knight => 4,
            PieceType::Lance => 5,
            PieceType::Pawn => 6,
            PieceType::King => unreachable!("King cannot be in hand"),
        };
        debug_assert!(count <= MAX_HAND_PIECES[hand_idx]);

        let base = 2106; // After board pieces

        let piece_offset = match piece_type {
            PieceType::Rook => 0,
            PieceType::Bishop => 2,
            PieceType::Gold => 4,
            PieceType::Silver => 8,
            PieceType::Knight => 12,
            PieceType::Lance => 16,
            PieceType::Pawn => 20,
            PieceType::King => unreachable!("King cannot be in hand"),
        };

        let color_offset = if color == Color::White { 38 } else { 0 };
        let index = base + piece_offset + (count - 1) as u16 + color_offset;

        BonaPiece(index)
    }

    /// Get feature index
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Total number of features (board pieces + hand pieces)
pub const FE_END: usize = 2106 + 76; // 2106 board + 76 hand

/// Calculate HalfKP feature index
pub fn halfkp_index(king_sq: Square, piece: BonaPiece) -> usize {
    let index = king_sq.index() * FE_END + piece.index();
    debug_assert!(index < 81 * FE_END);
    index
}

/// Feature transformer for HalfKP -> 256-dimensional features
pub struct FeatureTransformer {
    /// Weights for feature transformation [INPUT_DIM][256]
    pub weights: Vec<i16>,
    /// Biases for output features [256]
    pub biases: Vec<i32>,
}

impl FeatureTransformer {
    /// Create zero-initialized feature transformer
    pub fn zero() -> Self {
        FeatureTransformer {
            weights: vec![0; 81 * FE_END * 256], // 81 king squares * features * 256 outputs
            biases: vec![0; 256],
        }
    }

    /// Get weight for specific feature and output index
    pub fn weight(&self, feature_idx: usize, output_idx: usize) -> i16 {
        debug_assert!(feature_idx < 81 * FE_END); // HalfKP index includes king position
        debug_assert!(output_idx < 256);
        self.weights[feature_idx * 256 + output_idx]
    }

    /// Get mutable weight reference
    pub fn weight_mut(&mut self, feature_idx: usize, output_idx: usize) -> &mut i16 {
        debug_assert!(feature_idx < 81 * FE_END);
        debug_assert!(output_idx < 256);
        &mut self.weights[feature_idx * 256 + output_idx]
    }
}

/// Extract active features from position
pub fn extract_features(pos: &Position, king_sq: Square, perspective: Color) -> Vec<usize> {
    let mut features = Vec::with_capacity(32);

    // Board pieces
    for &color in &[Color::Black, Color::White] {
        for piece_type in 0..8 {
            if piece_type == PieceType::King as usize {
                continue;
            }

            let pt = PieceType::try_from(piece_type as u8)
                .expect("Invalid piece type in feature extraction");
            let mut bb = pos.board.piece_bb[color as usize][piece_type];

            while let Some(sq) = bb.pop_lsb() {
                let piece = Piece::new(pt, color);

                // Adjust for perspective
                let (piece_adj, sq_adj) = if perspective == Color::Black {
                    (piece, sq)
                } else {
                    (piece.flip_color(), sq.flip())
                };

                let bona_piece = BonaPiece::from_board(piece_adj, sq_adj);
                let index = halfkp_index(king_sq, bona_piece);
                features.push(index);
            }
        }
    }

    // Hand pieces
    for &color in &[Color::Black, Color::White] {
        for piece_type in 0..7 {
            let count = pos.hands[color as usize][piece_type];
            if count > 0 {
                let pt = PieceType::try_from(piece_type as u8)
                    .expect("Invalid piece type in feature extraction");

                // Adjust color for perspective
                let color_adj = if perspective == Color::Black {
                    color
                } else {
                    color.flip()
                };

                let bona_piece = BonaPiece::from_hand(pt, color_adj, count);
                let index = halfkp_index(king_sq, bona_piece);
                features.push(index);
            }
        }
    }

    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bona_piece_from_board() {
        let piece = Piece::new(PieceType::Pawn, Color::Black);
        let sq = Square::new(4, 4); // 5e
        let bona = BonaPiece::from_board(piece, sq);

        assert_eq!(bona.index(), 40); // Pawn at index 40
    }

    #[test]
    fn test_bona_piece_from_hand() {
        let bona = BonaPiece::from_hand(PieceType::Pawn, Color::Black, 1);
        // Base 2106 + pawn offset 20 + (count-1) 0 + color offset 0 = 2126
        assert_eq!(bona.index(), 2126); // First black pawn in hand

        let bona = BonaPiece::from_hand(PieceType::Pawn, Color::Black, 17);
        assert_eq!(bona.index(), 2126 + 16); // 17th black pawn (max is 18 but array is 0-17)
    }

    #[test]
    fn test_halfkp_index() {
        let king_sq = Square::new(4, 8); // 5i
        let piece = BonaPiece(100);
        let index = halfkp_index(king_sq, piece);

        assert_eq!(index, 76 * FE_END + 100);
    }

    #[test]
    fn test_extract_features() {
        let pos = Position::startpos();
        let king_sq = Square::new(4, 8); // Black king position
        let features = extract_features(&pos, king_sq, Color::Black);

        // Starting position has 40 pieces (including kings)
        // Minus 2 kings = 38 features
        assert_eq!(features.len(), 38);
    }
}
