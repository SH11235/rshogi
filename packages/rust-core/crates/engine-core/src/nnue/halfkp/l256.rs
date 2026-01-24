//! HalfKP L1=256 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKP256CReLU, HalfKP256Pairwise, HalfKP256SCReLU};

crate::define_l1_variants!(
    enum HalfKPL256,
    feature_set HalfKP,
    l1 256,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<256>,
    stack AccumulatorStackHalfKP<256>,

    variants {
        (32, 32, CReLU,         "CReLU")    => CReLU32x32       : HalfKP256CReLU,
        (32, 32, SCReLU,        "SCReLU")   => SCReLU32x32      : HalfKP256SCReLU,
        (32, 32, PairwiseCReLU, "Pairwise") => Pairwise32x32    : HalfKP256Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKPL256::SUPPORTED_SPECS.len(), 3);

        let spec = &HalfKPL256::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKP);
        assert_eq!(spec.l1, 256);
        assert_eq!(spec.l2, 32);
        assert_eq!(spec.l3, 32);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKPL256::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 256);
        }
    }
}
