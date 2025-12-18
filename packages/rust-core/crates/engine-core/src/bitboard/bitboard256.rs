//! Bitboard256（256bit、角の4方向同時計算用）

use super::Bitboard;

/// Bitboard256（256bit、32バイトアライン）
///
/// 角の利き計算で4方向（左上・左下・右上・右下）を同時に計算するために使用。
/// 内部的には2つのBitboardまたは4つのu64で表現される。
///
/// # Memory Layout
/// - AVX2: `__m256i`（256bit SIMD）
/// - Scalar: `[u64; 4]`（4 × 64bit）
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(C, align(32))]
pub struct Bitboard256 {
    p: [u64; 4],
}

impl Bitboard256 {
    /// ゼロで初期化
    pub const ZERO: Bitboard256 = Bitboard256 { p: [0, 0, 0, 0] };

    /// 4つのu64から生成
    #[inline]
    pub const fn from_u64_array(p: [u64; 4]) -> Bitboard256 {
        Bitboard256 { p }
    }

    /// 単一のBitboardを複製して256bitに拡張
    ///
    /// 結果: `[bb.p[0], bb.p[1], bb.p[0], bb.p[1]]`
    #[inline]
    pub fn new(bb: Bitboard) -> Bitboard256 {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            // _mm_set_epi64xでアライメント問題を回避（引数順序: 上位, 下位）
            let bb_m = _mm_set_epi64x(bb.extract64::<1>() as i64, bb.extract64::<0>() as i64);
            let result_m = _mm256_broadcastsi128_si256(bb_m);
            let result_p: [u64; 4] = std::mem::transmute(result_m);
            Bitboard256 { p: result_p }
        }

        #[cfg(not(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2")))]
        {
            Bitboard256 {
                p: [
                    bb.extract64::<0>(),
                    bb.extract64::<1>(),
                    bb.extract64::<0>(),
                    bb.extract64::<1>(),
                ],
            }
        }
    }

    /// 2つのBitboardから生成
    ///
    /// # Arguments
    /// * `bb0` - 下位128bit
    /// * `bb1` - 上位128bit
    ///
    /// 結果: `[bb0.p[0], bb0.p[1], bb1.p[0], bb1.p[1]]`
    #[inline]
    pub fn from_bitboards(bb0: Bitboard, bb1: Bitboard) -> Bitboard256 {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            // _mm_set_epi64xでアライメント問題を回避（引数順序: 上位, 下位）
            let bb0_m = _mm_set_epi64x(bb0.extract64::<1>() as i64, bb0.extract64::<0>() as i64);
            let bb1_m = _mm_set_epi64x(bb1.extract64::<1>() as i64, bb1.extract64::<0>() as i64);
            let result_m = _mm256_castsi128_si256(bb0_m);
            let result_m = _mm256_inserti128_si256::<1>(result_m, bb1_m);
            let result_p: [u64; 4] = std::mem::transmute(result_m);
            Bitboard256 { p: result_p }
        }

        #[cfg(not(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2")))]
        {
            Bitboard256 {
                p: [
                    bb0.extract64::<0>(),
                    bb0.extract64::<1>(),
                    bb1.extract64::<0>(),
                    bb1.extract64::<1>(),
                ],
            }
        }
    }

    /// 2つのBitboardに分割
    ///
    /// # Returns
    /// `(下位128bit, 上位128bit)`
    #[inline]
    pub fn to_bitboards(self) -> (Bitboard, Bitboard) {
        (
            Bitboard::from_u64_pair(self.p[0], self.p[1]),
            Bitboard::from_u64_pair(self.p[2], self.p[3]),
        )
    }

    /// 単一のBitboardに統合（OR演算）
    ///
    /// 角の利き計算の最終段階で使用。
    #[inline]
    pub fn merge(self) -> Bitboard {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            // SAFETY: Bitboard256は32バイトアライン、[u64; 4]と__m256iは同一メモリレイアウト
            let m = std::mem::transmute::<[u64; 4], __m256i>(self.p);
            // 上位128bitを抽出
            let hi = _mm256_extracti128_si256::<1>(m);
            // 下位128bitを取得
            let lo = _mm256_castsi256_si128(m);
            // OR演算
            let result_m = _mm_or_si128(lo, hi);
            let result_p: [u64; 2] = std::mem::transmute(result_m);
            Bitboard::from_u64_pair(result_p[0], result_p[1])
        }

        #[cfg(not(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2")))]
        {
            Bitboard::from_u64_pair(self.p[0] | self.p[2], self.p[1] | self.p[3])
        }
    }

    /// バイト順序を反転
    ///
    /// 2つのBitboardを個別に反転。
    #[inline]
    pub fn byte_reverse(self) -> Bitboard256 {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            // SAFETY: Bitboard256は32バイトアライン、[u64; 4]と__m256iは同一メモリレイアウト
            let m = std::mem::transmute::<[u64; 4], __m256i>(self.p);
            // 各128bitレーン内でバイト順を反転
            let shuffle = _mm256_set_epi8(
                0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9,
                10, 11, 12, 13, 14, 15,
            );
            let result_m = _mm256_shuffle_epi8(m, shuffle);
            let result_p: [u64; 4] = std::mem::transmute(result_m);
            Bitboard256 { p: result_p }
        }

        #[cfg(not(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2")))]
        {
            let bb0 = Bitboard::from_u64_pair(self.p[0], self.p[1]);
            let bb1 = Bitboard::from_u64_pair(self.p[2], self.p[3]);
            Bitboard256::from_bitboards(bb0.byte_reverse(), bb1.byte_reverse())
        }
    }

    /// 2組のBitboard256ペアで256bit減算（静的メソッド）
    ///
    /// # Algorithm
    /// - `lo_out = lo_in - 1`（4つのu64すべて）
    /// - `hi_out = hi_in + (lo_in == 0 ? -1 : 0)`（各u64ごとに桁借り判定）
    ///
    /// # Performance
    /// - AVX2: `_mm256_add_epi64` + `_mm256_cmpeq_epi64`
    /// - Scalar: 各u64を個別に処理
    #[inline]
    pub fn decrement_pair(hi_in: Bitboard256, lo_in: Bitboard256) -> (Bitboard256, Bitboard256) {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            let hi_m = std::mem::transmute::<[u64; 4], __m256i>(hi_in.p);
            let lo_m = std::mem::transmute::<[u64; 4], __m256i>(lo_in.p);
            let hi_out_m = _mm256_add_epi64(hi_m, _mm256_cmpeq_epi64(lo_m, _mm256_setzero_si256()));
            let lo_out_m = _mm256_add_epi64(lo_m, _mm256_set1_epi64x(-1));
            let hi_out_p: [u64; 4] = std::mem::transmute(hi_out_m);
            let lo_out_p: [u64; 4] = std::mem::transmute(lo_out_m);
            (Bitboard256 { p: hi_out_p }, Bitboard256 { p: lo_out_p })
        }

        #[cfg(not(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2")))]
        {
            let mut hi_out_p = [0u64; 4];
            let mut lo_out_p = [0u64; 4];
            for i in 0..4 {
                hi_out_p[i] = hi_in.p[i].wrapping_add(if lo_in.p[i] == 0 { u64::MAX } else { 0 });
                lo_out_p[i] = lo_in.p[i].wrapping_sub(1);
            }
            (Bitboard256 { p: hi_out_p }, Bitboard256 { p: lo_out_p })
        }
    }

    /// 256bit unpack命令の実装（静的メソッド）
    ///
    /// YaneuraOuのBitboard256::unpackと同等の処理。
    ///
    /// # Algorithm
    /// SSE2のunpackを2回適用：
    /// - `hi_out = unpack_hi(lo_in, hi_in)`
    /// - `lo_out = unpack_lo(lo_in, hi_in)`
    ///
    /// # Performance
    /// - AVX2: `_mm256_unpackhi_epi64` + `_mm256_unpacklo_epi64`
    /// - SSE2: 2組の`_mm_unpackhi_epi64` + `_mm_unpacklo_epi64`
    /// - Scalar: 手動シャッフル
    #[inline]
    pub fn unpack(hi_in: Bitboard256, lo_in: Bitboard256) -> (Bitboard256, Bitboard256) {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            let hi_m = std::mem::transmute::<[u64; 4], __m256i>(hi_in.p);
            let lo_m = std::mem::transmute::<[u64; 4], __m256i>(lo_in.p);
            let hi_out_m = _mm256_unpackhi_epi64(lo_m, hi_m);
            let lo_out_m = _mm256_unpacklo_epi64(lo_m, hi_m);
            let hi_out_p: [u64; 4] = std::mem::transmute(hi_out_m);
            let lo_out_p: [u64; 4] = std::mem::transmute(lo_out_m);
            (Bitboard256 { p: hi_out_p }, Bitboard256 { p: lo_out_p })
        }

        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(all(feature = "simd_avx2", target_feature = "avx2"))
        ))]
        unsafe {
            use std::arch::x86_64::*;

            // 下位128bit
            let hi_lo = _mm_set_epi64x(hi_in.p[1] as i64, hi_in.p[0] as i64);
            let lo_lo = _mm_set_epi64x(lo_in.p[1] as i64, lo_in.p[0] as i64);
            let hi_out_lo = _mm_unpackhi_epi64(lo_lo, hi_lo);
            let lo_out_lo = _mm_unpacklo_epi64(lo_lo, hi_lo);

            // 上位128bit
            let hi_hi = _mm_set_epi64x(hi_in.p[3] as i64, hi_in.p[2] as i64);
            let lo_hi = _mm_set_epi64x(lo_in.p[3] as i64, lo_in.p[2] as i64);
            let hi_out_hi = _mm_unpackhi_epi64(lo_hi, hi_hi);
            let lo_out_hi = _mm_unpacklo_epi64(lo_hi, hi_hi);

            // 結果を配列に変換
            let hi_out_lo_arr: [u64; 2] = std::mem::transmute(hi_out_lo);
            let hi_out_hi_arr: [u64; 2] = std::mem::transmute(hi_out_hi);
            let lo_out_lo_arr: [u64; 2] = std::mem::transmute(lo_out_lo);
            let lo_out_hi_arr: [u64; 2] = std::mem::transmute(lo_out_hi);

            (
                Bitboard256 {
                    p: [
                        hi_out_lo_arr[0],
                        hi_out_lo_arr[1],
                        hi_out_hi_arr[0],
                        hi_out_hi_arr[1],
                    ],
                },
                Bitboard256 {
                    p: [
                        lo_out_lo_arr[0],
                        lo_out_lo_arr[1],
                        lo_out_hi_arr[0],
                        lo_out_hi_arr[1],
                    ],
                },
            )
        }

        #[cfg(not(all(target_arch = "x86_64", target_feature = "sse2")))]
        {
            // スカラー版
            // unpackhi: lo[1,3], hi[1,3]
            // unpacklo: lo[0,2], hi[0,2]
            let hi_out = Bitboard256 {
                p: [lo_in.p[1], hi_in.p[1], lo_in.p[3], hi_in.p[3]],
            };
            let lo_out = Bitboard256 {
                p: [lo_in.p[0], hi_in.p[0], lo_in.p[2], hi_in.p[2]],
            };
            (hi_out, lo_out)
        }
    }
}

// ビット演算
impl std::ops::BitAnd for Bitboard256 {
    type Output = Bitboard256;

    #[inline]
    fn bitand(self, rhs: Bitboard256) -> Bitboard256 {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            // SAFETY: Bitboard256は32バイトアライン、[u64; 4]と__m256iは同一メモリレイアウト
            let lhs_m = std::mem::transmute::<[u64; 4], __m256i>(self.p);
            let rhs_m = std::mem::transmute::<[u64; 4], __m256i>(rhs.p);
            let result_m = _mm256_and_si256(lhs_m, rhs_m);
            let result_p: [u64; 4] = std::mem::transmute(result_m);
            Bitboard256 { p: result_p }
        }

        #[cfg(not(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2")))]
        {
            Bitboard256 {
                p: [
                    self.p[0] & rhs.p[0],
                    self.p[1] & rhs.p[1],
                    self.p[2] & rhs.p[2],
                    self.p[3] & rhs.p[3],
                ],
            }
        }
    }
}

impl std::ops::BitOr for Bitboard256 {
    type Output = Bitboard256;

    #[inline]
    fn bitor(self, rhs: Bitboard256) -> Bitboard256 {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            // SAFETY: Bitboard256は32バイトアライン、[u64; 4]と__m256iは同一メモリレイアウト
            let lhs_m = std::mem::transmute::<[u64; 4], __m256i>(self.p);
            let rhs_m = std::mem::transmute::<[u64; 4], __m256i>(rhs.p);
            let result_m = _mm256_or_si256(lhs_m, rhs_m);
            let result_p: [u64; 4] = std::mem::transmute(result_m);
            Bitboard256 { p: result_p }
        }

        #[cfg(not(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2")))]
        {
            Bitboard256 {
                p: [
                    self.p[0] | rhs.p[0],
                    self.p[1] | rhs.p[1],
                    self.p[2] | rhs.p[2],
                    self.p[3] | rhs.p[3],
                ],
            }
        }
    }
}

impl std::ops::BitXor for Bitboard256 {
    type Output = Bitboard256;

    #[inline]
    fn bitxor(self, rhs: Bitboard256) -> Bitboard256 {
        #[cfg(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2"))]
        unsafe {
            use std::arch::x86_64::*;
            // SAFETY: Bitboard256は32バイトアライン、[u64; 4]と__m256iは同一メモリレイアウト
            let lhs_m = std::mem::transmute::<[u64; 4], __m256i>(self.p);
            let rhs_m = std::mem::transmute::<[u64; 4], __m256i>(rhs.p);
            let result_m = _mm256_xor_si256(lhs_m, rhs_m);
            let result_p: [u64; 4] = std::mem::transmute(result_m);
            Bitboard256 { p: result_p }
        }

        #[cfg(not(all(feature = "simd_avx2", target_arch = "x86_64", target_feature = "avx2")))]
        {
            Bitboard256 {
                p: [
                    self.p[0] ^ rhs.p[0],
                    self.p[1] ^ rhs.p[1],
                    self.p[2] ^ rhs.p[2],
                    self.p[3] ^ rhs.p[3],
                ],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitboard256_new() {
        let bb = Bitboard::from_u64_pair(0x1234, 0x5678);
        let bb256 = Bitboard256::new(bb);
        assert_eq!(bb256.p[0], 0x1234);
        assert_eq!(bb256.p[1], 0x5678);
        assert_eq!(bb256.p[2], 0x1234);
        assert_eq!(bb256.p[3], 0x5678);
    }

    #[test]
    fn test_bitboard256_from_bitboards() {
        let bb0 = Bitboard::from_u64_pair(0xAAAA, 0xBBBB);
        let bb1 = Bitboard::from_u64_pair(0xCCCC, 0xDDDD);
        let bb256 = Bitboard256::from_bitboards(bb0, bb1);
        assert_eq!(bb256.p[0], 0xAAAA);
        assert_eq!(bb256.p[1], 0xBBBB);
        assert_eq!(bb256.p[2], 0xCCCC);
        assert_eq!(bb256.p[3], 0xDDDD);
    }

    #[test]
    fn test_bitboard256_merge() {
        let bb256 = Bitboard256::from_u64_array([0x1111, 0x2222, 0x3333, 0x4444]);
        let merged = bb256.merge();
        assert_eq!(merged.extract64::<0>(), 0x1111 | 0x3333);
        assert_eq!(merged.extract64::<1>(), 0x2222 | 0x4444);
    }

    #[test]
    fn test_bitboard256_decrement_pair() {
        let hi = Bitboard256::from_u64_array([10, 20, 30, 40]);
        let lo = Bitboard256::from_u64_array([5, 3, 7, 9]);

        let (hi_out, lo_out) = Bitboard256::decrement_pair(hi, lo);

        // lo - 1
        assert_eq!(lo_out.p[0], 4);
        assert_eq!(lo_out.p[1], 2);
        assert_eq!(lo_out.p[2], 6);
        assert_eq!(lo_out.p[3], 8);

        // hiは変化なし（lo != 0）
        assert_eq!(hi_out.p[0], 10);
        assert_eq!(hi_out.p[1], 20);
        assert_eq!(hi_out.p[2], 30);
        assert_eq!(hi_out.p[3], 40);
    }

    #[test]
    fn test_bitboard256_decrement_pair_with_borrow() {
        let hi = Bitboard256::from_u64_array([10, 20, 30, 40]);
        let lo = Bitboard256::from_u64_array([0, 1, 0, 5]);

        let (hi_out, lo_out) = Bitboard256::decrement_pair(hi, lo);

        // lo - 1
        assert_eq!(lo_out.p[0], u64::MAX); // underflow
        assert_eq!(lo_out.p[1], 0);
        assert_eq!(lo_out.p[2], u64::MAX); // underflow
        assert_eq!(lo_out.p[3], 4);

        // 桁借り
        assert_eq!(hi_out.p[0], 9); // p[0]: lo == 0なので桁借り
        assert_eq!(hi_out.p[1], 20); // p[1]: lo != 0なので変化なし
        assert_eq!(hi_out.p[2], 29); // p[2]: lo == 0なので桁借り
        assert_eq!(hi_out.p[3], 40); // p[3]: lo != 0なので変化なし
    }

    #[test]
    fn test_bitboard256_unpack() {
        let hi = Bitboard256::from_u64_array([0xA0, 0xA1, 0xA2, 0xA3]);
        let lo = Bitboard256::from_u64_array([0xB0, 0xB1, 0xB2, 0xB3]);

        let (hi_out, lo_out) = Bitboard256::unpack(hi, lo);

        // unpackhi: lo[1,3], hi[1,3]
        assert_eq!(hi_out.p[0], 0xB1);
        assert_eq!(hi_out.p[1], 0xA1);
        assert_eq!(hi_out.p[2], 0xB3);
        assert_eq!(hi_out.p[3], 0xA3);

        // unpacklo: lo[0,2], hi[0,2]
        assert_eq!(lo_out.p[0], 0xB0);
        assert_eq!(lo_out.p[1], 0xA0);
        assert_eq!(lo_out.p[2], 0xB2);
        assert_eq!(lo_out.p[3], 0xA2);
    }

    #[test]
    fn test_bitboard256_bitand() {
        let bb1 = Bitboard256::from_u64_array([0xFF00, 0x00FF, 0xF0F0, 0x0F0F]);
        let bb2 = Bitboard256::from_u64_array([0xF0F0, 0x0F0F, 0xFF00, 0x00FF]);
        let result = bb1 & bb2;
        assert_eq!(result.p[0], 0xF000);
        assert_eq!(result.p[1], 0x000F);
        assert_eq!(result.p[2], 0xF000);
        assert_eq!(result.p[3], 0x000F);
    }

    #[test]
    fn test_bitboard256_bitor() {
        let bb1 = Bitboard256::from_u64_array([0xFF00, 0x00FF, 0xF0F0, 0x0F0F]);
        let bb2 = Bitboard256::from_u64_array([0x0F0F, 0xF0F0, 0x00FF, 0xFF00]);
        let result = bb1 | bb2;
        assert_eq!(result.p[0], 0xFF0F);
        assert_eq!(result.p[1], 0xF0FF);
        assert_eq!(result.p[2], 0xF0FF);
        assert_eq!(result.p[3], 0xFF0F);
    }

    #[test]
    fn test_bitboard256_bitxor() {
        let bb1 = Bitboard256::from_u64_array([0xFF00, 0x00FF, 0xF0F0, 0x0F0F]);
        let bb2 = Bitboard256::from_u64_array([0xFFFF, 0xFFFF, 0xFFFF, 0xFFFF]);
        let result = bb1 ^ bb2;
        assert_eq!(result.p[0], 0x00FF);
        assert_eq!(result.p[1], 0xFF00);
        assert_eq!(result.p[2], 0x0F0F);
        assert_eq!(result.p[3], 0xF0F0);
    }

    #[test]
    fn test_bitboard256_byte_reverse() {
        let bb = Bitboard256::from_u64_array([
            0x0102030405060708,
            0x090A0B0C0D0E0F10,
            0x1112131415161718,
            0x191A1B1C1D1E1F20,
        ]);
        let reversed = bb.byte_reverse();
        // 各128bitレーン内でバイト反転
        // 下位レーン: [p0, p1] -> byte_reverse -> [swap(p1), swap(p0)]
        assert_eq!(reversed.p[0], 0x100F0E0D0C0B0A09);
        assert_eq!(reversed.p[1], 0x0807060504030201);
        // 上位レーン: [p2, p3] -> byte_reverse -> [swap(p3), swap(p2)]
        assert_eq!(reversed.p[2], 0x201F1E1D1C1B1A19);
        assert_eq!(reversed.p[3], 0x1817161514131211);
    }
}
