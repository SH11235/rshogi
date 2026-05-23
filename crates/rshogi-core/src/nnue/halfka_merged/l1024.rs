//! HalfKaMerged L1=1024 のアーキテクチャバリアント
// NOTE: 公式表記(HalfKaMerged)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_merged::AccumulatorStackHalfKaMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaMerged1024_8_32CReLU, HalfKaMerged1024_8_32Pairwise, HalfKaMerged1024_8_32SCReLU,
    HalfKaMerged1024_8_64CReLU, HalfKaMerged1024_8_64Pairwise, HalfKaMerged1024_8_64SCReLU,
    HalfKaMerged1024CReLU, HalfKaMerged1024Pairwise, HalfKaMerged1024SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaMerged_L1024,
    feature_set HalfKaMerged,
    l1 1024,
    acc crate::nnue::network_halfka_merged::AccumulatorHalfKaMerged<1024>,
    stack AccumulatorStackHalfKaMerged<1024>,

    variants {
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKaMerged1024_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKaMerged1024_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKaMerged1024_8_64Pairwise,
        // L2=8, L3=96 バリアント
        (8,  96, CReLU)         => CReLU8x96     : HalfKaMerged1024CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKaMerged1024SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKaMerged1024Pairwise,
        // L2=8, L3=32 バリアント
        (8,  32, CReLU)         => CReLU8x32     : HalfKaMerged1024_8_32CReLU,
        (8,  32, SCReLU)        => SCReLU8x32    : HalfKaMerged1024_8_32SCReLU,
        (8,  32, PairwiseCReLU) => Pairwise8x32  : HalfKaMerged1024_8_32Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKaMerged_L1024::SUPPORTED_SPECS.len(), 9);

        // 8-64 CReLU
        let spec = &HalfKaMerged_L1024::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKaMerged);
        assert_eq!(spec.l1, 1024);
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 64);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKaMerged_L1024::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 1024);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKaMerged_L1024::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKaMerged-1024-"),
                "Architecture name should start with 'HalfKaMerged-1024-', got: {name}"
            );
        }
    }

    /// マクロ生成: 3 種の活性化関数がすべて登録されていることを確認
    #[test]
    fn test_supported_activations() {
        let activations: Vec<_> =
            HalfKaMerged_L1024::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();
        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }

    /// マクロ生成: L2/L3 の組み合わせが複数あることを確認
    #[test]
    fn test_multiple_l2_l3_combinations() {
        let combinations: Vec<_> =
            HalfKaMerged_L1024::SUPPORTED_SPECS.iter().map(|s| (s.l2, s.l3)).collect();

        assert!(combinations.contains(&(8, 64)), "Should support L2=8, L3=64");
        assert!(combinations.contains(&(8, 96)), "Should support L2=8, L3=96");
        assert!(combinations.contains(&(8, 32)), "Should support L2=8, L3=32");
    }
}
