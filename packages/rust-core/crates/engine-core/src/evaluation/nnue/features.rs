//! HalfKP feature extraction for NNUE
//!
//! HalfKP uses the king position and all other pieces as features

use super::CLASSIC_ACC_DIM;
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

/// us 視点の HalfKP インデックスを them 視点へ写像する
///
/// - 王位置は盤面反転 (`Square::flip()`)
/// - 盤上駒は色を反転し座標を反転
/// - 手駒は色のみ反転（個数は不変）
#[inline]
pub fn flip_us_them(feature_idx: usize) -> usize {
    debug_assert!(feature_idx < SHOGI_BOARD_SIZE * FE_END);

    let king_idx = feature_idx / FE_END;
    let piece_idx = feature_idx % FE_END;

    let king_sq = Square(king_idx as u8);
    let king_sq_flipped = king_sq.flip();

    let flipped_piece_idx = flip_bona_piece_index(piece_idx);

    king_sq_flipped.index() * FE_END + flipped_piece_idx
}

const BOARD_FEATURE_GROUPS: usize = 13;
const HAND_BASE_INDEX: usize = 2106;
const HAND_ENTRIES_PER_COLOR: usize = 38;
const HAND_OFFSETS: [usize; 7] = [0, 2, 4, 8, 12, 16, 20];
const HAND_LENGTHS: [usize; 7] = [2, 2, 4, 4, 4, 4, 18];

#[inline]
fn flip_bona_piece_index(idx: usize) -> usize {
    if idx < HAND_BASE_INDEX {
        flip_board_piece_index(idx)
    } else {
        flip_hand_piece_index(idx)
    }
}

#[inline]
fn flip_board_piece_index(idx: usize) -> usize {
    let per_color = BOARD_FEATURE_GROUPS * SHOGI_BOARD_SIZE;
    debug_assert!(idx < per_color * 2);

    let (color_offset, piece_within_color) = if idx >= per_color {
        (per_color, idx - per_color)
    } else {
        (0, idx)
    };

    let piece_group = piece_within_color / SHOGI_BOARD_SIZE;
    let sq_idx = piece_within_color % SHOGI_BOARD_SIZE;
    let flipped_sq_idx = Square(sq_idx as u8).flip().index();

    let new_color_offset = if color_offset == 0 { per_color } else { 0 };
    piece_group * SHOGI_BOARD_SIZE + flipped_sq_idx + new_color_offset
}

#[inline]
fn flip_hand_piece_index(idx: usize) -> usize {
    debug_assert!(idx < HAND_BASE_INDEX + HAND_ENTRIES_PER_COLOR * 2);
    let offset = idx - HAND_BASE_INDEX;
    let (color_offset, within_color) = if offset >= HAND_ENTRIES_PER_COLOR {
        (HAND_ENTRIES_PER_COLOR, offset - HAND_ENTRIES_PER_COLOR)
    } else {
        (0, offset)
    };

    let (piece_offset, _piece_len) = HAND_OFFSETS
        .iter()
        .zip(HAND_LENGTHS.iter())
        .find(|(start, len)| within_color >= **start && within_color < **start + **len)
        .map(|(start, len)| (*start, *len))
        .unwrap_or_else(|| unreachable!("within_color out of range: {within_color}"));
    let count_offset = within_color - piece_offset;
    let new_color_offset = if color_offset == 0 {
        HAND_ENTRIES_PER_COLOR
    } else {
        0
    };
    HAND_BASE_INDEX + piece_offset + count_offset + new_color_offset
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
    pub const DEFAULT_DIM: usize = CLASSIC_ACC_DIM;

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
                // Respect promotion state recorded on the board
                let is_promoted = pos.board.promoted_bb.test(sq);
                let mut piece = Piece::new(pt, color);
                if is_promoted && pt.can_promote() {
                    piece = piece.promote();
                }

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

#[inline]
fn orient_board_piece(perspective: Color, piece: Piece, square: Square) -> (Piece, Square) {
    if perspective == Color::Black {
        (piece, square)
    } else {
        (piece.flip_color(), square.flip())
    }
}

#[inline]
fn orient_hand_color(perspective: Color, owner: Color) -> Color {
    if perspective == Color::Black {
        owner
    } else {
        owner.flip()
    }
}

/// 盤上駒を単一視点で HalfKP インデックスへ写像する。
/// `king_sq` には視点ごとに整合した王座標（白視点は flip 済み）を渡す。
#[inline]
pub fn oriented_board_feature_index(
    perspective: Color,
    king_sq: Square,
    piece: Piece,
    square: Square,
) -> Option<usize> {
    let (piece_adj, square_adj) = orient_board_piece(perspective, piece, square);
    BonaPiece::from_board(piece_adj, square_adj).map(|bp| halfkp_index(king_sq, bp))
}

/// 手駒を単一視点で HalfKP インデックスへ写像する。
/// `owner` は実際の手駒所有者。視点に応じた色変換は内部で行う。
#[inline]
pub fn oriented_hand_feature_index(
    perspective: Color,
    king_sq: Square,
    piece_type: PieceType,
    owner: Color,
    count: u8,
) -> Result<usize, &'static str> {
    let color_adj = orient_hand_color(perspective, owner);
    let bona = BonaPiece::from_hand(piece_type, color_adj, count)?;
    Ok(halfkp_index(king_sq, bona))
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

    #[test]
    fn test_flip_us_them_involution() {
        let sample_pieces = [
            0,
            SHOGI_BOARD_SIZE - 1,
            HAND_BASE_INDEX - 1,
            HAND_BASE_INDEX,
            HAND_BASE_INDEX + HAND_ENTRIES_PER_COLOR - 1,
            FE_END - 1,
        ];
        let sample_kings = [0, 10, 40, 80];

        for &k in &sample_kings {
            for &p in &sample_pieces {
                let idx = k * FE_END + p;
                assert_eq!(flip_us_them(flip_us_them(idx)), idx);
            }
        }
    }
}
