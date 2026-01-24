//! HalfKA L1=512 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka::AccumulatorStackHalfKA;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKA512CReLU, HalfKA512Pairwise, HalfKA512SCReLU, HalfKA512_32_32CReLU,
    HalfKA512_32_32Pairwise, HalfKA512_32_32SCReLU,
};

crate::define_l1_variants!(
    enum HalfKAL512,
    feature_set HalfKA,
    l1 512,
    acc crate::nnue::network_halfka::AccumulatorHalfKA<512>,
    stack AccumulatorStackHalfKA<512>,

    variants {
        // L2=8, L3=96
        (8,  96, CReLU,         "CReLU")    => CReLU8x96      : HalfKA512CReLU,
        (8,  96, SCReLU,        "SCReLU")   => SCReLU8x96     : HalfKA512SCReLU,
        (8,  96, PairwiseCReLU, "Pairwise") => Pairwise8x96   : HalfKA512Pairwise,
        // L2=32, L3=32
        (32, 32, CReLU,         "CReLU")    => CReLU32x32     : HalfKA512_32_32CReLU,
        (32, 32, SCReLU,        "SCReLU")   => SCReLU32x32    : HalfKA512_32_32SCReLU,
        (32, 32, PairwiseCReLU, "Pairwise") => Pairwise32x32  : HalfKA512_32_32Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKAL512::SUPPORTED_SPECS.len(), 6);

        let spec = &HalfKAL512::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKA);
        assert_eq!(spec.l1, 512);
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 96);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKAL512::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 512);
        }
    }
}
