//! FtActivation トレイトと活性化関数の実装
//!
//! FeatureTransformer出力の活性化関数を型パラメータで切り替え可能にする。
//!
//! # サポートする活性化関数
//!
//! | 名前 | 数式 | 出力次元比 | 用途 |
//! |------|------|-----------|------|
//! | CReLU | `clamp(x, 0, QA)` | 1:1 | 従来互換 |
//! | PairwiseCReLU | `clamp(a, 0, QA) * clamp(b, 0, QA) >> shift` | 2:1 | Stockfish方式 |
//! | SCReLU | `clamp(x, 0, QA)²` | 1:1 | bullet-shogi互換 |
//!
//! # アーキテクチャ文字列との対応
//!
//! | サフィックス | 活性化関数 |
//! |-------------|-----------|
//! | なし | CReLU |
//! | `-PairwiseCReLU` | PairwiseCReLU |
//! | `-SCReLU` | SCReLU |

use super::constants::WEIGHT_SCALE_BITS;

/// FeatureTransformer出力の活性化関数トレイト
///
/// # 型パラメータ
///
/// このトレイトを実装する型は、ネットワークの型パラメータとして使用される。
/// 各活性化関数は出力次元の変換比率（`OUTPUT_DIM_DIVISOR`）を定義し、
/// L1層の入力次元を決定する。
pub trait FtActivation: Clone + Copy + Default + Send + Sync + 'static {
    /// 出力次元の除数
    ///
    /// L1層入力次元 = FT出力次元 * 2 / OUTPUT_DIM_DIVISOR
    ///
    /// - CReLU, SCReLU: 1（次元維持）
    /// - PairwiseCReLU: 2（次元半減）
    const OUTPUT_DIM_DIVISOR: usize;

    /// i16入力からu8出力への活性化関数適用
    ///
    /// # 引数
    /// - `input`: FeatureTransformer出力（i16）
    /// - `output`: 活性化後の出力（u8）
    /// - `qa`: クリッピング閾値（通常127または255）
    fn activate_i16_to_u8(input: &[i16], output: &mut [u8], qa: i16);

    /// i32入力からu8出力への活性化関数適用（中間層用）
    ///
    /// 中間層では固定のスケーリング係数を使用（FT層のQAとは異なる）。
    ///
    /// # 引数
    /// - `input`: AffineTransform出力（i32）
    /// - `output`: 活性化後の出力（u8）
    fn activate_i32_to_u8(input: &[i32], output: &mut [u8]);

    /// アーキテクチャ文字列のサフィックス
    ///
    /// ヘッダー文字列のマッチングに使用。
    fn header_suffix() -> &'static str;

    /// この活性化関数の名前
    fn name() -> &'static str;
}

// =============================================================================
// CReLU - Clipped ReLU
// =============================================================================

/// Clipped ReLU 活性化関数
///
/// `y = clamp(x, 0, QA)`
///
/// 従来のNNUE実装で使用される標準的な活性化関数。
#[derive(Clone, Copy, Default)]
pub struct CReLU;

impl FtActivation for CReLU {
    const OUTPUT_DIM_DIVISOR: usize = 1;

    #[inline]
    fn activate_i16_to_u8(input: &[i16], output: &mut [u8], qa: i16) {
        debug_assert_eq!(input.len(), output.len());
        crelu_i16_to_u8(input, output, qa);
    }

    #[inline]
    fn activate_i32_to_u8(input: &[i32], output: &mut [u8]) {
        debug_assert_eq!(input.len(), output.len());
        crelu_i32_to_u8(input, output);
    }

    fn header_suffix() -> &'static str {
        ""
    }

    fn name() -> &'static str {
        "CReLU"
    }
}

/// CReLU: i16 → u8（SIMD最適化版）
fn crelu_i16_to_u8(input: &[i16], output: &mut [u8], qa: i16) {
    let mut processed = 0;

    // AVX2: 32要素ずつ処理
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        let num_chunks = input.len() / 16;
        if num_chunks > 0 {
            unsafe {
                use std::arch::x86_64::*;
                let zero = _mm256_setzero_si256();
                let max_val = _mm256_set1_epi16(qa);

                let in_ptr = input.as_ptr();
                let out_ptr = output.as_mut_ptr();

                for i in 0..num_chunks {
                    let v = _mm256_loadu_si256(in_ptr.add(i * 16) as *const __m256i);
                    let clamped = _mm256_min_epi16(_mm256_max_epi16(v, zero), max_val);
                    let packed = _mm256_packus_epi16(clamped, clamped);
                    let result = _mm256_permute4x64_epi64(packed, 0b11011000);
                    _mm_storeu_si128(
                        out_ptr.add(i * 16) as *mut __m128i,
                        _mm256_castsi256_si128(result),
                    );
                }
            }
            processed = num_chunks * 16;
        }
    }

    // SSE2: 8要素ずつ処理
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "sse2",
        not(target_feature = "avx2")
    ))]
    {
        let remaining = input.len() - processed;
        let num_chunks = remaining / 16;
        if num_chunks > 0 {
            unsafe {
                use std::arch::x86_64::*;
                let zero = _mm_setzero_si128();
                let max_val = _mm_set1_epi16(qa);

                let in_ptr = input.as_ptr().add(processed);
                let out_ptr = output.as_mut_ptr().add(processed);

                for i in 0..num_chunks {
                    let v0 = _mm_loadu_si128(in_ptr.add(i * 16) as *const __m128i);
                    let v1 = _mm_loadu_si128(in_ptr.add(i * 16 + 8) as *const __m128i);

                    let clamped0 = _mm_min_epi16(_mm_max_epi16(v0, zero), max_val);
                    let clamped1 = _mm_min_epi16(_mm_max_epi16(v1, zero), max_val);

                    let packed = _mm_packus_epi16(clamped0, clamped1);
                    _mm_storeu_si128(out_ptr.add(i * 16) as *mut __m128i, packed);
                }
            }
            processed += num_chunks * 16;
        }
    }

    // WASM SIMD128
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        let remaining = input.len() - processed;
        let num_chunks = remaining / 8;
        if num_chunks > 0 {
            unsafe {
                use std::arch::wasm32::*;
                let zero = i16x8_splat(0);
                let max_val = i16x8_splat(qa);

                let in_ptr = input.as_ptr().add(processed);
                let out_ptr = output.as_mut_ptr().add(processed);

                for i in 0..num_chunks {
                    let v = v128_load(in_ptr.add(i * 8) as *const v128);
                    let clamped = i16x8_min(i16x8_max(v, zero), max_val);
                    let packed = u8x16_narrow_i16x8(clamped, clamped);
                    v128_store64_lane::<0>(out_ptr.add(i * 8) as *mut v128, packed);
                }
            }
            processed += num_chunks * 8;
        }
    }

    // スカラーフォールバック
    for i in processed..input.len() {
        output[i] = input[i].clamp(0, qa) as u8;
    }
}

/// CReLU: i32 → u8（SIMD最適化版）
///
/// 中間層では固定で 0-127 にクランプする（u8 出力のため）
fn crelu_i32_to_u8(input: &[i32], output: &mut [u8]) {
    let mut processed = 0;

    // AVX2: 32要素ずつ処理
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        let num_chunks = input.len() / 32;
        if num_chunks > 0 {
            unsafe {
                use std::arch::x86_64::*;
                let zero = _mm256_setzero_si256();
                let offsets = _mm256_set_epi32(7, 3, 6, 2, 5, 1, 4, 0);
                let in_ptr = input.as_ptr() as *const __m256i;
                let out_ptr = output.as_mut_ptr() as *mut __m256i;

                for i in 0..num_chunks {
                    let in0 = _mm256_loadu_si256(in_ptr.add(i * 4));
                    let in1 = _mm256_loadu_si256(in_ptr.add(i * 4 + 1));
                    let in2 = _mm256_loadu_si256(in_ptr.add(i * 4 + 2));
                    let in3 = _mm256_loadu_si256(in_ptr.add(i * 4 + 3));

                    let words0 =
                        _mm256_srai_epi16(_mm256_packs_epi32(in0, in1), WEIGHT_SCALE_BITS as i32);
                    let words1 =
                        _mm256_srai_epi16(_mm256_packs_epi32(in2, in3), WEIGHT_SCALE_BITS as i32);

                    let bytes = _mm256_max_epi8(_mm256_packs_epi16(words0, words1), zero);
                    let result = _mm256_permutevar8x32_epi32(bytes, offsets);

                    _mm256_storeu_si256(out_ptr.add(i), result);
                }
            }
            processed = num_chunks * 32;
        }
    }

    // SSE2: 16要素ずつ処理
    #[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
    {
        let remaining = input.len() - processed;
        let num_chunks = remaining / 16;
        if num_chunks > 0 {
            unsafe {
                use std::arch::x86_64::*;

                #[cfg(target_feature = "sse4.1")]
                let zero = _mm_setzero_si128();
                #[cfg(not(target_feature = "sse4.1"))]
                let k0x80s = _mm_set1_epi8(-128i8);

                let in_ptr = input.as_ptr().add(processed) as *const __m128i;
                let out_ptr = output.as_mut_ptr().add(processed) as *mut __m128i;

                for i in 0..num_chunks {
                    let in0 = _mm_loadu_si128(in_ptr.add(i * 4));
                    let in1 = _mm_loadu_si128(in_ptr.add(i * 4 + 1));
                    let in2 = _mm_loadu_si128(in_ptr.add(i * 4 + 2));
                    let in3 = _mm_loadu_si128(in_ptr.add(i * 4 + 3));

                    let words0 =
                        _mm_srai_epi16(_mm_packs_epi32(in0, in1), WEIGHT_SCALE_BITS as i32);
                    let words1 =
                        _mm_srai_epi16(_mm_packs_epi32(in2, in3), WEIGHT_SCALE_BITS as i32);

                    let packedbytes = _mm_packs_epi16(words0, words1);

                    #[cfg(target_feature = "sse4.1")]
                    let result = _mm_max_epi8(packedbytes, zero);
                    #[cfg(not(target_feature = "sse4.1"))]
                    let result = _mm_subs_epi8(_mm_adds_epi8(packedbytes, k0x80s), k0x80s);

                    _mm_storeu_si128(out_ptr.add(i), result);
                }
            }
            processed += num_chunks * 16;
        }
    }

    // スカラーフォールバック
    for i in processed..input.len() {
        let shifted = input[i] >> WEIGHT_SCALE_BITS;
        output[i] = shifted.clamp(0, 127) as u8;
    }
}

// =============================================================================
// PairwiseCReLU
// =============================================================================

/// Pairwise CReLU 活性化関数
///
/// `y[j] = clamp(a, 0, QA) * clamp(b, 0, QA) >> shift`
///
/// Stockfishで使用される方式。入力の前半と後半をペアにして乗算し、
/// 出力次元を半分にする。
#[derive(Clone, Copy, Default)]
pub struct PairwiseCReLU;

impl FtActivation for PairwiseCReLU {
    const OUTPUT_DIM_DIVISOR: usize = 2;

    #[inline]
    fn activate_i16_to_u8(input: &[i16], output: &mut [u8], qa: i16) {
        debug_assert_eq!(input.len(), output.len() * 2);
        pairwise_crelu_i16_to_u8(input, output, qa);
    }

    #[inline]
    fn activate_i32_to_u8(input: &[i32], output: &mut [u8]) {
        debug_assert_eq!(input.len(), output.len() * 2);
        pairwise_crelu_i32_to_u8(input, output);
    }

    fn header_suffix() -> &'static str {
        "-PairwiseCReLU"
    }

    fn name() -> &'static str {
        "PairwiseCReLU"
    }
}

/// PairwiseCReLU: i16 → u8
fn pairwise_crelu_i16_to_u8(input: &[i16], output: &mut [u8], qa: i16) {
    let half = input.len() / 2;
    debug_assert_eq!(output.len(), half, "output length must be half of input length");
    let (qa_i32, shift) = if qa >= 255 { (255i32, 9) } else { (127i32, 7) };

    // TODO: SIMD最適化
    for j in 0..half {
        let a = i32::from(input[j]).clamp(0, qa_i32);
        let b = i32::from(input[j + half]).clamp(0, qa_i32);
        output[j] = ((a * b) >> shift).min(127) as u8;
    }
}

/// PairwiseCReLU: i32 → u8
///
/// 中間層では固定のスケーリングを使用（QB=64相当、shift=7）
fn pairwise_crelu_i32_to_u8(input: &[i32], output: &mut [u8]) {
    let half = input.len() / 2;
    debug_assert_eq!(output.len(), half, "output length must be half of input length");

    // TODO: SIMD最適化
    for j in 0..half {
        let a = (input[j] >> WEIGHT_SCALE_BITS).clamp(0, 127);
        let b = (input[j + half] >> WEIGHT_SCALE_BITS).clamp(0, 127);
        output[j] = ((a * b) >> 7).min(127) as u8;
    }
}

// =============================================================================
// SCReLU - Squared Clipped ReLU
// =============================================================================

/// Squared Clipped ReLU 活性化関数
///
/// `y = clamp(x, 0, QA)²`
///
/// bullet-shogiで使用される活性化関数。
/// クリッピング後に二乗することで、より強い非線形性を持つ。
#[derive(Clone, Copy, Default)]
pub struct SCReLU;

impl FtActivation for SCReLU {
    const OUTPUT_DIM_DIVISOR: usize = 1;

    #[inline]
    fn activate_i16_to_u8(input: &[i16], output: &mut [u8], qa: i16) {
        debug_assert_eq!(input.len(), output.len());
        screlu_i16_to_u8(input, output, qa);
    }

    #[inline]
    fn activate_i32_to_u8(input: &[i32], output: &mut [u8]) {
        debug_assert_eq!(input.len(), output.len());
        screlu_i32_to_u8(input, output);
    }

    fn header_suffix() -> &'static str {
        "-SCReLU"
    }

    fn name() -> &'static str {
        "SCReLU"
    }
}

/// SCReLU: i16 → u8
///
/// シフト量が qa に依存するため、SIMD 版は qa=127 と qa=255 で分岐して実装。
/// 現時点ではシンプルなスカラー実装のみ。
fn screlu_i16_to_u8(input: &[i16], output: &mut [u8], qa: i16) {
    debug_assert_eq!(input.len(), output.len(), "input and output must have same length");
    let qa_i32 = qa as i32;
    let shift = if qa >= 255 { 9 } else { 7 };

    // スカラー実装
    for i in 0..input.len() {
        let clamped = i32::from(input[i]).clamp(0, qa_i32);
        output[i] = ((clamped * clamped) >> shift).min(127) as u8;
    }
}

/// SCReLU: i32 → u8
///
/// 中間層では固定のスケーリングを使用。
/// - クランプ: 0-127（FT層のQAに関係なく固定）
/// - スケーリング: clamped² / QB（QB=64）
///
/// 参考: bullet-shogi の L1 以降の実装と同様
fn screlu_i32_to_u8(input: &[i32], output: &mut [u8]) {
    use super::constants::SCRELU_QB;
    debug_assert_eq!(input.len(), output.len(), "input and output must have same length");

    // スカラー実装（SIMD最適化は必要に応じて追加）
    for (i, &v) in input.iter().enumerate() {
        let shifted = v >> WEIGHT_SCALE_BITS;
        let clamped = shifted.clamp(0, 127);
        let squared = clamped * clamped;
        output[i] = (squared / SCRELU_QB).min(127) as u8;
    }
}

// =============================================================================
// ヘルパー関数
// =============================================================================

/// アーキテクチャ文字列から活性化関数を検出
///
/// # 戻り値
/// - `Some("CReLU")`: サフィックスなし
/// - `Some("PairwiseCReLU")`: `-PairwiseCReLU` サフィックス
/// - `Some("SCReLU")`: `-SCReLU` サフィックス
pub fn detect_activation_from_arch(arch_str: &str) -> &'static str {
    if arch_str.contains(SCReLU::header_suffix()) {
        SCReLU::name()
    } else if arch_str.contains(PairwiseCReLU::header_suffix()) {
        PairwiseCReLU::name()
    } else {
        CReLU::name()
    }
}

// =============================================================================
// テスト
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crelu_i16_to_u8() {
        let input = [0i16, 50, 127, 200, -10, -50];
        let mut output = [0u8; 6];

        CReLU::activate_i16_to_u8(&input, &mut output, 127);

        assert_eq!(output[0], 0);
        assert_eq!(output[1], 50);
        assert_eq!(output[2], 127);
        assert_eq!(output[3], 127); // clamped
        assert_eq!(output[4], 0); // negative → 0
        assert_eq!(output[5], 0); // negative → 0
    }

    #[test]
    fn test_crelu_i32_to_u8() {
        // WEIGHT_SCALE_BITS = 6
        let input = [0i32, 64, 128, 8192, -64, 64 * 100];
        let mut output = [0u8; 6];

        CReLU::activate_i32_to_u8(&input, &mut output);

        assert_eq!(output[0], 0); // 0 >> 6 = 0
        assert_eq!(output[1], 1); // 64 >> 6 = 1
        assert_eq!(output[2], 2); // 128 >> 6 = 2
        assert_eq!(output[3], 127); // 8192 >> 6 = 128 → clamped to 127
        assert_eq!(output[4], 0); // -64 >> 6 = -1 → clamped to 0
        assert_eq!(output[5], 100); // 6400 >> 6 = 100
    }

    #[test]
    fn test_pairwise_crelu_i16_to_u8() {
        // 入力: [a0, a1, a2, a3, b0, b1, b2, b3]
        // 出力: [a0*b0, a1*b1, a2*b2, a3*b3] >> 7
        let input = [64i16, 100, 127, 0, 64, 50, 127, 100];
        let mut output = [0u8; 4];

        PairwiseCReLU::activate_i16_to_u8(&input, &mut output, 127);

        // (64 * 64) >> 7 = 4096 >> 7 = 32
        assert_eq!(output[0], 32);
        // (100 * 50) >> 7 = 5000 >> 7 = 39
        assert_eq!(output[1], 39);
        // (127 * 127) >> 7 = 16129 >> 7 = 126
        assert_eq!(output[2], 126);
        // (0 * 100) >> 7 = 0
        assert_eq!(output[3], 0);
    }

    #[test]
    fn test_screlu_i16_to_u8() {
        let input = [0i16, 50, 127, 200, -10];
        let mut output = [0u8; 5];

        SCReLU::activate_i16_to_u8(&input, &mut output, 127);

        // 0² >> 7 = 0
        assert_eq!(output[0], 0);
        // 50² >> 7 = 2500 >> 7 = 19
        assert_eq!(output[1], 19);
        // 127² >> 7 = 16129 >> 7 = 126
        assert_eq!(output[2], 126);
        // clamped to 127, then 127² >> 7 = 126
        assert_eq!(output[3], 126);
        // negative → 0, then 0² = 0
        assert_eq!(output[4], 0);
    }

    #[test]
    fn test_detect_activation() {
        assert_eq!(detect_activation_from_arch("HalfKA_hm^512x2-8-96"), "CReLU");
        assert_eq!(detect_activation_from_arch("HalfKA_hm^512x2-8-96-SCReLU"), "SCReLU");
        assert_eq!(detect_activation_from_arch("HalfKP256x2-32-32-PairwiseCReLU"), "PairwiseCReLU");
    }

    #[test]
    fn test_output_dim_divisor() {
        assert_eq!(CReLU::OUTPUT_DIM_DIVISOR, 1);
        assert_eq!(PairwiseCReLU::OUTPUT_DIM_DIVISOR, 2);
        assert_eq!(SCReLU::OUTPUT_DIM_DIVISOR, 1);
    }
}
