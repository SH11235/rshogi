//! Type-safe piece constants and conversion utilities
//!
//! This module provides compile-time constants for piece types and type-safe
//! conversion functions, eliminating the need for unsafe match statements.

use super::board::PieceType;

/// All piece types in the game
pub const ALL_PIECE_TYPES: [PieceType; 8] = [
    PieceType::King,
    PieceType::Rook,
    PieceType::Bishop,
    PieceType::Gold,
    PieceType::Silver,
    PieceType::Knight,
    PieceType::Lance,
    PieceType::Pawn,
];

/// All piece types that can be on the board (excluding King)
/// This is useful for feature extraction and move generation
pub const BOARD_PIECE_TYPES: [PieceType; 7] = [
    PieceType::Rook,
    PieceType::Bishop,
    PieceType::Gold,
    PieceType::Silver,
    PieceType::Knight,
    PieceType::Lance,
    PieceType::Pawn,
];

/// All piece types that can be in hand
/// Hand pieces use a specific ordering for indexing
pub const HAND_PIECE_TYPES: [PieceType; 7] = [
    PieceType::Rook,   // index 0
    PieceType::Bishop, // index 1
    PieceType::Gold,   // index 2
    PieceType::Silver, // index 3
    PieceType::Knight, // index 4
    PieceType::Lance,  // index 5
    PieceType::Pawn,   // index 6
];

/// Maximum number of each piece type that can be in hand
/// Indexed by hand array index (same order as HAND_PIECE_TYPES)
pub const MAX_HAND_PIECES: [u8; 7] = [
    2,  // Rook: max 2
    2,  // Bishop: max 2
    4,  // Gold: max 4
    4,  // Silver: max 4
    4,  // Knight: max 4
    4,  // Lance: max 4
    18, // Pawn: max 18
];

/// Convert PieceType to its standard index (0-7)
#[inline]
pub const fn piece_type_to_index(pt: PieceType) -> usize {
    pt as usize
}

/// Convert index to PieceType (returns None for invalid indices)
#[inline]
pub const fn index_to_piece_type(index: usize) -> Option<PieceType> {
    match index {
        0 => Some(PieceType::King),
        1 => Some(PieceType::Rook),
        2 => Some(PieceType::Bishop),
        3 => Some(PieceType::Gold),
        4 => Some(PieceType::Silver),
        5 => Some(PieceType::Knight),
        6 => Some(PieceType::Lance),
        7 => Some(PieceType::Pawn),
        _ => None,
    }
}

/// Get hand array index for a piece type
/// Returns an error if King is passed, as King cannot be in hand
#[inline]
pub fn piece_type_to_hand_index(pt: PieceType) -> Result<usize, &'static str> {
    match pt {
        PieceType::Rook => Ok(0),
        PieceType::Bishop => Ok(1),
        PieceType::Gold => Ok(2),
        PieceType::Silver => Ok(3),
        PieceType::Knight => Ok(4),
        PieceType::Lance => Ok(5),
        PieceType::Pawn => Ok(6),
        PieceType::King => Err("King cannot be in hand"),
    }
}

/// Convert hand array index to piece type
#[inline]
pub const fn hand_index_to_piece_type(index: usize) -> Option<PieceType> {
    match index {
        0 => Some(PieceType::Rook),
        1 => Some(PieceType::Bishop),
        2 => Some(PieceType::Gold),
        3 => Some(PieceType::Silver),
        4 => Some(PieceType::Knight),
        5 => Some(PieceType::Lance),
        6 => Some(PieceType::Pawn),
        _ => None,
    }
}

// ==== SEE (Static Exchange Evaluation) Constants ====

/// Performance thresholds for SEE optimizations (in nanoseconds)
/// These values determine when to use simple vs complex SEE algorithms
pub const SEE_SIMPLE_CAPTURE_THRESHOLD: i32 = 200; // ns - Use simple algorithm for fast paths
pub const SEE_WITH_PINS_THRESHOLD: i32 = 250; // ns - Include pin calculations for accuracy

/// Maximum depth for SEE calculation (captures/recaptures on a single square)
/// In shogi, it's extremely rare to have more than 16 exchanges on a single square
pub const SEE_MAX_DEPTH: usize = 16;

/// Size of the gain array for SEE calculation
/// Must be larger than SEE_MAX_DEPTH for safety margin
/// Using 2 * SEE_MAX_DEPTH provides sufficient buffer
pub const SEE_GAIN_ARRAY_SIZE: usize = 32;

/// SEE piece values table indexed by [promoted][piece_type]
/// Index 0: unpromoted, Index 1: promoted
pub const SEE_PIECE_VALUES: [[i32; 8]; 2] = [
    // Unpromoted values
    [
        10000, // King (0)
        900,   // Rook (1)
        700,   // Bishop (2)
        600,   // Gold (3)
        500,   // Silver (4)
        400,   // Knight (5)
        300,   // Lance (6)
        100,   // Pawn (7)
    ],
    // Promoted values
    [
        10000, // King cannot promote (0)
        1200,  // Dragon (promoted Rook) (1)
        900,   // Horse (promoted Bishop) (2)
        600,   // Gold cannot promote (3)
        600,   // Promoted Silver (4)
        600,   // Promoted Knight (5)
        600,   // Promoted Lance (6)
        600,   // Tokin (promoted Pawn) (7)
    ],
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_piece_type_to_index() {
        assert_eq!(piece_type_to_index(PieceType::King), 0);
        assert_eq!(piece_type_to_index(PieceType::Rook), 1);
        assert_eq!(piece_type_to_index(PieceType::Bishop), 2);
        assert_eq!(piece_type_to_index(PieceType::Gold), 3);
        assert_eq!(piece_type_to_index(PieceType::Silver), 4);
        assert_eq!(piece_type_to_index(PieceType::Knight), 5);
        assert_eq!(piece_type_to_index(PieceType::Lance), 6);
        assert_eq!(piece_type_to_index(PieceType::Pawn), 7);
    }

    #[test]
    fn test_index_to_piece_type() {
        assert_eq!(index_to_piece_type(0), Some(PieceType::King));
        assert_eq!(index_to_piece_type(7), Some(PieceType::Pawn));
        assert_eq!(index_to_piece_type(8), None);
    }

    #[test]
    fn test_piece_type_to_hand_index() {
        assert_eq!(piece_type_to_hand_index(PieceType::Rook), Ok(0));
        assert_eq!(piece_type_to_hand_index(PieceType::Pawn), Ok(6));
        assert_eq!(piece_type_to_hand_index(PieceType::King), Err("King cannot be in hand"));
    }

    #[test]
    fn test_hand_index_to_piece_type() {
        assert_eq!(hand_index_to_piece_type(0), Some(PieceType::Rook));
        assert_eq!(hand_index_to_piece_type(6), Some(PieceType::Pawn));
        assert_eq!(hand_index_to_piece_type(7), None);
    }

    #[test]
    fn test_constant_arrays() {
        // Verify all arrays have expected lengths
        assert_eq!(ALL_PIECE_TYPES.len(), 8);
        assert_eq!(BOARD_PIECE_TYPES.len(), 7);
        assert_eq!(HAND_PIECE_TYPES.len(), 7);

        // Verify BOARD_PIECE_TYPES excludes King
        assert!(!BOARD_PIECE_TYPES.contains(&PieceType::King));
    }
}
