//! Zobrist hashing for position identification
//!
//! Provides fast incremental hash computation for transposition tables and repetition detection

use lazy_static::lazy_static;
use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;

use crate::{
    shogi::board::{BOARD_SQUARES, HAND_ORDER, MAX_PIECE_INDEX},
    Color, Piece, PieceType, Square,
};

use super::Position;

/// Maximum count per piece type that a player can hold in hand.
///
/// This is the upper bound for the “count” axis of the hand Zobrist table
/// for any piece type (King excluded). We choose 18 because, in theory,
/// one side can hold all pawns at once (there are 18 pawns in total).
/// Other theoretical maxima per side: Lance/Knight/Silver/Gold = 4, Bishop/Rook = 2, King = 0.
const MAX_HAND_COUNT: usize = 18;

/// Zobrist hash tables
pub struct ZobristTable {
    /// Hash values for pieces on squares \[color\]\[piece_kind\]\[square\]
    /// piece_kind includes promoted pieces (0-15)
    pub piece_square: [[[u64; BOARD_SQUARES]; MAX_PIECE_INDEX]; 2],

    /// Hash values for pieces in hand \[color\]\[piece_type\]\[count\]
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
            piece_square: [[[0; BOARD_SQUARES]; MAX_PIECE_INDEX]; 2],
            hand: [[[0; MAX_HAND_COUNT + 1]; 7]; 2],
            side_to_move: rng.random(),
        };

        // Generate random values for pieces on squares
        for color in 0..2 {
            for piece_kind in 0..MAX_PIECE_INDEX {
                for sq in 0..BOARD_SQUARES {
                    table.piece_square[color][piece_kind][sq] = rng.random();
                }
            }
        }

        // Generate random values for pieces in hand
        for color in 0..2 {
            for piece_type in 0..7 {
                for count in 0..=MAX_HAND_COUNT {
                    table.hand[color][piece_type][count] = rng.random();
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
        let piece_idx = piece_type.hand_index().expect("King has no hand index");
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
    pub static ref ZOBRIST: ZobristTable = {
        #[cfg(debug_assertions)]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static ZOBRIST_INIT_STARTED: AtomicBool = AtomicBool::new(false);
            if ZOBRIST_INIT_STARTED.swap(true, Ordering::SeqCst) {
                panic!("ZOBRIST initialization re-entered! Circular dependency detected.");
            }
            // Debug output removed to prevent I/O deadlock in subprocess context
        }

        // Debug output removed to prevent I/O deadlock in subprocess context
        ZobristTable::new()
    };
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

impl ZobristHashing for Position {
    fn zobrist_hash(&self) -> u64 {
        let mut hash = 0u64;

        // Hash pieces on board
        for sq_idx in 0..(BOARD_SQUARES as u8) {
            let sq = Square(sq_idx);
            if let Some(piece) = self.board.piece_on(sq) {
                hash ^= ZOBRIST.piece_square_hash(piece, sq);
            }
        }

        // Hash pieces in hand
        for color in [Color::Black, Color::White] {
            let color_idx = color as usize;
            // Skip King (index 0), iterate through other piece types using HAND_ORDER
            for (piece_idx, &piece_type) in HAND_ORDER.iter().enumerate() {
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
            // NOTE: hand_index() returns the "hand (base) slot" even for promoted types.
            // Promoted piece is treated as its base type in hand (e.g., ProRook → Rook).
            // If this behavior changes in the future, explicitly normalize here.
            let color_idx = moving.color as usize;
            let captured_hand_type = captured_piece.piece_type; // base type slot for hand_index()
            let piece_idx =
                captured_hand_type.hand_index().expect("King is never captured to hand");
            let old_count = self.hands[color_idx][piece_idx];
            let new_count = old_count + 1;

            // Remove old hand hash
            if old_count > 0 {
                hash ^= ZOBRIST.hand_hash(moving.color, captured_hand_type, old_count);
            }
            // Add new hand hash
            hash ^= ZOBRIST.hand_hash(moving.color, captured_hand_type, new_count);
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
        // Validate hand count BEFORE touching hash to keep hash consistent on early return
        let color_idx = color as usize;
        let piece_idx = piece_type.hand_index().expect("King cannot be dropped");
        let old_count = self.hands[color_idx][piece_idx];
        // old_count==0 の場合 hand_hash は 0 を返してしまうため、debug_assert で検出する
        debug_assert!(old_count > 0, "Dropping {:?} for {:?} but hand is empty", piece_type, color);
        if old_count == 0 {
            log::warn!("Dropping {:?} for {:?} but hand is empty", piece_type, color);
            // リリースでも安全側に無変更で返す（ハッシュにまだ触れていない）
            return hash;
        }

        // Add piece to board
        let piece = Piece::new(piece_type, color);
        hash ^= ZOBRIST.piece_square_hash(piece, to);

        // Update hand hash
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
impl Position {
    /// Get zobrist hash for piece on square
    #[inline]
    pub fn piece_square_zobrist(&self, piece: Piece, sq: Square) -> u64 {
        ZOBRIST.piece_square_hash(piece, sq)
    }

    /// Get zobrist hash for hand piece
    #[inline]
    pub fn hand_zobrist(&self, color: Color, piece_type: PieceType, count: u8) -> u64 {
        ZOBRIST.hand_hash(color, piece_type, count)
    }

    /// Get zobrist hash for side to move
    #[inline]
    pub fn side_to_move_zobrist(&self) -> u64 {
        ZOBRIST.side_hash(self.side_to_move)
    }
}

#[cfg(test)]
mod tests {
    use crate::shogi::board::MAX_PIECE_INDEX;
    use crate::usi::parse_usi_square;

    use super::*;

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
            for piece in 0..MAX_PIECE_INDEX {
                for sq in 0..crate::shogi::SHOGI_BOARD_SIZE {
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
        let sq1 = parse_usi_square("5e").unwrap();
        let sq2 = parse_usi_square("4f").unwrap();

        // Move piece from sq1 to sq2 and back should return original hash
        let hash2 = pos.update_hash_move(hash, sq1, sq2, piece, None);
        let hash3 = pos.update_hash_move(hash2, sq2, sq1, piece, None);

        // Account for side to move changes (two moves = two XORs)
        let expected = hash ^ ZOBRIST.side_to_move ^ ZOBRIST.side_to_move;
        assert_eq!(hash3, expected); // Should be back to original
    }

    #[test]
    fn test_side_to_move_difference() {
        // Test that the same position with different side to move has different hash
        let mut pos_black = Position::empty();
        let mut pos_white = Position::empty();

        // Set up identical board positions
        let piece = Piece::new(PieceType::King, Color::Black);
        let sq = parse_usi_square("5i").unwrap();
        pos_black.board.put_piece(sq, piece);
        pos_white.board.put_piece(sq, piece);

        let piece2 = Piece::new(PieceType::King, Color::White);
        let sq2 = parse_usi_square("5a").unwrap();
        pos_black.board.put_piece(sq2, piece2);
        pos_white.board.put_piece(sq2, piece2);

        // Set different side to move
        pos_black.side_to_move = Color::Black;
        pos_white.side_to_move = Color::White;

        // Calculate hashes
        pos_black.hash = pos_black.compute_hash();
        pos_black.zobrist_hash = pos_black.hash;
        pos_white.hash = pos_white.compute_hash();
        pos_white.zobrist_hash = pos_white.hash;

        let hash_black = pos_black.zobrist_hash;
        let hash_white = pos_white.zobrist_hash;

        // Hashes should differ by exactly the side_to_move value
        assert_ne!(hash_black, hash_white);
        assert_eq!(hash_black ^ hash_white, ZOBRIST.side_to_move);
    }

    #[test]
    fn test_hand_pieces_difference() {
        // Test that the same board position with different hand pieces has different hash
        let mut pos1 = Position::empty();
        let mut pos2 = Position::empty();

        // Set up identical board positions with kings
        let king_black = Piece::new(PieceType::King, Color::Black);
        let king_white = Piece::new(PieceType::King, Color::White);
        pos1.board.put_piece(parse_usi_square("5i").unwrap(), king_black);
        pos1.board.put_piece(parse_usi_square("5a").unwrap(), king_white);
        pos2.board.put_piece(parse_usi_square("5i").unwrap(), king_black);
        pos2.board.put_piece(parse_usi_square("5a").unwrap(), king_white);

        // Add different hand pieces
        pos1.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
        pos2.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 2;

        // Calculate hashes
        pos1.hash = pos1.compute_hash();
        pos1.zobrist_hash = pos1.hash;
        pos2.hash = pos2.compute_hash();
        pos2.zobrist_hash = pos2.hash;

        let hash1 = pos1.zobrist_hash;
        let hash2 = pos2.zobrist_hash;

        // Hashes should be different
        assert_ne!(hash1, hash2);

        // Test with different piece types in hand
        let mut pos3 = Position::empty();
        pos3.board.put_piece(parse_usi_square("5i").unwrap(), king_black);
        pos3.board.put_piece(parse_usi_square("5a").unwrap(), king_white);
        pos3.hands[Color::Black as usize][PieceType::Rook.hand_index().unwrap()] = 1;

        pos3.hash = pos3.compute_hash();
        pos3.zobrist_hash = pos3.hash;

        let hash3 = pos3.zobrist_hash;
        assert_ne!(hash1, hash3);
        assert_ne!(hash2, hash3);
    }

    #[test]
    fn test_incremental_updates() {
        // Test that incremental hash updates match full recalculation
        let pos = Position::startpos();
        let initial_hash = pos.zobrist_hash();

        // Make a move
        let from = parse_usi_square("7g").unwrap();
        let to = parse_usi_square("7f").unwrap();
        let moving_piece = pos.piece_at(from).unwrap();

        // Simulate move
        let mut pos_after = pos.clone();
        pos_after.board.remove_piece(from);
        pos_after.board.put_piece(to, moving_piece);
        pos_after.side_to_move = match pos_after.side_to_move {
            Color::Black => Color::White,
            Color::White => Color::Black,
        };

        // Calculate hash from scratch
        pos_after.hash = pos_after.compute_hash();
        pos_after.zobrist_hash = pos_after.hash;
        let hash_from_scratch = pos_after.zobrist_hash;

        // Calculate incremental hash
        let hash_incremental = pos.update_hash_move(initial_hash, from, to, moving_piece, None);

        // They should match
        assert_eq!(hash_from_scratch, hash_incremental);

        // Test with capture
        let mut pos_capture = Position::empty();
        let black_pawn = Piece::new(PieceType::Pawn, Color::Black);
        let white_pawn = Piece::new(PieceType::Pawn, Color::White);
        let black_king = Piece::new(PieceType::King, Color::Black);
        let white_king = Piece::new(PieceType::King, Color::White);

        pos_capture.board.put_piece(parse_usi_square("5i").unwrap(), black_king);
        pos_capture.board.put_piece(parse_usi_square("5a").unwrap(), white_king);
        pos_capture.board.put_piece(parse_usi_square("5e").unwrap(), black_pawn);
        pos_capture.board.put_piece(parse_usi_square("5d").unwrap(), white_pawn);

        pos_capture.hash = pos_capture.compute_hash();
        pos_capture.zobrist_hash = pos_capture.hash;
        let initial_capture_hash = pos_capture.zobrist_hash;

        // Capture white pawn with black pawn
        let mut pos_after_capture = pos_capture.clone();
        pos_after_capture.board.remove_piece(parse_usi_square("5e").unwrap());
        pos_after_capture.board.remove_piece(parse_usi_square("5d").unwrap());
        pos_after_capture.board.put_piece(parse_usi_square("5d").unwrap(), black_pawn);
        pos_after_capture.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
        pos_after_capture.side_to_move = Color::White;

        pos_after_capture.hash = pos_after_capture.compute_hash();
        pos_after_capture.zobrist_hash = pos_after_capture.hash;
        let hash_capture_scratch = pos_after_capture.zobrist_hash;
        let hash_capture_incremental = pos_capture.update_hash_move(
            initial_capture_hash,
            parse_usi_square("5e").unwrap(),
            parse_usi_square("5d").unwrap(),
            black_pawn,
            Some(white_pawn),
        );

        assert_eq!(hash_capture_scratch, hash_capture_incremental);
    }

    #[test]
    fn test_symmetric_positions() {
        // Test that symmetric positions have different hashes
        let mut pos1 = Position::empty();
        let mut pos2 = Position::empty();

        // Create horizontally symmetric positions
        // Position 1: Rook on 1a
        pos1.board
            .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::Rook, Color::White));
        pos1.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos1.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        // Position 2: Rook on 9a (symmetric)
        pos2.board
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::Rook, Color::White));
        pos2.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos2.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        pos1.hash = pos1.compute_hash();
        pos1.zobrist_hash = pos1.hash;
        pos2.hash = pos2.compute_hash();
        pos2.zobrist_hash = pos2.hash;

        let hash1 = pos1.zobrist_hash;
        let hash2 = pos2.zobrist_hash;

        // Symmetric positions should have different hashes
        assert_ne!(hash1, hash2);

        // Test with multiple pieces
        let mut pos3 = Position::empty();
        let mut pos4 = Position::empty();

        // Add kings
        pos3.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos3.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos4.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos4.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

        // Add pawns in symmetric positions
        pos3.board
            .put_piece(parse_usi_square("3e").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
        pos4.board
            .put_piece(parse_usi_square("7e").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

        pos3.hash = pos3.compute_hash();
        pos3.zobrist_hash = pos3.hash;
        pos4.hash = pos4.compute_hash();
        pos4.zobrist_hash = pos4.hash;

        let hash3 = pos3.zobrist_hash;
        let hash4 = pos4.zobrist_hash;

        assert_ne!(hash3, hash4);
    }

    #[test]
    fn test_hash_distribution() {
        use std::collections::HashMap;

        // Test statistical properties of hash values
        let mut bit_counts = vec![0u32; 64];
        let mut hash_values = Vec::new();

        // Generate many different positions
        for i in 0..1000 {
            let mut pos = Position::empty();

            // Add kings (required) - vary their positions too
            let black_king_file = ((i * 7) % crate::shogi::BOARD_FILES) as u8;
            let black_king_rank = b'g' + (i % 3) as u8;
            let black_king_file_char = (b'9' - black_king_file) as char;
            let black_king_rank_char = black_king_rank as char;
            if let Ok(sq) =
                parse_usi_square(&format!("{black_king_file_char}{black_king_rank_char}"))
            {
                pos.board.put_piece(sq, Piece::new(PieceType::King, Color::Black));
            } else {
                // Fallback position
                pos.board.put_piece(
                    parse_usi_square("5i").unwrap(),
                    Piece::new(PieceType::King, Color::Black),
                );
            }

            let white_king_file = ((i * 3) % crate::shogi::BOARD_FILES) as u8;
            let white_king_rank = b'a' + (i % 3) as u8;
            let white_king_file_char = (b'9' - white_king_file) as char;
            let white_king_rank_char = white_king_rank as char;
            if let Ok(sq) =
                parse_usi_square(&format!("{white_king_file_char}{white_king_rank_char}"))
            {
                pos.board.put_piece(sq, Piece::new(PieceType::King, Color::White));
            } else {
                // Fallback position
                pos.board.put_piece(
                    parse_usi_square("5a").unwrap(),
                    Piece::new(PieceType::King, Color::White),
                );
            }

            // Add various pieces to create diversity
            let piece_types = [
                PieceType::Pawn,
                PieceType::Lance,
                PieceType::Knight,
                PieceType::Silver,
                PieceType::Gold,
                PieceType::Bishop,
                PieceType::Rook,
            ];

            // Add pieces based on different patterns
            for j in 0..5 {
                let idx = (i * 7 + j * 13) % crate::shogi::SHOGI_BOARD_SIZE;
                let file = idx % crate::shogi::BOARD_FILES;
                let rank = idx / crate::shogi::BOARD_FILES;

                let file_char = (b'9' - file as u8) as char;
                let rank_char = (b'a' + rank as u8) as char;
                if let Ok(sq) = parse_usi_square(&format!("{file_char}{rank_char}")) {
                    // Check if square is not occupied by king
                    if pos.piece_at(sq).is_none() {
                        let piece_type = piece_types[(i + j) % piece_types.len()];
                        let color = if (i + j) % 2 == 0 {
                            Color::Black
                        } else {
                            Color::White
                        };
                        pos.board.put_piece(sq, Piece::new(piece_type, color));
                    }
                }
            }

            // Vary hand pieces with more diversity
            let hand_piece_types = [
                PieceType::Pawn,
                PieceType::Lance,
                PieceType::Knight,
                PieceType::Silver,
                PieceType::Gold,
                PieceType::Bishop,
                PieceType::Rook,
            ];

            for (idx, &piece_type) in hand_piece_types.iter().enumerate() {
                if let Some(hand_idx) = piece_type.hand_index() {
                    pos.hands[Color::Black as usize][hand_idx] = ((i + idx * 7) % 5) as u8;
                    pos.hands[Color::White as usize][hand_idx] = ((i + idx * 11) % 4) as u8;
                }
            }

            // Vary side to move
            pos.side_to_move = if i % 2 == 0 {
                Color::Black
            } else {
                Color::White
            };

            // Vary ply for additional diversity
            pos.ply = (i % 200) as u16;

            pos.hash = pos.compute_hash();
            pos.zobrist_hash = pos.hash;
            let hash = pos.zobrist_hash;
            hash_values.push(hash);

            // Count bits (use enumerate to avoid needless_range_loop)
            for (bit, count) in bit_counts.iter_mut().enumerate() {
                if (hash >> bit) & 1 == 1 {
                    *count += 1;
                }
            }
        }

        // Check bit distribution (should be close to 50%)
        for (bit, count) in bit_counts.iter().enumerate() {
            let ratio = *count as f64 / 1000.0;
            assert!(
                (0.35..=0.65).contains(&ratio), // Slightly relaxed bounds
                "Bit {bit} has poor distribution: {:.2}%",
                ratio * 100.0
            );
        }

        // Check for collisions
        let unique_count = hash_values.iter().collect::<std::collections::HashSet<_>>().len();
        assert_eq!(unique_count, hash_values.len(), "Hash collisions detected!");

        // Check hash value distribution across buckets
        let mut buckets = HashMap::new();
        let bucket_size = u64::MAX / 16;

        for &hash in &hash_values {
            let bucket = hash / bucket_size;
            *buckets.entry(bucket).or_insert(0) += 1;
        }

        // Each bucket should have roughly 1000/16 ≈ 62 values
        let expected_per_bucket = 1000.0 / 16.0;
        for (bucket, count) in buckets {
            let ratio = count as f64 / expected_per_bucket;
            assert!(
                (0.3..=1.7).contains(&ratio),  // Slightly relaxed bounds
                "Bucket {bucket} has poor distribution: {count} values (expected ~{expected_per_bucket})"
            );
        }
    }

    #[test]
    fn test_promoted_piece_capture_hash() {
        // Test that capturing promoted pieces correctly updates the hash
        // Promoted pieces should revert to base type when added to hand
        let mut pos = Position::empty();

        // Place kings
        let black_king = Piece::new(PieceType::King, Color::Black);
        let white_king = Piece::new(PieceType::King, Color::White);
        pos.board.put_piece(parse_usi_square("5i").unwrap(), black_king);
        pos.board.put_piece(parse_usi_square("5a").unwrap(), white_king);

        // Place a promoted pawn (tokin) that will be captured
        let promoted_pawn = Piece::promoted(PieceType::Pawn, Color::White);
        let capture_sq = parse_usi_square("5e").unwrap();
        pos.board.put_piece(capture_sq, promoted_pawn);

        // Place black silver that will capture the tokin
        let black_silver = Piece::new(PieceType::Silver, Color::Black);
        let from_sq = parse_usi_square("4f").unwrap();
        pos.board.put_piece(from_sq, black_silver);

        // Calculate initial hash
        pos.hash = pos.compute_hash();
        pos.zobrist_hash = pos.hash;
        let initial_hash = pos.zobrist_hash;

        // Simulate the capture move
        let mut pos_after = pos.clone();
        pos_after.board.remove_piece(from_sq);
        pos_after.board.remove_piece(capture_sq);
        pos_after.board.put_piece(capture_sq, black_silver);

        // IMPORTANT: Promoted piece becomes unpromoted when captured
        pos_after.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
        pos_after.side_to_move = Color::White;

        // Calculate hash from scratch
        pos_after.hash = pos_after.compute_hash();
        pos_after.zobrist_hash = pos_after.hash;
        let hash_from_scratch = pos_after.zobrist_hash;

        // Calculate incremental hash
        let hash_incremental = pos.update_hash_move(
            initial_hash,
            from_sq,
            capture_sq,
            black_silver,
            Some(promoted_pawn),
        );

        assert_eq!(
            hash_from_scratch, hash_incremental,
            "Hash mismatch when capturing promoted piece"
        );

        // Test with promoted rook
        let mut pos2 = Position::empty();
        pos2.board.put_piece(parse_usi_square("5i").unwrap(), black_king);
        pos2.board.put_piece(parse_usi_square("5a").unwrap(), white_king);

        let promoted_rook = Piece::promoted(PieceType::Rook, Color::White);
        let capture_sq2 = parse_usi_square("3c").unwrap();
        pos2.board.put_piece(capture_sq2, promoted_rook);

        let black_gold = Piece::new(PieceType::Gold, Color::Black);
        let from_sq2 = parse_usi_square("4d").unwrap();
        pos2.board.put_piece(from_sq2, black_gold);

        pos2.hash = pos2.compute_hash();
        pos2.zobrist_hash = pos2.hash;
        let initial_hash2 = pos2.zobrist_hash;

        // Simulate capture
        let mut pos2_after = pos2.clone();
        pos2_after.board.remove_piece(from_sq2);
        pos2_after.board.remove_piece(capture_sq2);
        pos2_after.board.put_piece(capture_sq2, black_gold);
        pos2_after.hands[Color::Black as usize][PieceType::Rook.hand_index().unwrap()] = 1;
        pos2_after.side_to_move = Color::White;

        pos2_after.hash = pos2_after.compute_hash();
        pos2_after.zobrist_hash = pos2_after.hash;
        let hash2_from_scratch = pos2_after.zobrist_hash;

        let hash2_incremental = pos2.update_hash_move(
            initial_hash2,
            from_sq2,
            capture_sq2,
            black_gold,
            Some(promoted_rook),
        );

        assert_eq!(
            hash2_from_scratch, hash2_incremental,
            "Hash mismatch when capturing promoted rook"
        );
    }

    #[test]
    fn test_update_hash_drop_guard_keeps_hash_unchanged() {
        // In debug builds, this path triggers a debug_assert! to catch engine bugs.
        // Skip the assertion-triggering scenario in debug to avoid expected panic.
        if cfg!(debug_assertions) {
            return;
        }
        // When dropping from empty hand, update_hash_drop must NOT modify hash
        let mut pos = Position::empty();

        // Place kings only (minimal legal-ish board)
        let black_king = Piece::new(PieceType::King, Color::Black);
        let white_king = Piece::new(PieceType::King, Color::White);
        pos.board.put_piece(parse_usi_square("5i").unwrap(), black_king);
        pos.board.put_piece(parse_usi_square("5a").unwrap(), white_king);

        // Compute initial hash
        pos.hash = pos.compute_hash();
        pos.zobrist_hash = pos.hash;
        let initial = pos.zobrist_hash;

        // Black has no pawns in hand; attempt to compute hash for a drop
        // The function must return unchanged hash when hand is empty
        let sq = parse_usi_square("5e").unwrap();
        let new_hash = pos.update_hash_drop(initial, PieceType::Pawn, sq, Color::Black);

        assert_eq!(new_hash, initial, "Hash must remain unchanged on invalid drop");
    }
}
