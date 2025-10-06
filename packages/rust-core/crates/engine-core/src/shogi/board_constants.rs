//! Board-related constants for shogi
//!
//! This module provides compile-time constants for board geometry,
//! bitboard masks, and other board-related values.

use super::board::Bitboard;

/// Number of squares on a shogi board (9x9)
pub const SHOGI_BOARD_SIZE: usize = 81;

/// Board dimensions
pub const BOARD_FILES: usize = 9;
pub const BOARD_RANKS: usize = 9;

/// Calculate linear square index from file and rank
#[inline]
pub const fn square_index(file: usize, rank: usize) -> usize {
    rank * BOARD_FILES + file
}

// ==== Rank Mask Constants ====

/// Rank mask constants for optimization (u128 format for Bitboard)
pub const RANK_1_MASK: u128 = 0x1FF; // 1st rank (9 bits)
pub const RANK_2_MASK: u128 = 0x1FF << 9; // 2nd rank
pub const RANK_3_MASK: u128 = 0x1FF << 18; // 3rd rank
pub const RANK_4_MASK: u128 = 0x1FF << 27; // 4th rank
pub const RANK_5_MASK: u128 = 0x1FF << 36; // 5th rank
pub const RANK_6_MASK: u128 = 0x1FF << 45; // 6th rank
pub const RANK_7_MASK: u128 = 0x1FF << 54; // 7th rank
pub const RANK_8_MASK: u128 = 0x1FF << 63; // 8th rank
pub const RANK_9_MASK: u128 = 0x1FF << 72; // 9th rank

/// Combined rank masks
pub const RANK_1_2_MASK: u128 = 0x3FFFF; // 1st and 2nd ranks (18 bits)
pub const RANK_8_9_MASK: u128 = 0x3FFFF << 63; // 8th and 9th ranks

/// Promotion zone masks
pub const BLACK_PROMOTION_ZONE: u128 = 0x7FFFFFF; // Ranks 1-3 (27 bits)
pub const WHITE_PROMOTION_ZONE: u128 = 0x1ffffffc0000000000000; // Ranks 7-9 (27 bits shifted by 54)

// ==== File Mask Constants ====

/// File mask constants for optimization
/// File 1 is rightmost in shogi notation (internal file 8)
/// File 9 is leftmost in shogi notation (internal file 0)
/// Each file contains 9 squares, spaced 9 bits apart
pub const FILE_9_MASK: u128 = 0x00000000000001008040201008040201; // Internal file 0 = 9筋
pub const FILE_8_MASK: u128 = FILE_9_MASK << 1; // Internal file 1 = 8筋
pub const FILE_7_MASK: u128 = FILE_9_MASK << 2; // Internal file 2 = 7筋
pub const FILE_6_MASK: u128 = FILE_9_MASK << 3; // Internal file 3 = 6筋
pub const FILE_5_MASK: u128 = FILE_9_MASK << 4; // Internal file 4 = 5筋
pub const FILE_4_MASK: u128 = FILE_9_MASK << 5; // Internal file 5 = 4筋
pub const FILE_3_MASK: u128 = FILE_9_MASK << 6; // Internal file 6 = 3筋
pub const FILE_2_MASK: u128 = FILE_9_MASK << 7; // Internal file 7 = 2筋
pub const FILE_1_MASK: u128 = FILE_9_MASK << 8; // Internal file 8 = 1筋

/// File masks as Bitboard constants for convenience
pub const FILE_1_BB: Bitboard = Bitboard(FILE_1_MASK);
pub const FILE_2_BB: Bitboard = Bitboard(FILE_2_MASK);
pub const FILE_3_BB: Bitboard = Bitboard(FILE_3_MASK);
pub const FILE_4_BB: Bitboard = Bitboard(FILE_4_MASK);
pub const FILE_5_BB: Bitboard = Bitboard(FILE_5_MASK);
pub const FILE_6_BB: Bitboard = Bitboard(FILE_6_MASK);
pub const FILE_7_BB: Bitboard = Bitboard(FILE_7_MASK);
pub const FILE_8_BB: Bitboard = Bitboard(FILE_8_MASK);
pub const FILE_9_BB: Bitboard = Bitboard(FILE_9_MASK);

/// Array of file masks for indexed access
pub const FILE_MASKS: [u128; 9] = [
    FILE_9_MASK, // Internal file 0 = 9筋
    FILE_8_MASK, // Internal file 1 = 8筋
    FILE_7_MASK, // Internal file 2 = 7筋
    FILE_6_MASK, // Internal file 3 = 6筋
    FILE_5_MASK, // Internal file 4 = 5筋
    FILE_4_MASK, // Internal file 5 = 4筋
    FILE_3_MASK, // Internal file 6 = 3筋
    FILE_2_MASK, // Internal file 7 = 2筋
    FILE_1_MASK, // Internal file 8 = 1筋
];

/// Array of rank masks for indexed access
pub const RANK_MASKS: [u128; 9] = [
    RANK_1_MASK,
    RANK_2_MASK,
    RANK_3_MASK,
    RANK_4_MASK,
    RANK_5_MASK,
    RANK_6_MASK,
    RANK_7_MASK,
    RANK_8_MASK,
    RANK_9_MASK,
];

/// Get file mask as Bitboard
#[inline]
pub fn file_mask_bb(file: usize) -> Bitboard {
    debug_assert!(file < BOARD_FILES);
    Bitboard(FILE_MASKS[file])
}

/// Get rank mask as Bitboard
#[inline]
pub fn rank_mask_bb(rank: usize) -> Bitboard {
    debug_assert!(rank < BOARD_RANKS);
    Bitboard(RANK_MASKS[rank])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shogi::Square;

    #[test]
    fn test_rank_masks() {
        // Test that each rank mask covers exactly 9 squares
        for (rank, &mask_val) in RANK_MASKS.iter().enumerate() {
            let mask = Bitboard(mask_val);
            assert_eq!(mask.count_ones(), 9);

            // Verify correct squares are set
            for file in 0..BOARD_FILES {
                let sq = Square::new(file as u8, rank as u8);
                assert!(mask.test(sq));
            }
        }
    }

    #[test]
    fn test_file_masks() {
        // Test that each file mask covers exactly 9 squares
        for (file, &mask_val) in FILE_MASKS.iter().enumerate() {
            let mask = Bitboard(mask_val);
            assert_eq!(mask.count_ones(), 9);

            // Verify correct squares are set
            for rank in 0..BOARD_RANKS {
                let sq = Square::new(file as u8, rank as u8);
                assert!(mask.test(sq));
            }
        }
    }

    #[test]
    fn test_promotion_zones() {
        let black_zone = Bitboard(BLACK_PROMOTION_ZONE);
        let white_zone = Bitboard(WHITE_PROMOTION_ZONE);

        // Black promotion zone: ranks 0-2
        assert_eq!(black_zone.count_ones(), 27);
        for rank in 0..3 {
            for file in 0..BOARD_FILES {
                let sq = Square::new(file as u8, rank as u8);
                assert!(black_zone.test(sq));
            }
        }

        // White promotion zone: ranks 6-8
        assert_eq!(white_zone.count_ones(), 27);
        for rank in 6..9 {
            for file in 0..BOARD_FILES {
                let sq = Square::new(file as u8, rank as u8);
                assert!(white_zone.test(sq));
            }
        }
    }
}
