//! HalfKA L1=1024 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka::AccumulatorStackHalfKA;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKA1024CReLU, HalfKA1024Pairwise, HalfKA1024SCReLU, HalfKA1024_8_32CReLU,
    HalfKA1024_8_32Pairwise, HalfKA1024_8_32SCReLU,
};

crate::define_l1_variants!(
    enum HalfKAL1024,
    feature_set HalfKA,
    l1 1024,
    acc crate::nnue::network_halfka::AccumulatorHalfKA<1024>,
    stack AccumulatorStackHalfKA<1024>,

    variants {
        // L2=8, L3=96 バリアント
        (8,  96, CReLU,         "CReLU")    => CReLU8x96     : HalfKA1024CReLU,
        (8,  96, SCReLU,        "SCReLU")   => SCReLU8x96    : HalfKA1024SCReLU,
        (8,  96, PairwiseCReLU, "Pairwise") => Pairwise8x96  : HalfKA1024Pairwise,
        // L2=8, L3=32 バリアント
        (8,  32, CReLU,         "CReLU")    => CReLU8x32     : HalfKA1024_8_32CReLU,
        (8,  32, SCReLU,        "SCReLU")   => SCReLU8x32    : HalfKA1024_8_32SCReLU,
        (8,  32, PairwiseCReLU, "Pairwise") => Pairwise8x32  : HalfKA1024_8_32Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKAL1024::SUPPORTED_SPECS.len(), 6);

        // 8-96 CReLU
        let spec = &HalfKAL1024::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKA);
        assert_eq!(spec.l1, 1024);
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 96);
        assert_eq!(spec.activation, Activation::CReLU);

        // 8-32 CReLU
        let spec = &HalfKAL1024::SUPPORTED_SPECS[3];
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 32);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKAL1024::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 1024);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKAL1024::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKA-1024-"),
                "Architecture name should start with 'HalfKA-1024-', got: {name}"
            );
        }
    }

    /// マクロ生成: 活性化関数の output_dim_divisor テスト
    #[test]
    fn test_activation_output_dim_divisor() {
        for spec in HalfKAL1024::SUPPORTED_SPECS {
            match spec.activation {
                Activation::CReLU | Activation::SCReLU => {
                    assert_eq!(spec.activation.output_dim_divisor(), 1);
                }
                Activation::PairwiseCReLU => {
                    assert_eq!(spec.activation.output_dim_divisor(), 2);
                }
            }
        }
    }

    /// マクロ生成: L2/L3 の組み合わせが複数あることを確認
    #[test]
    fn test_multiple_l2_l3_combinations() {
        let combinations: Vec<_> =
            HalfKAL1024::SUPPORTED_SPECS.iter().map(|s| (s.l2, s.l3)).collect();

        // L2=8, L3=96 と L2=8, L3=32 の2パターン
        assert!(combinations.contains(&(8, 96)), "Should support L2=8, L3=96");
        assert!(combinations.contains(&(8, 32)), "Should support L2=8, L3=32");
    }

    /// マクロ生成: すべての活性化タイプがサポートされていることを確認
    #[test]
    fn test_all_activations_present() {
        let activations: Vec<_> =
            HalfKAL1024::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();

        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }
}
