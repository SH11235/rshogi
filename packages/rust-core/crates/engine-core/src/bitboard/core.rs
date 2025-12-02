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

    /// u64ペアから生成（YaneuraOuのBitboard(p0,p1)相当）
    #[inline]
    pub const fn from_u64_pair(p0: u64, p1: u64) -> Bitboard {
        Bitboard::new(p0, p1)
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

    /// (~self) & rhs を計算
    #[inline]
    pub const fn andnot(self, rhs: Bitboard) -> Bitboard {
        Bitboard {
            p: [
                (!self.p[0] & rhs.p[0]) & 0x7FFF_FFFF_FFFF_FFFF,
                (!self.p[1] & rhs.p[1]) & 0x0003_FFFF,
            ],
        }
    }

    // ==== Qugiyアルゴリズム用メソッド ====

    /// Squareがp[0]とp[1]のどちらに属するかを返す
    ///
    /// # Returns
    /// - `0`: p[0]に属する（1-7筋、index < 63）
    /// - `1`: p[1]に属する（8-9筋、index >= 63）
    #[inline]
    pub const fn part(sq: Square) -> usize {
        if sq.index() < 63 {
            0
        } else {
            1
        }
    }

    /// p[N]を取得（N = 0 or 1）
    ///
    /// # Type Parameters
    /// * `N` - 0 (p[0]) または 1 (p[1])
    #[inline]
    pub const fn extract64<const N: usize>(self) -> u64 {
        self.p[N]
    }

    /// 128bit全体で1減算（Qugiyアルゴリズム用）
    ///
    /// # Algorithm
    /// - p[0] - 1を計算
    /// - p[0]がゼロの場合のみp[1]から桁借り
    ///
    /// # Performance
    /// - SSE4.1: `_mm_cmpeq_epi64` + `_mm_alignr_epi8` + `_mm_add_epi64`
    /// - SSE2: `_mm_sub_epi64` + MSB抽出 + 追加減算
    /// - Scalar: 条件分岐
    #[inline]
    pub fn decrement(self) -> Bitboard {
        #[cfg(all(target_arch = "x86_64", target_feature = "sse4.1"))]
        unsafe {
            use std::arch::x86_64::*;
            let m = std::mem::transmute::<[u64; 2], __m128i>(self.p);
            let t2 = _mm_cmpeq_epi64(m, _mm_setzero_si128());
            let t2 = _mm_alignr_epi8::<8>(t2, _mm_set1_epi64x(-1));
            let t1 = _mm_add_epi64(m, t2);
            let p: [u64; 2] = std::mem::transmute(t1);
            Bitboard { p }
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "sse4.1")
        ))]
        unsafe {
            use std::arch::x86_64::*;
            let m = std::mem::transmute::<[u64; 2], __m128i>(self.p);
            let c = _mm_set_epi64x(0, 1);
            let t1 = _mm_sub_epi64(m, c);
            let t2 = _mm_srli_epi64::<63>(t1);
            let t2 = _mm_slli_si128::<8>(t2);
            let t1 = _mm_sub_epi64(t1, t2);
            let p: [u64; 2] = std::mem::transmute(t1);
            Bitboard { p }
        }

        #[cfg(not(all(target_arch = "x86_64", target_feature = "sse2")))]
        {
            Bitboard::from_u64_pair(
                self.p[0].wrapping_sub(1),
                if self.p[0] == 0 {
                    self.p[1].wrapping_sub(1)
                } else {
                    self.p[1]
                },
            )
        }
    }

    /// バイト順序を反転（Qugiyアルゴリズム用）
    ///
    /// 飛車の右方向と角の右上・右下方向の利きを求める際に使用。
    ///
    /// # Performance
    /// - SSSE3: `_mm_shuffle_epi8`
    /// - Scalar: `swap_bytes` + p[0]/p[1]交換
    #[inline]
    pub fn byte_reverse(self) -> Bitboard {
        #[cfg(all(target_arch = "x86_64", target_feature = "ssse3"))]
        unsafe {
            use std::arch::x86_64::*;
            let m = std::mem::transmute::<[u64; 2], __m128i>(self.p);
            let shuffle = _mm_set_epi8(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15);
            let result = _mm_shuffle_epi8(m, shuffle);
            let p: [u64; 2] = std::mem::transmute(result);
            Bitboard { p }
        }

        #[cfg(not(all(target_arch = "x86_64", target_feature = "ssse3")))]
        {
            Bitboard::from_u64_pair(self.p[1].swap_bytes(), self.p[0].swap_bytes())
        }
    }

    /// SSE2 unpack命令の実装（静的メソッド）
    ///
    /// # Arguments
    /// * `hi_in`, `lo_in` - 入力Bitboard
    ///
    /// # Returns
    /// `(hi_out, lo_out)` where:
    /// - `hi_out.p[0] = lo_in.p[1]`, `hi_out.p[1] = hi_in.p[1]`
    /// - `lo_out.p[0] = lo_in.p[0]`, `lo_out.p[1] = hi_in.p[0]`
    #[inline]
    pub fn unpack(hi_in: Bitboard, lo_in: Bitboard) -> (Bitboard, Bitboard) {
        #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
        unsafe {
            use std::arch::x86_64::*;
            let hi_m = std::mem::transmute::<[u64; 2], __m128i>(hi_in.p);
            let lo_m = std::mem::transmute::<[u64; 2], __m128i>(lo_in.p);
            let hi_out_m = _mm_unpackhi_epi64(lo_m, hi_m);
            let lo_out_m = _mm_unpacklo_epi64(lo_m, hi_m);
            let hi_out_p: [u64; 2] = std::mem::transmute(hi_out_m);
            let lo_out_p: [u64; 2] = std::mem::transmute(lo_out_m);
            (Bitboard { p: hi_out_p }, Bitboard { p: lo_out_p })
        }

        #[cfg(not(all(target_arch = "x86_64", target_feature = "sse2")))]
        {
            let hi_out = Bitboard::from_u64_pair(lo_in.p[1], hi_in.p[1]);
            let lo_out = Bitboard::from_u64_pair(lo_in.p[0], hi_in.p[0]);
            (hi_out, lo_out)
        }
    }

    /// 2組のBitboardペアで128bit減算（静的メソッド）
    ///
    /// hi_in, lo_inをそれぞれ128bit整数とみなして1減算。
    ///
    /// # Algorithm
    /// - `lo_out = lo_in - 1`
    /// - `hi_out = hi_in + (lo_in == 0 ? -1 : 0)`（桁借り）
    #[inline]
    pub fn decrement_pair(hi_in: Bitboard, lo_in: Bitboard) -> (Bitboard, Bitboard) {
        #[cfg(all(target_arch = "x86_64", target_feature = "sse4.1"))]
        unsafe {
            use std::arch::x86_64::*;
            let hi_m = std::mem::transmute::<[u64; 2], __m128i>(hi_in.p);
            let lo_m = std::mem::transmute::<[u64; 2], __m128i>(lo_in.p);
            let hi_out_m = _mm_add_epi64(hi_m, _mm_cmpeq_epi64(lo_m, _mm_setzero_si128()));
            let lo_out_m = _mm_add_epi64(lo_m, _mm_set1_epi64x(-1));
            let hi_out_p: [u64; 2] = std::mem::transmute(hi_out_m);
            let lo_out_p: [u64; 2] = std::mem::transmute(lo_out_m);
            (Bitboard { p: hi_out_p }, Bitboard { p: lo_out_p })
        }

        #[cfg(not(all(target_arch = "x86_64", target_feature = "sse4.1")))]
        {
            let hi_out_p0 = hi_in.p[0].wrapping_add(if lo_in.p[0] == 0 { u64::MAX } else { 0 });
            let hi_out_p1 = hi_in.p[1].wrapping_add(if lo_in.p[1] == 0 { u64::MAX } else { 0 });
            let lo_out_p0 = lo_in.p[0].wrapping_sub(1);
            let lo_out_p1 = lo_in.p[1].wrapping_sub(1);
            (
                Bitboard::from_u64_pair(hi_out_p0, hi_out_p1),
                Bitboard::from_u64_pair(lo_out_p0, lo_out_p1),
            )
        }
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
    fn test_bitboard_from_u64_pair() {
        let bb = Bitboard::from_u64_pair(0x1234, 0x5678);
        assert_eq!(bb.p0(), 0x1234);
        assert_eq!(bb.p1(), 0x5678);
    }

    #[test]
    fn test_bitboard_andnot() {
        let sq1 = Square::new(File::File1, Rank::Rank1);
        let sq2 = Square::new(File::File2, Rank::Rank2);
        let sq3 = Square::new(File::File3, Rank::Rank3);

        let a = Bitboard::from_square(sq1) | Bitboard::from_square(sq2);
        let b = Bitboard::from_square(sq2) | Bitboard::from_square(sq3);

        let expected = (!a) & b;
        let result = a.andnot(b);

        assert_eq!(result, expected);
        assert!(result.contains(sq3));
        assert!(!result.contains(sq2));
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

    // ==== Qugiyアルゴリズム用メソッドのテスト ====

    #[test]
    fn test_from_u64_pair() {
        let bb = Bitboard::from_u64_pair(0x1234567890ABCDEF, 0xFEDCBA09);
        assert_eq!(bb.extract64::<0>(), 0x1234567890ABCDEF);
        assert_eq!(bb.extract64::<1>(), 0xFEDCBA09);
    }

    #[test]
    fn test_part() {
        let sq1 = Square::new(File::File1, Rank::Rank1); // index = 0
        let sq62 = Square::new(File::File7, Rank::Rank9); // index = 62
        let sq63 = Square::new(File::File8, Rank::Rank1); // index = 63
        let sq80 = Square::new(File::File9, Rank::Rank9); // index = 80

        assert_eq!(Bitboard::part(sq1), 0);
        assert_eq!(Bitboard::part(sq62), 0);
        assert_eq!(Bitboard::part(sq63), 1);
        assert_eq!(Bitboard::part(sq80), 1);
    }

    #[test]
    fn test_decrement_basic() {
        // p[0]のみの減算
        let bb = Bitboard::from_u64_pair(5, 0);
        let result = bb.decrement();
        assert_eq!(result.extract64::<0>(), 4);
        assert_eq!(result.extract64::<1>(), 0);
    }

    #[test]
    fn test_decrement_with_borrow() {
        // p[0] = 0の場合、p[1]から桁借り
        let bb = Bitboard::from_u64_pair(0, 1);
        let result = bb.decrement();
        assert_eq!(result.extract64::<0>(), u64::MAX);
        assert_eq!(result.extract64::<1>(), 0);
    }

    #[test]
    fn test_byte_reverse() {
        let bb = Bitboard::from_u64_pair(0x0102030405060708, 0x090A0B0C0D0E0F10);
        let reversed = bb.byte_reverse();
        // バイト反転：p[0]とp[1]を交換し、各u64のバイト順を反転
        assert_eq!(reversed.extract64::<0>(), 0x100F0E0D0C0B0A09);
        assert_eq!(reversed.extract64::<1>(), 0x0807060504030201);
    }

    #[test]
    fn test_unpack() {
        let hi = Bitboard::from_u64_pair(0xAAAAAAAA, 0xBBBBBBBB);
        let lo = Bitboard::from_u64_pair(0xCCCCCCCC, 0xDDDDDDDD);

        let (hi_out, lo_out) = Bitboard::unpack(hi, lo);

        // hi_out.p[0] = lo.p[1], hi_out.p[1] = hi.p[1]
        assert_eq!(hi_out.extract64::<0>(), 0xDDDDDDDD);
        assert_eq!(hi_out.extract64::<1>(), 0xBBBBBBBB);

        // lo_out.p[0] = lo.p[0], lo_out.p[1] = hi.p[0]
        assert_eq!(lo_out.extract64::<0>(), 0xCCCCCCCC);
        assert_eq!(lo_out.extract64::<1>(), 0xAAAAAAAA);
    }

    #[test]
    fn test_decrement_pair() {
        let hi = Bitboard::from_u64_pair(10, 20);
        let lo = Bitboard::from_u64_pair(5, 3);

        let (hi_out, lo_out) = Bitboard::decrement_pair(hi, lo);

        // lo - 1
        assert_eq!(lo_out.extract64::<0>(), 4);
        assert_eq!(lo_out.extract64::<1>(), 2);

        // hiは変化なし（lo != 0）
        assert_eq!(hi_out.extract64::<0>(), 10);
        assert_eq!(hi_out.extract64::<1>(), 20);
    }

    #[test]
    fn test_decrement_pair_with_borrow() {
        let hi = Bitboard::from_u64_pair(10, 20);
        let lo = Bitboard::from_u64_pair(0, 0);

        let (hi_out, lo_out) = Bitboard::decrement_pair(hi, lo);

        // lo - 1 (underflow)
        assert_eq!(lo_out.extract64::<0>(), u64::MAX);
        assert_eq!(lo_out.extract64::<1>(), u64::MAX);

        // hi - 1 (両方とも桁借り)
        assert_eq!(hi_out.extract64::<0>(), 9);
        assert_eq!(hi_out.extract64::<1>(), 19);
    }
}
