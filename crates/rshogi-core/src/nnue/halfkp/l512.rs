//! HalfKP L1=512 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKP512_8_64CReLU, HalfKP512_8_64Pairwise, HalfKP512_8_64SCReLU, HalfKP512_32_32CReLU,
    HalfKP512_32_32Pairwise, HalfKP512_32_32SCReLU, HalfKP512CReLU, HalfKP512Pairwise,
    HalfKP512SCReLU,
};

crate::define_l1_variants!(
    enum HalfKPL512,
    feature_set HalfKP,
    l1 512,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<512>,
    stack AccumulatorStackHalfKP<512>,

    variants {
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKP512_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKP512_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKP512_8_64Pairwise,
        // L2=8, L3=96 バリアント
        (8,  96, CReLU)         => CReLU8x96     : HalfKP512CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKP512SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKP512Pairwise,
        // L2=32, L3=32 バリアント
        (32, 32, CReLU)         => CReLU32x32    : HalfKP512_32_32CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKP512_32_32SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKP512_32_32Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKPL512::SUPPORTED_SPECS.len(), 9);

        // 8-64 CReLU
        let spec = &HalfKPL512::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKP);
        assert_eq!(spec.l1, 512);
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 64);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKPL512::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 512);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKPL512::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKP-512-"),
                "Architecture name should start with 'HalfKP-512-', got: {name}"
            );
        }
    }

    /// マクロ生成: 3 種の活性化関数がすべて登録されていることを確認
    #[test]
    fn test_supported_activations() {
        let activations: Vec<_> =
            HalfKPL512::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();
        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }

    /// マクロ生成: L2/L3 の組み合わせが複数あることを確認
    #[test]
    fn test_multiple_l2_l3_combinations() {
        let combinations: Vec<_> =
            HalfKPL512::SUPPORTED_SPECS.iter().map(|s| (s.l2, s.l3)).collect();

        assert!(combinations.contains(&(8, 64)), "Should support L2=8, L3=64");
        assert!(combinations.contains(&(8, 96)), "Should support L2=8, L3=96");
        assert!(combinations.contains(&(32, 32)), "Should support L2=32, L3=32");
    }
}
