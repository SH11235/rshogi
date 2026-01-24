//! HalfKP L1=1024 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKP1024_8_32CReLU, HalfKP1024_8_32Pairwise, HalfKP1024_8_32SCReLU};

crate::define_l1_variants!(
    enum HalfKPL1024,
    feature_set HalfKP,
    l1 1024,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<1024>,
    stack AccumulatorStackHalfKP<1024>,

    variants {
        // L2=8, L3=32 バリアント
        (8,  32, CReLU,         "CReLU")    => CReLU8x32     : HalfKP1024_8_32CReLU,
        (8,  32, SCReLU,        "SCReLU")   => SCReLU8x32    : HalfKP1024_8_32SCReLU,
        (8,  32, PairwiseCReLU, "Pairwise") => Pairwise8x32  : HalfKP1024_8_32Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKPL1024::SUPPORTED_SPECS.len(), 3);

        // 8-32 CReLU
        let spec = &HalfKPL1024::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKP);
        assert_eq!(spec.l1, 1024);
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 32);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKPL1024::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 1024);
        }
    }
}
