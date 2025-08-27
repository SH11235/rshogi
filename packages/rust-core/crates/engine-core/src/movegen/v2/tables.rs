use crate::shogi::{Bitboard, Color, Square};
use once_cell::sync::Lazy;

// Re-export HAND_ORDER for convenience
pub use crate::shogi::board::HAND_ORDER;

/// Attack tables for various piece types
pub struct AttackTables {
    /// King attack patterns indexed by square
    pub king_attacks: [Bitboard; 81],
    /// Gold attack patterns indexed by [color][square]
    pub gold_attacks: [[Bitboard; 81]; 2],
    /// Silver attack patterns indexed by [color][square]
    pub silver_attacks: [[Bitboard; 81]; 2],
    /// Knight attack patterns indexed by [color][square]
    pub knight_attacks: [[Bitboard; 81]; 2],
    /// Pawn attack patterns indexed by [color][square]
    pub pawn_attacks: [[Bitboard; 81]; 2],
    /// Lance attack patterns indexed by [color][square]
    pub lance_attacks: [[Bitboard; 81]; 2],
}

impl AttackTables {
    /// Generate all attack tables
    pub fn generate() -> Self {
        let mut tables = Self {
            king_attacks: [Bitboard::EMPTY; 81],
            gold_attacks: [[Bitboard::EMPTY; 81]; 2],
            silver_attacks: [[Bitboard::EMPTY; 81]; 2],
            knight_attacks: [[Bitboard::EMPTY; 81]; 2],
            pawn_attacks: [[Bitboard::EMPTY; 81]; 2],
            lance_attacks: [[Bitboard::EMPTY; 81]; 2],
        };

        // Generate attack patterns for each square
        for i in 0..81 {
            let sq = Square::new((i % 9) as u8, (i / 9) as u8);
            let index = sq.index();

            // King attacks (8 directions)
            tables.king_attacks[index] = generate_king_attacks(sq);

            // Piece attacks for both colors
            for &color in &[Color::Black, Color::White] {
                let color_idx = color as usize;
                tables.gold_attacks[color_idx][index] = generate_gold_attacks(sq, color);
                tables.silver_attacks[color_idx][index] = generate_silver_attacks(sq, color);
                tables.knight_attacks[color_idx][index] = generate_knight_attacks(sq, color);
                tables.pawn_attacks[color_idx][index] = generate_pawn_attacks(sq, color);
                tables.lance_attacks[color_idx][index] = generate_lance_attacks(sq, color);
            }
        }

        tables
    }
}

/// Zobrist keys for hashing
pub struct ZobristKeys {
    /// Keys for pieces indexed by [piece_type][color][square]
    pub pieces: [[[u64; 81]; 2]; 14],
    /// Key for side to move
    pub side_to_move: u64,
    /// Keys for pieces in hand indexed by [piece_type][color][count]
    pub hands: [[[u64; 19]; 2]; 7],
}

impl ZobristKeys {
    /// Generate Zobrist keys using a deterministic random number generator
    pub fn generate() -> Self {
        use rand::{Rng, SeedableRng};
        use rand_xoshiro::Xoroshiro128Plus;

        // Use a fixed seed for reproducibility
        let mut rng = Xoroshiro128Plus::seed_from_u64(0x1234567890abcdef);

        let mut keys = Self {
            pieces: [[[0; 81]; 2]; 14],
            side_to_move: rng.random(),
            hands: [[[0; 19]; 2]; 7],
        };

        // Generate piece keys
        for piece_type in 0..14 {
            for color in 0..2 {
                for square in 0..81 {
                    keys.pieces[piece_type][color][square] = rng.random();
                }
            }
        }

        // Generate hand keys
        for piece_type in 0..7 {
            for color in 0..2 {
                for count in 0..19 {
                    keys.hands[piece_type][color][count] = rng.random();
                }
            }
        }

        keys
    }
}

// Global static instances using once_cell
pub static ATTACK_TABLES: Lazy<AttackTables> = Lazy::new(AttackTables::generate);
pub static ZOBRIST_KEYS: Lazy<ZobristKeys> = Lazy::new(ZobristKeys::generate);

// Helper functions to generate attack patterns

fn generate_king_attacks(sq: Square) -> Bitboard {
    let mut attacks = Bitboard::EMPTY;
    let (file, rank) = (sq.file(), sq.rank());

    // All 8 directions
    let offsets = [
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];

    for (df, dr) in offsets {
        let new_file = file as i8 + df;
        let new_rank = rank as i8 + dr;

        if (0..=8).contains(&new_file) && (0..=8).contains(&new_rank) {
            let target = Square::new(new_file as u8, new_rank as u8);
            attacks |= Bitboard::from_square(target);
        }
    }

    attacks
}

fn generate_gold_attacks(sq: Square, color: Color) -> Bitboard {
    let mut attacks = Bitboard::EMPTY;
    let (file, rank) = (sq.file(), sq.rank());

    // Gold moves like king except backward diagonals
    let offsets = match color {
        Color::Black => [
            (-1, -1),
            (0, -1),
            (1, -1), // Forward
            (-1, 0),
            (1, 0), // Sides
            (0, 1), // Back
        ],
        Color::White => [
            (-1, 1),
            (0, 1),
            (1, 1), // Forward (reversed)
            (-1, 0),
            (1, 0),  // Sides
            (0, -1), // Back
        ],
    };

    for (df, dr) in offsets {
        let new_file = file as i8 + df;
        let new_rank = rank as i8 + dr;

        if (0..=8).contains(&new_file) && (0..=8).contains(&new_rank) {
            let target = Square::new(new_file as u8, new_rank as u8);
            attacks |= Bitboard::from_square(target);
        }
    }

    attacks
}

fn generate_silver_attacks(sq: Square, color: Color) -> Bitboard {
    let mut attacks = Bitboard::EMPTY;
    let (file, rank) = (sq.file(), sq.rank());

    // Silver moves forward and diagonally
    let offsets = match color {
        Color::Black => [
            (-1, -1),
            (0, -1),
            (1, -1), // Forward
            (-1, 1),
            (1, 1), // Backward diagonals
        ],
        Color::White => [
            (-1, 1),
            (0, 1),
            (1, 1), // Forward (reversed)
            (-1, -1),
            (1, -1), // Backward diagonals
        ],
    };

    for (df, dr) in offsets {
        let new_file = file as i8 + df;
        let new_rank = rank as i8 + dr;

        if (0..=8).contains(&new_file) && (0..=8).contains(&new_rank) {
            let target = Square::new(new_file as u8, new_rank as u8);
            attacks |= Bitboard::from_square(target);
        }
    }

    attacks
}

fn generate_knight_attacks(sq: Square, color: Color) -> Bitboard {
    let mut attacks = Bitboard::EMPTY;
    let (file, rank) = (sq.file(), sq.rank());

    // Knight jumps in L-shape
    let offsets = match color {
        Color::Black => [(-1, -2), (1, -2)], // Two squares forward, one to the side
        Color::White => [(-1, 2), (1, 2)],   // Reversed for white
    };

    for (df, dr) in offsets {
        let new_file = file as i8 + df;
        let new_rank = rank as i8 + dr;

        if (0..=8).contains(&new_file) && (0..=8).contains(&new_rank) {
            let target = Square::new(new_file as u8, new_rank as u8);
            attacks |= Bitboard::from_square(target);
        }
    }

    attacks
}

fn generate_pawn_attacks(sq: Square, color: Color) -> Bitboard {
    let (file, rank) = (sq.file(), sq.rank());

    let new_rank = match color {
        Color::Black => rank as i8 - 1,
        Color::White => rank as i8 + 1,
    };

    if (0..=8).contains(&new_rank) {
        let target = Square::new(file, new_rank as u8);
        Bitboard::from_square(target)
    } else {
        Bitboard::EMPTY
    }
}

fn generate_lance_attacks(sq: Square, color: Color) -> Bitboard {
    let mut attacks = Bitboard::EMPTY;
    let file = sq.file();
    let rank = sq.rank();

    // Lance attacks all squares in front
    match color {
        Color::Black => {
            for r in 1..rank {
                let target = Square::new(file, r);
                attacks |= Bitboard::from_square(target);
            }
        }
        Color::White => {
            for r in (rank + 1)..=8 {
                let target = Square::new(file, r);
                attacks |= Bitboard::from_square(target);
            }
        }
    }

    attacks
}

// Public API functions for accessing attack patterns
#[inline]
pub fn king_attacks(sq: Square) -> Bitboard {
    ATTACK_TABLES.king_attacks[sq.index()]
}

// Get the first square from a bitboard (for iterating)
#[inline]
pub fn to_square(bb: Bitboard) -> Option<Square> {
    bb.lsb()
}

#[inline]
pub fn gold_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.gold_attacks[color as usize][sq.index()]
}

#[inline]
pub fn silver_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.silver_attacks[color as usize][sq.index()]
}

#[inline]
pub fn knight_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.knight_attacks[color as usize][sq.index()]
}

#[inline]
pub fn pawn_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.pawn_attacks[color as usize][sq.index()]
}

#[inline]
pub fn lance_attacks(sq: Square, color: Color) -> Bitboard {
    ATTACK_TABLES.lance_attacks[color as usize][sq.index()]
}
