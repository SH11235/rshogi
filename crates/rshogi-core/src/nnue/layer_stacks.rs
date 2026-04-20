//! nnue-pytorch LayerStacks アーキテクチャ
//!
//! nnue-pytorch の LayerStacks 構造を実装する。
//! 9個のバケットを持ち、局面に応じてバケットを選択して推論を行う。
//!
//! ## アーキテクチャ
//!
//! ```text
//! Feature Transformer: 73,305 → L1 (1536 or 768)
//! SqrClippedReLU: L1*2 → L1
//! LayerStacks (bucket選択後):
//!   L1: L1 → 16, split [15, 1]
//!   Sqr(15) → 30
//!   L2: 30 → 32, ReLU
//!   Output: 32 → 1 + skip
//! ```

use super::accumulator::Aligned;
use super::constants::{
    LAYER_STACK_L1_OUT, LAYER_STACK_L2_IN, NNUE_PYTORCH_L2, NNUE_PYTORCH_L3,
    NUM_LAYER_STACK_BUCKETS,
};
#[cfg(feature = "nnue-hand-count-dense")]
use super::hand_count::HAND_COUNT_DIMS;
use super::layers::AffineTransform;
use std::io::{self, Read};

/// L2 入力のパディング済み次元数（padded_input(30) = 32）
const L2_PADDED_INPUT: usize = super::layers::padded_input(LAYER_STACK_L2_IN);

/// Output 入力のパディング済み次元数（padded_input(32) = 32）
const OUTPUT_PADDED_INPUT: usize = super::layers::padded_input(NNUE_PYTORCH_L3);

/// LayerStack L1 層 (`AffineTransform<L1, LAYER_STACK_L1_OUT=16>`) の重み index を
/// SIMD 有効ビルドの scramble 形式に変換する。
///
/// `AffineTransform<_, 16>::should_use_scrambled_weights()` は AVX2 (OUT % 8 == 0) /
/// SSSE3 (OUT % 4 == 0) ビルドで常に `true` になる。scramble は
/// `i = output * padded_input + input` 形式の linear index を input_chunk ベースの
/// レイアウトに変換する:
///
/// ```text
/// scrambled = (i / CHUNK_SIZE) % (padded_input / CHUNK_SIZE) * OUTPUT_DIM * CHUNK_SIZE
///           + (i / padded_input) * CHUNK_SIZE
///           + i % CHUNK_SIZE
/// ```
///
/// `AffineTransform::get_weight_index_scrambled` と同じ式だが、こちらは
/// `read_with_hand_count` から参照するため module-level const fn として用意する。
#[cfg(feature = "nnue-hand-count-dense")]
#[inline]
const fn scrambled_l1_weight_index(i: usize, padded_input: usize) -> usize {
    #[cfg(any(
        all(target_arch = "x86_64", target_feature = "avx2"),
        all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        )
    ))]
    {
        // AVX2 環境: CHUNK_SIZE = 4、OUTPUT_DIM (=16) % 8 == 0 で scramble 有効
        // SSSE3 環境: CHUNK_SIZE = 4、OUTPUT_DIM (=16) % 4 == 0 で scramble 有効
        const CHUNK_SIZE: usize = 4;
        const OUTPUT_DIM: usize = LAYER_STACK_L1_OUT;
        (i / CHUNK_SIZE) % (padded_input / CHUNK_SIZE) * OUTPUT_DIM * CHUNK_SIZE
            + (i / padded_input) * CHUNK_SIZE
            + (i % CHUNK_SIZE)
    }
    #[cfg(not(any(
        all(target_arch = "x86_64", target_feature = "avx2"),
        all(
            target_arch = "x86_64",
            target_feature = "ssse3",
            not(target_feature = "avx2")
        )
    )))]
    {
        let _ = padded_input;
        i
    }
}

#[cfg(test)]
fn sqr_clipped_relu_explicit<const DIM: usize>(input: &[i32; DIM], output: &mut [u8; DIM]) {
    for i in 0..DIM {
        output[i] = ((input[i] as i64 * input[i] as i64) >> 19).clamp(0, 127) as u8;
    }
}

// =============================================================================
// LayerStack 単一バケット
// =============================================================================

/// HandCount Dense L1 重み (bucket ごと)
///
/// レイアウト: `weights[out_idx][in_idx]`（row-major, `LAYER_STACK_L1_OUT × HAND_COUNT_DIMS`）
///
/// 量子化: bullet 側は `QB = 64` で i8 化する（FT L1 主経路と同じスケール）。
/// 一方 FT パスは入力が u8 (scale QA=127) × 重み i8 (scale QB=64) = scale 8128 で
/// L1 出力に寄与するのに対し、HandCount パスは入力が raw i16 × 重み i8 (scale QB=64) =
/// scale 64 なので 127 倍のスケールギャップが生じる。`propagate_with_hand_count` 内で
/// HC 寄与に `× 127` を掛けて FT と同じ scale に揃えることで float 推論と一致させる。
#[cfg(feature = "nnue-hand-count-dense")]
pub struct HandCountL1Weights {
    /// weights[out_idx][in_idx]: i8 (量子化スケール QB=64)
    pub weights: [[i8; HAND_COUNT_DIMS]; LAYER_STACK_L1_OUT],
}

/// LayerStack 単一バケットの層
///
/// 各バケットは以下の構造を持つ:
/// - L1: L1 → 16
/// - L2: 30 → 32
/// - Output: 32 → 1
///
/// 各層は `AffineTransform` を使用し、AVX512/AVX2/SSSE3/WASM SIMD128 に対応。
pub struct LayerStackBucket<const L1: usize> {
    /// L1層: L1 → 16
    pub l1: AffineTransform<L1, LAYER_STACK_L1_OUT>,
    /// L2層: 30 → 32
    pub l2: AffineTransform<LAYER_STACK_L2_IN, NNUE_PYTORCH_L3>,
    /// 出力層: 32 → 1
    pub output: AffineTransform<NNUE_PYTORCH_L3, 1>,
    /// HandCount Dense 入力の L1 層重み (`HandCountDense=14` モデル時のみ `Some`)
    #[cfg(feature = "nnue-hand-count-dense")]
    pub l1_hand_count: Option<HandCountL1Weights>,
}

impl<const L1: usize> LayerStackBucket<L1> {
    /// 新規作成（ゼロ初期化）
    pub fn new() -> Self {
        Self {
            l1: AffineTransform::new(),
            l2: AffineTransform::new(),
            output: AffineTransform::new(),
            #[cfg(feature = "nnue-hand-count-dense")]
            l1_hand_count: None,
        }
    }

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let l1 = AffineTransform::read(reader)?;
        let l2 = AffineTransform::read(reader)?;
        let output = AffineTransform::read(reader)?;
        Ok(Self {
            l1,
            l2,
            output,
            #[cfg(feature = "nnue-hand-count-dense")]
            l1_hand_count: None,
        })
    }

    /// HandCount Dense 付きで読み込み
    ///
    /// bullet 側の save format (feat/nnue-hand-count-dense) は L1 重みを 1 行あたり
    /// `pad32(ft_out + hc_dims)` byte で書き出す。byte layout は以下:
    ///
    /// ```text
    /// [biases: 16 × i32 LE]
    /// for out_idx in 0..16:
    ///   [0..ft_padded)            : FT L1 重み (bucket_w + shared_w を i8 量子化)
    ///   [ft_padded..ft_padded+hc) : HandCount 重み (bucket_w のみを i8 量子化)
    ///   [ft_padded+hc..total_pad) : padding (0)
    /// ```
    ///
    /// `ft_padded = pad32(L1)`、`total_pad = pad32(L1 + hc_dims)`。
    /// 既存 `AffineTransform::read` は 1 行 = `ft_padded` byte を想定しているため、
    /// HandCount 行では行境界がずれる。本関数は手動で byte stream を main / HC /
    /// padding に振り分け、main は `AffineTransform::read` と同じ scramble を適用し、
    /// HC は row-major `[OUT][HC_DIMS]` に格納する。
    #[cfg(feature = "nnue-hand-count-dense")]
    pub fn read_with_hand_count<R: Read>(reader: &mut R, hc_dims: usize) -> io::Result<Self> {
        use super::accumulator::AlignedBox;
        use super::layers::padded_input;

        if hc_dims != HAND_COUNT_DIMS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("HandCountDense dims mismatch: model={hc_dims}, engine={HAND_COUNT_DIMS}"),
            ));
        }

        // L1 biases (i32 LE × 16)
        let mut l1_biases = [0i32; LAYER_STACK_L1_OUT];
        let mut buf4 = [0u8; 4];
        for bias in l1_biases.iter_mut() {
            reader.read_exact(&mut buf4)?;
            *bias = i32::from_le_bytes(buf4);
        }

        // L1 weights: per-row = ft_padded FT bytes + hc_dims HC bytes + pad_extra padding bytes
        let ft_padded = padded_input(L1);
        let total_padded = padded_input(L1 + hc_dims);
        if total_padded < ft_padded + hc_dims {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "HandCountDense padding invariant violated: ft_padded={ft_padded}, hc_dims={hc_dims}, total_padded={total_padded}"
                ),
            ));
        }
        let pad_extra = total_padded - ft_padded - hc_dims;

        let weight_size = LAYER_STACK_L1_OUT * ft_padded;
        let mut main_weights = AlignedBox::<i8>::new_zeroed(weight_size);
        let mut hc_weights = [[0i8; HAND_COUNT_DIMS]; LAYER_STACK_L1_OUT];

        let mut buf1 = [0u8; 1];
        for (out_idx, hc_row) in hc_weights.iter_mut().enumerate() {
            // Main FT part
            for in_idx in 0..ft_padded {
                reader.read_exact(&mut buf1)?;
                let linear = out_idx * ft_padded + in_idx;
                let target = scrambled_l1_weight_index(linear, ft_padded);
                main_weights[target] = buf1[0] as i8;
            }
            // HandCount part
            for hc_cell in hc_row.iter_mut().take(hc_dims) {
                reader.read_exact(&mut buf1)?;
                *hc_cell = buf1[0] as i8;
            }
            // padding (0)
            for _ in 0..pad_extra {
                reader.read_exact(&mut buf1)?;
            }
        }

        let l1 = AffineTransform::<L1, LAYER_STACK_L1_OUT> {
            biases: l1_biases,
            weights: main_weights,
        };

        // L2 / Output は通常の AffineTransform::read（byte layout 変更なし）
        let l2 = AffineTransform::read(reader)?;
        let output = AffineTransform::read(reader)?;

        Ok(Self {
            l1,
            l2,
            output,
            l1_hand_count: Some(HandCountL1Weights {
                weights: hc_weights,
            }),
        })
    }

    /// 順伝播
    ///
    /// 入力: SqrClippedReLU後のL1次元 (u8)
    /// 出力: スケーリング前の生スコア (i32)
    ///
    /// `nnue-hand-count-dense` feature 有効時は `propagate_with_hand_count(input, None)`
    /// のラッパーとして動作する。
    pub fn propagate(&self, input: &[u8; L1]) -> i32 {
        #[cfg(feature = "nnue-hand-count-dense")]
        {
            self.propagate_with_hand_count(input, None)
        }
        #[cfg(not(feature = "nnue-hand-count-dense"))]
        {
            self.propagate_no_hand_count(input)
        }
    }

    /// 順伝播（HandCount Dense 寄与込み）
    ///
    /// `hand_count` が `Some` かつ `l1_hand_count` が `Some` のとき、L1 主経路の結果に
    /// `sum_i hand_count[i] * hc_w[k][i] * 127` を加算する。× 127 は FT 入力の QA=127
    /// スケールを吸収するための補正。詳細は `HandCountL1Weights` の docstring 参照。
    #[cfg(feature = "nnue-hand-count-dense")]
    pub fn propagate_with_hand_count(
        &self,
        input: &[u8; L1],
        hand_count: Option<&[i16; HAND_COUNT_DIMS]>,
    ) -> i32 {
        let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
        let mut l2_input = Aligned([0u8; L2_PADDED_INPUT]);
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        let mut output_arr = [0i32; 1];

        // L1: L1 → 16
        self.l1.propagate(input, &mut l1_out);

        // HandCount Dense の寄与
        //
        // Scale correction: FT 寄与は u8(127) × i8(64) = 8128、HC 寄与は raw × i8(64) = 64
        // なので HC 側に × 127 を乗じて scale を 8128 に揃える。
        // 最大値: |hc[i]| ≤ 18, |i8| ≤ 128, 14 terms → |partial| ≤ 32,256
        //         × 127 → 4,096,512 < i32::MAX (overflow しない)
        if let (Some(hc), Some(hcw)) = (hand_count, self.l1_hand_count.as_ref()) {
            for (l1_cell, hcw_row) in l1_out.iter_mut().zip(hcw.weights.iter()) {
                let partial: i32 =
                    hc.iter().zip(hcw_row.iter()).map(|(&h, &w)| (h as i32) * (w as i32)).sum();
                *l1_cell += partial * 127;
            }
        }

        // Split: [15, 1]
        let l1_skip = l1_out[NNUE_PYTORCH_L2];

        l1_sqr_clipped_relu_activation(&l1_out, &mut l2_input.0);

        self.l2.propagate(&l2_input.0, &mut l2_out);
        clipped_relu_i32_to_u8(&l2_out, &mut l2_relu.0);

        self.output.propagate(&l2_relu.0, &mut output_arr);

        output_arr[0] + l1_skip
    }

    /// `propagate` の内部実装（nnue-hand-count-dense feature 無効時）。
    ///
    /// feature 有効時は `propagate_with_hand_count` がメイン実装、`propagate` は
    /// その `None` 呼び出しのラッパーとして機能する。
    #[cfg(not(feature = "nnue-hand-count-dense"))]
    fn propagate_no_hand_count(&self, input: &[u8; L1]) -> i32 {
        let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
        let mut l2_input = Aligned([0u8; L2_PADDED_INPUT]);
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        let mut output_arr = [0i32; 1];

        self.l1.propagate(input, &mut l1_out);

        let l1_skip = l1_out[NNUE_PYTORCH_L2];

        l1_sqr_clipped_relu_activation(&l1_out, &mut l2_input.0);

        self.l2.propagate(&l2_input.0, &mut l2_out);
        clipped_relu_i32_to_u8(&l2_out, &mut l2_relu.0);

        self.output.propagate(&l2_relu.0, &mut output_arr);

        output_arr[0] + l1_skip
    }

    /// 順伝播（診断情報付き）
    ///
    /// 戻り値: (raw_score, l1_out, l1_skip)
    ///
    /// `nnue-hand-count-dense` feature 有効時は `propagate_with_diagnostics_with_hand_count(input, None)`
    /// のラッパーとして動作し、HC 入力なしの従来動作を保つ。
    #[cfg(feature = "diagnostics")]
    pub fn propagate_with_diagnostics(
        &self,
        input: &[u8; L1],
    ) -> (i32, [i32; LAYER_STACK_L1_OUT], i32) {
        #[cfg(feature = "nnue-hand-count-dense")]
        {
            self.propagate_with_diagnostics_with_hand_count(input, None)
        }
        #[cfg(not(feature = "nnue-hand-count-dense"))]
        {
            self.propagate_with_diagnostics_no_hand_count(input)
        }
    }

    /// 順伝播（診断情報付き、HandCount Dense 寄与込み）
    #[cfg(all(feature = "diagnostics", feature = "nnue-hand-count-dense"))]
    pub fn propagate_with_diagnostics_with_hand_count(
        &self,
        input: &[u8; L1],
        hand_count: Option<&[i16; HAND_COUNT_DIMS]>,
    ) -> (i32, [i32; LAYER_STACK_L1_OUT], i32) {
        let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
        let mut l2_input = Aligned([0u8; L2_PADDED_INPUT]);
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        let mut output_arr = [0i32; 1];

        self.l1.propagate(input, &mut l1_out);

        // HandCount Dense 寄与 (propagate_with_hand_count と同一処理)
        if let (Some(hc), Some(hcw)) = (hand_count, self.l1_hand_count.as_ref()) {
            for (l1_cell, hcw_row) in l1_out.iter_mut().zip(hcw.weights.iter()) {
                let partial: i32 =
                    hc.iter().zip(hcw_row.iter()).map(|(&h, &w)| (h as i32) * (w as i32)).sum();
                *l1_cell += partial * 127;
            }
        }

        let l1_skip = l1_out[NNUE_PYTORCH_L2];
        l1_sqr_clipped_relu_activation(&l1_out, &mut l2_input.0);

        self.l2.propagate(&l2_input.0, &mut l2_out);
        clipped_relu_i32_to_u8(&l2_out, &mut l2_relu.0);

        self.output.propagate(&l2_relu.0, &mut output_arr);

        let raw_score = output_arr[0] + l1_skip;
        (raw_score, l1_out, l1_skip)
    }

    /// 順伝播（診断情報付き、HandCount なし: feature OFF 時の実体）
    #[cfg(all(feature = "diagnostics", not(feature = "nnue-hand-count-dense")))]
    fn propagate_with_diagnostics_no_hand_count(
        &self,
        input: &[u8; L1],
    ) -> (i32, [i32; LAYER_STACK_L1_OUT], i32) {
        let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
        let mut l2_input = Aligned([0u8; L2_PADDED_INPUT]);
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        let mut output_arr = [0i32; 1];

        self.l1.propagate(input, &mut l1_out);

        let l1_skip = l1_out[NNUE_PYTORCH_L2];
        l1_sqr_clipped_relu_activation(&l1_out, &mut l2_input.0);

        self.l2.propagate(&l2_input.0, &mut l2_out);
        clipped_relu_i32_to_u8(&l2_out, &mut l2_relu.0);

        self.output.propagate(&l2_relu.0, &mut output_arr);

        let raw_score = output_arr[0] + l1_skip;
        (raw_score, l1_out, l1_skip)
    }
}

impl<const L1: usize> Default for LayerStackBucket<L1> {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// LayerStacks (9バケット)
// =============================================================================

/// LayerStacks: 9個のバケットを持つ構造
pub struct LayerStacks<const L1: usize> {
    /// 9個のバケット
    pub buckets: [LayerStackBucket<L1>; NUM_LAYER_STACK_BUCKETS],
    /// HandCount Dense 入力が読み込まれているか
    ///
    /// `NetworkLayerStacks::evaluate_with_bucket` で HC 抽出を分岐するために使用。
    /// 全バケットが一括でロードされるため、`buckets[0].l1_hand_count.is_some()` と
    /// 同値だがホットパスで `is_some()` を呼ばないようにここにキャッシュする。
    #[cfg(feature = "nnue-hand-count-dense")]
    pub has_hand_count: bool,
}

impl<const L1: usize> LayerStacks<L1> {
    /// 新規作成
    pub fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| LayerStackBucket::new()),
            #[cfg(feature = "nnue-hand-count-dense")]
            has_hand_count: false,
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

    /// HandCount Dense 付きでファイルから読み込み
    ///
    /// `HandCountDense=hc_dims,` を arch_str に含むモデル向け。各バケットの L1 重みを
    /// `LayerStackBucket::read_with_hand_count` で読み込み、`has_hand_count` を `true` に
    /// セットする。
    #[cfg(feature = "nnue-hand-count-dense")]
    pub fn read_with_hand_count<R: Read>(reader: &mut R, hc_dims: usize) -> io::Result<Self> {
        let mut stacks = Self::new();
        let mut buf4 = [0u8; 4];

        for bucket in stacks.buckets.iter_mut() {
            reader.read_exact(&mut buf4)?;
            let _fc_hash = u32::from_le_bytes(buf4);
            *bucket = LayerStackBucket::read_with_hand_count(reader, hc_dims)?;
        }

        stacks.has_hand_count = true;
        Ok(stacks)
    }

    /// 生スコアを計算（スケーリング前）
    pub fn evaluate_raw(&self, bucket_index: usize, input: &[u8; L1]) -> i32 {
        debug_assert!(bucket_index < NUM_LAYER_STACK_BUCKETS);
        // SAFETY: bucket_index は progress_sum_to_bucket() または clamp(0, NUM-1) 由来で
        //         常に NUM_LAYER_STACK_BUCKETS 未満。
        unsafe { self.buckets.get_unchecked(bucket_index) }.propagate(input)
    }

    /// 生スコアを計算（HandCount Dense 寄与込み）
    ///
    /// `has_hand_count == true` のときに `NetworkLayerStacks::evaluate_with_bucket` から
    /// 呼ばれる。`hand_count` が `None` の場合は `propagate` と同じ結果になる。
    #[cfg(feature = "nnue-hand-count-dense")]
    pub fn evaluate_raw_with_hand_count(
        &self,
        bucket_index: usize,
        input: &[u8; L1],
        hand_count: Option<&[i16; HAND_COUNT_DIMS]>,
    ) -> i32 {
        debug_assert!(bucket_index < NUM_LAYER_STACK_BUCKETS);
        // SAFETY: 同上
        unsafe { self.buckets.get_unchecked(bucket_index) }
            .propagate_with_hand_count(input, hand_count)
    }

    /// 生スコアを計算（診断情報付き）
    ///
    /// 戻り値: (raw_score, l1_out, l1_skip)
    #[cfg(feature = "diagnostics")]
    pub fn evaluate_raw_with_diagnostics(
        &self,
        bucket_index: usize,
        input: &[u8; L1],
    ) -> (i32, [i32; LAYER_STACK_L1_OUT], i32) {
        debug_assert!(bucket_index < NUM_LAYER_STACK_BUCKETS);
        self.buckets[bucket_index].propagate_with_diagnostics(input)
    }

    /// 生スコアを計算（診断情報付き、HandCount Dense 寄与込み）
    ///
    /// 戻り値: (raw_score, l1_out, l1_skip)
    #[cfg(all(feature = "diagnostics", feature = "nnue-hand-count-dense"))]
    pub fn evaluate_raw_with_diagnostics_with_hand_count(
        &self,
        bucket_index: usize,
        input: &[u8; L1],
        hand_count: Option<&[i16; HAND_COUNT_DIMS]>,
    ) -> (i32, [i32; LAYER_STACK_L1_OUT], i32) {
        debug_assert!(bucket_index < NUM_LAYER_STACK_BUCKETS);
        self.buckets[bucket_index].propagate_with_diagnostics_with_hand_count(input, hand_count)
    }
}

impl<const L1: usize> Default for LayerStacks<L1> {
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
/// L1→L2 activation: SqrClippedReLU + ClippedReLU（15要素 → 30 u8）
///
/// l1_out の最初の 15 要素 (NNUE_PYTORCH_L2) に対して:
/// - SqrClippedReLU: min(127, (input^2) >> 19) → l2_input[0..15]
/// - ClippedReLU:    clamp(input >> 6, 0, 127)  → l2_input[15..30]
///
/// 16番目の要素 (l1_skip) は呼び出し側で別途取得済み。
#[inline]
fn l1_sqr_clipped_relu_activation(l1_out: &[i32; LAYER_STACK_L1_OUT], l2_input: &mut [u8]) {
    // 16要素のみなので SIMD 化のメリットが小さく、スカラーで十分。
    // 注意: 二乗は i64 で計算する必要がある。
    // i32 乗算は |val| > ~46340 (sqrt(i32::MAX)) でオーバーフローし、
    // 中盤局面の L1 出力は数万〜数十万に達するため i64 が必須。
    for (i, &val) in l1_out.iter().enumerate().take(NNUE_PYTORCH_L2) {
        let input_val = val as i64;
        let sqr = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
        let clamped = (val >> 6).clamp(0, 127) as u8;
        l2_input[i] = sqr;
        l2_input[NNUE_PYTORCH_L2 + i] = clamped;
    }
}

/// L2→Output activation: ClippedReLU（32要素 i32 → u8）
///
/// clamp(input >> 6, 0, 127)
#[inline]
fn clipped_relu_i32_to_u8(input: &[i32; NNUE_PYTORCH_L3], output: &mut [u8]) {
    // AVX2: 32 i32 → 32 u8（8要素ずつ4回）
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    {
        // SAFETY:
        // - input は 32 要素（NNUE_PYTORCH_L3）
        // - output は OUTPUT_PADDED_INPUT(=32) 要素
        // - >>6 + clamp(0,127) の結果は [0, 127] → u8 に収まる
        unsafe {
            use std::arch::x86_64::*;
            let zero = _mm256_setzero_si256();
            let max127 = _mm256_set1_epi32(127);

            let in_ptr = input.as_ptr();
            let out_ptr = output.as_mut_ptr();

            // 8要素ずつ4回 = 32要素
            for chunk in 0..4 {
                let offset = chunk * 8;
                let v = _mm256_loadu_si256(in_ptr.add(offset) as *const __m256i);
                let shifted = _mm256_srai_epi32(v, 6);
                let clamped = _mm256_min_epi32(_mm256_max_epi32(shifted, zero), max127);

                // i32 → u8 パック
                let packed16 = _mm256_packs_epi32(clamped, clamped);
                let packed8 = _mm256_packus_epi16(packed16, packed16);
                let lo = _mm256_castsi256_si128(packed8);
                let hi = _mm256_extracti128_si256(packed8, 1);
                let combined = _mm_unpacklo_epi32(lo, hi);
                _mm_storel_epi64(out_ptr.add(offset) as *mut __m128i, combined);
            }
        }
    }

    // スカラーフォールバック
    #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
    {
        for (out, &val) in output.iter_mut().zip(input.iter()) {
            *out = (val >> 6).clamp(0, 127) as u8;
        }
    }
}

/// 入力: 両視点のアキュムレータ (各L1次元, i16)
/// 出力: SqrClippedReLU後のL1次元 (u8)
pub fn sqr_clipped_relu_transform<const L1: usize>(
    us_acc: &[i16; L1],
    them_acc: &[i16; L1],
    output: &mut [u8; L1],
) {
    let half = L1 / 2;

    // AVX512BW: 512bit = 32 x i16、2セット同時処理で 64 i16 → 64 u8
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    {
        // SAFETY:
        // - us_acc, them_acc: AccumulatorLayerStacks 内 Aligned<[i16; L1]> で 64 バイトアライン
        // - output: Aligned<[u8; L1]> で 64 バイトアライン
        // - half=L1/2, half/32 → 各ループで全要素カバー
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
    }

    // AVX2: 256bit = 16 x i16、2セット同時処理で 32 i16 → 32 u8
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx2",
        not(all(target_feature = "avx512f", target_feature = "avx512bw"))
    ))]
    {
        // SAFETY:
        // - us_acc, them_acc: AccumulatorLayerStacks 内 Aligned<[i16; L1]> で 64 バイトアライン
        // - output: Aligned<[u8; L1]> で 64 バイトアライン
        // - half=L1/2, half/32 → 各ループで全要素カバー
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
    }

    // スカラーフォールバック
    #[cfg(not(any(
        all(target_arch = "x86_64", target_feature = "sse2"),
        all(target_arch = "wasm32", target_feature = "simd128")
    )))]
    {
        // 前半 half 要素: us_acc[0..half] * us_acc[half..L1]
        // 後半 half 要素: them_acc[0..half] * them_acc[half..L1]
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
    use crate::nnue::accumulator::Aligned;
    use crate::nnue::constants::NNUE_PYTORCH_L1;
    use crate::nnue::layers::ClippedReLU;

    /// テスト用の具体的な L1 サイズ
    const TEST_L1: usize = NNUE_PYTORCH_L1; // 1536

    #[test]
    fn test_layer_stack_bucket_new() {
        let bucket = LayerStackBucket::<TEST_L1>::new();
        assert_eq!(bucket.l1.biases.len(), LAYER_STACK_L1_OUT);
        assert_eq!(bucket.l2.biases.len(), NNUE_PYTORCH_L3);
    }

    #[test]
    fn test_layer_stacks_new() {
        let stacks = LayerStacks::<TEST_L1>::new();
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
        let bucket = LayerStackBucket::<TEST_L1>::new(); // ゼロ初期化（weights=0, biases=0）

        // biases を設定して l1_out を制御する
        // l1_out = bias（weights が全 0 なので入力に依存しない）
        let mut bucket_with_biases = LayerStackBucket::<TEST_L1>::new();
        // index 0 の bias を 8192 に設定 → sqr = 127, 旧実装なら 126
        bucket_with_biases.l1.biases[0] = 8192;
        // index 1 の bias を 8128 に設定 → sqr = 126 (両方同じ)
        bucket_with_biases.l1.biases[1] = 8128;

        let input = Aligned([0u8; TEST_L1]);
        let result = bucket_with_biases.propagate(&input.0);

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
        use super::super::accumulator::Aligned;

        // SIMD パス（AVX2/AVX512）は aligned load を使うため 64 バイトアラインが必要
        let mut us_acc = Aligned([0i16; TEST_L1]);
        let mut them_acc = Aligned([0i16; TEST_L1]);
        let mut output = Aligned([0u8; TEST_L1]);

        // 入力が0の場合、出力も0
        sqr_clipped_relu_transform(&us_acc.0, &them_acc.0, &mut output.0);
        assert!(
            output.0.iter().all(|&x| x == 0),
            "all zeros input should produce all zeros output"
        );

        // 最大値テスト: 127 * 127 >> 7 = 16129 >> 7 = 126
        let half = TEST_L1 / 2;
        for i in 0..half {
            us_acc.0[i] = 127;
            us_acc.0[half + i] = 127;
            them_acc.0[i] = 127;
            them_acc.0[half + i] = 127;
        }

        sqr_clipped_relu_transform(&us_acc.0, &them_acc.0, &mut output.0);

        // 期待値: (127 * 127) >> 7 = 126
        for (i, &val) in output.0.iter().enumerate().take(TEST_L1) {
            assert_eq!(val, 126, "max input should produce 126 at index {i}");
        }

        // 負の値はクランプされて0になる
        for i in 0..TEST_L1 {
            us_acc.0[i] = -100;
            them_acc.0[i] = -100;
        }

        sqr_clipped_relu_transform(&us_acc.0, &them_acc.0, &mut output.0);
        assert!(output.0.iter().all(|&x| x == 0), "negative input should be clamped to 0");
    }

    #[test]
    fn test_layer_stack_l2_input_matches_scalar_reference() {
        let cases = [
            [
                -50000, -40000, -33000, -32768, -32000, -1000, 0, 64, 724, 8128, 8192, 8256, 20000,
                32767, 40000, 50000,
            ],
            [
                -1, 1, 63, 127, 128, 255, 256, 4096, 8191, 8192, 16384, 24576, 32768, 40000, 65535,
                70000,
            ],
        ];

        for l1_out in cases {
            let mut l1_relu = [0u8; LAYER_STACK_L1_OUT];
            let mut l2_input_opt = Aligned([0u8; L2_PADDED_INPUT]);
            let mut l2_sqr = [0u8; LAYER_STACK_L1_OUT];

            ClippedReLU::<LAYER_STACK_L1_OUT>::propagate(&l1_out, &mut l1_relu);
            sqr_clipped_relu_explicit::<LAYER_STACK_L1_OUT>(&l1_out, &mut l2_sqr);
            l2_input_opt.0[..LAYER_STACK_L1_OUT].copy_from_slice(&l2_sqr);
            l2_input_opt.0[NNUE_PYTORCH_L2..NNUE_PYTORCH_L2 + NNUE_PYTORCH_L2]
                .copy_from_slice(&l1_relu[..NNUE_PYTORCH_L2]);

            let mut l2_input_ref = Aligned([0u8; L2_PADDED_INPUT]);
            for (i, &val) in l1_out.iter().enumerate().take(NNUE_PYTORCH_L2) {
                let input_val = i64::from(val);
                l2_input_ref.0[i] = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
                l2_input_ref.0[NNUE_PYTORCH_L2 + i] = (val >> 6).clamp(0, 127) as u8;
            }

            assert_eq!(
                l2_input_opt.0, l2_input_ref.0,
                "optimized l2_input must match scalar reference for l1_out={l1_out:?}"
            );
        }
    }

    #[test]
    fn test_layer_stack_l2_relu_matches_scalar_reference() {
        let input = [
            -50000, -40000, -33000, -32768, -32000, -1000, -1, 0, 1, 63, 64, 127, 128, 255, 256,
            4096, 8191, 8192, 16384, 24576, 32767, 32768, 40000, 50000, 65535, 70000, 80000, 90000,
            100000, 110000, 120000, 130000,
        ];
        let mut opt = [0u8; NNUE_PYTORCH_L3];
        let mut reference = [0u8; NNUE_PYTORCH_L3];

        ClippedReLU::<NNUE_PYTORCH_L3>::propagate(&input, &mut opt);
        for (dst, &value) in reference.iter_mut().zip(input.iter()) {
            *dst = (value >> 6).clamp(0, 127) as u8;
        }

        assert_eq!(opt, reference);
    }

    #[test]
    fn test_layer_stack_bucket_propagate_matches_scalar_reference() {
        fn affine_from_bytes<const INPUT_DIM: usize, const OUTPUT_DIM: usize>(
            biases: [i32; OUTPUT_DIM],
            weights: &[i8],
        ) -> AffineTransform<INPUT_DIM, OUTPUT_DIM> {
            let mut bytes = Vec::with_capacity(OUTPUT_DIM * 4 + weights.len());
            for bias in biases {
                bytes.extend_from_slice(&bias.to_le_bytes());
            }
            for &weight in weights {
                bytes.push(weight as u8);
            }
            AffineTransform::<INPUT_DIM, OUTPUT_DIM>::read(&mut &bytes[..]).unwrap()
        }

        fn scalar_reference(bucket: &LayerStackBucket<TEST_L1>, input: &[u8; TEST_L1]) -> i32 {
            let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
            bucket.l1.propagate(input, &mut l1_out);
            let l1_skip = l1_out[NNUE_PYTORCH_L2];

            let mut l2_input = Aligned([0u8; L2_PADDED_INPUT]);
            for (i, &val) in l1_out.iter().enumerate().take(NNUE_PYTORCH_L2) {
                let input_val = i64::from(val);
                l2_input.0[i] = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
                l2_input.0[NNUE_PYTORCH_L2 + i] = (val >> 6).clamp(0, 127) as u8;
            }

            let mut l2_out = [0i32; NNUE_PYTORCH_L3];
            bucket.l2.propagate(&l2_input.0, &mut l2_out);

            let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
            for (dst, &val) in l2_relu.0.iter_mut().zip(l2_out.iter()) {
                *dst = (val >> 6).clamp(0, 127) as u8;
            }

            let mut output_arr = [0i32; 1];
            bucket.output.propagate(&l2_relu.0, &mut output_arr);
            output_arr[0] + l1_skip
        }

        let l1_biases = [
            -50000, -40000, -33000, -32768, -32000, -1000, 0, 64, 724, 8128, 8192, 8256, 20000,
            32767, 40000, 50000,
        ];
        let l1_weights = vec![0i8; LAYER_STACK_L1_OUT * TEST_L1];

        let mut l2_biases = [0i32; NNUE_PYTORCH_L3];
        for (i, bias) in l2_biases.iter_mut().enumerate() {
            *bias = (i as i32 - 16) * 37;
        }
        let mut l2_weights = vec![0i8; NNUE_PYTORCH_L3 * L2_PADDED_INPUT];
        for (i, weight) in l2_weights.iter_mut().enumerate() {
            *weight = ((i as i32 % 7) - 3) as i8;
        }

        let output_biases = [123i32; 1];
        let mut output_weights = vec![0i8; OUTPUT_PADDED_INPUT];
        for (i, weight) in output_weights.iter_mut().enumerate() {
            *weight = ((i as i32 % 5) - 2) as i8;
        }

        let bucket = LayerStackBucket {
            l1: affine_from_bytes::<TEST_L1, LAYER_STACK_L1_OUT>(l1_biases, &l1_weights),
            l2: affine_from_bytes::<LAYER_STACK_L2_IN, NNUE_PYTORCH_L3>(l2_biases, &l2_weights),
            output: affine_from_bytes::<NNUE_PYTORCH_L3, 1>(output_biases, &output_weights),
            #[cfg(feature = "nnue-hand-count-dense")]
            l1_hand_count: None,
        };

        let input = Aligned([0u8; TEST_L1]);
        let mut l1_out = [0i32; LAYER_STACK_L1_OUT];
        let mut l1_relu = [0u8; LAYER_STACK_L1_OUT];
        let mut l2_input_opt = Aligned([0u8; L2_PADDED_INPUT]);
        let mut l2_input_ref = Aligned([0u8; L2_PADDED_INPUT]);
        let mut l2_sqr = [0u8; LAYER_STACK_L1_OUT];
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        let mut l2_relu_opt = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        let mut l2_relu_ref = Aligned([0u8; OUTPUT_PADDED_INPUT]);

        bucket.l1.propagate(&input.0, &mut l1_out);
        ClippedReLU::<LAYER_STACK_L1_OUT>::propagate(&l1_out, &mut l1_relu);
        sqr_clipped_relu_explicit::<LAYER_STACK_L1_OUT>(&l1_out, &mut l2_sqr);
        l2_input_opt.0[..LAYER_STACK_L1_OUT].copy_from_slice(&l2_sqr);
        l2_input_opt.0[NNUE_PYTORCH_L2..NNUE_PYTORCH_L2 + NNUE_PYTORCH_L2]
            .copy_from_slice(&l1_relu[..NNUE_PYTORCH_L2]);

        for (i, &val) in l1_out.iter().enumerate().take(NNUE_PYTORCH_L2) {
            let input_val = i64::from(val);
            l2_input_ref.0[i] = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
            l2_input_ref.0[NNUE_PYTORCH_L2 + i] = (val >> 6).clamp(0, 127) as u8;
        }
        assert_eq!(l2_input_opt.0, l2_input_ref.0);

        bucket.l2.propagate(&l2_input_opt.0, &mut l2_out);
        ClippedReLU::<NNUE_PYTORCH_L3>::propagate(&l2_out, &mut l2_relu_opt.0);
        for (dst, &val) in l2_relu_ref.0.iter_mut().zip(l2_out.iter()) {
            *dst = (val >> 6).clamp(0, 127) as u8;
        }
        assert_eq!(l2_relu_opt.0, l2_relu_ref.0);

        let mut output_arr = [0i32; 1];
        bucket.output.propagate(&l2_relu_opt.0, &mut output_arr);
        let optimized_inline = output_arr[0] + l1_out[NNUE_PYTORCH_L2];

        let optimized = bucket.propagate(&input.0);
        let reference = scalar_reference(&bucket, &input.0);

        assert_eq!(optimized_inline, reference);
        assert_eq!(optimized, reference);
    }

    /// l1_out の値が大きい場合（i32 乗算でオーバーフローするケース）の回帰テスト。
    /// PR #416 で修正した AVX2 パスの i32 オーバーフローが再発しないことを確認。
    #[test]
    fn test_l1_sqr_clipped_relu_activation_large_values() {
        // |val| = 50000 のとき i32 乗算は 2_500_000_000 > i32::MAX でオーバーフローする
        let l1_out = [50_000i32; LAYER_STACK_L1_OUT];
        let mut l2_input = [0u8; L2_PADDED_INPUT];
        l1_sqr_clipped_relu_activation(&l1_out, &mut l2_input);
        // SqrClippedReLU: (50000^2 >> 19) = 4768 → clamp → 127
        assert_eq!(l2_input[0], 127, "SqrClippedReLU should saturate to 127");
        // ClippedReLU: 50000 >> 6 = 781 → clamp → 127
        assert_eq!(l2_input[NNUE_PYTORCH_L2], 127, "ClippedReLU should saturate to 127");
    }

    // =============================================================================
    // HandCount Dense 関連テスト (nnue-hand-count-dense feature)
    //
    // Round-trip 検証: bullet-shogi 側の save format を模したバイト列を生成し、
    // `LayerStackBucket::read_with_hand_count` で読み込んで、既知の入力を
    // `propagate_with_hand_count` に流して期待値（スカラー参照実装）と一致するかを確認する。
    // これにより以下を同時に保証する:
    //   1. byte layout (biases / main FT weights / HC weights / padding の行境界)
    //   2. scramble 変換（AVX2/SSSE3 ビルドでの重みインデックス）
    //   3. scale 補正 × 127（FT 入力 u8=127× と HC 入力 raw i16 の整合）
    // =============================================================================

    #[cfg(feature = "nnue-hand-count-dense")]
    #[test]
    fn test_read_with_hand_count_round_trip_matches_scalar() {
        use super::super::hand_count::HAND_COUNT_DIMS;
        use super::super::layers::padded_input;

        // -----------------------------------------------------------------------
        // Step 1: 既知の重み・バイアスを生成
        // -----------------------------------------------------------------------

        // L1 biases (i32): index ごとに決定的に変化させる
        let mut l1_biases = [0i32; LAYER_STACK_L1_OUT];
        for (i, b) in l1_biases.iter_mut().enumerate() {
            *b = ((i as i32) - 8) * 123; // -984 .. +861
        }

        // L1 main weights (i8): row-major `[out][in]`、入力 i/j からの決定関数
        let ft_padded = padded_input(TEST_L1);
        let mut main_weights_row_major = vec![0i8; LAYER_STACK_L1_OUT * ft_padded];
        for out_idx in 0..LAYER_STACK_L1_OUT {
            for in_idx in 0..TEST_L1 {
                // 値は [-3, 3] に収める（propagate 出力の overflow を避ける）
                let v = ((in_idx as i32 + out_idx as i32) % 7) - 3;
                main_weights_row_major[out_idx * ft_padded + in_idx] = v as i8;
            }
            // [TEST_L1..ft_padded) は AffineTransform::read の padding と同じ扱い (全 0)
        }

        // HC weights (i8): row-major `[out][in]`
        let mut hc_weights_row_major = vec![0i8; LAYER_STACK_L1_OUT * HAND_COUNT_DIMS];
        for out_idx in 0..LAYER_STACK_L1_OUT {
            for in_idx in 0..HAND_COUNT_DIMS {
                let v = ((in_idx as i32 * 2) + out_idx as i32) % 5 - 2;
                hc_weights_row_major[out_idx * HAND_COUNT_DIMS + in_idx] = v as i8;
            }
        }

        // L2 / Output は単純に 0 にしてスキップ接続経由で L1 の振る舞いを直接観察
        let l2_biases = [0i32; NNUE_PYTORCH_L3];
        let l2_weights = vec![0i8; NNUE_PYTORCH_L3 * padded_input(LAYER_STACK_L2_IN)];
        let output_biases = [0i32; 1];
        let output_weights = vec![0i8; padded_input(NNUE_PYTORCH_L3)];

        // -----------------------------------------------------------------------
        // Step 2: bullet 側の save format でバイト列を組み立てる
        // -----------------------------------------------------------------------

        let hc_dims = HAND_COUNT_DIMS;
        let total_padded = padded_input(TEST_L1 + hc_dims);
        let pad_extra = total_padded - ft_padded - hc_dims;

        let mut bytes: Vec<u8> = Vec::new();

        // L1 biases
        for &b in &l1_biases {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        // L1 weights (per-row: main FT + HC + padding)
        for out_idx in 0..LAYER_STACK_L1_OUT {
            for in_idx in 0..ft_padded {
                bytes.push(main_weights_row_major[out_idx * ft_padded + in_idx] as u8);
            }
            for in_idx in 0..hc_dims {
                bytes.push(hc_weights_row_major[out_idx * hc_dims + in_idx] as u8);
            }
            bytes.extend(std::iter::repeat_n(0u8, pad_extra));
        }
        // L2 biases
        for &b in &l2_biases {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        // L2 weights
        for &w in &l2_weights {
            bytes.push(w as u8);
        }
        // Output biases
        for &b in &output_biases {
            bytes.extend_from_slice(&b.to_le_bytes());
        }
        // Output weights
        for &w in &output_weights {
            bytes.push(w as u8);
        }

        // -----------------------------------------------------------------------
        // Step 3: 読み込み
        // -----------------------------------------------------------------------

        let mut cursor = std::io::Cursor::new(bytes);
        let bucket =
            LayerStackBucket::<TEST_L1>::read_with_hand_count(&mut cursor, hc_dims).unwrap();

        // HC 重みが正しく格納されているか
        let hcw = bucket.l1_hand_count.as_ref().unwrap();
        for out_idx in 0..LAYER_STACK_L1_OUT {
            for in_idx in 0..hc_dims {
                assert_eq!(
                    hcw.weights[out_idx][in_idx],
                    hc_weights_row_major[out_idx * hc_dims + in_idx],
                    "HC weights mismatch at [{out_idx}][{in_idx}]"
                );
            }
        }

        // -----------------------------------------------------------------------
        // Step 4: 既知の入力で propagate_with_hand_count を実行し、スカラー参照と比較
        // -----------------------------------------------------------------------

        // 入力 u8: 決定的に変化させる
        let mut input = Aligned([0u8; TEST_L1]);
        for (i, v) in input.0.iter_mut().enumerate().take(TEST_L1) {
            *v = ((i * 3 + 7) % 128) as u8;
        }

        // hand_count: 各駒種の典型的な手駒数を設定
        let hand_count: [i16; HAND_COUNT_DIMS] = [2, 1, 0, 1, 0, 1, 0, 3, 0, 1, 0, 0, 1, 0];

        let actual = bucket.propagate_with_hand_count(&input.0, Some(&hand_count));

        // スカラー参照: 既知の weights で L1 出力を手計算
        let mut expected_l1_out = [0i32; LAYER_STACK_L1_OUT];
        for (out_idx, expected_cell) in expected_l1_out.iter_mut().enumerate() {
            // L1 bias
            *expected_cell = l1_biases[out_idx];
            // Main FT contribution: sum(input[i] * weight[out][i])
            for in_idx in 0..TEST_L1 {
                let w = main_weights_row_major[out_idx * ft_padded + in_idx];
                *expected_cell += (input.0[in_idx] as i32) * (w as i32);
            }
            // HC contribution: sum(hand_count[i] * weight[out][i]) * 127
            let mut hc_partial: i32 = 0;
            for in_idx in 0..hc_dims {
                let w = hc_weights_row_major[out_idx * hc_dims + in_idx];
                hc_partial += (hand_count[in_idx] as i32) * (w as i32);
            }
            *expected_cell += hc_partial * 127;
        }

        // L1 skip = expected_l1_out[NNUE_PYTORCH_L2]（L2/Output が 0 なのでこれがそのまま生スコア）
        let expected_score = expected_l1_out[NNUE_PYTORCH_L2];

        assert_eq!(
            actual, expected_score,
            "propagate_with_hand_count output mismatch:\n  \
             expected (scalar reference) = {expected_score}\n  \
             actual (bucket.propagate_with_hand_count) = {actual}\n  \
             expected l1_out = {expected_l1_out:?}"
        );
    }

    /// hand_count が `None` の場合は `propagate` と同じ結果になる（後方互換性）
    #[cfg(feature = "nnue-hand-count-dense")]
    #[test]
    fn test_propagate_with_hand_count_none_matches_propagate() {
        use super::super::hand_count::HAND_COUNT_DIMS;

        let mut bucket = LayerStackBucket::<TEST_L1>::new();
        bucket.l1.biases[0] = 12345;
        bucket.l1.biases[5] = -6789;
        bucket.l1_hand_count = Some(HandCountL1Weights {
            weights: [[99i8; HAND_COUNT_DIMS]; LAYER_STACK_L1_OUT],
        });

        let input = Aligned([0u8; TEST_L1]);
        let baseline = bucket.propagate(&input.0);
        let with_none = bucket.propagate_with_hand_count(&input.0, None);

        assert_eq!(baseline, with_none, "propagate_with_hand_count(None) should match propagate()");
    }
}
