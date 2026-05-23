//! HalfKaHmSplit L1=1024 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm_split::AccumulatorStackHalfKaHmSplit;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaHmSplit1024_8_32CReLU, HalfKaHmSplit1024_8_32Pairwise, HalfKaHmSplit1024_8_32SCReLU,
    HalfKaHmSplit1024_8_64CReLU, HalfKaHmSplit1024_8_64Pairwise, HalfKaHmSplit1024_8_64SCReLU,
    HalfKaHmSplit1024CReLU, HalfKaHmSplit1024Pairwise, HalfKaHmSplit1024SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaHmSplitL1024,
    feature_set HalfKaHmSplit,
    l1 1024,
    acc crate::nnue::network_halfka_hm_split::AccumulatorHalfKaHmSplit<1024>,
    stack AccumulatorStackHalfKaHmSplit<1024>,

    variants {
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKaHmSplit1024_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKaHmSplit1024_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKaHmSplit1024_8_64Pairwise,
        // L2=8, L3=96 バリアント
        (8,  96, CReLU)         => CReLU8x96     : HalfKaHmSplit1024CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKaHmSplit1024SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKaHmSplit1024Pairwise,
        // L2=8, L3=32 バリアント
        (8,  32, CReLU)         => CReLU8x32     : HalfKaHmSplit1024_8_32CReLU,
        (8,  32, SCReLU)        => SCReLU8x32    : HalfKaHmSplit1024_8_32SCReLU,
        (8,  32, PairwiseCReLU) => Pairwise8x32  : HalfKaHmSplit1024_8_32Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKaHmSplitL1024::SUPPORTED_SPECS.len(), 9);

        // 8-64 CReLU
        let spec = &HalfKaHmSplitL1024::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKaHmSplit);
        assert_eq!(spec.l1, 1024);
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 64);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKaHmSplitL1024::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 1024);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKaHmSplitL1024::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKaHmSplit-1024-"),
                "Architecture name should start with 'HalfKaHmSplit-1024-', got: {name}"
            );
        }
    }

    /// マクロ生成: 3 種の活性化関数がすべて登録されていることを確認
    #[test]
    fn test_supported_activations() {
        let activations: Vec<_> =
            HalfKaHmSplitL1024::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();
        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }

    /// マクロ生成: L2/L3 の組み合わせが複数あることを確認
    #[test]
    fn test_multiple_l2_l3_combinations() {
        let combinations: Vec<_> =
            HalfKaHmSplitL1024::SUPPORTED_SPECS.iter().map(|s| (s.l2, s.l3)).collect();

        assert!(combinations.contains(&(8, 64)), "Should support L2=8, L3=64");
        assert!(combinations.contains(&(8, 96)), "Should support L2=8, L3=96");
        assert!(combinations.contains(&(8, 32)), "Should support L2=8, L3=32");
    }
}
