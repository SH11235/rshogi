//! Attack tables and piece movement patterns
//!
//! Pre-computed attack patterns for fast move generation

use super::board::{Bitboard, Color, PieceType, Square};
use once_cell::sync::Lazy;

/// Direction offsets for piece movements
#[derive(Clone, Copy, Debug)]
pub enum Direction {
    North = -9,      // Up
    NorthEast = -8,  // Up-Right
    East = 1,        // Right
    SouthEast = 10,  // Down-Right
    South = 9,       // Down
    SouthWest = 8,   // Down-Left
    West = -1,       // Left
    NorthWest = -10, // Up-Left
}

impl Direction {
    /// All directions (for King and promoted pieces)
    pub const ALL: [Direction; 8] = [
        Direction::North,
        Direction::NorthEast,
        Direction::East,
        Direction::SouthEast,
        Direction::South,
        Direction::SouthWest,
        Direction::West,
        Direction::NorthWest,
    ];

    /// Diagonal directions (for Bishop)
    pub const DIAGONALS: [Direction; 4] = [
        Direction::NorthEast,
        Direction::SouthEast,
        Direction::SouthWest,
        Direction::NorthWest,
    ];

    /// Orthogonal directions (for Rook)
    pub const ORTHOGONALS: [Direction; 4] = [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ];
}

/// Pre-computed attack tables
pub struct AttackTables {
    /// King attacks from each square
    pub king_attacks: [Bitboard; 81],

    /// Gold attacks from each square (also used for promoted pieces)
    pub gold_attacks: [[Bitboard; 81]; 2], // [color][square]

    /// Silver attacks from each square
    pub silver_attacks: [[Bitboard; 81]; 2], // [color][square]

    /// Knight attacks from each square
    pub knight_attacks: [[Bitboard; 81]; 2], // [color][square]

    /// Lance attacks from each square (without blockers)
    pub lance_attacks: [[Bitboard; 81]; 2], // [color][square]

    /// Pawn attacks from each square
    pub pawn_attacks: [[Bitboard; 81]; 2], // [color][square]
}

impl Default for AttackTables {
    fn default() -> Self {
        Self::new()
    }
}

impl AttackTables {
    /// Generate all attack tables
    pub fn new() -> Self {
        let mut tables = AttackTables {
            king_attacks: [Bitboard::EMPTY; 81],
            gold_attacks: [[Bitboard::EMPTY; 81]; 2],
            silver_attacks: [[Bitboard::EMPTY; 81]; 2],
            knight_attacks: [[Bitboard::EMPTY; 81]; 2],
            lance_attacks: [[Bitboard::EMPTY; 81]; 2],
            pawn_attacks: [[Bitboard::EMPTY; 81]; 2],
        };

        // Generate tables for each square
        for sq in 0..81 {
            let square = Square(sq);
            tables.king_attacks[sq as usize] = tables.generate_king_attacks(square);

            for color in [Color::Black, Color::White] {
                let color_idx = color as usize;
                tables.gold_attacks[color_idx][sq as usize] =
                    tables.generate_gold_attacks(square, color);
                tables.silver_attacks[color_idx][sq as usize] =
                    tables.generate_silver_attacks(square, color);
                tables.knight_attacks[color_idx][sq as usize] =
                    tables.generate_knight_attacks(square, color);
                tables.lance_attacks[color_idx][sq as usize] =
                    tables.generate_lance_attacks(square, color);
                tables.pawn_attacks[color_idx][sq as usize] =
                    tables.generate_pawn_attacks(square, color);
            }
        }

        tables
    }

    /// Generate king attacks from a square
    fn generate_king_attacks(&self, from: Square) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;
        let file = from.file() as i8;
        let rank = from.rank() as i8;

        // Check all 8 adjacent squares
        for file_delta in -1..=1 {
            for rank_delta in -1..=1 {
                if file_delta == 0 && rank_delta == 0 {
                    continue; // Skip the square itself
                }

                let new_file = file + file_delta;
                let new_rank = rank + rank_delta;

                if (0..9).contains(&new_file) && (0..9).contains(&new_rank) {
                    attacks.set(Square::new(new_file as u8, new_rank as u8));
                }
            }
        }

        attacks
    }

    /// Generate gold attacks from a square
    fn generate_gold_attacks(&self, from: Square, color: Color) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;
        let file = from.file() as i8;
        let rank = from.rank() as i8;

        // Gold moves: all adjacent except diagonal backwards
        let directions = match color {
            Color::Black => [
                Direction::South,     // Forward
                Direction::SouthEast, // Diagonal forward-right
                Direction::East,      // Right
                Direction::West,      // Left
                Direction::SouthWest, // Diagonal forward-left
                Direction::North,     // Backward
            ],
            Color::White => [
                Direction::North,     // Forward
                Direction::NorthEast, // Diagonal forward-left
                Direction::East,      // Right (from White's perspective, actually left)
                Direction::West,      // Left (from White's perspective, actually right)
                Direction::NorthWest, // Diagonal forward-right
                Direction::South,     // Backward
            ],
        };

        for &dir in &directions {
            let (file_delta, rank_delta) = match dir {
                Direction::North => (0, -1),
                Direction::NorthEast => (1, -1),
                Direction::East => (1, 0),
                Direction::SouthEast => (1, 1),
                Direction::South => (0, 1),
                Direction::SouthWest => (-1, 1),
                Direction::West => (-1, 0),
                Direction::NorthWest => (-1, -1),
            };
            let new_file = file + file_delta;
            let new_rank = rank + rank_delta;

            if (0..9).contains(&new_file) && (0..9).contains(&new_rank) {
                attacks.set(Square::new(new_file as u8, new_rank as u8));
            }
        }

        attacks
    }

    /// Generate silver attacks from a square
    fn generate_silver_attacks(&self, from: Square, color: Color) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;
        let file = from.file() as i8;
        let rank = from.rank() as i8;

        // Silver moves: forward and all diagonals
        let directions = match color {
            Color::Black => [
                Direction::South,     // Forward for Black (towards rank 8)
                Direction::NorthEast, // Diagonal backward-right
                Direction::SouthEast, // Diagonal forward-right
                Direction::SouthWest, // Diagonal forward-left
                Direction::NorthWest, // Diagonal backward-left
            ],
            Color::White => [
                Direction::North,     // Forward for White (towards rank 0)
                Direction::SouthEast, // Diagonal backward-left
                Direction::NorthEast, // Diagonal forward-left
                Direction::NorthWest, // Diagonal forward-right
                Direction::SouthWest, // Diagonal backward-right
            ],
        };

        for &dir in &directions {
            let (file_delta, rank_delta) = match dir {
                Direction::North => (0, -1),
                Direction::NorthEast => (1, -1),
                Direction::East => (1, 0),
                Direction::SouthEast => (1, 1),
                Direction::South => (0, 1),
                Direction::SouthWest => (-1, 1),
                Direction::West => (-1, 0),
                Direction::NorthWest => (-1, -1),
            };
            let new_file = file + file_delta;
            let new_rank = rank + rank_delta;

            if (0..9).contains(&new_file) && (0..9).contains(&new_rank) {
                attacks.set(Square::new(new_file as u8, new_rank as u8));
            }
        }

        attacks
    }

    /// Generate knight attacks from a square
    fn generate_knight_attacks(&self, from: Square, color: Color) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;
        let file = from.file() as i8;
        let rank = from.rank() as i8;

        // Knight moves: two forward, one to the side
        let (rank_offset, min_rank, max_rank) = match color {
            Color::Black => (2, 0, 6), // Black moves towards rank 8, can't move from rank 7-8
            Color::White => (-2, 2, 8), // White moves towards rank 0, can't move from rank 0-1
        };

        let new_rank = rank + rank_offset;
        if new_rank >= min_rank && new_rank <= max_rank {
            // Left
            if file > 0 {
                attacks.set(Square::new((file - 1) as u8, new_rank as u8));
            }
            // Right
            if file < 8 {
                attacks.set(Square::new((file + 1) as u8, new_rank as u8));
            }
        }

        attacks
    }

    /// Generate lance attacks from a square (without blockers)
    fn generate_lance_attacks(&self, from: Square, color: Color) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;
        let file = from.file();
        let rank = from.rank() as i8;

        let (start, end, step) = match color {
            Color::Black => (rank + 1, 9, 1),   // Black moves towards rank 8
            Color::White => (rank - 1, -1, -1), // White moves towards rank 0
        };

        let mut r = start;
        while r != end {
            attacks.set(Square::new(file, r as u8));
            r += step;
        }

        attacks
    }

    /// Generate pawn attacks from a square
    fn generate_pawn_attacks(&self, from: Square, color: Color) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;
        let file = from.file();
        let rank = from.rank() as i8;

        let new_rank = match color {
            Color::Black => rank + 1, // Black moves towards rank 8
            Color::White => rank - 1, // White moves towards rank 0
        };

        if (0..9).contains(&new_rank) {
            attacks.set(Square::new(file, new_rank as u8));
        }

        attacks
    }

    /// Get king attacks
    #[inline]
    pub fn king_attacks(&self, sq: Square) -> Bitboard {
        self.king_attacks[sq.index()]
    }

    /// Get gold attacks (also for promoted pieces)
    #[inline]
    pub fn gold_attacks(&self, sq: Square, color: Color) -> Bitboard {
        self.gold_attacks[color as usize][sq.index()]
    }

    /// Get silver attacks
    #[inline]
    pub fn silver_attacks(&self, sq: Square, color: Color) -> Bitboard {
        self.silver_attacks[color as usize][sq.index()]
    }

    /// Get knight attacks
    #[inline]
    pub fn knight_attacks(&self, sq: Square, color: Color) -> Bitboard {
        self.knight_attacks[color as usize][sq.index()]
    }

    /// Get lance attacks (need to mask with occupied squares)
    #[inline]
    pub fn lance_attacks(&self, sq: Square, color: Color) -> Bitboard {
        self.lance_attacks[color as usize][sq.index()]
    }

    /// Get pawn attacks
    #[inline]
    pub fn pawn_attacks(&self, sq: Square, color: Color) -> Bitboard {
        self.pawn_attacks[color as usize][sq.index()]
    }

    /// Get sliding piece attacks (Rook/Bishop) using classical approach
    /// Magic Bitboard will be implemented later for optimization
    pub fn sliding_attacks(
        &self,
        sq: Square,
        occupied: Bitboard,
        piece_type: PieceType,
    ) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;

        let directions = match piece_type {
            PieceType::Rook => &Direction::ORTHOGONALS[..],
            PieceType::Bishop => &Direction::DIAGONALS[..],
            _ => return attacks,
        };

        for &dir in directions {
            attacks |= self.ray_attacks(sq, dir, occupied);
        }

        attacks
    }

    /// Get attacks along a ray until blocked
    fn ray_attacks(&self, from: Square, dir: Direction, occupied: Bitboard) -> Bitboard {
        let mut attacks = Bitboard::EMPTY;
        let file = from.file() as i8;
        let rank = from.rank() as i8;

        let (file_delta, rank_delta) = match dir {
            Direction::North => (0, -1),
            Direction::NorthEast => (1, -1),
            Direction::East => (1, 0),
            Direction::SouthEast => (1, 1),
            Direction::South => (0, 1),
            Direction::SouthWest => (-1, 1),
            Direction::West => (-1, 0),
            Direction::NorthWest => (-1, -1),
        };

        let mut f = file + file_delta;
        let mut r = rank + rank_delta;

        while (0..9).contains(&f) && (0..9).contains(&r) {
            let sq = Square::new(f as u8, r as u8);
            attacks.set(sq);

            if occupied.test(sq) {
                break; // Blocked by a piece
            }

            f += file_delta;
            r += rank_delta;
        }

        attacks
    }

    /// Get bitboard of squares between two squares (exclusive)
    pub fn between_bb(&self, sq1: Square, sq2: Square) -> Bitboard {
        let file1 = sq1.file() as i8;
        let rank1 = sq1.rank() as i8;
        let file2 = sq2.file() as i8;
        let rank2 = sq2.rank() as i8;

        let file_diff = file2 - file1;
        let rank_diff = rank2 - rank1;

        // Not on same ray
        if file_diff != 0 && rank_diff != 0 && file_diff.abs() != rank_diff.abs() {
            return Bitboard::EMPTY;
        }

        // Adjacent squares have nothing between
        if file_diff.abs() <= 1 && rank_diff.abs() <= 1 {
            return Bitboard::EMPTY;
        }

        let file_delta = file_diff.signum();
        let rank_delta = rank_diff.signum();

        let mut between = Bitboard::EMPTY;
        let mut f = file1 + file_delta;
        let mut r = rank1 + rank_delta;

        while f != file2 || r != rank2 {
            between.set(Square::new(f as u8, r as u8));
            f += file_delta;
            r += rank_delta;
        }

        between
    }
}

// Global attack tables instance
pub static ATTACK_TABLES: Lazy<AttackTables> = Lazy::new(AttackTables::new);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_king_attacks() {
        // King in center
        let sq = Square::new(4, 4);
        let attacks = ATTACK_TABLES.king_attacks(sq);
        assert_eq!(attacks.count_ones(), 8); // All 8 adjacent squares

        // King in corner
        let sq = Square::new(0, 0);
        let attacks = ATTACK_TABLES.king_attacks(sq);
        assert_eq!(attacks.count_ones(), 3); // Only 3 adjacent squares
    }

    #[test]
    fn test_pawn_attacks() {
        // Black pawn
        let sq = Square::new(4, 4);
        let attacks = ATTACK_TABLES.pawn_attacks(sq, Color::Black);
        assert_eq!(attacks.count_ones(), 1);
        assert!(attacks.test(Square::new(4, 5))); // Black moves towards rank 8

        // White pawn
        let attacks = ATTACK_TABLES.pawn_attacks(sq, Color::White);
        assert_eq!(attacks.count_ones(), 1);
        assert!(attacks.test(Square::new(4, 3))); // White moves towards rank 0
    }

    #[test]
    fn test_knight_attacks() {
        // Black knight in center
        let sq = Square::new(4, 4);
        let attacks = ATTACK_TABLES.knight_attacks(sq, Color::Black);
        assert_eq!(attacks.count_ones(), 2);
        assert!(attacks.test(Square::new(3, 6))); // 2 forward (towards rank 8), 1 left
        assert!(attacks.test(Square::new(5, 6))); // 2 forward (towards rank 8), 1 right

        // Black knight can't move from rank 7 or 8
        let sq = Square::new(4, 7);
        let attacks = ATTACK_TABLES.knight_attacks(sq, Color::Black);
        assert_eq!(attacks.count_ones(), 0);
    }

    #[test]
    fn test_sliding_attacks() {
        // Rook attacks
        let sq = Square::new(4, 4);
        let occupied = Bitboard::EMPTY;
        let attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        assert_eq!(attacks.count_ones(), 8 + 8); // 8 vertical + 8 horizontal - 1 (self)

        // Rook attacks with blocker
        let mut occupied = Bitboard::EMPTY;
        occupied.set(Square::new(4, 2)); // Block upward
        let attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        assert!(attacks.test(Square::new(4, 3)));
        assert!(attacks.test(Square::new(4, 2))); // Can capture blocker
        assert!(!attacks.test(Square::new(4, 1))); // Cannot go past blocker
    }
}
