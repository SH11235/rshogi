//! Bitboard（128bit盤面表現）

use crate::types::Square;

/// Bitboard（128bit、16バイトアラインメント）
///
/// 縦型配置:
/// - p[0]: 1-7筋 (bit 0-62使用、bit 63未使用)
/// - p[1]: 8-9筋 (bit 0-17使用)
#[derive(Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(C, align(16))]
pub struct Bitboard {
    p: [u64; 2],
}

impl Bitboard {
    /// 空のBitboard
    pub const EMPTY: Bitboard = Bitboard { p: [0, 0] };

    /// 全マスが立っているBitboard
    pub const ALL: Bitboard = Bitboard {
        p: [0x7FFF_FFFF_FFFF_FFFF, 0x0003_FFFF],
    };

    /// 内部配列を直接指定して生成
    #[inline]
    pub const fn new(p0: u64, p1: u64) -> Bitboard {
        Bitboard { p: [p0, p1] }
    }

    /// 単一マスのBitboard
    #[inline]
    pub const fn from_square(sq: Square) -> Bitboard {
        let idx = sq.index();
        if idx < 63 {
            Bitboard {
                p: [1u64 << idx, 0],
            }
        } else {
            Bitboard {
                p: [0, 1u64 << (idx - 63)],
            }
        }
    }

    /// 空かどうか
    #[inline]
    pub const fn is_empty(self) -> bool {
        (self.p[0] | self.p[1]) == 0
    }

    /// u128として取得（ハッシュ計算用）
    #[inline]
    pub const fn as_u128(self) -> u128 {
        (self.p[1] as u128) << 64 | self.p[0] as u128
    }

    /// 空でないかどうか
    #[inline]
    pub const fn is_not_empty(self) -> bool {
        !self.is_empty()
    }

    /// ビットが立っている数
    #[inline]
    pub const fn count(self) -> u32 {
        self.p[0].count_ones() + self.p[1].count_ones()
    }

    /// 2つ以上のビットが立っているか
    #[inline]
    pub const fn more_than_one(self) -> bool {
        // p[0]だけで2つ以上、またはp[1]だけで2つ以上、または両方に1つ以上
        if self.p[0] != 0 && (self.p[0] & (self.p[0] - 1)) != 0 {
            return true;
        }
        if self.p[1] != 0 && (self.p[1] & (self.p[1] - 1)) != 0 {
            return true;
        }
        self.p[0] != 0 && self.p[1] != 0
    }

    /// 最下位ビットのSquareを取得して消す
    #[inline]
    pub fn pop(&mut self) -> Square {
        if self.is_empty() {
            debug_assert!(!self.is_empty(), "pop() called on empty Bitboard");
            return Square::SQ_11;
        }

        if self.p[0] != 0 {
            let idx = self.p[0].trailing_zeros();
            self.p[0] &= self.p[0] - 1;
            // SAFETY: idx < 63 で有効なSquare範囲内
            unsafe { Square::from_u8_unchecked(idx as u8) }
        } else {
            let idx = self.p[1].trailing_zeros();
            self.p[1] &= self.p[1] - 1;
            // SAFETY: 63 + idx < 81 で有効なSquare範囲内
            unsafe { Square::from_u8_unchecked(63 + idx as u8) }
        }
    }

    /// 最下位ビットのSquareを取得（消さない）
    ///
    /// 空の場合は不正な値を返す可能性があるため、
    /// 空でないことが保証されている場合のみ使用すること。
    #[inline]
    pub const fn lsb_unchecked(self) -> Square {
        if self.p[0] != 0 {
            // SAFETY: trailing_zeros() < 64、かつp[0]の有効ビットは0-62
            unsafe { Square::from_u8_unchecked(self.p[0].trailing_zeros() as u8) }
        } else {
            // SAFETY: 63 + trailing_zeros() < 81
            unsafe { Square::from_u8_unchecked(63 + self.p[1].trailing_zeros() as u8) }
        }
    }

    /// 最下位ビットのSquareを取得（消さない）
    ///
    /// 空の場合はNoneを返す。
    #[inline]
    pub fn lsb(self) -> Option<Square> {
        if self.is_empty() {
            None
        } else {
            Some(self.lsb_unchecked())
        }
    }

    /// 指定マスにビットが立っているか
    #[inline]
    pub const fn contains(self, sq: Square) -> bool {
        let idx = sq.index();
        if idx < 63 {
            (self.p[0] >> idx) & 1 != 0
        } else {
            (self.p[1] >> (idx - 63)) & 1 != 0
        }
    }

    /// ビットを立てる
    #[inline]
    pub fn set(&mut self, sq: Square) {
        let idx = sq.index();
        if idx < 63 {
            self.p[0] |= 1u64 << idx;
        } else {
            self.p[1] |= 1u64 << (idx - 63);
        }
    }

    /// ビットを消す
    #[inline]
    pub fn clear(&mut self, sq: Square) {
        let idx = sq.index();
        if idx < 63 {
            self.p[0] &= !(1u64 << idx);
        } else {
            self.p[1] &= !(1u64 << (idx - 63));
        }
    }

    /// ビットをXOR（トグル）
    #[inline]
    pub fn toggle(&mut self, sq: Square) {
        let idx = sq.index();
        if idx < 63 {
            self.p[0] ^= 1u64 << idx;
        } else {
            self.p[1] ^= 1u64 << (idx - 63);
        }
    }

    /// p[0]を取得
    #[inline]
    pub const fn p0(self) -> u64 {
        self.p[0]
    }

    /// p[1]を取得
    #[inline]
    pub const fn p1(self) -> u64 {
        self.p[1]
    }

    /// イテレータを返す
    #[inline]
    pub const fn iter(self) -> BitboardIter {
        BitboardIter(self)
    }
}

// ビット演算
impl std::ops::BitAnd for Bitboard {
    type Output = Bitboard;

    #[inline]
    fn bitand(self, rhs: Bitboard) -> Bitboard {
        Bitboard {
            p: [self.p[0] & rhs.p[0], self.p[1] & rhs.p[1]],
        }
    }
}

impl std::ops::BitAndAssign for Bitboard {
    #[inline]
    fn bitand_assign(&mut self, rhs: Bitboard) {
        self.p[0] &= rhs.p[0];
        self.p[1] &= rhs.p[1];
    }
}

impl std::ops::BitOr for Bitboard {
    type Output = Bitboard;

    #[inline]
    fn bitor(self, rhs: Bitboard) -> Bitboard {
        Bitboard {
            p: [self.p[0] | rhs.p[0], self.p[1] | rhs.p[1]],
        }
    }
}

impl std::ops::BitOrAssign for Bitboard {
    #[inline]
    fn bitor_assign(&mut self, rhs: Bitboard) {
        self.p[0] |= rhs.p[0];
        self.p[1] |= rhs.p[1];
    }
}

impl std::ops::BitXor for Bitboard {
    type Output = Bitboard;

    #[inline]
    fn bitxor(self, rhs: Bitboard) -> Bitboard {
        Bitboard {
            p: [self.p[0] ^ rhs.p[0], self.p[1] ^ rhs.p[1]],
        }
    }
}

impl std::ops::BitXorAssign for Bitboard {
    #[inline]
    fn bitxor_assign(&mut self, rhs: Bitboard) {
        self.p[0] ^= rhs.p[0];
        self.p[1] ^= rhs.p[1];
    }
}

impl std::ops::Not for Bitboard {
    type Output = Bitboard;

    #[inline]
    fn not(self) -> Bitboard {
        // 未使用ビットはマスク
        Bitboard {
            p: [!self.p[0] & 0x7FFF_FFFF_FFFF_FFFF, !self.p[1] & 0x0003_FFFF],
        }
    }
}

impl std::fmt::Debug for Bitboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Bitboard {{")?;
        // 盤面形式で表示（1段目から9段目、9筋から1筋）
        for rank in 0..9 {
            write!(f, "  ")?;
            for file in (0..9).rev() {
                let sq_idx = file * 9 + rank;
                let bit = if sq_idx < 63 {
                    (self.p[0] >> sq_idx) & 1
                } else {
                    (self.p[1] >> (sq_idx - 63)) & 1
                };
                write!(f, "{}", if bit == 1 { "●" } else { "・" })?;
            }
            writeln!(f)?;
        }
        write!(f, "}}")
    }
}

/// Bitboardイテレータ
pub struct BitboardIter(Bitboard);

impl Iterator for BitboardIter {
    type Item = Square;

    #[inline]
    fn next(&mut self) -> Option<Square> {
        if self.0.is_empty() {
            None
        } else {
            Some(self.0.pop())
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.0.count() as usize;
        (count, Some(count))
    }
}

impl ExactSizeIterator for BitboardIter {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_bitboard_empty() {
        let bb = Bitboard::EMPTY;
        assert!(bb.is_empty());
        assert!(!bb.is_not_empty());
        assert_eq!(bb.count(), 0);
    }

    #[test]
    fn test_bitboard_all() {
        let bb = Bitboard::ALL;
        assert!(!bb.is_empty());
        assert!(bb.is_not_empty());
        assert_eq!(bb.count(), 81);
    }

    #[test]
    fn test_bitboard_from_square() {
        // 1一 (idx=0)
        let sq11 = Square::new(File::File1, Rank::Rank1);
        let bb = Bitboard::from_square(sq11);
        assert_eq!(bb.count(), 1);
        assert!(bb.contains(sq11));
        assert_eq!(bb.p0(), 1);
        assert_eq!(bb.p1(), 0);

        // 5五 (idx=40)
        let sq55 = Square::new(File::File5, Rank::Rank5);
        let bb = Bitboard::from_square(sq55);
        assert_eq!(bb.count(), 1);
        assert!(bb.contains(sq55));

        // 8一 (idx=63)
        let sq81 = Square::new(File::File8, Rank::Rank1);
        let bb = Bitboard::from_square(sq81);
        assert_eq!(bb.count(), 1);
        assert!(bb.contains(sq81));
        assert_eq!(bb.p0(), 0);
        assert_eq!(bb.p1(), 1);

        // 9九 (idx=80)
        let sq99 = Square::new(File::File9, Rank::Rank9);
        let bb = Bitboard::from_square(sq99);
        assert_eq!(bb.count(), 1);
        assert!(bb.contains(sq99));
    }

    #[test]
    fn test_bitboard_set_clear() {
        let mut bb = Bitboard::EMPTY;
        let sq = Square::new(File::File3, Rank::Rank7);

        bb.set(sq);
        assert!(bb.contains(sq));
        assert_eq!(bb.count(), 1);

        bb.clear(sq);
        assert!(!bb.contains(sq));
        assert_eq!(bb.count(), 0);
    }

    #[test]
    fn test_bitboard_toggle() {
        let mut bb = Bitboard::EMPTY;
        let sq = Square::new(File::File5, Rank::Rank5);

        bb.toggle(sq);
        assert!(bb.contains(sq));

        bb.toggle(sq);
        assert!(!bb.contains(sq));
    }

    #[test]
    fn test_bitboard_lsb_pop() {
        let sq1 = Square::new(File::File2, Rank::Rank3);
        let sq2 = Square::new(File::File7, Rank::Rank8);
        let mut bb = Bitboard::from_square(sq1) | Bitboard::from_square(sq2);

        assert_eq!(bb.count(), 2);

        // sq1の方がインデックスが小さいはず
        let lsb = bb.lsb();
        assert_eq!(lsb, Some(sq1));

        let popped = bb.pop();
        assert_eq!(popped, sq1);
        assert_eq!(bb.count(), 1);

        let popped = bb.pop();
        assert_eq!(popped, sq2);
        assert!(bb.is_empty());
    }

    #[test]
    fn test_bitboard_more_than_one() {
        let sq1 = Square::new(File::File1, Rank::Rank1);
        let sq2 = Square::new(File::File9, Rank::Rank9);

        let bb0 = Bitboard::EMPTY;
        assert!(!bb0.more_than_one());

        let bb1 = Bitboard::from_square(sq1);
        assert!(!bb1.more_than_one());

        let bb2 = Bitboard::from_square(sq1) | Bitboard::from_square(sq2);
        assert!(bb2.more_than_one());
    }

    #[test]
    fn test_bitboard_bitand() {
        let sq1 = Square::new(File::File1, Rank::Rank1);
        let sq2 = Square::new(File::File2, Rank::Rank2);
        let sq3 = Square::new(File::File3, Rank::Rank3);

        let bb1 = Bitboard::from_square(sq1) | Bitboard::from_square(sq2);
        let bb2 = Bitboard::from_square(sq2) | Bitboard::from_square(sq3);

        let bb_and = bb1 & bb2;
        assert_eq!(bb_and.count(), 1);
        assert!(bb_and.contains(sq2));
    }

    #[test]
    fn test_bitboard_bitor() {
        let sq1 = Square::new(File::File1, Rank::Rank1);
        let sq2 = Square::new(File::File2, Rank::Rank2);

        let bb1 = Bitboard::from_square(sq1);
        let bb2 = Bitboard::from_square(sq2);

        let bb_or = bb1 | bb2;
        assert_eq!(bb_or.count(), 2);
        assert!(bb_or.contains(sq1));
        assert!(bb_or.contains(sq2));
    }

    #[test]
    fn test_bitboard_bitxor() {
        let sq1 = Square::new(File::File1, Rank::Rank1);
        let sq2 = Square::new(File::File2, Rank::Rank2);

        let bb1 = Bitboard::from_square(sq1) | Bitboard::from_square(sq2);
        let bb2 = Bitboard::from_square(sq2);

        let bb_xor = bb1 ^ bb2;
        assert_eq!(bb_xor.count(), 1);
        assert!(bb_xor.contains(sq1));
        assert!(!bb_xor.contains(sq2));
    }

    #[test]
    fn test_bitboard_not() {
        let bb = Bitboard::EMPTY;
        let bb_not = !bb;
        assert_eq!(bb_not, Bitboard::ALL);

        let bb_not_not = !bb_not;
        assert_eq!(bb_not_not, Bitboard::EMPTY);
    }

    #[test]
    fn test_bitboard_iter() {
        let sq1 = Square::new(File::File1, Rank::Rank1);
        let sq2 = Square::new(File::File5, Rank::Rank5);
        let sq3 = Square::new(File::File9, Rank::Rank9);

        let bb =
            Bitboard::from_square(sq1) | Bitboard::from_square(sq2) | Bitboard::from_square(sq3);

        let squares: Vec<_> = bb.iter().collect();
        assert_eq!(squares.len(), 3);
        assert!(squares.contains(&sq1));
        assert!(squares.contains(&sq2));
        assert!(squares.contains(&sq3));
    }

    #[test]
    fn test_bitboard_iter_exact_size() {
        let bb = Bitboard::ALL;
        let iter = bb.iter();
        assert_eq!(iter.len(), 81);
    }

    #[test]
    fn test_bitboard_boundary() {
        // p[0]とp[1]の境界（62と63）
        let sq62 = Square::new(File::File7, Rank::Rank9); // 6*9+8 = 62
        let sq63 = Square::new(File::File8, Rank::Rank1); // 7*9+0 = 63

        let bb62 = Bitboard::from_square(sq62);
        assert_eq!(bb62.p0(), 1u64 << 62);
        assert_eq!(bb62.p1(), 0);

        let bb63 = Bitboard::from_square(sq63);
        assert_eq!(bb63.p0(), 0);
        assert_eq!(bb63.p1(), 1);
    }
}
