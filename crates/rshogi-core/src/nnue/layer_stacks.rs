//! nnue-pytorch LayerStacks アーキテクチャ
//!
//! nnue-pytorch の LayerStacks 構造を実装する。
//! 9個のバケットを持ち、局面に応じてバケットを選択して推論を行う。
//!
//! ## アーキテクチャ
//!
//! ```text
//! Feature Transformer: 73,305 → L1 (512 / 768 / 1536)
//! SqrClippedReLU: L1*2 → L1
//! LayerStacks (bucket選択後):
//!   L1: L1 → LS_L1_OUT, split [LS_L1_OUT - 1, 1]
//!   Sqr + ClippedReLU: LS_L1_OUT - 1 → LS_L2_IN (= 2 * (LS_L1_OUT - 1))
//!   L2: LS_L2_IN → 32, ReLU
//!   Output: 32 → 1 + skip
//! ```

use super::accumulator::Aligned;
use super::constants::{DEFAULT_NUM_BUCKETS, MAX_LAYER_STACK_BUCKETS, NNUE_PYTORCH_L3};
use super::layers::AffineTransform;
use std::io::{self, Read};

/// Output 入力のパディング済み次元数（padded_input(32) = 32）
const OUTPUT_PADDED_INPUT: usize = super::layers::padded_input(NNUE_PYTORCH_L3);

#[cfg(test)]
fn sqr_clipped_relu_explicit<const DIM: usize>(input: &[i32; DIM], output: &mut [u8; DIM]) {
    for i in 0..DIM {
        output[i] = ((input[i] as i64 * input[i] as i64) >> 19).clamp(0, 127) as u8;
    }
}

/// 活性段ごとの 127 飽和カウント（診断用）。
///
/// ClippedReLU / SqrClippedReLU 系の活性は u8 [0,127] に clamp されるため、
/// 127 到達率が高いほど量子化天井で情報が落ちている。
#[derive(Debug, Default, Clone, Copy)]
pub struct LsSaturationCounts {
    /// L1→L2 activation (SqrClippedReLU + ClippedReLU) の 127 到達数
    pub l1_act_sat: u64,
    pub l1_act_total: u64,
    /// L2→output activation (ClippedReLU) の 127 到達数
    pub l2_act_sat: u64,
    pub l2_act_total: u64,
}

// =============================================================================
// LayerStack 単一バケット
// =============================================================================

/// LayerStack 単一バケットの層
///
/// 各バケットは以下の構造を持つ:
/// - L1: L1 → LS_L1_OUT
/// - L2: LS_L2_IN → 32
/// - Output: 32 → 1
///
/// 各層は `AffineTransform` を使用し、AVX512/AVX2/SSSE3/WASM SIMD128 に対応。
pub struct LayerStackBucket<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> {
    /// L1層: L1 → LS_L1_OUT
    l1: AffineTransform<L1, LS_L1_OUT>,
    /// L2層: LS_L2_IN → 32
    pub l2: AffineTransform<LS_L2_IN, NNUE_PYTORCH_L3>,
    /// 出力層: 32 → 1
    pub output: AffineTransform<NNUE_PYTORCH_L3, 1>,
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        target_feature = "avx512vnni"
    ))]
    fused_weights: Option<super::ls_l1_kernel::avx512::FusedWeights>,
}

impl<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> LayerStackBucket<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>
{
    const MAIN_DIM: usize = LS_L1_OUT - 1;

    /// 新規作成（ゼロ初期化）
    pub fn new() -> Self {
        Self::from_layers(AffineTransform::new(), AffineTransform::new(), AffineTransform::new())
    }

    /// 層から bucket を構築し、対象形状では融合用重みも用意する。
    pub fn from_layers(
        l1: AffineTransform<L1, LS_L1_OUT>,
        l2: AffineTransform<LS_L2_IN, NNUE_PYTORCH_L3>,
        output: AffineTransform<NNUE_PYTORCH_L3, 1>,
    ) -> Self {
        const {
            assert!(LS_L1_OUT >= 2, "LayerStacks L1 output must be at least 2");
            assert!(
                LS_L2_IN == (LS_L1_OUT - 1) * 2,
                "LayerStacks L2 input must be 2 * (L1 output - 1)"
            );
            assert!(
                LS_L2_PADDED_INPUT == super::layers::padded_input(LS_L2_IN),
                "LayerStacks L2 padded input must match padded_input(L2_IN)"
            );
        }
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw",
            target_feature = "avx512vnni"
        ))]
        let fused_weights = super::ls_l1_kernel::avx512::reorder(&l1);
        Self {
            l1,
            l2,
            output,
            #[cfg(all(
                target_arch = "x86_64",
                target_feature = "avx512f",
                target_feature = "avx512bw",
                target_feature = "avx512vnni"
            ))]
            fused_weights,
        }
    }

    /// L1 層を読み取る。
    pub fn l1(&self) -> &AffineTransform<L1, LS_L1_OUT> {
        &self.l1
    }

    /// L1 層を編集し、融合用重みを再構築する。
    /// 編集中に unwind した場合も、変更済みの L1 に合わせて再構築する。
    pub fn edit_l1(&mut self, edit: impl FnOnce(&mut AffineTransform<L1, LS_L1_OUT>)) {
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw",
            target_feature = "avx512vnni"
        ))]
        super::ls_l1_kernel::avx512::edit_l1(&mut self.l1, &mut self.fused_weights, edit);
        #[cfg(not(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw",
            target_feature = "avx512vnni"
        )))]
        edit(&mut self.l1);
    }

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        target_feature = "avx512vnni"
    ))]
    #[inline]
    pub(super) fn propagate_accumulators(&self, us: &[i16; L1], them: &[i16; L1]) -> i32 {
        let weights = self.fused_weights.as_ref().expect("1536x16 bucket has fused weights");
        let l1_out = super::ls_l1_kernel::avx512::fused(us, them, weights, &self.l1.biases);
        self.propagate_from_l1(&l1_out)
    }

    /// ファイルから読み込み
    pub fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        let l1 = AffineTransform::read(reader)?;
        let l2 = AffineTransform::read(reader)?;
        let output = AffineTransform::read(reader)?;
        Ok(Self::from_layers(l1, l2, output))
    }

    /// 順伝播
    ///
    /// 入力: SqrClippedReLU後のL1次元 (u8)
    /// 出力: スケーリング前の生スコア (i32)
    pub fn propagate(&self, input: &[u8; L1]) -> i32 {
        let mut l1_out = [0i32; LS_L1_OUT];
        let mut l2_input = Aligned([0u8; LS_L2_PADDED_INPUT]);
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        let mut output_arr = [0i32; 1];

        // L1: L1 → LS_L1_OUT
        self.l1.propagate_7bit(input, &mut l1_out);

        // Split: [main_dim, 1]
        // l1_skip は最後の 1 要素、残り main_dim 要素を L2 入力へ変換する。
        let l1_skip = l1_out[Self::MAIN_DIM];

        // main_dim 要素に SqrClippedReLU と ClippedReLU を適用して連結する。
        // SqrClippedReLU: min(127, (input^2) >> 19)
        // ClippedReLU:    clamp(input >> 6, 0, 127)
        l1_sqr_clipped_relu_activation::<LS_L1_OUT, LS_L2_IN>(&l1_out, &mut l2_input.0);

        // L2: LS_L2_IN → 32
        self.l2.propagate_7bit(&l2_input.0, &mut l2_out);
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        let output = clipped_relu_affine_32_to_1_avx2(&l2_out, &self.output);
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        let output = {
            clipped_relu_i32_to_u8(&l2_out, &mut l2_relu.0);
            self.output.propagate_7bit(&l2_relu.0, &mut output_arr);
            output_arr[0]
        };

        // Skip connection
        output + l1_skip
    }

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        target_feature = "avx512vnni"
    ))]
    fn propagate_from_l1(&self, l1_out: &[i32; LS_L1_OUT]) -> i32 {
        let mut l2_input = Aligned([0u8; LS_L2_PADDED_INPUT]);
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        let mut output_arr = [0i32; 1];

        // Split: [main_dim, 1]
        // l1_skip は最後の 1 要素、残り main_dim 要素を L2 入力へ変換する。
        let l1_skip = l1_out[Self::MAIN_DIM];

        // main_dim 要素に SqrClippedReLU と ClippedReLU を適用して連結する。
        // SqrClippedReLU: min(127, (input^2) >> 19)
        // ClippedReLU:    clamp(input >> 6, 0, 127)
        l1_sqr_clipped_relu_activation::<LS_L1_OUT, LS_L2_IN>(l1_out, &mut l2_input.0);

        // L2: LS_L2_IN → 32
        self.l2.propagate_7bit(&l2_input.0, &mut l2_out);
        #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
        let output = clipped_relu_affine_32_to_1_avx2(&l2_out, &self.output);
        #[cfg(not(all(target_arch = "x86_64", target_feature = "avx2")))]
        let output = {
            clipped_relu_i32_to_u8(&l2_out, &mut l2_relu.0);
            self.output.propagate_7bit(&l2_relu.0, &mut output_arr);
            output_arr[0]
        };

        // Skip connection
        output + l1_skip
    }

    /// 順伝播しつつ各活性段の 127 飽和を数える（診断用、ホットパス外）。
    ///
    /// スコアは `propagate` と bit 一致する。カウント対象:
    /// - L1→L2 activation (SqrClippedReLU + ClippedReLU の `LS_L2_IN` 要素)
    /// - L2→output activation (ClippedReLU の 32 要素)
    ///
    /// FT 段はここでは数えない。FT 出力は clamp 済み因子の積 `(a*b) >> 7` (最大 126) で
    /// 127 に到達しないため、FT の飽和は pairing 前の accumulator 値 (>= 127) を
    /// 呼び出し側で数える。
    pub fn propagate_counting_saturation(
        &self,
        input: &[u8; L1],
        counts: &mut LsSaturationCounts,
    ) -> i32 {
        let mut l1_out = [0i32; LS_L1_OUT];
        let mut l2_input = Aligned([0u8; LS_L2_PADDED_INPUT]);
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        let mut output_arr = [0i32; 1];

        self.l1.propagate_7bit(input, &mut l1_out);
        let l1_skip = l1_out[Self::MAIN_DIM];
        l1_sqr_clipped_relu_activation::<LS_L1_OUT, LS_L2_IN>(&l1_out, &mut l2_input.0);
        counts.l1_act_sat += l2_input.0[..LS_L2_IN].iter().filter(|&&v| v == 127).count() as u64;
        counts.l1_act_total += LS_L2_IN as u64;

        self.l2.propagate_7bit(&l2_input.0, &mut l2_out);
        clipped_relu_i32_to_u8(&l2_out, &mut l2_relu.0);
        counts.l2_act_sat +=
            l2_relu.0[..NNUE_PYTORCH_L3].iter().filter(|&&v| v == 127).count() as u64;
        counts.l2_act_total += NNUE_PYTORCH_L3 as u64;

        self.output.propagate_7bit(&l2_relu.0, &mut output_arr);
        output_arr[0] + l1_skip
    }

    /// 順伝播（診断情報付き）
    ///
    /// 戻り値: (raw_score, l1_out, l1_skip)
    #[cfg(feature = "diagnostics")]
    pub fn propagate_with_diagnostics(&self, input: &[u8; L1]) -> (i32, [i32; LS_L1_OUT], i32) {
        let mut l1_out = [0i32; LS_L1_OUT];
        let mut l2_input = Aligned([0u8; LS_L2_PADDED_INPUT]);
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        let mut l2_relu = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        let mut output_arr = [0i32; 1];

        self.l1.propagate_7bit(input, &mut l1_out);

        // Split: [main_dim, 1]
        let l1_skip = l1_out[Self::MAIN_DIM];
        l1_sqr_clipped_relu_activation::<LS_L1_OUT, LS_L2_IN>(&l1_out, &mut l2_input.0);

        // L2: LS_L2_IN → 32
        self.l2.propagate_7bit(&l2_input.0, &mut l2_out);
        clipped_relu_i32_to_u8(&l2_out, &mut l2_relu.0);

        // Output: 32 → 1
        self.output.propagate_7bit(&l2_relu.0, &mut output_arr);

        // Skip connection
        let raw_score = output_arr[0] + l1_skip;

        (raw_score, l1_out, l1_skip)
    }
}

impl<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> Default for LayerStackBucket<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>
{
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// LayerStacks (可変 num_buckets)
// =============================================================================

/// LayerStacks: 可変長 `num_buckets` 個の bucket を持つ構造
///
/// `buckets` は load 時に 1 回だけ `Vec::with_capacity` + push で構築され、
/// 以後 **read-only**。ホットパスでは indexing のみで再 alloc は発生しない
/// (ADR `2026-05-26` §2.7.3)。
pub struct LayerStacks<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> {
    /// `num_buckets` 個の bucket。長さは net file の `num_buckets` で決まる。
    pub buckets: Vec<LayerStackBucket<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>>,
}

impl<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> LayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>
{
    /// 新規作成 (default `DEFAULT_NUM_BUCKETS` 個の bucket をゼロ初期化)
    pub fn new() -> Self {
        Self::with_num_buckets(DEFAULT_NUM_BUCKETS)
    }

    /// 指定 `num_buckets` でゼロ初期化
    ///
    /// `num_buckets` は engine 側で `1..=MAX_LAYER_STACK_BUCKETS` を満たすことを
    /// 呼び出し元 (`read_with_options` 等) が保証する。
    pub fn with_num_buckets(num_buckets: usize) -> Self {
        debug_assert!((1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets));
        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            buckets.push(LayerStackBucket::new());
        }
        Self { buckets }
    }

    /// 現在の bucket 数
    #[inline]
    pub fn num_buckets(&self) -> usize {
        self.buckets.len()
    }

    /// ファイルから `num_buckets` 個の bucket を読み込む
    ///
    /// FC 層は常に非圧縮形式（raw bytes）で保存されている。
    /// LEB128 圧縮は Feature Transformer にのみ適用される。
    ///
    /// `num_buckets` は net file header の `num_buckets` field (legacy `.bin` の
    /// 場合は `DEFAULT_NUM_BUCKETS = 9`)。
    pub fn read<R: Read>(reader: &mut R, num_buckets: usize) -> io::Result<Self> {
        debug_assert!((1..=MAX_LAYER_STACK_BUCKETS).contains(&num_buckets));
        let mut buckets = Vec::with_capacity(num_buckets);

        // fc_hash をスキップして bucket ごとに読み込み
        let mut buf4 = [0u8; 4];
        for _ in 0..num_buckets {
            // fc_hash を読み飛ばす
            reader.read_exact(&mut buf4)?;
            let _fc_hash = u32::from_le_bytes(buf4);
            // バケットを読み込み（常に非圧縮形式）
            buckets.push(LayerStackBucket::read(reader)?);
        }

        Ok(Self { buckets })
    }

    #[cfg(feature = "prepacked-nnue")]
    pub(super) fn read_packed<R: Read + std::io::Seek>(
        reader: &mut R,
        num_buckets: usize,
        packed: &super::prepacked::PackedModel,
    ) -> io::Result<Self> {
        let mut buckets = Vec::with_capacity(num_buckets);
        for _ in 0..num_buckets {
            let mut hash = [0; 4];
            reader.read_exact(&mut hash)?;
            buckets.push(LayerStackBucket::from_layers(
                AffineTransform::read_packed(reader, packed)?,
                AffineTransform::read_packed(reader, packed)?,
                AffineTransform::read_packed(reader, packed)?,
            ));
        }
        Ok(Self { buckets })
    }

    /// 生スコアを計算（スケーリング前）
    pub fn evaluate_raw(&self, bucket_index: usize, input: &[u8; L1]) -> i32 {
        self.buckets[bucket_index].propagate(input)
    }

    /// 生スコアを計算（診断情報付き）
    ///
    /// 戻り値: (raw_score, l1_out, l1_skip)
    #[cfg(feature = "diagnostics")]
    pub fn evaluate_raw_with_diagnostics(
        &self,
        bucket_index: usize,
        input: &[u8; L1],
    ) -> (i32, [i32; LS_L1_OUT], i32) {
        debug_assert!(bucket_index < self.buckets.len());
        self.buckets[bucket_index].propagate_with_diagnostics(input)
    }
}

impl<
    const L1: usize,
    const LS_L1_OUT: usize,
    const LS_L2_IN: usize,
    const LS_L2_PADDED_INPUT: usize,
> Default for LayerStacks<L1, LS_L1_OUT, LS_L2_IN, LS_L2_PADDED_INPUT>
{
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
/// L1→L2 activation: SqrClippedReLU + ClippedReLU（main_dim 要素 → 2 * main_dim u8）
///
/// `main_dim = LS_L1_OUT - 1` とする。
/// `l1_out` の先頭 `main_dim` 要素に対して:
/// - SqrClippedReLU: min(127, (input^2) >> 19) → l2_input[0..main_dim]
/// - ClippedReLU:    clamp(input >> 6, 0, 127)  → l2_input[main_dim..2*main_dim]
///
/// 最後の 1 要素 (l1_skip) は呼び出し側で別途取得済み。
#[inline]
fn l1_sqr_clipped_relu_activation<const LS_L1_OUT: usize, const LS_L2_IN: usize>(
    l1_out: &[i32; LS_L1_OUT],
    l2_input: &mut [u8],
) {
    let main_dim = LS_L1_OUT - 1;
    debug_assert_eq!(LS_L2_IN, main_dim * 2);

    // 16入力を一度だけpackし、sqr/clipの両方を生成するAVX2変換。16個目はskip出力なので、
    // sqr側の余剰byteをclip側の先頭storeで上書きし、clip側の余剰byteはpadded inputに書く。
    // i32 -> i16は32767超を32767、-32768未満を-32768へ飽和する。前者はsqr/clipとも
    // 127、後者はsqrが127、clipが0になるためscalarと一致する。
    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    if LS_L1_OUT == 16 && LS_L2_IN == 30 && l2_input.len() >= 31 {
        // SAFETY:
        // - LS_L1_OUT == 16 なので l1_out から i32 を 16 個 load できる。
        // - l2_input.len() >= 31 なので [0,16) と [15,31) の store は範囲内。
        // - load/store は unaligned 版を使うため追加の alignment 要件はない。
        unsafe {
            use std::arch::x86_64::*;

            let input = l1_out.as_ptr() as *const __m256i;
            let mut words =
                _mm256_packs_epi32(_mm256_loadu_si256(input), _mm256_loadu_si256(input.add(1)));
            words = _mm256_permute4x64_epi64(words, 0b1101_1000);

            // mulhi gives square >> 16; another 3 bits produce square >> 19.
            let sq_words = _mm256_srli_epi16(_mm256_mulhi_epi16(words, words), 3);
            let sq_bytes = _mm_packs_epi16(
                _mm256_castsi256_si128(sq_words),
                _mm256_extracti128_si256(sq_words, 1),
            );

            let clip_words = _mm256_srli_epi16(_mm256_max_epi16(words, _mm256_setzero_si256()), 6);
            let clip_bytes = _mm_packs_epi16(
                _mm256_castsi256_si128(clip_words),
                _mm256_extracti128_si256(clip_words, 1),
            );

            let output = l2_input.as_mut_ptr();
            _mm_storeu_si128(output as *mut __m128i, sq_bytes);
            _mm_storeu_si128(output.add(main_dim) as *mut __m128i, clip_bytes);
            // clip store の16個目は skip 出力であり、有効入力の直後へ書かれる。
            // padding weight が非zeroのnetでも従来値を保つため0へ戻す。
            *output.add(LS_L2_IN) = 0;
        }
        return;
    }

    // 注意: 二乗は i64 で計算する必要がある。
    // i32 乗算は |val| > ~46340 (sqrt(i32::MAX)) でオーバーフローし、
    // 中盤局面の L1 出力は数万〜数十万に達するため i64 が必須。
    for (i, &val) in l1_out.iter().enumerate().take(main_dim) {
        let input_val = val as i64;
        let sqr = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
        let clamped = (val >> 6).clamp(0, 127) as u8;
        l2_input[i] = sqr;
        l2_input[main_dim + i] = clamped;
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

/// LayerStacks の L2 ClippedReLU と 32→1 affine を中間配列なしで計算する。
///
/// `packus` が負値を0へ飽和するため、shift後は上限127だけを明示的にclampすれば
/// `clamp(input >> 6, 0, 127)` と一致する。32要素をレジスタ内でu8へpackし、そのまま
/// 出力層の内積へ渡すことで、従来の32-byte store/reloadを省く。
#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
#[inline]
fn clipped_relu_affine_32_to_1_avx2(
    input: &[i32; NNUE_PYTORCH_L3],
    affine: &AffineTransform<NNUE_PYTORCH_L3, 1>,
) -> i32 {
    const { assert!(NNUE_PYTORCH_L3 == 32) };

    // SAFETY:
    // - input は32要素なので4本の256-bit loadが有効。
    // - affine.weights は32要素以上かつ64-byte aligned。
    // - shift/clamp後は[0,127]なので maddubs の隣接2項和はi16に収まる。
    unsafe {
        use std::arch::x86_64::*;

        let input_ptr = input.as_ptr() as *const __m256i;
        let max127 = _mm256_set1_epi32(127);
        let v0 = _mm256_min_epi32(_mm256_srai_epi32(_mm256_loadu_si256(input_ptr), 6), max127);
        let v1 =
            _mm256_min_epi32(_mm256_srai_epi32(_mm256_loadu_si256(input_ptr.add(1)), 6), max127);
        let v2 =
            _mm256_min_epi32(_mm256_srai_epi32(_mm256_loadu_si256(input_ptr.add(2)), 6), max127);
        let v3 =
            _mm256_min_epi32(_mm256_srai_epi32(_mm256_loadu_si256(input_ptr.add(3)), 6), max127);

        let words01 = _mm256_packs_epi32(v0, v1);
        let words23 = _mm256_packs_epi32(v2, v3);
        let packed = _mm256_packus_epi16(words01, words23);
        // pack命令は128-bit laneごとに動くため、4-byte groupを入力順へ戻す。
        let reorder = _mm256_setr_epi32(0, 4, 1, 5, 2, 6, 3, 7);
        let activations = _mm256_permutevar8x32_epi32(packed, reorder);

        let weights = _mm256_load_si256(affine.weights.as_ptr() as *const __m256i);
        let product16 = _mm256_maddubs_epi16(activations, weights);
        let product32 = _mm256_madd_epi16(product16, _mm256_set1_epi16(1));

        let lo = _mm256_castsi256_si128(product32);
        let hi = _mm256_extracti128_si256(product32, 1);
        let sum128 = _mm_add_epi32(lo, hi);
        let sum64 = _mm_add_epi32(sum128, _mm_unpackhi_epi64(sum128, sum128));
        let sum32 = _mm_add_epi32(sum64, _mm_shuffle_epi32(sum64, 1));
        affine.biases[0] + _mm_cvtsi128_si32(sum32)
    }
}

/// 入力: 両視点のアキュムレータ (各L1次元, i16)
/// 出力: SqrClippedReLU後のL1次元 (u8)
pub fn sqr_clipped_relu_transform<const L1: usize>(
    us_acc: &[i16; L1],
    them_acc: &[i16; L1],
    output: &mut [u8; L1],
) {
    // SAFETY: output は L1 バイトの書き込み可能な領域で、入力とは重ならない。
    unsafe { sqr_clipped_relu_write(us_acc, them_acc, output.as_mut_ptr()) };
}

/// SIMD の書き込み完了後に、有効な配列として出力を返す。
///
/// 64 の倍数なら全 backend が両視点の全要素を書き込むため、先行ゼロ埋めは不要。
pub(crate) fn sqr_clipped_relu_new<const L1: usize>(
    us_acc: &[i16; L1],
    them_acc: &[i16; L1],
) -> Aligned<[u8; L1]> {
    const { assert!(L1.is_multiple_of(64)) };
    let mut output = std::mem::MaybeUninit::<Aligned<[u8; L1]>>::uninit();
    // SAFETY: raw pointer への書き込みだけを行い、未初期化の配列への参照は作らない。
    // 各 backend は half=L1/2 の両領域を余りなく書き込む。
    // 書き込み完了後は全 u8 が有効な値を持ち、Aligned は追加フィールドを持たない。
    unsafe {
        sqr_clipped_relu_write(us_acc, them_acc, output.as_mut_ptr().cast::<u8>());
        output.assume_init()
    }
}

/// # Safety
/// output は L1 バイトを書き込める領域で、入力領域と重ならないこと。
/// 入力のアライメントは要求せず、出力の既存値は読み取らない。
unsafe fn sqr_clipped_relu_write<const L1: usize>(
    us_acc: &[i16; L1],
    them_acc: &[i16; L1],
    output: *mut u8,
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
        // - us_acc, them_acc は各 L1 要素。unaligned load のためアライメント不要。
        // - output は呼び出し側が保証する L1 バイトの書き込み可能領域。
        // - 各視点で half/32 個の 32 要素ブロックを処理し、領域外へアクセスしない。
        // - 乗算結果: max 127*127=16129 < i16::MAX(32767)、>>7 後は [0, 126] → packus で u8 に収まる
        unsafe {
            use std::arch::x86_64::*;
            let zero = _mm512_setzero_si512();
            let max127 = _mm512_set1_epi16(127);

            // マクロで us/them を処理（出力オフセットが異なるだけ）
            for (acc, out_offset) in [(us_acc.as_ptr(), 0usize), (them_acc.as_ptr(), half)] {
                let acc_a = acc;
                let acc_b = acc.add(half);
                let out_ptr = output.add(out_offset);

                for i in 0..(half / 32) {
                    let offset = i * 32;
                    let va = _mm512_loadu_si512(acc_a.add(offset) as *const __m512i);
                    let vb = _mm512_loadu_si512(acc_b.add(offset) as *const __m512i);

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
        // - us_acc, them_acc は各 L1 要素。unaligned load のためアライメント不要。
        // - output は呼び出し側が保証する L1 バイトの書き込み可能領域。
        // - 各視点で half/32 個の 32 要素ブロックを処理し、領域外へアクセスしない。
        // - 乗算結果: max 127*127=16129 < i16::MAX(32767)、>>7 後は [0, 126] → packus で u8 に収まる
        unsafe {
            use std::arch::x86_64::*;
            let zero = _mm256_setzero_si256();
            let max127 = _mm256_set1_epi16(127);

            for (acc, out_offset) in [(us_acc.as_ptr(), 0usize), (them_acc.as_ptr(), half)] {
                let acc_a = acc;
                let acc_b = acc.add(half);
                let out_ptr = output.add(out_offset);

                // 32要素ずつ処理（2 × 16 i16 → 32 u8）
                for i in 0..(half / 32) {
                    let offset = i * 32;

                    let va0 = _mm256_loadu_si256(acc_a.add(offset) as *const __m256i);
                    let vb0 = _mm256_loadu_si256(acc_b.add(offset) as *const __m256i);
                    let a0 = _mm256_min_epi16(_mm256_max_epi16(va0, zero), max127);
                    let b0 = _mm256_min_epi16(_mm256_max_epi16(vb0, zero), max127);
                    let shifted0 = _mm256_srli_epi16(_mm256_mullo_epi16(a0, b0), 7);

                    let va1 = _mm256_loadu_si256(acc_a.add(offset + 16) as *const __m256i);
                    let vb1 = _mm256_loadu_si256(acc_b.add(offset + 16) as *const __m256i);
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
        // SAFETY: 各入力は L1 要素。half/16 個のブロックは入力・出力範囲内。
        unsafe {
            use std::arch::x86_64::*;
            let zero = _mm_setzero_si128();
            let max127 = _mm_set1_epi16(127);

            for (acc, out_offset) in [(us_acc.as_ptr(), 0usize), (them_acc.as_ptr(), half)] {
                let acc_a = acc;
                let acc_b = acc.add(half);
                let out_ptr = output.add(out_offset);

                // 16要素ずつ処理（2 × 8 i16 → 16 u8）
                for i in 0..(half / 16) {
                    let offset = i * 16;

                    let va0 = _mm_loadu_si128(acc_a.add(offset) as *const __m128i);
                    let vb0 = _mm_loadu_si128(acc_b.add(offset) as *const __m128i);
                    let a0 = _mm_min_epi16(_mm_max_epi16(va0, zero), max127);
                    let b0 = _mm_min_epi16(_mm_max_epi16(vb0, zero), max127);
                    let shifted0 = _mm_srli_epi16(_mm_mullo_epi16(a0, b0), 7);

                    let va1 = _mm_loadu_si128(acc_a.add(offset + 8) as *const __m128i);
                    let vb1 = _mm_loadu_si128(acc_b.add(offset + 8) as *const __m128i);
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
                let out_ptr = output.add(out_offset);

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
            // SAFETY: i < half で、呼び出し側が L1 バイトの出力領域を保証する。
            unsafe { output.add(i).write(us_prod as u8) };

            // them側
            let them_a = (them_acc[i] as i32).clamp(0, 127) as u32;
            let them_b = (them_acc[half + i] as i32).clamp(0, 127) as u32;
            let them_prod = ((them_a * them_b) >> 7).min(127);
            // SAFETY: half+i < L1。前半とは異なる要素への書き込み。
            unsafe { output.add(half + i).write(them_prod as u8) };
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
///
/// 本関数は legacy 9-bucket 固定方式 (king-rank 由来)。本番評価の bucket 選択は
/// `network_layer_stacks::compute_layer_stacks_bucket_index` の mode 分岐を経由する。
pub fn compute_bucket_index(f_king_rank: usize, e_king_rank: usize) -> usize {
    // 味方玉の段 → bucket オフセット
    const F_TO_INDEX: [usize; 9] = [0, 0, 0, 3, 3, 3, 6, 6, 6];
    // 相手玉の段 → bucket オフセット
    const E_TO_INDEX: [usize; 9] = [0, 0, 0, 1, 1, 1, 2, 2, 2];

    // 範囲外の値は最大インデックス(8)にクランプ
    let f_idx = F_TO_INDEX[f_king_rank.min(8)];
    let e_idx = E_TO_INDEX[e_king_rank.min(8)];

    (f_idx + e_idx).min(DEFAULT_NUM_BUCKETS - 1)
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
    use crate::nnue::constants::{
        LAYER_STACK_16X32_L1_OUT, LAYER_STACK_16X32_L2_IN, NNUE_PYTORCH_L1,
    };
    use crate::nnue::layers::ClippedReLU;

    #[test]
    fn initialized_output_matches_scalar_reference() {
        fn check<const N: usize>() {
            let values = [i16::MIN, -1, 0, 1, 63, 126, 127, 128, 255, i16::MAX, 97];
            let us = std::array::from_fn(|i| values[i % values.len()]);
            let them = std::array::from_fn(|i| values[(i * 3 + 5) % values.len()]);
            for (first, second) in [(&us, &them), (&them, &us)] {
                let actual = sqr_clipped_relu_new::<N>(first, second);
                let half = N / 2;
                for (offset, input) in [(0, first), (half, second)] {
                    for i in 0..half {
                        let a = i32::from(input[i]).clamp(0, 127);
                        let b = i32::from(input[half + i]).clamp(0, 127);
                        assert_eq!(actual.0[offset + i], ((a * b) >> 7) as u8);
                    }
                }
            }
        }
        check::<64>();
        check::<512>();
        check::<768>();
        check::<1024>();
        check::<1536>();
        check::<3072>();
    }

    /// テスト用の具体的な L1 サイズ
    const TEST_L1: usize = NNUE_PYTORCH_L1; // 1536
    const TEST_LS_L1_OUT: usize = LAYER_STACK_16X32_L1_OUT;
    const TEST_MAIN_DIM: usize = TEST_LS_L1_OUT - 1;
    const TEST_LS_L2_IN: usize = LAYER_STACK_16X32_L2_IN;
    const TEST_LS_L2_PADDED_INPUT: usize = 32;

    type TestLayerStackBucket =
        LayerStackBucket<TEST_L1, TEST_LS_L1_OUT, TEST_LS_L2_IN, TEST_LS_L2_PADDED_INPUT>;
    type TestLayerStacks =
        LayerStacks<TEST_L1, TEST_LS_L1_OUT, TEST_LS_L2_IN, TEST_LS_L2_PADDED_INPUT>;

    #[test]
    fn test_layer_stacks_new() {
        let stacks = TestLayerStacks::new();
        assert_eq!(stacks.buckets.len(), DEFAULT_NUM_BUCKETS);
    }

    #[test]
    fn test_layer_stacks_with_num_buckets() {
        for &n in &[1usize, 5, 8, 9, 12, MAX_LAYER_STACK_BUCKETS] {
            let stacks = TestLayerStacks::with_num_buckets(n);
            assert_eq!(stacks.buckets.len(), n);
            assert_eq!(stacks.num_buckets(), n);
        }
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

    #[test]
    fn propagate_counting_saturation_matches_propagate_and_counts() {
        let mut bucket = TestLayerStackBucket::new();
        bucket.l1.biases[0] = 8192; // sqr=(8192^2)>>19=128→127 / clipped=8192>>6=128→127 で両方飽和
        bucket.l1.biases[1] = 8000; // sqr=122 / clipped=125 で非飽和

        let input = Aligned([0u8; TEST_L1]);

        let mut counts = LsSaturationCounts::default();
        let score = bucket.propagate_counting_saturation(&input.0, &mut counts);
        assert_eq!(score, bucket.propagate(&input.0));

        assert_eq!(counts.l1_act_sat, 2);
        assert_eq!(counts.l1_act_total, TEST_LS_L2_IN as u64);
        assert_eq!(counts.l2_act_sat, 0);
        assert_eq!(counts.l2_act_total, NNUE_PYTORCH_L3 as u64);
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
            [
                i32::MIN,
                i32::MIN + 1,
                -65536,
                -32769,
                -32768,
                -32767,
                -8192,
                -64,
                0,
                64,
                8192,
                32767,
                32768,
                65536,
                i32::MAX - 1,
                i32::MAX,
            ],
        ];

        for l1_out in cases {
            let mut l2_input_opt = Aligned([0u8; TEST_LS_L2_PADDED_INPUT]);
            l1_sqr_clipped_relu_activation::<TEST_LS_L1_OUT, TEST_LS_L2_IN>(
                &l1_out,
                &mut l2_input_opt.0,
            );

            let mut l2_input_ref = Aligned([0u8; TEST_LS_L2_PADDED_INPUT]);
            for (i, &val) in l1_out.iter().enumerate().take(TEST_MAIN_DIM) {
                let input_val = i64::from(val);
                l2_input_ref.0[i] = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
                l2_input_ref.0[TEST_MAIN_DIM + i] = (val >> 6).clamp(0, 127) as u8;
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

    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]
    #[test]
    fn test_clipped_relu_affine_32_to_1_avx2_matches_scalar_reference() {
        let input = [
            i32::MIN,
            -1_000_000,
            -32769,
            -32768,
            -65,
            -64,
            -63,
            -1,
            0,
            1,
            63,
            64,
            127,
            128,
            8127,
            8128,
            8191,
            8192,
            32767,
            32768,
            65535,
            65536,
            100_000,
            1_000_000,
            i32::MAX,
            -4096,
            4096,
            -8192,
            16_384,
            24_576,
            32_768,
            40_000,
        ];
        let weights: [i8; NNUE_PYTORCH_L3] = [
            -128, 127, -127, 126, -64, 63, -32, 31, -16, 15, -8, 7, -4, 3, -2, 1, 0, -1, 2, -3, 4,
            -7, 8, -15, 16, -31, 32, -63, 64, -126, 127, -128,
        ];
        let bias = 123_456i32;
        let mut bytes = Vec::with_capacity(4 + weights.len());
        bytes.extend_from_slice(&bias.to_le_bytes());
        bytes.extend(weights.iter().map(|&weight| weight as u8));
        let affine =
            AffineTransform::<NNUE_PYTORCH_L3, 1>::read(&mut &bytes[..]).expect("valid affine");

        let reference = bias
            + input
                .iter()
                .zip(weights)
                .map(|(&value, weight)| (value >> 6).clamp(0, 127) * i32::from(weight))
                .sum::<i32>();

        assert_eq!(clipped_relu_affine_32_to_1_avx2(&input, &affine), reference);
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

        fn scalar_reference(bucket: &TestLayerStackBucket, input: &[u8; TEST_L1]) -> i32 {
            let mut l1_out = [0i32; TEST_LS_L1_OUT];
            bucket.l1.propagate(input, &mut l1_out);
            let l1_skip = l1_out[TEST_MAIN_DIM];

            let mut l2_input = Aligned([0u8; TEST_LS_L2_PADDED_INPUT]);
            for (i, &val) in l1_out.iter().enumerate().take(TEST_MAIN_DIM) {
                let input_val = i64::from(val);
                l2_input.0[i] = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
                l2_input.0[TEST_MAIN_DIM + i] = (val >> 6).clamp(0, 127) as u8;
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
        let l1_weights = vec![0i8; TEST_LS_L1_OUT * TEST_L1];

        let mut l2_biases = [0i32; NNUE_PYTORCH_L3];
        for (i, bias) in l2_biases.iter_mut().enumerate() {
            *bias = (i as i32 - 16) * 37;
        }
        let mut l2_weights = vec![0i8; NNUE_PYTORCH_L3 * TEST_LS_L2_PADDED_INPUT];
        for (i, weight) in l2_weights.iter_mut().enumerate() {
            *weight = ((i as i32 % 7) - 3) as i8;
        }

        let output_biases = [123i32; 1];
        let mut output_weights = vec![0i8; OUTPUT_PADDED_INPUT];
        for (i, weight) in output_weights.iter_mut().enumerate() {
            *weight = ((i as i32 % 5) - 2) as i8;
        }

        let bucket = TestLayerStackBucket::from_layers(
            affine_from_bytes::<TEST_L1, TEST_LS_L1_OUT>(l1_biases, &l1_weights),
            affine_from_bytes::<TEST_LS_L2_IN, NNUE_PYTORCH_L3>(l2_biases, &l2_weights),
            affine_from_bytes::<NNUE_PYTORCH_L3, 1>(output_biases, &output_weights),
        );

        let input = Aligned([0u8; TEST_L1]);
        let mut l1_out = [0i32; TEST_LS_L1_OUT];
        let mut l1_relu = [0u8; TEST_LS_L1_OUT];
        let mut l2_input_opt = Aligned([0u8; TEST_LS_L2_PADDED_INPUT]);
        let mut l2_input_ref = Aligned([0u8; TEST_LS_L2_PADDED_INPUT]);
        let mut l2_sqr = [0u8; TEST_LS_L1_OUT];
        let mut l2_out = [0i32; NNUE_PYTORCH_L3];
        let mut l2_relu_opt = Aligned([0u8; OUTPUT_PADDED_INPUT]);
        let mut l2_relu_ref = Aligned([0u8; OUTPUT_PADDED_INPUT]);

        bucket.l1.propagate(&input.0, &mut l1_out);
        ClippedReLU::<TEST_LS_L1_OUT>::propagate(&l1_out, &mut l1_relu);
        sqr_clipped_relu_explicit::<TEST_LS_L1_OUT>(&l1_out, &mut l2_sqr);
        l2_input_opt.0[..TEST_LS_L1_OUT].copy_from_slice(&l2_sqr);
        l2_input_opt.0[TEST_MAIN_DIM..TEST_MAIN_DIM + TEST_MAIN_DIM]
            .copy_from_slice(&l1_relu[..TEST_MAIN_DIM]);

        for (i, &val) in l1_out.iter().enumerate().take(TEST_MAIN_DIM) {
            let input_val = i64::from(val);
            l2_input_ref.0[i] = ((input_val * input_val) >> 19).clamp(0, 127) as u8;
            l2_input_ref.0[TEST_MAIN_DIM + i] = (val >> 6).clamp(0, 127) as u8;
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
        let optimized_inline = output_arr[0] + l1_out[TEST_MAIN_DIM];

        let optimized = bucket.propagate(&input.0);
        let reference = scalar_reference(&bucket, &input.0);

        assert_eq!(optimized_inline, reference);
        assert_eq!(optimized, reference);

        // hot path と診断 path は別実装のため、非自明入力での bit 一致をここで固定する
        let mut counts = LsSaturationCounts::default();
        assert_eq!(bucket.propagate_counting_saturation(&input.0, &mut counts), reference);
    }

    /// l1_out の値が大きい場合（i32 乗算でオーバーフローするケース）の回帰テスト。
    /// AVX2 パスの i32 オーバーフローが再発しないことを確認。
    #[test]
    fn test_l1_sqr_clipped_relu_activation_large_values() {
        // |val| = 50000 のとき i32 乗算は 2_500_000_000 > i32::MAX でオーバーフローする
        let l1_out = [50_000i32; TEST_LS_L1_OUT];
        let mut l2_input = [0u8; TEST_LS_L2_PADDED_INPUT];
        l1_sqr_clipped_relu_activation::<TEST_LS_L1_OUT, TEST_LS_L2_IN>(&l1_out, &mut l2_input);
        // SqrClippedReLU: (50000^2 >> 19) = 4768 → clamp → 127
        assert_eq!(l2_input[0], 127, "SqrClippedReLU should saturate to 127");
        // ClippedReLU: 50000 >> 6 = 781 → clamp → 127
        assert_eq!(l2_input[TEST_MAIN_DIM], 127, "ClippedReLU should saturate to 127");
    }
}

#[cfg(all(
    test,
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw",
    target_feature = "avx512vnni"
))]
pub(super) fn assert_fused_l1_matches_reference(
    bucket: &LayerStackBucket<1536, 16, 30, 32>,
    us: &[i16; 1536],
    them: &[i16; 1536],
) {
    use super::ls_l1_kernel::avx512;
    let legacy = sqr_clipped_relu_new(us, them);
    let mut expected = bucket.l1.biases;
    for (out, value) in expected.iter_mut().enumerate() {
        for (i, &input) in legacy.0.iter().enumerate() {
            *value = value
                .wrapping_add(i32::from(input) * i32::from(bucket.l1.file_weight(out * 1536 + i)));
        }
    }
    let mut dense = [0; 16];
    bucket.l1.propagate_7bit(&legacy.0, &mut dense);
    assert_eq!(expected, dense);
    let fused = avx512::fused(us, them, bucket.fused_weights.as_ref().unwrap(), &bucket.l1.biases);
    assert_eq!(expected, fused);
    let score = bucket.propagate(&legacy.0);
    assert_eq!(score, bucket.propagate_accumulators(us, them));
}

#[cfg(all(
    test,
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw",
    target_feature = "avx512vnni"
))]
mod kernel_tests {
    use super::super::ls_l1_kernel::avx512;
    use super::*;
    use rand::{Rng, SeedableRng};
    use rand_xoshiro::Xoshiro256PlusPlus;

    type Bucket = LayerStackBucket<1536, 16, 30, 32>;

    fn random_bucket(rng: &mut Xoshiro256PlusPlus) -> Bucket {
        let mut bytes = Vec::new();
        for (input, output) in [(1536, 16), (32, 32), (32, 1)] {
            for _ in 0..output {
                bytes.extend_from_slice(&rng.random_range(-8192i32..8192).to_le_bytes());
            }
            bytes.extend((0..input * output).map(|_| rng.random::<u8>()));
        }
        Bucket::read(&mut &bytes[..]).unwrap()
    }

    #[test]
    fn all_buckets_full_range_and_weight_edits_match() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1536_0016_0064);
        let boundaries = [
            i16::MIN,
            -128,
            -1,
            0,
            1,
            63,
            64,
            126,
            127,
            128,
            255,
            i16::MAX,
        ];
        for _ in 0..DEFAULT_NUM_BUCKETS {
            let mut bucket = random_bucket(&mut rng);
            let mut us = [0; 1536];
            let mut them = [0; 1536];
            for case in 0..64 {
                for i in 0..1536 {
                    us[i] = if case < boundaries.len() {
                        boundaries[(i + case) % boundaries.len()]
                    } else {
                        rng.random()
                    };
                    them[i] = if case < boundaries.len() {
                        boundaries[(i * 5 + case) % boundaries.len()]
                    } else {
                        rng.random()
                    };
                }
                assert_fused_l1_matches_reference(&bucket, &us, &them);
            }
            // .bin の論理index経由の delta と、層全体の差し替えを両方検証する。
            bucket.edit_l1(|l1| {
                l1.apply_file_weight_delta(1536 * 7 + 991, 17);
                l1.biases[15] += 123;
            });
            assert_fused_l1_matches_reference(&bucket, &us, &them);
            let replacement = random_bucket(&mut rng);
            bucket.edit_l1(|l1| *l1 = replacement.l1);
            assert_fused_l1_matches_reference(&bucket, &us, &them);
        }
    }

    #[test]
    fn fused_covers_every_i16_value() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(123);
        let bucket = random_bucket(&mut rng);
        let mut us = [127; 1536];
        let mut them = [127; 1536];
        for start in (0..65536).step_by(768) {
            for i in 0..768 {
                us[i] = (start + i) as i16;
                them[i + 768] = (start + i) as i16;
            }
            assert_fused_l1_matches_reference(&bucket, &us, &them);
        }
    }

    #[test]
    fn fused_l1_wraps_like_dense_with_extreme_biases() {
        let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
        let mut bucket = random_bucket(&mut rng);
        bucket.edit_l1(|l1| {
            for (i, bias) in l1.biases.iter_mut().enumerate() {
                *bias = if i % 2 == 0 { i32::MIN } else { i32::MAX };
            }
        });
        let us = [127; 1536];
        let mut expected = [0; 16];
        bucket.l1.propagate_7bit(&sqr_clipped_relu_new(&us, &us).0, &mut expected);
        assert_eq!(
            expected,
            avx512::fused(&us, &us, bucket.fused_weights.as_ref().unwrap(), &bucket.l1.biases)
        );
    }

    #[test]
    fn unsupported_shapes_do_not_allocate_fused_weights() {
        fn check<const N: usize, const OUT: usize, const IN: usize, const PAD: usize>() {
            let bucket = LayerStackBucket::<N, OUT, IN, PAD>::new();
            assert!(bucket.fused_weights.is_none());
        }
        check::<768, 16, 30, 32>();
        check::<1536, 32, 62, 64>();
    }

    #[test]
    fn interrupted_edit_rebuilds_fused_weights() {
        let mut bucket = Bucket::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bucket.edit_l1(|l1| {
                l1.weights.make_mut().fill(1);
                panic!("edit interrupted");
            });
        }));
        assert!(result.is_err());
        assert!(bucket.fused_weights.is_some());
        let us = [127; 1536];
        assert_eq!(
            bucket.propagate(&sqr_clipped_relu_new(&us, &us).0),
            bucket.propagate_accumulators(&us, &us)
        );
    }
}
