//! Zobrist hashing for position identification
//!
//! Provides fast incremental hash computation for transposition tables and repetition detection

use super::board::{Color, Piece, PieceType, Square};
use lazy_static::lazy_static;
use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

/// Maximum number of pieces that can be held in hand (18 pieces max)
const MAX_HAND_COUNT: usize = 18;

/// Zobrist hash tables
pub struct ZobristTable {
    /// Hash values for pieces on squares [color][piece_kind][square]
    /// piece_kind includes promoted pieces (0-15)
    pub piece_square: [[[u64; 81]; 16]; 2],

    /// Hash values for pieces in hand [color][piece_type][count]
    /// piece_type is 0-6 (no King), count is 0-MAX_HAND_COUNT (max possible)
    pub hand: [[[u64; MAX_HAND_COUNT + 1]; 7]; 2],

    /// Hash value for side to move (White)
    pub side_to_move: u64,
}

impl Default for ZobristTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ZobristTable {
    /// Create new Zobrist table with random values
    pub fn new() -> Self {
        // Use fixed seed for reproducibility
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1234567890ABCDEF);

        let mut table = ZobristTable {
            piece_square: [[[0; 81]; 16]; 2],
            hand: [[[0; MAX_HAND_COUNT + 1]; 7]; 2],
            side_to_move: rng.gen(),
        };

        // Generate random values for pieces on squares
        for color in 0..2 {
            for piece_kind in 0..16 {
                for sq in 0..81 {
                    table.piece_square[color][piece_kind][sq] = rng.gen();
                }
            }
        }

        // Generate random values for pieces in hand
        for color in 0..2 {
            for piece_type in 0..7 {
                for count in 0..=MAX_HAND_COUNT {
                    table.hand[color][piece_type][count] = rng.gen();
                }
            }
        }

        table
    }

    /// Get hash value for a piece on a square
    #[inline]
    pub fn piece_square_hash(&self, piece: Piece, sq: Square) -> u64 {
        let color = piece.color as usize;
        let piece_kind = piece.to_index();
        self.piece_square[color][piece_kind][sq.index()]
    }

    /// Get hash value for pieces in hand
    #[inline]
    pub fn hand_hash(&self, color: Color, piece_type: PieceType, count: u8) -> u64 {
        if count == 0 {
            return 0;
        }
        let color_idx = color as usize;
        let piece_idx = piece_type as usize - 1; // Skip King (0)
        let count_idx = (count as usize).min(MAX_HAND_COUNT);
        self.hand[color_idx][piece_idx][count_idx]
    }

    /// Get hash value for side to move
    #[inline]
    pub fn side_hash(&self, color: Color) -> u64 {
        match color {
            Color::Black => 0,
            Color::White => self.side_to_move,
        }
    }
}

// Global Zobrist table instance
lazy_static! {
    pub static ref ZOBRIST: ZobristTable = ZobristTable::new();
}

/// Extension trait for Position to add Zobrist hashing
pub trait ZobristHashing {
    /// Compute Zobrist hash from scratch
    fn zobrist_hash(&self) -> u64;

    /// Update hash when moving a piece
    fn update_hash_move(
        &self,
        hash: u64,
        from: Square,
        to: Square,
        moving: Piece,
        captured: Option<Piece>,
    ) -> u64;

    /// Update hash when dropping a piece
    fn update_hash_drop(&self, hash: u64, piece_type: PieceType, to: Square, color: Color) -> u64;

    /// Update hash when promoting a piece
    fn update_hash_promote(&self, hash: u64, sq: Square, piece: Piece) -> u64;

    /// Update hash for side to move change
    fn update_hash_side(&self, hash: u64) -> u64;
}

impl ZobristHashing for super::board::Position {
    fn zobrist_hash(&self) -> u64 {
        let mut hash = 0u64;

        // Hash pieces on board
        for sq_idx in 0..81 {
            let sq = Square(sq_idx);
            if let Some(piece) = self.board.piece_on(sq) {
                hash ^= ZOBRIST.piece_square_hash(piece, sq);
            }
        }

        // Hash pieces in hand
        for color in [Color::Black, Color::White] {
            let color_idx = color as usize;
            // Skip King (index 0), iterate through other piece types
            for piece_idx in 0..7 {
                let piece_type = match piece_idx {
                    0 => PieceType::Rook,
                    1 => PieceType::Bishop,
                    2 => PieceType::Gold,
                    3 => PieceType::Silver,
                    4 => PieceType::Knight,
                    5 => PieceType::Lance,
                    6 => PieceType::Pawn,
                    _ => unreachable!(),
                };
                let count = self.hands[color_idx][piece_idx];
                if count > 0 {
                    hash ^= ZOBRIST.hand_hash(color, piece_type, count);
                }
            }
        }

        // Hash side to move
        hash ^= ZOBRIST.side_hash(self.side_to_move);

        hash
    }

    fn update_hash_move(
        &self,
        mut hash: u64,
        from: Square,
        to: Square,
        moving: Piece,
        captured: Option<Piece>,
    ) -> u64 {
        // Remove moving piece from source
        hash ^= ZOBRIST.piece_square_hash(moving, from);

        // Remove captured piece if any
        if let Some(captured_piece) = captured {
            hash ^= ZOBRIST.piece_square_hash(captured_piece, to);

            // Update hand hash (piece goes to hand)
            let color_idx = moving.color as usize;
            let piece_idx = captured_piece.piece_type as usize - 1; // Skip King
            let old_count = self.hands[color_idx][piece_idx];
            let new_count = old_count + 1;

            // Remove old hand hash
            if old_count > 0 {
                hash ^= ZOBRIST.hand_hash(moving.color, captured_piece.piece_type, old_count);
            }
            // Add new hand hash
            hash ^= ZOBRIST.hand_hash(moving.color, captured_piece.piece_type, new_count);
        }

        // Add moving piece to destination
        hash ^= ZOBRIST.piece_square_hash(moving, to);

        // Update side to move
        hash ^= ZOBRIST.side_to_move;

        hash
    }

    fn update_hash_drop(
        &self,
        mut hash: u64,
        piece_type: PieceType,
        to: Square,
        color: Color,
    ) -> u64 {
        // Add piece to board
        let piece = Piece::new(piece_type, color);
        hash ^= ZOBRIST.piece_square_hash(piece, to);

        // Update hand hash
        let color_idx = color as usize;
        let piece_idx = piece_type as usize - 1; // Skip King
        let old_count = self.hands[color_idx][piece_idx];
        let new_count = old_count - 1;

        // Remove old hand hash
        hash ^= ZOBRIST.hand_hash(color, piece_type, old_count);
        // Add new hand hash if still have pieces
        if new_count > 0 {
            hash ^= ZOBRIST.hand_hash(color, piece_type, new_count);
        }

        // Update side to move
        hash ^= ZOBRIST.side_to_move;

        hash
    }

    fn update_hash_promote(&self, mut hash: u64, sq: Square, piece: Piece) -> u64 {
        // Remove unpromoted piece
        hash ^= ZOBRIST.piece_square_hash(piece, sq);

        // Add promoted piece
        let promoted = Piece::promoted(piece.piece_type, piece.color);
        hash ^= ZOBRIST.piece_square_hash(promoted, sq);

        hash
    }

    fn update_hash_side(&self, hash: u64) -> u64 {
        hash ^ ZOBRIST.side_to_move
    }
}

/// Helper methods for Position to use in do_move
impl super::board::Position {
    /// Get zobrist hash for piece on square
    pub fn piece_square_zobrist(&self, piece: Piece, sq: Square) -> u64 {
        ZOBRIST.piece_square_hash(piece, sq)
    }

    /// Get zobrist hash for hand piece
    pub fn hand_zobrist(&self, color: Color, piece_type: PieceType, count: u8) -> u64 {
        ZOBRIST.hand_hash(color, piece_type, count)
    }

    /// Get zobrist hash for side to move
    pub fn side_to_move_zobrist(&self) -> u64 {
        ZOBRIST.side_hash(self.side_to_move)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::board::Position;

    #[test]
    fn test_zobrist_deterministic() {
        // Zobrist values should be deterministic with fixed seed
        let table1 = ZobristTable::new();
        let table2 = ZobristTable::new();

        // Check a few values are the same
        assert_eq!(table1.side_to_move, table2.side_to_move);
        assert_eq!(table1.piece_square[0][0][0], table2.piece_square[0][0][0]);
        assert_eq!(table1.hand[0][0][0], table2.hand[0][0][0]);
    }

    #[test]
    fn test_zobrist_uniqueness() {
        let table = ZobristTable::new();

        // Check that different positions have different hash values
        let mut values = std::collections::HashSet::new();

        // Test piece-square values
        for color in 0..2 {
            for piece in 0..14 {
                for sq in 0..81 {
                    let hash = table.piece_square[color][piece][sq];
                    assert!(values.insert(hash), "Duplicate hash found");
                }
            }
        }

        // Test hand values
        for color in 0..2 {
            for piece in 0..7 {
                for count in 0..=MAX_HAND_COUNT {
                    let hash = table.hand[color][piece][count];
                    assert!(values.insert(hash), "Duplicate hash found");
                }
            }
        }
    }

    #[test]
    fn test_position_hash() {
        let pos = Position::startpos();
        let hash1 = pos.zobrist_hash();
        let hash2 = pos.zobrist_hash();

        // Same position should have same hash
        assert_eq!(hash1, hash2);

        // Empty position should have different hash
        let empty_pos = Position::empty();
        let empty_hash = empty_pos.zobrist_hash();
        assert_ne!(hash1, empty_hash);
    }

    #[test]
    fn test_hash_symmetry() {
        let pos = Position::startpos();
        let hash = pos.zobrist_hash();

        // Create a piece and square for testing
        let piece = Piece::new(PieceType::Pawn, Color::Black);
        let sq1 = Square::new(4, 4);
        let sq2 = Square::new(5, 5);

        // Move piece from sq1 to sq2 and back should return original hash
        let hash2 = pos.update_hash_move(hash, sq1, sq2, piece, None);
        let hash3 = pos.update_hash_move(hash2, sq2, sq1, piece, None);

        // Account for side to move changes (two moves = two XORs)
        let expected = hash ^ ZOBRIST.side_to_move ^ ZOBRIST.side_to_move;
        assert_eq!(hash3, expected); // Should be back to original
    }
}
