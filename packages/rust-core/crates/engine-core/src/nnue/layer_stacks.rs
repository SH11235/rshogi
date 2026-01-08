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

use super::accumulator::AlignedBox;
use super::constants::{
    LAYER_STACK_L1_OUT, LAYER_STACK_L2_IN, NNUE_PYTORCH_L1, NNUE_PYTORCH_L2, NNUE_PYTORCH_L3,
    NNUE_PYTORCH_NNUE2SCORE, NUM_LAYER_STACK_BUCKETS,
};
use std::io::{self, Read};

/// パディング済み入力次元（SIMDアライメント用）
const fn padded_input(input_dim: usize) -> usize {
    input_dim.div_ceil(32) * 32
}

// =============================================================================
// LayerStack 単一バケット
// =============================================================================

/// LayerStack 単一バケットの層
///
/// 各バケットは以下の構造を持つ:
/// - L1: 1536 → 16
/// - L2: 30 → 32
/// - Output: 32 → 1
pub struct LayerStackBucket {
    /// L1層: 1536 → 16
    pub l1_biases: [i32; LAYER_STACK_L1_OUT],
    pub l1_weights: AlignedBox<i8>,

    /// L2層: 30 → 32
    pub l2_biases: [i32; NNUE_PYTORCH_L3],
    pub l2_weights: AlignedBox<i8>,

    /// 出力層: 32 → 1
    pub output_bias: i32,
    pub output_weights: AlignedBox<i8>,
}

impl LayerStackBucket {
    const L1_PADDED_INPUT: usize = padded_input(NNUE_PYTORCH_L1);
    const L2_PADDED_INPUT: usize = padded_input(LAYER_STACK_L2_IN);
    const OUTPUT_PADDED_INPUT: usize = padded_input(NNUE_PYTORCH_L3);

    /// 新規作成（ゼロ初期化）
    pub fn new() -> Self {
        Self {
            l1_biases: [0; LAYER_STACK_L1_OUT],
            l1_weights: AlignedBox::new_zeroed(LAYER_STACK_L1_OUT * Self::L1_PADDED_INPUT),
            l2_biases: [0; NNUE_PYTORCH_L3],
            l2_weights: AlignedBox::new_zeroed(NNUE_PYTORCH_L3 * Self::L2_PADDED_INPUT),
            output_bias: 0,
            output_weights: AlignedBox::new_zeroed(Self::OUTPUT_PADDED_INPUT),
        }
    }

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let mut bucket = Self::new();

        // L1層: bias
        let mut buf4 = [0u8; 4];
        for bias in bucket.l1_biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // L1層: weights
        let mut buf1 = [0u8; 1];
        for i in 0..(LAYER_STACK_L1_OUT * Self::L1_PADDED_INPUT) {
            reader.read_exact(&mut buf1)?;
            bucket.l1_weights[i] = buf1[0] as i8;
        }

        // L2層: bias
        for bias in bucket.l2_biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // L2層: weights
        for i in 0..(NNUE_PYTORCH_L3 * Self::L2_PADDED_INPUT) {
            reader.read_exact(&mut buf1)?;
            bucket.l2_weights[i] = buf1[0] as i8;
        }

        // 出力層: bias
        reader.read_exact(&mut buf4)?;
        bucket.output_bias = i32::from_le_bytes(buf4);

        // 出力層: weights
        for i in 0..Self::OUTPUT_PADDED_INPUT {
            reader.read_exact(&mut buf1)?;
            bucket.output_weights[i] = buf1[0] as i8;
        }

        Ok(bucket)
    }

    /// 順伝播
    ///
    /// 入力: SqrClippedReLU後の1536次元 (u8)
    /// 出力: スケーリング前の生スコア (i32)
    pub fn propagate(&self, input: &[u8; NNUE_PYTORCH_L1]) -> i32 {
        // L1: 1536 → 16
        let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
        self.propagate_l1(input, &mut l1_out);

        // Split: [15, 1]
        // l1_x: 最初の15要素、l1_skip: 最後の1要素
        let l1_skip = l1_out[NNUE_PYTORCH_L2]; // index 15

        // ClippedReLU + Sqr for first 15 elements, then concat with original
        // l1x_ = clamp(cat([pow(l1x_, 2) * (127/128), l1x_]), 0, 1)
        // 量子化: i32 >> 6 → clamp(0, 127)
        let mut l2_input = [0u8; LAYER_STACK_L2_IN]; // 30

        for i in 0..NNUE_PYTORCH_L2 {
            // 15
            let val = l1_out[i] >> 6; // WEIGHT_SCALE_BITS
            let clamped = val.clamp(0, 127) as u8;

            // 二乗部分: (clamped^2) * (127/128) ≈ (clamped^2) >> 7
            let sqr = ((clamped as u32 * clamped as u32) >> 7).min(127) as u8;
            l2_input[i] = sqr; // 最初の15要素: 二乗
            l2_input[NNUE_PYTORCH_L2 + i] = clamped; // 次の15要素: 元の値
        }

        // L2: 30 → 32
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        self.propagate_l2(&l2_input, &mut l2_out);

        // ClippedReLU
        let mut l2_relu = [0u8; NNUE_PYTORCH_L3];
        for i in 0..NNUE_PYTORCH_L3 {
            let val = l2_out[i] >> 6;
            l2_relu[i] = val.clamp(0, 127) as u8;
        }

        // Output: 32 → 1
        let output = self.propagate_output(&l2_relu);

        // Skip connection
        output + l1_skip
    }

    #[inline]
    fn propagate_l1(&self, input: &[u8; NNUE_PYTORCH_L1], output: &mut [i32; LAYER_STACK_L1_OUT]) {
        output.copy_from_slice(&self.l1_biases);
        for (i, &in_val) in input.iter().enumerate() {
            let in_i32 = in_val as i32;
            for (j, out) in output.iter_mut().enumerate() {
                let weight_idx = j * Self::L1_PADDED_INPUT + i;
                *out += self.l1_weights[weight_idx] as i32 * in_i32;
            }
        }
    }

    #[inline]
    fn propagate_l2(&self, input: &[u8; LAYER_STACK_L2_IN], output: &mut [i32; NNUE_PYTORCH_L3]) {
        output.copy_from_slice(&self.l2_biases);
        for (i, &in_val) in input.iter().enumerate() {
            let in_i32 = in_val as i32;
            for (j, out) in output.iter_mut().enumerate() {
                let weight_idx = j * Self::L2_PADDED_INPUT + i;
                *out += self.l2_weights[weight_idx] as i32 * in_i32;
            }
        }
    }

    #[inline]
    fn propagate_output(&self, input: &[u8; NNUE_PYTORCH_L3]) -> i32 {
        let mut sum = self.output_bias;
        for (i, &in_val) in input.iter().enumerate() {
            sum += self.output_weights[i] as i32 * in_val as i32;
        }
        sum
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
        self.propagate_l1(input, &mut l1_out);

        // Split: [15, 1]
        let l1_skip = l1_out[NNUE_PYTORCH_L2]; // index 15

        // ClippedReLU + Sqr for first 15 elements
        let mut l2_input = [0u8; LAYER_STACK_L2_IN];
        for i in 0..NNUE_PYTORCH_L2 {
            let val = l1_out[i] >> 6;
            let clamped = val.clamp(0, 127) as u8;
            // (clamped^2) * (127/128) ≈ (clamped^2) >> 7
            let sqr = ((clamped as u32 * clamped as u32) >> 7).min(127) as u8;
            l2_input[i] = sqr;
            l2_input[NNUE_PYTORCH_L2 + i] = clamped;
        }

        // L2: 30 → 32
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        self.propagate_l2(&l2_input, &mut l2_out);

        // ClippedReLU
        let mut l2_relu = [0u8; NNUE_PYTORCH_L3];
        for i in 0..NNUE_PYTORCH_L3 {
            let val = l2_out[i] >> 6;
            l2_relu[i] = val.clamp(0, 127) as u8;
        }

        // Output: 32 → 1
        let output = self.propagate_output(&l2_relu);

        // Skip connection
        let raw_score = output + l1_skip;

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

    /// 評価値を計算
    ///
    /// bucket_index: 局面に応じて選択されたバケットインデックス (0-8)
    /// input: SqrClippedReLU後の1536次元ベクトル
    pub fn evaluate(&self, bucket_index: usize, input: &[u8; NNUE_PYTORCH_L1]) -> i32 {
        debug_assert!(bucket_index < NUM_LAYER_STACK_BUCKETS);
        let output = self.buckets[bucket_index].propagate(input);
        output / NNUE_PYTORCH_NNUE2SCORE
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

/// SqrClippedReLU 変換
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
    // 両視点を連結して 3072次元に
    // us視点: [us_acc, them_acc]
    // → ClippedReLU → 4分割 → ペア乗算 → 1536次元

    let half = NNUE_PYTORCH_L1 / 2; // 768

    // 前半768要素: us_acc[0..768] * us_acc[768..1536]
    // 後半768要素: them_acc[0..768] * them_acc[768..1536]
    for i in 0..half {
        // us側
        let us_a = (us_acc[i] as i32).clamp(0, 127) as u32;
        let us_b = (us_acc[half + i] as i32).clamp(0, 127) as u32;
        // (a * b) * (127/128) ≈ (a * b) >> 7
        let us_prod = ((us_a * us_b) >> 7).min(127);
        output[i] = us_prod as u8;

        // them側
        let them_a = (them_acc[i] as i32).clamp(0, 127) as u32;
        let them_b = (them_acc[half + i] as i32).clamp(0, 127) as u32;
        let them_prod = ((them_a * them_b) >> 7).min(127);
        output[half + i] = them_prod as u8;
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
        assert_eq!(bucket.l1_biases.len(), LAYER_STACK_L1_OUT);
        assert_eq!(bucket.l2_biases.len(), NNUE_PYTORCH_L3);
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
}
