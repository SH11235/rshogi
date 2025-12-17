//! ネットワーク層の実装
//!
//! - `AffineTransform`: 全結合アフィン変換層（入力×重み + バイアス）
//! - `ClippedReLU`: 整数スケーリング付きのクリップ付き ReLU 層

use super::constants::WEIGHT_SCALE_BITS;
use std::io::{self, Read};

/// パディング済み入力次元（SIMDアライメント用）
const fn padded_input(input_dim: usize) -> usize {
    input_dim.div_ceil(32) * 32
}

/// AVX2での水平加算（i32×8 → i32）
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline]
unsafe fn hsum_i32_avx2(v: std::arch::x86_64::__m256i) -> i32 {
    use std::arch::x86_64::*;

    // 上位128bitと下位128bitを加算
    let hi = _mm256_extracti128_si256(v, 1);
    let lo = _mm256_castsi256_si128(v);
    let sum128 = _mm_add_epi32(lo, hi);

    // 64bit加算
    let hi64 = _mm_unpackhi_epi64(sum128, sum128);
    let sum64 = _mm_add_epi32(sum128, hi64);

    // 32bit加算
    let hi32 = _mm_shuffle_epi32(sum64, 1);
    let sum32 = _mm_add_epi32(sum64, hi32);

    _mm_cvtsi128_si32(sum32)
}

/// SSE2での水平加算（i32×4 → i32）
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "sse2",
    not(target_feature = "avx2")
))]
#[inline]
unsafe fn hsum_i32_sse2(v: std::arch::x86_64::__m128i) -> i32 {
    use std::arch::x86_64::*;

    // 64bit加算
    let hi64 = _mm_unpackhi_epi64(v, v);
    let sum64 = _mm_add_epi32(v, hi64);

    // 32bit加算
    let hi32 = _mm_shuffle_epi32(sum64, 1);
    let sum32 = _mm_add_epi32(sum64, hi32);

    _mm_cvtsi128_si32(sum32)
}

/// アフィン変換層
pub struct AffineTransform<const INPUT_DIM: usize, const OUTPUT_DIM: usize> {
    /// バイアス
    pub biases: [i32; OUTPUT_DIM],
    /// 重み（転置形式で保持）
    pub weights: Box<[i8]>,
}

impl<const INPUT_DIM: usize, const OUTPUT_DIM: usize> AffineTransform<INPUT_DIM, OUTPUT_DIM> {
    const PADDED_INPUT: usize = padded_input(INPUT_DIM);

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        // バイアスを読み込み
        let mut biases = [0i32; OUTPUT_DIM];
        let mut buf4 = [0u8; 4];
        for bias in biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // 重みを読み込み
        let weight_size = OUTPUT_DIM * Self::PADDED_INPUT;
        let mut weights = vec![0i8; weight_size].into_boxed_slice();
        let mut buf1 = [0u8; 1];
        for weight in weights.iter_mut() {
            reader.read_exact(&mut buf1)?;
            *weight = buf1[0] as i8;
        }

        Ok(Self { biases, weights })
    }

    /// 順伝播
    ///
    /// AVX2/SSE2/WASMのSIMD最適化版。
    /// 密な行列積方式（YaneuraOuスタイル）で実装。
    ///
    /// 入力密度実測結果（2025-12-18）: 約40%（39-42%）
    /// → スパース最適化には高すぎるため、密な行列積方式が正しい選択。
    /// 詳細は `network.rs` の diagnostics 計測コードを参照。
    pub fn propagate(&self, input: &[u8], output: &mut [i32; OUTPUT_DIM]) {
        // AVX2: 256bit = 32 x u8/i8
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // SAFETY:
            // - 入力とウェイトは適切なサイズが保証されている
            // - PADDED_INPUTは32の倍数
            unsafe {
                use std::arch::x86_64::*;

                let num_chunks = Self::PADDED_INPUT / 32;

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = _mm256_setzero_si256();
                    let weight_row = &self.weights[j * Self::PADDED_INPUT..];

                    // 入力を32バイトずつ処理
                    for k in 0..num_chunks {
                        let offset = k * 32;
                        let in_vec = _mm256_loadu_si256(input[offset..].as_ptr() as *const __m256i);
                        let w_vec =
                            _mm256_loadu_si256(weight_row[offset..].as_ptr() as *const __m256i);

                        // u8 × i8 → i16 (隣接2ペアの積和、16個のi16)
                        let prod16 = _mm256_maddubs_epi16(in_vec, w_vec);

                        // i16 → i32 にワイドニング加算
                        // _mm256_madd_epi16(a, 1) で隣接2個のi16を加算してi32に
                        let one = _mm256_set1_epi16(1);
                        let prod32 = _mm256_madd_epi16(prod16, one);

                        acc = _mm256_add_epi32(acc, prod32);
                    }

                    // 水平加算してバイアスを加える
                    *out = bias + hsum_i32_avx2(acc);
                }
            }
            return;
        }

        // SSE2: 128bit = 16 x u8/i8
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "sse2",
            not(target_feature = "avx2")
        ))]
        {
            // SAFETY: 同上
            unsafe {
                use std::arch::x86_64::*;

                let num_chunks = Self::PADDED_INPUT / 16;

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = _mm_setzero_si128();
                    let weight_row = &self.weights[j * Self::PADDED_INPUT..];

                    // 入力を16バイトずつ処理
                    for k in 0..num_chunks {
                        let offset = k * 16;
                        let in_vec = _mm_loadu_si128(input[offset..].as_ptr() as *const __m128i);
                        let w_vec =
                            _mm_loadu_si128(weight_row[offset..].as_ptr() as *const __m128i);

                        // SSE2にはmaddubs_epi16がないので、手動で実装
                        // u8をi16に拡張
                        let in_lo = _mm_unpacklo_epi8(in_vec, _mm_setzero_si128());
                        let in_hi = _mm_unpackhi_epi8(in_vec, _mm_setzero_si128());
                        // i8をi16に拡張（符号拡張）
                        let w_lo = _mm_unpacklo_epi8(w_vec, _mm_setzero_si128());
                        let w_hi = _mm_unpackhi_epi8(w_vec, _mm_setzero_si128());
                        // 符号拡張を正しく行う
                        let w_lo = _mm_sub_epi16(
                            w_lo,
                            _mm_and_si128(_mm_set1_epi16(256), _mm_srai_epi16(w_lo, 7)),
                        );
                        let w_hi = _mm_sub_epi16(
                            w_hi,
                            _mm_and_si128(_mm_set1_epi16(256), _mm_srai_epi16(w_hi, 7)),
                        );

                        // i16乗算
                        let prod_lo = _mm_mullo_epi16(in_lo, w_lo);
                        let prod_hi = _mm_mullo_epi16(in_hi, w_hi);

                        // i16 → i32 にワイドニング加算
                        let one = _mm_set1_epi16(1);
                        let sum32_lo = _mm_madd_epi16(prod_lo, one);
                        let sum32_hi = _mm_madd_epi16(prod_hi, one);

                        acc = _mm_add_epi32(acc, sum32_lo);
                        acc = _mm_add_epi32(acc, sum32_hi);
                    }

                    // 水平加算してバイアスを加える
                    *out = bias + hsum_i32_sse2(acc);
                }
            }
            return;
        }

        // WASM SIMD128
        #[cfg(target_arch = "wasm32")]
        {
            // SAFETY: 同上
            unsafe {
                use std::arch::wasm32::*;

                let num_chunks = Self::PADDED_INPUT / 16;

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = i32x4_splat(0);
                    let weight_row = &self.weights[j * Self::PADDED_INPUT..];

                    // 入力を16バイトずつ処理
                    for k in 0..num_chunks {
                        let offset = k * 16;
                        let in_vec = v128_load(input[offset..].as_ptr() as *const v128);
                        let w_vec = v128_load(weight_row[offset..].as_ptr() as *const v128);

                        // u8をi16に拡張
                        let in_lo = i16x8_extend_low_u8x16(in_vec);
                        let in_hi = i16x8_extend_high_u8x16(in_vec);
                        // i8をi16に拡張
                        let w_lo = i16x8_extend_low_i8x16(w_vec);
                        let w_hi = i16x8_extend_high_i8x16(w_vec);

                        // i16乗算
                        let prod_lo = i16x8_mul(in_lo, w_lo);
                        let prod_hi = i16x8_mul(in_hi, w_hi);

                        // i16 → i32 に拡張して加算
                        let sum32_lo_lo = i32x4_extend_low_i16x8(prod_lo);
                        let sum32_lo_hi = i32x4_extend_high_i16x8(prod_lo);
                        let sum32_hi_lo = i32x4_extend_low_i16x8(prod_hi);
                        let sum32_hi_hi = i32x4_extend_high_i16x8(prod_hi);

                        acc = i32x4_add(acc, sum32_lo_lo);
                        acc = i32x4_add(acc, sum32_lo_hi);
                        acc = i32x4_add(acc, sum32_hi_lo);
                        acc = i32x4_add(acc, sum32_hi_hi);
                    }

                    // 水平加算
                    let sum = i32x4_extract_lane::<0>(acc)
                        + i32x4_extract_lane::<1>(acc)
                        + i32x4_extract_lane::<2>(acc)
                        + i32x4_extract_lane::<3>(acc);

                    *out = bias + sum;
                }
            }
            return;
        }

        // スカラーフォールバック
        #[allow(unreachable_code)]
        {
            // バイアスで初期化
            output.copy_from_slice(&self.biases);

            // 行列×ベクトル（密な計算）
            for (i, &in_byte) in input.iter().enumerate().take(INPUT_DIM) {
                let in_val = in_byte as i32;
                for (j, out) in output.iter_mut().enumerate() {
                    let weight_idx = j * Self::PADDED_INPUT + i;
                    *out += self.weights[weight_idx] as i32 * in_val;
                }
            }
        }
    }
}

/// ClippedReLU層
/// 入力: i32、出力: u8（0-127にクランプ）
pub struct ClippedReLU<const DIM: usize>;

impl<const DIM: usize> ClippedReLU<DIM> {
    /// 順伝播
    pub fn propagate(input: &[i32; DIM], output: &mut [u8; DIM]) {
        for i in 0..DIM {
            let shifted = input[i] >> WEIGHT_SCALE_BITS;
            output[i] = shifted.clamp(0, 127) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_affine_transform_propagate() {
        // 小さいテスト用の変換
        let transform: AffineTransform<4, 2> = AffineTransform {
            biases: [10, 20],
            weights: vec![
                1, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0,
            ]
            .into_boxed_slice(),
        };

        let input = [1u8, 2, 0, 0];
        let mut output = [0i32; 2];

        transform.propagate(&input, &mut output);

        // output[0] = 10 + 1*1 + 2*2 = 15
        // output[1] = 20 + 1*3 + 2*4 = 31
        assert_eq!(output[0], 15);
        assert_eq!(output[1], 31);
    }

    #[test]
    fn test_clipped_relu() {
        let input = [0i32, 64, 128, -64, 256];
        let mut output = [0u8; 5];

        // WEIGHT_SCALE_BITS = 6 なので、64 >> 6 = 1, 128 >> 6 = 2, etc.
        ClippedReLU::propagate(&input, &mut output);

        assert_eq!(output[0], 0); // 0 >> 6 = 0
        assert_eq!(output[1], 1); // 64 >> 6 = 1
        assert_eq!(output[2], 2); // 128 >> 6 = 2
        assert_eq!(output[3], 0); // -64 >> 6 = -1, clamped to 0
        assert_eq!(output[4], 4); // 256 >> 6 = 4
    }
}
