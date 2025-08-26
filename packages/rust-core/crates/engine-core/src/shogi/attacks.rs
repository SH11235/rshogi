//! Attack tables and piece movement patterns
//!
//! Pre-computed attack patterns for fast move generation

use super::board::{Bitboard, Color, PieceType, Square};
use lazy_static::lazy_static;

/// Direction offsets for piece movements
#[derive(Clone, Copy, Debug)]
enum Direction {
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
struct AttackTables {
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

    /// File masks for quick file operations
    pub file_masks: [Bitboard; 9],

    /// Rank masks for quick rank operations
    pub rank_masks: [Bitboard; 9],

    /// Bitboard of squares between two squares (exclusive)
    /// between_bb[sq1][sq2] gives the squares between sq1 and sq2
    pub between_bb: [[Bitboard; 81]; 81],

    /// Ray attacks from each square in each direction (without blockers)
    /// ray_bb[sq][dir] gives all squares in direction dir from sq
    pub ray_bb: [[Bitboard; 8]; 81], // 8 directions per square

    /// Lance-specific ray attacks for optimization
    /// lance_rays[color][sq] gives all squares a lance can attack from sq
    pub lance_rays: [[Bitboard; 81]; 2], // [color][square]
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
            file_masks: [Bitboard::EMPTY; 9],
            rank_masks: [Bitboard::EMPTY; 9],
            between_bb: [[Bitboard::EMPTY; 81]; 81],
            ray_bb: [[Bitboard::EMPTY; 8]; 81],
            lance_rays: [[Bitboard::EMPTY; 81]; 2],
        };

        // Generate file and rank masks
        for file in 0..9 {
            let mut file_mask = Bitboard::EMPTY;
            for rank in 0..9 {
                file_mask.set(Square::new(file, rank));
            }
            tables.file_masks[file as usize] = file_mask;
        }

        for rank in 0..9 {
            let mut rank_mask = Bitboard::EMPTY;
            for file in 0..9 {
                rank_mask.set(Square::new(file, rank));
            }
            tables.rank_masks[rank as usize] = rank_mask;
        }

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

                // Generate lance rays (full ray without blockers)
                tables.lance_rays[color_idx][sq as usize] =
                    tables.generate_lance_attacks(square, color);
            }

            // Generate ray attacks for all directions
            for (dir_idx, &dir) in Direction::ALL.iter().enumerate() {
                tables.ray_bb[sq as usize][dir_idx] = tables.generate_ray_attacks(square, dir);
            }
        }

        // Generate between bitboards
        for sq1 in 0..81 {
            for sq2 in 0..81 {
                tables.between_bb[sq1][sq2] =
                    tables.generate_between_bb(Square(sq1 as u8), Square(sq2 as u8));
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
                Direction::North,     // Forward (towards rank 0)
                Direction::NorthEast, // Diagonal forward-right
                Direction::East,      // Right
                Direction::West,      // Left
                Direction::NorthWest, // Diagonal forward-left
                Direction::South,     // Backward
            ],
            Color::White => [
                Direction::South,     // Forward (towards rank 8)
                Direction::SouthEast, // Diagonal forward-left
                Direction::East,      // Right
                Direction::West,      // Left
                Direction::SouthWest, // Diagonal forward-right
                Direction::North,     // Backward
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
                Direction::North,     // Forward for Black (towards rank 0)
                Direction::NorthEast, // Diagonal forward-right
                Direction::NorthWest, // Diagonal forward-left
                Direction::SouthEast, // Diagonal backward-right
                Direction::SouthWest, // Diagonal backward-left
            ],
            Color::White => [
                Direction::South,     // Forward for White (towards rank 8)
                Direction::SouthEast, // Diagonal forward-left
                Direction::SouthWest, // Diagonal forward-right
                Direction::NorthEast, // Diagonal backward-left
                Direction::NorthWest, // Diagonal backward-right
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
        let rank_offset = match color {
            Color::Black => -2, // Black (Sente) moves towards rank 0
            Color::White => 2,  // White (Gote) moves towards rank 8
        };

        let new_rank = rank + rank_offset;
        if (0..9).contains(&new_rank) {
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
            Color::Black => (rank - 1, -1, -1), // Black (Sente) moves towards rank 0 (up)
            Color::White => (rank + 1, 9, 1),   // White (Gote) moves towards rank 8 (down)
        };

        // Check if lance can move at all (not already at the edge)
        if (color == Color::Black && rank == 0) || (color == Color::White && rank == 8) {
            return attacks; // Lance at edge cannot move
        }

        let mut r = start;
        while r != end && (0..9).contains(&r) {
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
            Color::Black => rank - 1, // Black (Sente) moves towards rank 0 (up the board)
            Color::White => rank + 1, // White (Gote) moves towards rank 8 (down the board)
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

    /// Get file mask for a given file
    #[inline]
    pub fn file_mask(&self, file: u8) -> Bitboard {
        self.file_masks[file as usize]
    }

    /// Get rank mask for a given rank
    #[inline]
    pub fn rank_mask(&self, rank: u8) -> Bitboard {
        self.rank_masks[rank as usize]
    }

    /// Get bitboard of squares between two squares (exclusive) - uses pre-computed table
    #[inline]
    pub fn between_bb(&self, sq1: Square, sq2: Square) -> Bitboard {
        self.between_bb[sq1.index()][sq2.index()]
    }

    /// Generate ray attacks from a square in a given direction (for initialization)
    fn generate_ray_attacks(&self, from: Square, dir: Direction) -> Bitboard {
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
            attacks.set(Square::new(f as u8, r as u8));
            f += file_delta;
            r += rank_delta;
        }

        attacks
    }

    /// Generate bitboard of squares between two squares (for initialization)
    fn generate_between_bb(&self, sq1: Square, sq2: Square) -> Bitboard {
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
lazy_static! {
    static ref ATTACK_TABLES: AttackTables = {
        #[cfg(debug_assertions)]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static ATTACK_TABLES_INIT_STARTED: AtomicBool = AtomicBool::new(false);
            if ATTACK_TABLES_INIT_STARTED.swap(true, Ordering::SeqCst) {
                panic!("ATTACK_TABLES initialization re-entered! Circular dependency detected.");
            }
            // Debug output removed to prevent I/O deadlock in subprocess context
        }

        // Debug output removed to prevent I/O deadlock in subprocess context
        AttackTables::new()
    };
}

// ============================================================================
// Public API functions to avoid direct ATTACK_TABLES access
// ============================================================================

/// Get king attacks from a square
#[inline]
pub fn king_attacks(sq: Square) -> Bitboard {
    ATTACK_TABLES.king_attacks(sq)
}

/// Get gold attacks from a square
#[inline]
pub fn gold_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.gold_attacks(sq, color)
}

/// Get silver attacks from a square
#[inline]
pub fn silver_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.silver_attacks(sq, color)
}

/// Get knight attacks from a square
#[inline]
pub fn knight_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.knight_attacks(sq, color)
}

/// Get lance attacks from a square
#[inline]
pub fn lance_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.lance_attacks(sq, color)
}

/// Get pawn attacks from a square
#[inline]
pub fn pawn_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.pawn_attacks(sq, color)
}

/// Get sliding piece attacks (Rook/Bishop)
#[inline]
pub fn sliding_attacks(sq: Square, occupied: Bitboard, piece_type: PieceType) -> Bitboard {
    ATTACK_TABLES.sliding_attacks(sq, occupied, piece_type)
}

/// Get file mask for a given file
#[inline]
pub fn file_mask(file: u8) -> Bitboard {
    debug_assert!(file < 9);
    ATTACK_TABLES.file_mask(file)
}

/// Get rank mask for a given rank
#[inline]
pub fn rank_mask(rank: u8) -> Bitboard {
    debug_assert!(rank < 9);
    ATTACK_TABLES.rank_mask(rank)
}

/// Get bitboard of squares between two squares (exclusive)
#[inline]
pub fn between_bb(sq1: Square, sq2: Square) -> Bitboard {
    ATTACK_TABLES.between_bb(sq1, sq2)
}

/// Returns the forward ray squares a lance of `color` could reach if it stood on `sq`.
///
/// NOTE: To find squares that can ATTACK `sq` by a lance of `by_color`,
///       call with the opposite color: `lance_ray_from(sq, by_color.opposite())`.
#[inline]
pub fn lance_ray_from(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.lance_rays[color as usize][sq.index()]
}

#[cfg(test)]
mod tests {
    use crate::usi::parse_usi_square;

    use super::*;

    #[test]
    fn test_king_attacks() {
        // King in center
        let sq = parse_usi_square("5e").unwrap();
        let attacks = king_attacks(sq);
        assert_eq!(attacks.count_ones(), 8); // All 8 adjacent squares

        // King in corner
        let sq = parse_usi_square("9a").unwrap();
        let attacks = king_attacks(sq);
        assert_eq!(attacks.count_ones(), 3); // Only 3 adjacent squares
    }

    #[test]
    fn test_pawn_attacks() {
        // Black pawn (Sente)
        let sq = parse_usi_square("5e").unwrap();
        let attacks = pawn_attacks(sq, Color::Black);
        assert_eq!(attacks.count_ones(), 1);
        assert!(attacks.test(parse_usi_square("5d").unwrap())); // Black (Sente) moves towards rank 0

        // White pawn (Gote)
        let attacks = pawn_attacks(sq, Color::White);
        assert_eq!(attacks.count_ones(), 1);
        assert!(attacks.test(parse_usi_square("5f").unwrap())); // White (Gote) moves towards rank 8
    }

    #[test]
    fn test_knight_attacks() {
        // Black knight in center
        let sq = parse_usi_square("5e").unwrap();
        let attacks = knight_attacks(sq, Color::Black);
        assert_eq!(attacks.count_ones(), 2);
        assert!(attacks.test(parse_usi_square("6c").unwrap())); // 2 forward (towards rank 0), 1 left
        assert!(attacks.test(parse_usi_square("4c").unwrap())); // 2 forward (towards rank 0), 1 right

        // Black knight can't move from rank 0 or 1
        let sq = parse_usi_square("5b").unwrap();
        let attacks = knight_attacks(sq, Color::Black);
        assert_eq!(attacks.count_ones(), 0);

        // White knight in center
        let sq = parse_usi_square("5e").unwrap();
        let attacks = knight_attacks(sq, Color::White);
        assert_eq!(attacks.count_ones(), 2);
        assert!(attacks.test(parse_usi_square("6g").unwrap())); // 2 forward (towards rank 8), 1 left
        assert!(attacks.test(parse_usi_square("4g").unwrap())); // 2 forward (towards rank 8), 1 right

        // White knight can't move from rank 7 or 8
        let sq = parse_usi_square("5h").unwrap();
        let attacks = knight_attacks(sq, Color::White);
        assert_eq!(attacks.count_ones(), 0);
    }

    #[test]
    fn test_sliding_attacks() {
        // Rook attacks
        let sq = parse_usi_square("5e").unwrap();
        let occupied = Bitboard::EMPTY;
        let attacks = sliding_attacks(sq, occupied, PieceType::Rook);
        assert_eq!(attacks.count_ones(), 8 + 8); // 8 vertical + 8 horizontal - 1 (self)

        // Rook attacks with blocker
        let mut occupied = Bitboard::EMPTY;
        occupied.set(parse_usi_square("5c").unwrap()); // Block upward
        let attacks = sliding_attacks(sq, occupied, PieceType::Rook);
        assert!(attacks.test(parse_usi_square("5d").unwrap()));
        assert!(attacks.test(parse_usi_square("5c").unwrap())); // Can capture blocker
        assert!(!attacks.test(parse_usi_square("5b").unwrap())); // Cannot go past blocker
    }

    #[test]
    fn test_lance_ray_equivalence() {
        let sq = parse_usi_square("5e").unwrap();

        // lance_ray_from(sq, color) は「その色の香が sq にいるときのレイ」
        // = precomputed な lance_attacks(sq, color) と一致するはず
        let from_black = lance_ray_from(sq, Color::Black);
        let from_white = lance_ray_from(sq, Color::White);
        assert_eq!(from_black, lance_attacks(sq, Color::Black));
        assert_eq!(from_white, lance_attacks(sq, Color::White));

        // Test the "opposite" pattern used throughout the codebase
        // To find squares that can attack sq, we use opposite color
        let can_attack_from_black = lance_ray_from(sq, Color::White); // White lance ray = where black lances can be to attack sq
        let can_attack_from_white = lance_ray_from(sq, Color::Black); // Black lance ray = where white lances can be to attack sq

        // Verify specific squares
        assert!(can_attack_from_black.test(parse_usi_square("5f").unwrap())); // Black lance at 5f can attack 5e
        assert!(can_attack_from_white.test(parse_usi_square("5d").unwrap())); // White lance at 5d can attack 5e
    }
}
