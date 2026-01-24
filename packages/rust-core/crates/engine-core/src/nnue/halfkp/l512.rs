//! HalfKP L1=512 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKP512CReLU, HalfKP512Pairwise, HalfKP512SCReLU, HalfKP512_32_32CReLU,
};

crate::define_l1_variants!(
    enum HalfKPL512,
    feature_set HalfKP,
    l1 512,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<512>,
    stack AccumulatorStackHalfKP<512>,

    variants {
        // L2=8, L3=96 バリアント
        (8,  96, CReLU,         "CReLU")    => CReLU8x96     : HalfKP512CReLU,
        (8,  96, SCReLU,        "SCReLU")   => SCReLU8x96    : HalfKP512SCReLU,
        (8,  96, PairwiseCReLU, "Pairwise") => Pairwise8x96  : HalfKP512Pairwise,
        // L2=32, L3=32 バリアント
        (32, 32, CReLU,         "CReLU")    => CReLU32x32    : HalfKP512_32_32CReLU,
        // 将来の追加はここに1行追加するだけ
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKPL512::SUPPORTED_SPECS.len(), 4);

        // 8-96 CReLU
        let spec = &HalfKPL512::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKP);
        assert_eq!(spec.l1, 512);
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 96);
        assert_eq!(spec.activation, Activation::CReLU);

        // 32-32 CReLU
        let spec = &HalfKPL512::SUPPORTED_SPECS[3];
        assert_eq!(spec.l2, 32);
        assert_eq!(spec.l3, 32);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKPL512::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 512);
        }
    }
}
