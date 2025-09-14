//! HalfKP feature extraction for NNUE
//!
//! HalfKP uses the king position and all other pieces as features

use crate::{
    shogi::{
        piece_type_to_hand_index, BOARD_PIECE_TYPES, HAND_PIECE_TYPES, MAX_HAND_PIECES,
        SHOGI_BOARD_SIZE,
    },
    Color, Piece, PieceType, Position, Square,
};

#[cfg(debug_assertions)]
use log::{error, warn};

// Use global MAX_HAND_PIECES from piece_constants

/// Piece representation for feature indexing
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BonaPiece(u16);

impl BonaPiece {
    /// Create BonaPiece from board piece
    pub fn from_board(piece: Piece, sq: Square) -> Option<Self> {
        let s = SHOGI_BOARD_SIZE as u16;
        let piece_offset = match (piece.piece_type, piece.is_promoted()) {
            (PieceType::Pawn, false) => 0,
            (PieceType::Lance, false) => s,
            (PieceType::Knight, false) => 2 * s,
            (PieceType::Silver, false) => 3 * s,
            (PieceType::Gold, false) => 4 * s,
            (PieceType::Bishop, false) => 5 * s,
            (PieceType::Rook, false) => 6 * s,
            (PieceType::Pawn, true) => 7 * s,    // Tokin
            (PieceType::Lance, true) => 8 * s,   // Promoted Lance
            (PieceType::Knight, true) => 9 * s,  // Promoted Knight
            (PieceType::Silver, true) => 10 * s, // Promoted Silver
            (PieceType::Bishop, true) => 11 * s, // Horse
            (PieceType::Rook, true) => 12 * s,   // Dragon
            (PieceType::King, _) => {
                #[cfg(debug_assertions)]
                warn!("[NNUE] Attempted to create BonaPiece for King");
                return None;
            }
            (PieceType::Gold, true) => {
                #[cfg(debug_assertions)]
                warn!("[NNUE] Attempted to create BonaPiece for promoted Gold");
                return None;
            }
        };

        // 13 piece groups (excluding kings as features) per color
        let color_offset = if piece.color == Color::White {
            (13 * SHOGI_BOARD_SIZE) as u16
        } else {
            0
        };
        let index = piece_offset + sq.index() as u16 + color_offset;

        Some(BonaPiece(index))
    }

    /// Create BonaPiece from hand piece
    /// Returns an error if piece_type is King (which cannot be in hand)
    pub fn from_hand(piece_type: PieceType, color: Color, count: u8) -> Result<Self, &'static str> {
        debug_assert!(count > 0);

        // Use type-safe function to get hand array index
        let hand_idx = piece_type_to_hand_index(piece_type)?;
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
            PieceType::King => return Err("King cannot be in hand"),
        };

        let color_offset = if color == Color::White { 38 } else { 0 };
        let index = base + piece_offset + (count - 1) as u16 + color_offset;

        Ok(BonaPiece(index))
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
    debug_assert!(index < SHOGI_BOARD_SIZE * FE_END);
    index
}

/// Feature transformer for HalfKP -> acc_dim-dimensional features
pub struct FeatureTransformer {
    /// Weights for feature transformation \[INPUT_DIM\]\[acc_dim\]
    pub weights: Vec<i16>,
    /// Biases for output features \[acc_dim\]
    pub biases: Vec<i32>,
    /// Output feature dimension (formerly fixed to 256)
    pub(crate) acc_dim: usize,
}

impl FeatureTransformer {
    /// Default accumulator/output dimension
    pub const DEFAULT_DIM: usize = 256;

    /// Create zero-initialized feature transformer with default dimension (256)
    pub fn zero() -> Self {
        Self::zero_with_dim(Self::DEFAULT_DIM)
    }

    /// Create zero-initialized feature transformer with given dimension
    pub fn zero_with_dim(acc_dim: usize) -> Self {
        FeatureTransformer {
            // king squares * features * acc_dim outputs
            weights: vec![0; SHOGI_BOARD_SIZE * FE_END * acc_dim],
            biases: vec![0; acc_dim],
            acc_dim,
        }
    }

    /// Get output feature dimension (acc_dim)
    #[inline]
    pub fn acc_dim(&self) -> usize {
        self.acc_dim
    }

    /// Get weight for specific feature and output index
    pub fn weight(&self, feature_idx: usize, output_idx: usize) -> i16 {
        debug_assert!(feature_idx < SHOGI_BOARD_SIZE * FE_END); // HalfKP index includes king position
        debug_assert!(output_idx < self.acc_dim);
        self.weights[feature_idx * self.acc_dim + output_idx]
    }

    /// Get mutable weight reference
    pub fn weight_mut(&mut self, feature_idx: usize, output_idx: usize) -> &mut i16 {
        debug_assert!(feature_idx < SHOGI_BOARD_SIZE * FE_END);
        debug_assert!(output_idx < self.acc_dim);
        &mut self.weights[feature_idx * self.acc_dim + output_idx]
    }
}

/// Maximum number of active features
/// Increased from 64 to 128 to handle complex positions with many pieces and promoted pieces
/// Theoretical maximum: 38 board pieces (40 - 2 kings) + 76 hand pieces = 114
/// We use 128 for safety margin and cache alignment
pub const MAX_ACTIVE_FEATURES: usize = 128;

/// Structure to hold active features without heap allocation
pub struct ActiveFeatures {
    features: [usize; MAX_ACTIVE_FEATURES],
    count: usize,
}

impl Default for ActiveFeatures {
    fn default() -> Self {
        Self::new()
    }
}

impl ActiveFeatures {
    /// Create new empty feature set
    pub fn new() -> Self {
        ActiveFeatures {
            features: [0; MAX_ACTIVE_FEATURES],
            count: 0,
        }
    }

    /// Add a feature (with overflow check)
    #[inline]
    fn push(&mut self, feature: usize) {
        if self.count >= MAX_ACTIVE_FEATURES {
            #[cfg(debug_assertions)]
            warn!("[NNUE] Active features overflow, count={}, skipping feature", self.count);
            return;
        }
        self.features[self.count] = feature;
        self.count += 1;
    }

    /// Get active features as slice
    pub fn as_slice(&self) -> &[usize] {
        &self.features[..self.count]
    }

    /// Get number of active features
    pub fn len(&self) -> usize {
        self.count
    }

    /// Check if there are no active features
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// Extract active features from position
pub fn extract_features(pos: &Position, king_sq: Square, perspective: Color) -> ActiveFeatures {
    let mut features = ActiveFeatures::new();

    // Board pieces
    for &color in &[Color::Black, Color::White] {
        // Iterate through all non-King piece types using compile-time constant array
        for &pt in &BOARD_PIECE_TYPES {
            let mut bb = pos.board.piece_bb[color as usize][pt as usize];

            while let Some(sq) = bb.pop_lsb() {
                let piece = Piece::new(pt, color);

                // Adjust for perspective
                let (piece_adj, sq_adj) = if perspective == Color::Black {
                    (piece, sq)
                } else {
                    (piece.flip_color(), sq.flip())
                };

                if let Some(bona_piece) = BonaPiece::from_board(piece_adj, sq_adj) {
                    let index = halfkp_index(king_sq, bona_piece);
                    features.push(index);
                }
            }
        }
    }

    // Hand pieces
    for &color in &[Color::Black, Color::White] {
        // Use compile-time constant array for type-safe iteration
        for (hand_idx, &pt) in HAND_PIECE_TYPES.iter().enumerate() {
            let count = pos.hands[color as usize][hand_idx];
            if count > 0 {
                // Adjust color for perspective
                let color_adj = if perspective == Color::Black {
                    color
                } else {
                    color.flip()
                };

                match BonaPiece::from_hand(pt, color_adj, count) {
                    Ok(bona_piece) => {
                        let index = halfkp_index(king_sq, bona_piece);
                        features.push(index);
                    }
                    Err(_e) => {
                        #[cfg(debug_assertions)]
                        error!("[NNUE] Error creating BonaPiece from hand: {_e}");
                    }
                }
            }
        }
    }

    features
}

#[cfg(test)]
mod tests {
    use crate::usi::parse_usi_square;

    use super::*;

    #[test]
    fn test_bona_piece_from_board() {
        let piece = Piece::new(PieceType::Pawn, Color::Black);
        let sq = parse_usi_square("5e").unwrap(); // 5e
        let bona = BonaPiece::from_board(piece, sq).expect("Valid piece type");

        assert_eq!(bona.index(), 40); // Pawn at index 40
    }

    #[test]
    fn test_bona_piece_from_hand() {
        let bona =
            BonaPiece::from_hand(PieceType::Pawn, Color::Black, 1).expect("Valid piece type");
        // Base 2106 + pawn offset 20 + (count-1) 0 + color offset 0 = 2126
        assert_eq!(bona.index(), 2126); // First black pawn in hand

        let bona =
            BonaPiece::from_hand(PieceType::Pawn, Color::Black, 17).expect("Valid piece type");
        assert_eq!(bona.index(), 2126 + 16); // 17th black pawn (max is 18 but array is 0-17)
    }

    #[test]
    fn test_halfkp_index() {
        let king_sq = parse_usi_square("5i").unwrap(); // 5i
        let piece = BonaPiece(100);
        let index = halfkp_index(king_sq, piece);

        assert_eq!(index, 76 * FE_END + 100);
    }

    #[test]
    fn test_extract_features() {
        let pos = Position::startpos();
        let king_sq = parse_usi_square("5i").unwrap(); // Black king position
        let features = extract_features(&pos, king_sq, Color::Black);

        // Starting position has 40 pieces (including kings)
        // Minus 2 kings = 38 features
        assert_eq!(features.len(), 38);
    }
}
