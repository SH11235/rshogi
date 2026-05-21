//! HalfKP L1=1024 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKP1024_8_32CReLU, HalfKP1024_8_32Pairwise, HalfKP1024_8_32SCReLU, HalfKP1024_8_64CReLU,
    HalfKP1024_8_64Pairwise, HalfKP1024_8_64SCReLU,
};

crate::define_l1_variants!(
    enum HalfKPL1024,
    feature_set HalfKP,
    l1 1024,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<1024>,
    stack AccumulatorStackHalfKP<1024>,

    variants {
        // L2=8, L3=32 バリアント
        (8,  32, CReLU)         => CReLU8x32     : HalfKP1024_8_32CReLU,
        (8,  32, SCReLU)        => SCReLU8x32    : HalfKP1024_8_32SCReLU,
        (8,  32, PairwiseCReLU) => Pairwise8x32  : HalfKP1024_8_32Pairwise,
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKP1024_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKP1024_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKP1024_8_64Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKPL1024::SUPPORTED_SPECS.len(), 6);

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

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKPL1024::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKP-1024-"),
                "Architecture name should start with 'HalfKP-1024-', got: {name}"
            );
        }
    }

    /// マクロ生成: 3 種の活性化関数がすべて登録されていることを確認
    #[test]
    fn test_supported_activations() {
        let activations: Vec<_> =
            HalfKPL1024::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();
        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }

    /// マクロ生成: L2/L3 の組み合わせが複数あることを確認
    #[test]
    fn test_multiple_l2_l3_combinations() {
        let combinations: Vec<_> =
            HalfKPL1024::SUPPORTED_SPECS.iter().map(|s| (s.l2, s.l3)).collect();

        assert!(combinations.contains(&(8, 32)), "Should support L2=8, L3=32");
        assert!(combinations.contains(&(8, 64)), "Should support L2=8, L3=64");
    }
}
