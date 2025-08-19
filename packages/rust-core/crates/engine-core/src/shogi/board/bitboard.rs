//! Bitboard representation for shogi
//!
//! Provides efficient bit-level operations for board state representation

use super::types::Square;

/// Bitboard (81 squares)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bitboard(pub u128); // Use lower 81 bits

impl Bitboard {
    /// Empty bitboard
    pub const EMPTY: Self = Bitboard(0);

    /// All squares set
    pub const ALL: Self = Bitboard((1u128 << 81) - 1);

    /// Create bitboard with single square set
    #[inline]
    pub fn from_square(sq: Square) -> Self {
        debug_assert!(sq.0 < 81);
        Bitboard(1u128 << sq.index())
    }

    /// Set bit at square
    #[inline]
    pub fn set(&mut self, sq: Square) {
        debug_assert!(sq.0 < 81);
        self.0 |= 1u128 << sq.index();
    }

    /// Clear bit at square
    #[inline]
    pub fn clear(&mut self, sq: Square) {
        debug_assert!(sq.0 < 81);
        self.0 &= !(1u128 << sq.index());
    }

    /// Test bit at square
    #[inline]
    pub fn test(&self, sq: Square) -> bool {
        debug_assert!(sq.0 < 81);
        (self.0 >> sq.index()) & 1 != 0
    }

    /// Pop least significant bit
    #[inline]
    pub fn pop_lsb(&mut self) -> Option<Square> {
        if self.0 == 0 {
            return None;
        }
        let lsb = self.0.trailing_zeros() as u8;
        self.0 &= self.0 - 1; // Clear LSB
        Some(Square(lsb))
    }

    /// Get least significant bit without popping
    #[inline]
    pub fn lsb(&self) -> Option<Square> {
        if self.0 == 0 {
            return None;
        }
        let lsb = self.0.trailing_zeros() as u8;
        Some(Square(lsb))
    }

    /// Count set bits
    #[inline]
    pub fn count_ones(&self) -> u32 {
        self.0.count_ones()
    }

    /// Check if empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Bitboard {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Bitboard(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for Bitboard {
    type Output = Self;

    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        Bitboard(self.0 & rhs.0)
    }
}

impl std::ops::BitXor for Bitboard {
    type Output = Self;

    #[inline]
    fn bitxor(self, rhs: Self) -> Self::Output {
        Bitboard(self.0 ^ rhs.0)
    }
}

impl std::ops::Not for Bitboard {
    type Output = Self;

    #[inline]
    fn not(self) -> Self::Output {
        Bitboard(!self.0 & Self::ALL.0)
    }
}

impl std::ops::BitOrAssign for Bitboard {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAndAssign for Bitboard {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl std::ops::BitXorAssign for Bitboard {
    #[inline]
    fn bitxor_assign(&mut self, rhs: Self) {
        self.0 ^= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usi::parse_usi_square;

    #[test]
    fn test_bitboard_operations() {
        let mut bb = Bitboard::EMPTY;
        assert!(bb.is_empty());

        let sq = parse_usi_square("5e").unwrap();
        bb.set(sq);
        assert!(bb.test(sq));
        assert_eq!(bb.count_ones(), 1);

        bb.clear(sq);
        assert!(!bb.test(sq));
        assert!(bb.is_empty());
    }

    #[test]
    fn test_bitboard_pop_lsb() {
        let mut bb = Bitboard::EMPTY;
        bb.set(parse_usi_square("9a").unwrap());
        bb.set(parse_usi_square("5e").unwrap());
        bb.set(parse_usi_square("1i").unwrap());

        assert_eq!(bb.pop_lsb(), Some(parse_usi_square("9a").unwrap()));
        assert_eq!(bb.pop_lsb(), Some(parse_usi_square("5e").unwrap()));
        assert_eq!(bb.pop_lsb(), Some(parse_usi_square("1i").unwrap()));
        assert_eq!(bb.pop_lsb(), None);
    }

    #[test]
    fn test_attacks_file_mask() {
        use crate::shogi::attacks;

        // Test the attacks::file_mask function
        for file in 0..9 {
            let mask = attacks::file_mask(file);

            // Verify that all squares in the file are set
            for rank in 0..9 {
                let sq = Square(file + rank * 9);
                assert!(mask.test(sq), "file {file} rank {rank} should be set");
            }

            // Verify that squares in other files are not set
            for other_file in 0..9 {
                if other_file != file {
                    for rank in 0..9 {
                        let sq = Square(other_file + rank * 9);
                        assert!(!mask.test(sq), "file {other_file} rank {rank} should not be set");
                    }
                }
            }
        }
    }
}
