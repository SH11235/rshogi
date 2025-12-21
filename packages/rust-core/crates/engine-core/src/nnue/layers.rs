//! ネットワーク層の実装
//!
//! - `AffineTransform`: 全結合アフィン変換層（入力×重み + バイアス）
//! - `ClippedReLU`: 整数スケーリング付きのクリップ付き ReLU 層

use super::accumulator::AlignedBox;
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
    /// 重み（転置形式で保持、64バイトアライン）
    pub weights: AlignedBox<i8>,
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

        // 重みを読み込み（64バイトアラインで確保）
        let weight_size = OUTPUT_DIM * Self::PADDED_INPUT;
        let mut weights = AlignedBox::new_zeroed(weight_size);
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
    /// # アライメント要件
    ///
    /// **重要**: 入力スライスは64バイトアライメントが必要です。
    ///
    /// | ターゲット | 必要アライメント | 使用命令 |
    /// |-----------|-----------------|----------|
    /// | AVX2 (`x86_64`) | 32バイト以上 | `_mm256_load_si256` |
    /// | SSE2 (`x86_64`) | 16バイト以上 | `_mm_load_si128` |
    /// | WASM SIMD128 | 不要 | `v128_load`（任意アドレス対応） |
    /// | スカラー | 不要 | - |
    ///
    /// アライメントを保証するには、[`Aligned`](super::accumulator::Aligned) ラッパーを使用してください:
    ///
    /// ```ignore
    /// use crate::nnue::accumulator::Aligned;
    ///
    /// let mut input = Aligned([0u8; 512]);  // 64バイトアライン
    /// transform.propagate(&input.0, &mut output);
    /// ```
    ///
    /// **警告**: アライメントされていない入力を渡すと、AVX2/SSE2環境で
    /// 未定義動作（SIGSEGV等）が発生します。
    ///
    /// # 入力サイズの契約
    ///
    /// 入力スライスは `PADDED_INPUT` バイト以上である必要がある。
    /// SIMD実装は32バイト（AVX2）または16バイト（SSE2）単位で処理するため、
    /// `INPUT_DIM` より小さい入力を渡すと境界外アクセスが発生する。
    ///
    /// # 入力密度
    ///
    /// 実測結果（2025-12-18）: 約40%（39-42%）
    /// → スパース最適化には高すぎるため、密な行列積方式が正しい選択。
    /// 詳細は `network.rs` の diagnostics 計測コードを参照。
    pub fn propagate(&self, input: &[u8], output: &mut [i32; OUTPUT_DIM]) {
        debug_assert!(
            input.len() >= Self::PADDED_INPUT,
            "input length {} is less than PADDED_INPUT {}",
            input.len(),
            Self::PADDED_INPUT
        );
        // AVX2: 256bit = 32 x u8/i8
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        {
            // SAFETY:
            // - input.len() >= PADDED_INPUT (debug_assert で検証済み)
            // - weights.len() >= OUTPUT_DIM * PADDED_INPUT (構造上保証)
            // - input は Aligned<[u8; N]> で64バイトアライン
            // - weights は AlignedBox<i8> で64バイトアライン
            // - PADDED_INPUT は32の倍数なのでオフセットは常に32バイト境界
            unsafe {
                use std::arch::x86_64::*;

                let num_chunks = Self::PADDED_INPUT / 32;

                // 定数をループ外でホイスト
                let one = _mm256_set1_epi16(1);

                // ポインタを事前に取得（境界チェック排除）
                let input_ptr = input.as_ptr();
                let weights_ptr = self.weights.as_ptr();

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = _mm256_setzero_si256();
                    let weight_row_offset = j * Self::PADDED_INPUT;

                    // 入力を32バイトずつ処理
                    for k in 0..num_chunks {
                        let offset = k * 32;
                        let in_vec = _mm256_load_si256(input_ptr.add(offset) as *const __m256i);
                        let w_vec = _mm256_load_si256(
                            weights_ptr.add(weight_row_offset + offset) as *const __m256i
                        );

                        // u8 × i8 → i16 (隣接2ペアの積和、16個のi16)
                        let prod16 = _mm256_maddubs_epi16(in_vec, w_vec);

                        // i16 → i32 にワイドニング加算
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
            // SAFETY:
            // - input.len() >= PADDED_INPUT (debug_assert で検証済み)
            // - weights.len() >= OUTPUT_DIM * PADDED_INPUT (構造上保証)
            // - input は Aligned<[u8; N]> で64バイトアライン（16バイト境界も満たす）
            // - weights は AlignedBox<i8> で64バイトアライン
            // - PADDED_INPUT は32の倍数なのでオフセットは常に16バイト境界
            unsafe {
                use std::arch::x86_64::*;

                let num_chunks = Self::PADDED_INPUT / 16;

                // 定数をループ外でホイスト
                let one = _mm_set1_epi16(1);
                let zero = _mm_setzero_si128();

                // ポインタを事前に取得（境界チェック排除）
                let input_ptr = input.as_ptr();
                let weights_ptr = self.weights.as_ptr();

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = _mm_setzero_si128();
                    let weight_row_offset = j * Self::PADDED_INPUT;

                    // 入力を16バイトずつ処理
                    for k in 0..num_chunks {
                        let offset = k * 16;
                        let in_vec = _mm_load_si128(input_ptr.add(offset) as *const __m128i);
                        let w_vec = _mm_load_si128(
                            weights_ptr.add(weight_row_offset + offset) as *const __m128i
                        );

                        // SSE2にはmaddubs_epi16がないので、手動で実装
                        // u8をi16にゼロ拡張
                        let in_lo = _mm_unpacklo_epi8(in_vec, zero);
                        let in_hi = _mm_unpackhi_epi8(in_vec, zero);
                        // i8をi16に符号拡張（cmpgtで符号ビットマスクを生成）
                        let sign = _mm_cmpgt_epi8(zero, w_vec);
                        let w_lo = _mm_unpacklo_epi8(w_vec, sign);
                        let w_hi = _mm_unpackhi_epi8(w_vec, sign);

                        // i16乗算
                        let prod_lo = _mm_mullo_epi16(in_lo, w_lo);
                        let prod_hi = _mm_mullo_epi16(in_hi, w_hi);

                        // i16 → i32 にワイドニング加算
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
        #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
        {
            // SAFETY:
            // - input.len() >= PADDED_INPUT (debug_assert で検証済み)
            // - weights.len() >= OUTPUT_DIM * PADDED_INPUT (構造上保証)
            // - WASM SIMD128 はアライメント不要（v128_load は任意のアドレスで動作）
            unsafe {
                use std::arch::wasm32::*;

                let num_chunks = Self::PADDED_INPUT / 16;

                // ポインタを事前に取得（境界チェック排除）
                let input_ptr = input.as_ptr();
                let weights_ptr = self.weights.as_ptr();

                for (j, (out, &bias)) in output.iter_mut().zip(&self.biases).enumerate() {
                    let mut acc = i32x4_splat(0);
                    let weight_row_offset = j * Self::PADDED_INPUT;

                    // 入力を16バイトずつ処理
                    for k in 0..num_chunks {
                        let offset = k * 16;
                        let in_vec = v128_load(input_ptr.add(offset) as *const v128);
                        let w_vec =
                            v128_load(weights_ptr.add(weight_row_offset + offset) as *const v128);

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
    use crate::nnue::accumulator::Aligned;

    #[test]
    fn test_affine_transform_propagate() {
        // 小さいテスト用の変換
        // PADDED_INPUT = padded_input(4) = 32 なので、入力も32バイト必要
        let mut weights = AlignedBox::new_zeroed(64); // 2行 × 32バイト
        weights[0] = 1;
        weights[1] = 2; // 行0: [1, 2, 0, ...]
        weights[32] = 3;
        weights[33] = 4; // 行1: [3, 4, 0, ...]

        let transform: AffineTransform<4, 2> = AffineTransform {
            biases: [10, 20],
            weights,
        };

        // 入力はPADDED_INPUT（32バイト）にパディングする必要がある
        // SIMD実装は32バイト単位で処理するため、64バイトアライン必須
        let mut input = Aligned([0u8; 32]);
        input.0[0] = 1;
        input.0[1] = 2;
        let mut output = [0i32; 2];

        transform.propagate(&input.0, &mut output);

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

    #[test]
    fn test_affine_transform_real_size() {
        // 実際の使用サイズ（512入力→32出力）に近いテスト
        // PADDED_INPUT = padded_input(512) = 512
        let mut weights = AlignedBox::new_zeroed(32 * 512);
        // 対角成分を1に設定（出力iに入力iが1:1で対応）
        for i in 0..32 {
            weights[i * 512 + i] = 1;
        }

        let transform: AffineTransform<512, 32> = AffineTransform {
            biases: [10; 32],
            weights,
        };

        // 入力は64バイトアライン必須
        let mut input = Aligned([0u8; 512]);
        for (i, val) in input.0.iter_mut().take(32).enumerate() {
            *val = (i + 1) as u8; // 1, 2, 3, ..., 32
        }
        let mut output = [0i32; 32];

        transform.propagate(&input.0, &mut output);

        // output[i] = 10 + input[i] * 1 = 10 + (i+1)
        for (i, &val) in output.iter().enumerate() {
            assert_eq!(val, 10 + (i + 1) as i32, "mismatch at index {i}");
        }
    }
}
