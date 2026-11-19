//! LayerStacks の駒 FT 重み。ファイル上の i16 と同じ係数を保持する。

use std::io;

use super::accumulator::{IndexList, WeightBox};
use super::finny::apply_weight_changes_to_two;
use super::piece_list::PieceNumber;

#[cfg(not(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw"
)))]
pub(super) type PieceWeights = WeightBox<i16>;

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw"
))]
#[derive(Clone)]
pub(super) enum PieceWeights {
    I16(WeightBox<i16>),
    I8x2(WeightBox<i8>),
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw"
))]
impl From<super::accumulator::AlignedBox<i16>> for PieceWeights {
    fn from(weights: super::accumulator::AlignedBox<i16>) -> Self {
        Self::I16(weights.into())
    }
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw"
))]
impl PieceWeights {
    pub(super) fn share(&mut self, label: &str) {
        match self {
            Self::I16(weights) => weights.share(label),
            Self::I8x2(weights) => weights.share("FT weights (int8 x 2)"),
        }
    }

    #[cfg(test)]
    pub(super) fn make_mut(&mut self) -> &mut [i16] {
        match self {
            Self::I16(weights) => weights.make_mut(),
            Self::I8x2(_) => panic!("test requires unpacked FT weights"),
        }
    }
}

#[cfg(any(
    test,
    all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    )
))]
fn can_pack(weights: &[i16], width: usize) -> bool {
    // カーネルは 32 列ずつ処理する。端数のある幅は i16 のまま保持する。
    width != 0
        && width.is_multiple_of(32)
        && weights.iter().all(|&w| (-254..=254).contains(&w) && w & 1 == 0)
}

#[inline]
pub(super) fn select(weights: WeightBox<i16>, width: usize) -> io::Result<PieceWeights> {
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    {
        if can_pack(&weights, width) {
            let mut packed = super::accumulator::AlignedBox::try_new_zeroed(weights.len())?;
            for (dst, &src) in packed.iter_mut().zip(weights.iter()) {
                *dst = (src / 2) as i8;
            }
            Ok(PieceWeights::I8x2(packed.into()))
        } else {
            Ok(PieceWeights::I16(weights))
        }
    }
    #[cfg(not(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    )))]
    {
        let _ = width;
        Ok(weights)
    }
}

pub(super) fn storage(weights: &PieceWeights) -> &'static str {
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    if matches!(weights, PieceWeights::I8x2(_)) {
        return "int8 x 2";
    }
    let _ = weights;
    "i16"
}

pub(super) fn coefficient(weights: &PieceWeights, index: usize) -> i16 {
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    match weights {
        PieceWeights::I16(weights) => weights[index],
        PieceWeights::I8x2(weights) => i16::from(weights[index]) * 2,
    }
    #[cfg(not(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    )))]
    {
        weights[index]
    }
}

pub(super) fn coefficients(weights: &PieceWeights) -> impl ExactSizeIterator<Item = i16> + '_ {
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    let len = match weights {
        PieceWeights::I16(weights) => weights.len(),
        PieceWeights::I8x2(weights) => weights.len(),
    };
    #[cfg(not(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    )))]
    let len = weights.len();
    (0..len).map(|index| coefficient(weights, index))
}

macro_rules! with_piece_weights {
    ($weights:expr, |$rows:ident| $body:expr) => {{
        #[cfg(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        ))]
        match &$weights {
            $crate::nnue::piece_weights::PieceWeights::I16(weights) => {
                let $rows: &[i16] = weights;
                $body
            }
            $crate::nnue::piece_weights::PieceWeights::I8x2(weights) => {
                let $rows: &[i8] = weights;
                $body
            }
        }
        #[cfg(not(all(
            target_arch = "x86_64",
            target_feature = "avx512f",
            target_feature = "avx512bw"
        )))]
        {
            let $rows: &[i16] = &$weights;
            $body
        }
    }};
}
pub(super) use with_piece_weights;

pub(super) trait PieceWeight: Sized {
    fn unpacked(weights: &[Self]) -> &[i16];

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    fn packed(weights: &[Self]) -> Option<&[i8]>;
}

impl PieceWeight for i16 {
    #[inline(always)]
    fn unpacked(weights: &[Self]) -> &[i16] {
        weights
    }

    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    #[inline(always)]
    fn packed(_: &[Self]) -> Option<&[i8]> {
        None
    }
}

#[cfg(all(
    target_arch = "x86_64",
    target_feature = "avx512f",
    target_feature = "avx512bw"
))]
impl PieceWeight for i8 {
    #[inline(always)]
    fn unpacked(_: &[Self]) -> &[i16] {
        unreachable!("packed FT uses the int8 x 2 kernel");
    }

    #[inline(always)]
    fn packed(weights: &[Self]) -> Option<&[i8]> {
        Some(weights)
    }
}

#[inline]
pub(super) fn refresh_with_diff<const L1: usize, W: PieceWeight>(
    cache: &mut [i16; L1],
    biases: Option<&[i16; L1]>,
    accumulation: &mut [i16; L1],
    weights: &[W],
    removed: &IndexList<{ PieceNumber::NB }>,
    added: &IndexList<{ PieceNumber::NB }>,
) {
    #[cfg(all(
        target_arch = "x86_64",
        target_feature = "avx512f",
        target_feature = "avx512bw"
    ))]
    if let Some(weights) = W::packed(weights) {
        super::ft_packed8::refresh(cache, biases, accumulation, weights, removed, added);
        return;
    }
    apply_weight_changes_to_two::<L1>(
        cache,
        biases.map(|row| row.as_slice()),
        accumulation,
        W::unpacked(weights),
        removed,
        added,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed8_eligibility_requires_even_in_range_weights_and_full_vectors() {
        assert!(can_pack(&[-254, -128, 0, 126, 254], 768));
        for weight in [-256, -253, -1, 1, 253, 256, i16::MIN, i16::MAX] {
            assert!(!can_pack(&[0, 126, weight], 1536), "weight {weight}");
        }
        assert!(!can_pack(&[0], 48));
    }

    #[test]
    fn packed8_selection_preserves_coefficients() {
        use super::super::accumulator::AlignedBox;

        for (width, value, eligible) in [
            (32, -254, true),
            (32, 1, false),
            (32, -256, false),
            (48, 2, false),
        ] {
            let mut weights = AlignedBox::new_zeroed(width * 2);
            weights[width - 1] = 254;
            weights[width] = value;
            let expected = weights.to_vec();
            let weights = select(weights.into(), width).unwrap();
            let packed = eligible
                && cfg!(all(
                    target_arch = "x86_64",
                    target_feature = "avx512f",
                    target_feature = "avx512bw"
                ));
            assert_eq!(storage(&weights), if packed { "int8 x 2" } else { "i16" });
            assert!(coefficients(&weights).eq(expected));
        }
    }
}
