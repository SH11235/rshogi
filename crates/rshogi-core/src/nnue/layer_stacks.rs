//! nnue-pytorch LayerStacks アーキテクチャ
//!
//! nnue-pytorch の LayerStacks 構造を実装する。
//! 9個のバケットを持ち、局面に応じてバケットを選択して推論を行う。
//!
//! ## アーキテクチャ
//!
//! ```text
//! Feature Transformer: 73,305 → 1536
//! SqrClippedReLU: 3072 → 1536
//! LayerStacks (bucket選択後):
//!   L1: 1536 → 16, split [15, 1]
//!   Sqr(15) → 30
//!   L2: 30 → 32, ReLU
//!   Output: 32 → 1 + skip
//! ```

use super::accumulator::Aligned;
use super::constants::{
    LAYER_STACK_L1_OUT, LAYER_STACK_L2_IN, NNUE_PYTORCH_L1, NNUE_PYTORCH_L2, NNUE_PYTORCH_L3,
    NUM_LAYER_STACK_BUCKETS,
};
use super::layers::AffineTransform;
use std::io::{self, Read};

/// L2 入力のパディング済み次元数（padded_input(30) = 32）
const L2_PADDED_INPUT: usize = super::layers::padded_input(LAYER_STACK_L2_IN);

/// Output 入力のパディング済み次元数（padded_input(32) = 32）
const OUTPUT_PADDED_INPUT: usize = super::layers::padded_input(NNUE_PYTORCH_L3);

// =============================================================================
// LayerStack 単一バケット
// =============================================================================

/// LayerStack 単一バケットの層
///
/// 各バケットは以下の構造を持つ:
/// - L1: 1536 → 16
/// - L2: 30 → 32
/// - Output: 32 → 1
///
/// 各層は `AffineTransform` を使用し、AVX512/AVX2/SSSE3/WASM SIMD128 に対応。
pub struct LayerStackBucket {
    /// L1層: 1536 → 16
    pub l1: AffineTransform<NNUE_PYTORCH_L1, LAYER_STACK_L1_OUT>,
    /// L2層: 30 → 32
    pub l2: AffineTransform<LAYER_STACK_L2_IN, NNUE_PYTORCH_L3>,
    /// 出力層: 32 → 1
    pub output: AffineTransform<NNUE_PYTORCH_L3, 1>,
}

impl LayerStackBucket {
    /// 新規作成（ゼロ初期化）
    pub fn new() -> Self {
        Self {
            l1: AffineTransform::new(),
            l2: AffineTransform::new(),
            output: AffineTransform::new(),
        }
    }

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let l1 = AffineTransform::read(reader)?;
        let l2 = AffineTransform::read(reader)?;
        let output = AffineTransform::read(reader)?;
        Ok(Self { l1, l2, output })
    }

    /// 順伝播
    ///
    /// 入力: SqrClippedReLU後の1536次元 (u8)
    /// 出力: スケーリング前の生スコア (i32)
    pub fn propagate(&self, input: &[u8; NNUE_PYTORCH_L1]) -> i32 {
        // L1: 1536 → 16
        let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
        self.l1.propagate(input, &mut l1_out);

        // Split: [15, 1]
        // l1_x: 最初の15要素、l1_skip: 最後の1要素
        let l1_skip = l1_out[NNUE_PYTORCH_L2]; // index 15

        // ClippedReLU + Sqr for first 15 elements, then concat with original
        // 量子化: i32 >> 6 → clamp(0, 127)
        let mut l2_input = Aligned([0u8; L2_PADDED_INPUT]);

        for (i, &val) in l1_out.iter().enumerate().take(NNUE_PYTORCH_L2) {
            // SqrClippedReLU: min(127, (input^2) >> 19)
            let input_val = val as i64;
            let sqr = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
            // ClippedReLU: clamp(input >> WeightScaleBits, 0, 127)
            let clamped = (val >> 6).clamp(0, 127) as u8;
            l2_input.0[i] = sqr;
            l2_input.0[NNUE_PYTORCH_L2 + i] = clamped;
        }

        // L2: 30 → 32
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        self.l2.propagate(&l2_input.0, &mut l2_out);

        // ClippedReLU
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        for (out, &val) in l2_relu.0.iter_mut().zip(l2_out.iter()) {
            *out = (val >> 6).clamp(0, 127) as u8;
        }

        // Output: 32 → 1
        let mut output_arr = [0i32; 1];
        self.output.propagate(&l2_relu.0, &mut output_arr);

        // Skip connection
        output_arr[0] + l1_skip
    }

    /// 順伝播（診断情報付き）
    ///
    /// 戻り値: (raw_score, l1_out, l1_skip)
    #[cfg(feature = "diagnostics")]
    pub fn propagate_with_diagnostics(
        &self,
        input: &[u8; NNUE_PYTORCH_L1],
    ) -> (i32, [i32; LAYER_STACK_L1_OUT], i32) {
        // L1: 1536 → 16
        let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
        self.l1.propagate(input, &mut l1_out);

        // Split: [15, 1]
        let l1_skip = l1_out[NNUE_PYTORCH_L2]; // index 15

        // ClippedReLU + Sqr for first 15 elements
        let mut l2_input = Aligned([0u8; L2_PADDED_INPUT]);
        for (i, &val) in l1_out.iter().enumerate().take(NNUE_PYTORCH_L2) {
            let input_val = val as i64;
            let sqr = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
            let clamped = (val >> 6).clamp(0, 127) as u8;
            l2_input.0[i] = sqr;
            l2_input.0[NNUE_PYTORCH_L2 + i] = clamped;
        }

        // L2: 30 → 32
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        self.l2.propagate(&l2_input.0, &mut l2_out);

        // ClippedReLU
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        for (out, &val) in l2_relu.0.iter_mut().zip(l2_out.iter()) {
            *out = (val >> 6).clamp(0, 127) as u8;
        }

        // Output: 32 → 1
        let mut output_arr = [0i32; 1];
        self.output.propagate(&l2_relu.0, &mut output_arr);

        // Skip connection
        let raw_score = output_arr[0] + l1_skip;

        (raw_score, l1_out, l1_skip)
    }
}

impl Default for LayerStackBucket {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// LayerStacks (9バケット)
// =============================================================================

/// LayerStacks: 9個のバケットを持つ構造
pub struct LayerStacks {
    /// 9個のバケット
    pub buckets: [LayerStackBucket; NUM_LAYER_STACK_BUCKETS],
}

impl LayerStacks {
    /// 新規作成
    pub fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| LayerStackBucket::new()),
        }
    }

    /// ファイルから読み込み
    ///
    /// FC層は常に非圧縮形式（raw bytes）で保存されている。
    /// LEB128圧縮はFeature Transformerにのみ適用される。
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut stacks = Self::new();

        // fc_hash をスキップしてバケットごとに読み込み
        let mut buf4 = [0u8; 4];

        for bucket in stacks.buckets.iter_mut() {
            // fc_hash を読み飛ばす
            reader.read_exact(&mut buf4)?;
            let _fc_hash = u32::from_le_bytes(buf4);

            // バケットを読み込み（常に非圧縮形式）
            *bucket = LayerStackBucket::read(reader)?;
        }

        Ok(stacks)
    }

    /// 生スコアを計算（スケーリング前）
    pub fn evaluate_raw(&self, bucket_index: usize, input: &[u8; NNUE_PYTORCH_L1]) -> i32 {
        debug_assert!(bucket_index < NUM_LAYER_STACK_BUCKETS);
        self.buckets[bucket_index].propagate(input)
    }

    /// 生スコアを計算（診断情報付き）
    ///
    /// 戻り値: (raw_score, l1_out, l1_skip)
    #[cfg(feature = "diagnostics")]
    pub fn evaluate_raw_with_diagnostics(
        &self,
        bucket_index: usize,
        input: &[u8; NNUE_PYTORCH_L1],
    ) -> (i32, [i32; LAYER_STACK_L1_OUT], i32) {
        debug_assert!(bucket_index < NUM_LAYER_STACK_BUCKETS);
        self.buckets[bucket_index].propagate_with_diagnostics(input)
    }
}

impl Default for LayerStacks {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// SqrClippedReLU 変換
// =============================================================================

/// SqrClippedReLU 変換（SIMD最適化版）
///
/// nnue-pytorch の forward 処理:
/// ```python
/// l0_ = (us * cat([w, b], dim=1)) + (them * cat([b, w], dim=1))
/// l0_ = clamp(l0_, 0.0, 1.0)
/// l0_s = split(l0_, L1 // 2, dim=1)  # 4分割
/// l0_s1 = [l0_s[0] * l0_s[1], l0_s[2] * l0_s[3]]  # ペア乗算
/// l0_ = cat(l0_s1, dim=1) * (127 / 128)
/// ```
///
/// 量子化: Python の `a * b * (127/128)` を整数演算で `(a * b) >> 7` として近似。
/// 入力が [0, 127] の範囲なので、a * b の最大値は 127 * 127 = 16129。
/// `16129 >> 7 = 126` なので出力も [0, 127] に収まる。
///
/// 入力: 両視点のアキュムレータ (各1536次元, i16)
/// 出力: SqrClippedReLU後の1536次元 (u8)
pub fn sqr_clipped_relu_transform(
    us_acc: &[i16; NNUE_PYTORCH_L1],
    them_acc: &[i16; NNUE_PYTORCH_L1],
    output: &mut [u8; NNUE_PYTORCH_L1],
) {
    let half = NNUE_PYTORCH_L1 / 2; // 768

    // AVX512BW: 512bit = 32 x i16、2セット同時処理で 64 i16 → 64 u8
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    {
        // SAFETY:
        // - us_acc, them_acc: AccumulatorLayerStacks 内 Aligned<[i16; 1536]> で 64 バイトアライン
        // - output: Aligned<[u8; 1536]> で 64 バイトアライン
        // - half=768, 768/32=24 → 各ループ24回で全要素カバー
        // - 乗算結果: max 127*127=16129 < i16::MAX(32767)、>>7 後は [0, 126] → packus で u8 に収まる
        unsafe {
            use std::arch::x86_64::*;
            let zero = _mm512_setzero_si512();
            let max127 = _mm512_set1_epi16(127);

            // マクロで us/them を処理（出力オフセットが異なるだけ）
            for (acc, out_offset) in [(us_acc.as_ptr(), 0usize), (them_acc.as_ptr(), half)] {
                let acc_a = acc;
                let acc_b = acc.add(half);
                let out_ptr = output.as_mut_ptr().add(out_offset);

                for i in 0..(half / 32) {
                    let offset = i * 32;
                    let va = _mm512_load_si512(acc_a.add(offset) as *const __m512i);
                    let vb = _mm512_load_si512(acc_b.add(offset) as *const __m512i);

                    let a = _mm512_min_epi16(_mm512_max_epi16(va, zero), max127);
                    let b = _mm512_min_epi16(_mm512_max_epi16(vb, zero), max127);
                    let prod = _mm512_mullo_epi16(a, b);
                    let shifted = _mm512_srli_epi16(prod, 7);

                    // i16→u8 パック: packus は 128-bit レーンごとに動作
                    // packus(shifted, zero) → [s0..7,0*8, s8..15,0*8, s16..23,0*8, s24..31,0*8]
                    let packed = _mm512_packus_epi16(shifted, zero);
                    // レーン再配置: [0,2,4,6,1,3,5,7] → [s0..31, 0*32]
                    let perm = _mm512_setr_epi64(0, 2, 4, 6, 1, 3, 5, 7);
                    let fixed = _mm512_permutexvar_epi64(perm, packed);
                    // 下位 256 bit (32 u8) を store
                    _mm256_storeu_si256(
                        out_ptr.add(offset) as *mut __m256i,
                        _mm512_castsi512_si256(fixed),
                    );
                }
            }
        }
        return;
    }

    // AVX2: 256bit = 16 x i16、2セット同時処理で 32 i16 → 32 u8
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        not(target_feature = "avx512bw")
    ))]
    {
        // SAFETY:
        // - us_acc, them_acc: AccumulatorLayerStacks 内 Aligned<[i16; 1536]> で 64 バイトアライン
        // - output: Aligned<[u8; 1536]> で 64 バイトアライン
        // - half=768, 768/32=24 → 各ループ24回で全要素カバー
        // - 乗算結果: max 127*127=16129 < i16::MAX(32767)、>>7 後は [0, 126] → packus で u8 に収まる
        unsafe {
            use std::arch::x86_64::*;
            let zero = _mm256_setzero_si256();
            let max127 = _mm256_set1_epi16(127);

            for (acc, out_offset) in [(us_acc.as_ptr(), 0usize), (them_acc.as_ptr(), half)] {
                let acc_a = acc;
                let acc_b = acc.add(half);
                let out_ptr = output.as_mut_ptr().add(out_offset);

                // 32要素ずつ処理（2 × 16 i16 → 32 u8）
                for i in 0..(half / 32) {
                    let offset = i * 32;

                    let va0 = _mm256_load_si256(acc_a.add(offset) as *const __m256i);
                    let vb0 = _mm256_load_si256(acc_b.add(offset) as *const __m256i);
                    let a0 = _mm256_min_epi16(_mm256_max_epi16(va0, zero), max127);
                    let b0 = _mm256_min_epi16(_mm256_max_epi16(vb0, zero), max127);
                    let shifted0 = _mm256_srli_epi16(_mm256_mullo_epi16(a0, b0), 7);

                    let va1 = _mm256_load_si256(acc_a.add(offset + 16) as *const __m256i);
                    let vb1 = _mm256_load_si256(acc_b.add(offset + 16) as *const __m256i);
                    let a1 = _mm256_min_epi16(_mm256_max_epi16(va1, zero), max127);
                    let b1 = _mm256_min_epi16(_mm256_max_epi16(vb1, zero), max127);
                    let shifted1 = _mm256_srli_epi16(_mm256_mullo_epi16(a1, b1), 7);

                    // Pack 16+16 i16 → 32 u8
                    // packus は 128-bit レーンごとに動作:
                    // [s0[0..7],s1[0..7], s0[8..15],s1[8..15]]
                    let packed = _mm256_packus_epi16(shifted0, shifted1);
                    // レーン修正: 0xD8 = [0,2,1,3] → [s0[0..15], s1[0..15]]
                    let fixed = _mm256_permute4x64_epi64(packed, 0xD8);
                    _mm256_storeu_si256(out_ptr.add(offset) as *mut __m256i, fixed);
                }
            }
        }
        return;
    }

    // SSE2: 128bit = 8 x i16、2セット同時処理で 16 i16 → 16 u8
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "sse2",
        not(target_feature = "avx2")
    ))]
    {
        // SAFETY: 同上（16バイトアライン）
        unsafe {
            use std::arch::x86_64::*;
            let zero = _mm_setzero_si128();
            let max127 = _mm_set1_epi16(127);

            for (acc, out_offset) in [(us_acc.as_ptr(), 0usize), (them_acc.as_ptr(), half)] {
                let acc_a = acc;
                let acc_b = acc.add(half);
                let out_ptr = output.as_mut_ptr().add(out_offset);

                // 16要素ずつ処理（2 × 8 i16 → 16 u8）
                for i in 0..(half / 16) {
                    let offset = i * 16;

                    let va0 = _mm_load_si128(acc_a.add(offset) as *const __m128i);
                    let vb0 = _mm_load_si128(acc_b.add(offset) as *const __m128i);
                    let a0 = _mm_min_epi16(_mm_max_epi16(va0, zero), max127);
                    let b0 = _mm_min_epi16(_mm_max_epi16(vb0, zero), max127);
                    let shifted0 = _mm_srli_epi16(_mm_mullo_epi16(a0, b0), 7);

                    let va1 = _mm_load_si128(acc_a.add(offset + 8) as *const __m128i);
                    let vb1 = _mm_load_si128(acc_b.add(offset + 8) as *const __m128i);
                    let a1 = _mm_min_epi16(_mm_max_epi16(va1, zero), max127);
                    let b1 = _mm_min_epi16(_mm_max_epi16(vb1, zero), max127);
                    let shifted1 = _mm_srli_epi16(_mm_mullo_epi16(a1, b1), 7);

                    // Pack 8+8 i16 → 16 u8（SSE2 にはレーンクロスの問題なし）
                    let packed = _mm_packus_epi16(shifted0, shifted1);
                    _mm_storeu_si128(out_ptr.add(offset) as *mut __m128i, packed);
                }
            }
        }
        return;
    }

    // WASM SIMD128: 128bit = 8 x i16
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    {
        // SAFETY: WASM SIMD128 はアライメント不要
        unsafe {
            use std::arch::wasm32::*;
            let zero = i16x8_splat(0);
            let max127 = i16x8_splat(127);

            for (acc, out_offset) in [(us_acc.as_ptr(), 0usize), (them_acc.as_ptr(), half)] {
                let acc_a = acc;
                let acc_b = acc.add(half);
                let out_ptr = output.as_mut_ptr().add(out_offset);

                for i in 0..(half / 16) {
                    let offset = i * 16;

                    let va0 = v128_load(acc_a.add(offset) as *const v128);
                    let vb0 = v128_load(acc_b.add(offset) as *const v128);
                    let a0 = i16x8_min(i16x8_max(va0, zero), max127);
                    let b0 = i16x8_min(i16x8_max(vb0, zero), max127);
                    let shifted0 = u16x8_shr(i16x8_mul(a0, b0), 7);

                    let va1 = v128_load(acc_a.add(offset + 8) as *const v128);
                    let vb1 = v128_load(acc_b.add(offset + 8) as *const v128);
                    let a1 = i16x8_min(i16x8_max(va1, zero), max127);
                    let b1 = i16x8_min(i16x8_max(vb1, zero), max127);
                    let shifted1 = u16x8_shr(i16x8_mul(a1, b1), 7);

                    // Pack 8+8 i16 → 16 u8（符号なし飽和）
                    let packed = u8x16_narrow_i16x8(shifted0, shifted1);
                    v128_store(out_ptr.add(offset) as *mut v128, packed);
                }
            }
        }
        return;
    }

    // スカラーフォールバック
    #[allow(unreachable_code)]
    {
        // 前半768要素: us_acc[0..768] * us_acc[768..1536]
        // 後半768要素: them_acc[0..768] * them_acc[768..1536]
        for i in 0..half {
            // us側
            let us_a = (us_acc[i] as i32).clamp(0, 127) as u32;
            let us_b = (us_acc[half + i] as i32).clamp(0, 127) as u32;
            let us_prod = ((us_a * us_b) >> 7).min(127);
            output[i] = us_prod as u8;

            // them側
            let them_a = (them_acc[i] as i32).clamp(0, 127) as u32;
            let them_b = (them_acc[half + i] as i32).clamp(0, 127) as u32;
            let them_prod = ((them_a * them_b) >> 7).min(127);
            output[half + i] = them_prod as u8;
        }
    }
}

/// バケットインデックスを計算
///
/// nnue-pytorch の実装（training_data_loader.cpp:272-283）に基づく。
/// 両玉の段（rank）に基づいてバケットを選択する。
///
/// - 味方玉の段を3段階に分割: 0-2 → 0, 3-5 → 3, 6-8 → 6
/// - 相手玉の段を3段階に分割: 0-2 → 0, 3-5 → 1, 6-8 → 2
/// - bucket = f_index + e_index (0-8)
///
/// 引数:
/// - f_king_rank: 味方玉の段（0-8、味方から見た相対段）
/// - e_king_rank: 相手玉の段（0-8、相手から見た相対段）
pub fn compute_bucket_index(f_king_rank: usize, e_king_rank: usize) -> usize {
    // 味方玉の段 → bucket オフセット
    const F_TO_INDEX: [usize; 9] = [0, 0, 0, 3, 3, 3, 6, 6, 6];
    // 相手玉の段 → bucket オフセット
    const E_TO_INDEX: [usize; 9] = [0, 0, 0, 1, 1, 1, 2, 2, 2];

    // 範囲外の値は最大インデックス(8)にクランプ
    let f_idx = F_TO_INDEX[f_king_rank.min(8)];
    let e_idx = E_TO_INDEX[e_king_rank.min(8)];

    (f_idx + e_idx).min(NUM_LAYER_STACK_BUCKETS - 1)
}

/// Position から両玉の相対段を計算
///
/// 戻り値: (味方玉の相対段, 相手玉の相対段)
pub fn compute_king_ranks(
    side_to_move: crate::types::Color,
    f_king_sq: crate::types::Square,
    e_king_sq: crate::types::Square,
) -> (usize, usize) {
    use crate::types::Color;

    // 味方玉の段（味方から見た相対段: 先手なら上が0、後手なら反転）
    let f_rank = if side_to_move == Color::Black {
        f_king_sq.rank() as usize // 先手: そのまま
    } else {
        8 - f_king_sq.rank() as usize // 後手: 反転
    };

    // 相手玉の段（相手から見た相対段: 相手視点で反転）
    let e_rank = if side_to_move == Color::Black {
        8 - e_king_sq.rank() as usize // 先手から見て相手は後手 → 反転
    } else {
        e_king_sq.rank() as usize // 後手から見て相手は先手 → そのまま
    };

    (f_rank, e_rank)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_layer_stack_bucket_new() {
        let bucket = LayerStackBucket::new();
        assert_eq!(bucket.l1.biases.len(), LAYER_STACK_L1_OUT);
        assert_eq!(bucket.l2.biases.len(), NNUE_PYTORCH_L3);
    }

    #[test]
    fn test_layer_stacks_new() {
        let stacks = LayerStacks::new();
        assert_eq!(stacks.buckets.len(), NUM_LAYER_STACK_BUCKETS);
    }

    #[test]
    fn test_bucket_index() {
        // C++ reference:
        // kFToIndex = {0,0,0,3,3,3,6,6,6}
        // kEToIndex = {0,0,0,1,1,1,2,2,2}
        // bucket = kFToIndex[f_rank] + kEToIndex[e_rank]

        // f_rank=0, e_rank=0 -> 0+0 = 0
        assert_eq!(compute_bucket_index(0, 0), 0);
        // f_rank=1, e_rank=1 -> 0+0 = 0
        assert_eq!(compute_bucket_index(1, 1), 0);
        // f_rank=2, e_rank=2 -> 0+0 = 0
        assert_eq!(compute_bucket_index(2, 2), 0);
        // f_rank=3, e_rank=3 -> 3+1 = 4
        assert_eq!(compute_bucket_index(3, 3), 4);
        // f_rank=6, e_rank=6 -> 6+2 = 8
        assert_eq!(compute_bucket_index(6, 6), 8);
        // f_rank=8, e_rank=8 -> 6+2 = 8
        assert_eq!(compute_bucket_index(8, 8), 8);
        // f_rank=0, e_rank=8 -> 0+2 = 2
        assert_eq!(compute_bucket_index(0, 8), 2);
        // f_rank=8, e_rank=0 -> 6+0 = 6
        assert_eq!(compute_bucket_index(8, 0), 6);
        // 範囲外は clamp される
        assert_eq!(compute_bucket_index(10, 10), 8);
    }

    #[test]
    fn test_compute_king_ranks_hirate() {
        use crate::position::{Position, SFEN_HIRATE};
        use crate::types::Color;

        // 平手初期局面
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // 先手番の場合
        assert_eq!(pos.side_to_move(), Color::Black);

        let f_king_sq = pos.king_square(Color::Black); // 5i (rank=8)
        let e_king_sq = pos.king_square(Color::White); // 5a (rank=0)

        let (f_rank, e_rank) = compute_king_ranks(Color::Black, f_king_sq, e_king_sq);

        // 先手玉: 5i(rank=8) → 先手視点でそのまま8
        // 後手玉: 5a(rank=0) → 先手から見て反転 → 8-0=8
        assert_eq!(f_rank, 8, "f_rank for Black in hirate");
        assert_eq!(e_rank, 8, "e_rank for Black in hirate");

        // bucket = F_TO_INDEX[8] + E_TO_INDEX[8] = 6 + 2 = 8
        assert_eq!(compute_bucket_index(f_rank, e_rank), 8);
    }

    #[test]
    fn test_compute_king_ranks_positions() {
        use crate::position::Position;
        use crate::types::Color;

        // 玉が中央付近にいる局面
        // 先手玉が5e(rank=4)、後手玉が5e(rank=4)相当の局面
        let mut pos = Position::new();
        pos.set_sfen("4k4/9/9/9/4K4/9/9/9/9 b - 1").unwrap();

        let f_king_sq = pos.king_square(Color::Black); // 5e (rank=4)
        let e_king_sq = pos.king_square(Color::White); // 5a (rank=0)

        let (f_rank, e_rank) = compute_king_ranks(Color::Black, f_king_sq, e_king_sq);

        // 先手玉: 5e(rank=4) → 先手視点でそのまま4
        assert_eq!(f_rank, 4, "f_rank for Black king at 5e");
        // 後手玉: 5a(rank=0) → 先手から見て反転 → 8-0=8
        assert_eq!(e_rank, 8, "e_rank for White king at 5a");

        // bucket = F_TO_INDEX[4] + E_TO_INDEX[8] = 3 + 2 = 5
        assert_eq!(compute_bucket_index(f_rank, e_rank), 5);

        // 後手番の局面でテスト
        let mut pos2 = Position::new();
        pos2.set_sfen("4k4/9/9/9/4K4/9/9/9/9 w - 1").unwrap();

        let (f_rank2, e_rank2) = compute_king_ranks(
            Color::White,
            pos2.king_square(Color::White),
            pos2.king_square(Color::Black),
        );

        // 後手玉: 5a(rank=0) → 後手視点で反転 → 8-0=8
        assert_eq!(f_rank2, 8, "f_rank for White king at 5a");
        // 先手玉: 5e(rank=4) → 後手から見てそのまま → 4
        assert_eq!(e_rank2, 4, "e_rank for Black king at 5e");

        // bucket = F_TO_INDEX[8] + E_TO_INDEX[4] = 6 + 1 = 7
        assert_eq!(compute_bucket_index(f_rank2, e_rank2), 7);
    }

    /// L1 SqrClippedReLU の境界値テスト
    ///
    /// 正しい計算式: min(127, (input^2) >> 19)
    /// 旧実装: clamp(input>>6, 0, 127)^2 >> 7
    /// input > 8128 のとき旧実装では結果が異なっていた
    #[test]
    fn test_l1_sqr_clipped_relu_boundary() {
        fn sqr_clipped_relu(input: i32) -> u8 {
            ((input as i64 * input as i64) >> 19).clamp(0, 127) as u8
        }

        // 通常範囲: input=0..8128 は旧実装と一致
        assert_eq!(sqr_clipped_relu(0), 0);
        assert_eq!(sqr_clipped_relu(64), 0); // 64^2 >> 19 = 4096 >> 19 = 0
        assert_eq!(sqr_clipped_relu(724), 0); // 724^2 = 524176 >> 19 = 0
        assert_eq!(sqr_clipped_relu(8128), 126); // 8128^2 >> 19 = 66064384 >> 19 = 126

        // 境界値: input=8192 で旧実装と差が出ていたケース
        // 旧: clamp(8192>>6, 0, 127)^2 >> 7 = 127^2 >> 7 = 16129 >> 7 = 126
        // 正: (8192^2) >> 19 = 67108864 >> 19 = 127
        assert_eq!(sqr_clipped_relu(8192), 127);

        // input=8256: 旧=126, 正=127
        assert_eq!(sqr_clipped_relu(8256), 127);

        // 大きい入力でも 127 を超えない
        assert_eq!(sqr_clipped_relu(20000), 127);

        // 負の入力: (neg^2) >> 19 は正になる（i32 → i64 昇格で二乗は正）
        assert_eq!(sqr_clipped_relu(-8192), 127);

        // propagate を通した検証: L1 出力を直接設定して確認
        let bucket = LayerStackBucket::new(); // ゼロ初期化（weights=0, biases=0）

        // biases を設定して l1_out を制御する
        // l1_out = bias（weights が全 0 なので入力に依存しない）
        let mut bucket_with_biases = LayerStackBucket::new();
        // index 0 の bias を 8192 に設定 → sqr = 127, 旧実装なら 126
        bucket_with_biases.l1.biases[0] = 8192;
        // index 1 の bias を 8128 に設定 → sqr = 126 (両方同じ)
        bucket_with_biases.l1.biases[1] = 8128;

        let input = [0u8; NNUE_PYTORCH_L1];
        let result = bucket_with_biases.propagate(&input);

        // l2_input[0] = sqr(8192) = 127, l2_input[1] = sqr(8128) = 126
        // 具体的な result 値は L2/Output の weights にも依存するが、
        // ここでは weights が全 0 なのでスキップ接続のみ:
        // result = output_bias(0) + l1_skip(l1_biases[15]=0) = 0
        // L2 input は propagate 内部で消費されるため直接検証できないが、
        // ゼロ weights でパニックしないことを確認
        let _ = result;
        let _ = bucket; // suppress unused warning
    }

    #[test]
    fn test_sqr_clipped_relu_transform_basic() {
        // 基本的な入出力テスト
        let mut us_acc = [0i16; NNUE_PYTORCH_L1];
        let mut them_acc = [0i16; NNUE_PYTORCH_L1];
        let mut output = [0u8; NNUE_PYTORCH_L1];

        // 入力が0の場合、出力も0
        sqr_clipped_relu_transform(&us_acc, &them_acc, &mut output);
        assert!(
            output.iter().all(|&x| x == 0),
            "all zeros input should produce all zeros output"
        );

        // 最大値テスト: 127 * 127 >> 7 = 16129 >> 7 = 126
        let half = NNUE_PYTORCH_L1 / 2;
        for i in 0..half {
            us_acc[i] = 127;
            us_acc[half + i] = 127;
            them_acc[i] = 127;
            them_acc[half + i] = 127;
        }

        sqr_clipped_relu_transform(&us_acc, &them_acc, &mut output);

        // 期待値: (127 * 127) >> 7 = 126
        for (i, &val) in output.iter().enumerate().take(NNUE_PYTORCH_L1) {
            assert_eq!(val, 126, "max input should produce 126 at index {i}");
        }

        // 負の値はクランプされて0になる
        for i in 0..NNUE_PYTORCH_L1 {
            us_acc[i] = -100;
            them_acc[i] = -100;
        }

        sqr_clipped_relu_transform(&us_acc, &them_acc, &mut output);
        assert!(output.iter().all(|&x| x == 0), "negative input should be clamped to 0");
    }
}
