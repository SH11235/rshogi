use std::io::{self, Read};

use super::accumulator::Aligned;
use super::layer_stacks::{LsSaturationCounts, read_layer_stack_buckets};
use super::layers::AffineTransform;
#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw",
    any(target_feature = "avx512vnni", ls_avx512bw_fused_1536)
))]
use super::{layer_stacks::sqr_clipped_relu_new, ls_l1_kernel::avx512};

pub(super) const DENSE_CONCAT_FT: usize = if cfg!(feature = "layerstacks-dense-concat-1536x16x64") {
    1536
} else {
    2304
};

const FC1_IN: usize = 32;
const FC2_IN: usize = 160;

/// DenseConcat 後段の bucket。
#[derive(Clone)]
pub struct DenseConcatBucket<const L1: usize, const FC0_OUT: usize, const FC1_OUT: usize> {
    l1: AffineTransform<L1, FC0_OUT>,
    l2: AffineTransform<FC1_IN, FC1_OUT>,
    output: AffineTransform<FC2_IN, 1>,
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        any(target_feature = "avx512vnni", ls_avx512bw_fused_1536)
    ))]
    fused_weights: Option<avx512::FusedWeights<L1>>,
}

impl<const L1: usize, const FC0_OUT: usize, const FC1_OUT: usize>
    DenseConcatBucket<L1, FC0_OUT, FC1_OUT>
{
    fn read<R: Read>(reader: &mut R) -> io::Result<Self> {
        const {
            assert!(matches!(L1, 1536 | 2304));
            assert!(FC1_IN == 2 * FC0_OUT && FC2_IN == FC1_IN + 2 * FC1_OUT);
        }
        let l1 = AffineTransform::read(reader)?;
        Ok(Self {
            #[cfg(all(
                target_arch = "x86_64",
                target_feature = "avx512f",
                target_feature = "avx512bw",
                any(target_feature = "avx512vnni", ls_avx512bw_fused_1536)
            ))]
            fused_weights: avx512::reorder::<L1, FC0_OUT, FC1_OUT>(&l1),
            l1,
            l2: AffineTransform::read(reader)?,
            output: AffineTransform::read(reader)?,
        })
    }

    /// fc_0 層を返す。
    pub fn l1(&self) -> &AffineTransform<L1, FC0_OUT> {
        &self.l1
    }

    /// 変換済みの FT 出力からスケーリング前の生スコアを計算する。
    #[inline]
    pub fn propagate(&self, input: &[u8; L1]) -> i32 {
        self.propagate_from_l1(&self.propagate_l1(input), None).0
    }

    #[inline]
    fn propagate_l1(&self, input: &[u8; L1]) -> [i32; FC0_OUT] {
        let mut fc0 = [0; FC0_OUT];
        self.l1.propagate_7bit(input, &mut fc0);
        fc0
    }

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        any(target_feature = "avx512vnni", ls_avx512bw_fused_1536)
    ))]
    #[inline]
    pub(super) fn propagate_accumulators(&self, us: &[i16; L1], them: &[i16; L1]) -> i32 {
        let Some(weights) = &self.fused_weights else {
            return self.propagate(&sqr_clipped_relu_new(us, them).0);
        };
        let fc0 = avx512::propagate_l1(us, them, &self.l1, weights);
        self.propagate_from_l1(&fc0, None).0
    }

    #[cfg(all(
        feature = "nnue-threat",
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw",
        any(target_feature = "avx512vnni", ls_avx512bw_fused_1536)
    ))]
    #[inline]
    pub(super) fn propagate_accumulators_with_threat(
        &self,
        us: &[i16; L1],
        them: &[i16; L1],
        us_threat: &super::threat_width::ThreatAccumulation<L1>,
        them_threat: &super::threat_width::ThreatAccumulation<L1>,
    ) -> Option<i32> {
        let weights = self.fused_weights.as_ref()?;
        let fc0 =
            avx512::fused_with_threat(us, them, us_threat, them_threat, weights, &self.l1.biases);
        Some(self.propagate_from_l1(&fc0, None).0)
    }

    pub(super) fn propagate_counting_saturation(
        &self,
        input: &[u8; L1],
        counts: &mut LsSaturationCounts,
    ) -> i32 {
        self.propagate_from_l1(&self.propagate_l1(input), Some(counts)).0
    }

    #[inline]
    fn propagate_from_l1(
        &self,
        fc0: &[i32; FC0_OUT],
        counts: Option<&mut LsSaturationCounts>,
    ) -> (i32, i32) {
        let mut concat = Aligned([0u8; FC2_IN]);
        let (c0, c1) = concat.0.split_at_mut(FC1_IN);
        pair_activations(fc0, c0);

        let mut fc1 = [0; FC1_OUT];
        self.l2.propagate_7bit(c0, &mut fc1);
        pair_activations(&fc1, c1);
        if let Some(counts) = counts {
            counts.l1_act_sat += c0.iter().filter(|&&v| v == 127).count() as u64;
            counts.l1_act_total += c0.len() as u64;
            counts.l2_act_sat += c1.iter().filter(|&&v| v == 127).count() as u64;
            counts.l2_act_total += c1.len() as u64;
        }

        let mut fc2 = [0];
        self.output.propagate_7bit(&concat.0, &mut fc2);
        let skip = fc0[FC0_OUT - 2] - fc0[FC0_OUT - 1];
        (fc2[0] + skip, skip)
    }
}

#[inline]
fn pair_activations(input: &[i32], output: &mut [u8]) {
    let (squared, clipped) = output.split_at_mut(input.len());
    for ((squared, clipped), &value) in squared.iter_mut().zip(clipped).zip(input) {
        // 絶対値が 46340 を超えると、i32 では二乗がオーバーフローする。
        let wide = i64::from(value);
        *squared = ((wide * wide) >> 19).clamp(0, 127) as u8;
        *clipped = (value >> 6).clamp(0, 127) as u8;
    }
}

/// DenseConcat 後段の bucket 集合。
#[derive(Clone)]
pub struct DenseConcatLayerStacks<const L1: usize, const FC0_OUT: usize, const FC1_OUT: usize> {
    /// ファイルに格納されている順の bucket。
    pub buckets: Box<[DenseConcatBucket<L1, FC0_OUT, FC1_OUT>]>,
}

impl<const L1: usize, const FC0_OUT: usize, const FC1_OUT: usize>
    DenseConcatLayerStacks<L1, FC0_OUT, FC1_OUT>
{
    pub(super) fn read<R: Read>(reader: &mut R, num_buckets: usize) -> io::Result<Self> {
        Ok(Self {
            buckets: read_layer_stack_buckets(reader, num_buckets, DenseConcatBucket::read)?,
        })
    }

    /// 指定 bucket のスケーリング前の生スコアを返す。
    #[inline]
    pub fn evaluate_raw(&self, bucket_index: usize, input: &[u8; L1]) -> i32 {
        self.buckets[bucket_index].propagate(input)
    }

    #[cfg(feature = "diagnostics")]
    pub(super) fn evaluate_raw_with_diagnostics(
        &self,
        bucket_index: usize,
        input: &[u8; L1],
    ) -> (i32, [i32; FC0_OUT], i32) {
        let bucket = &self.buckets[bucket_index];
        let fc0 = bucket.propagate_l1(input);
        let (raw, skip) = bucket.propagate_from_l1(&fc0, None);
        (raw, fc0, skip)
    }
}
