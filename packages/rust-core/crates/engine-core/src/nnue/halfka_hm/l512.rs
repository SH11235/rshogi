//! HalfKA_hm L1=512 のアーキテクチャバリアント
// NOTE: 公式表記(HalfKA_hm)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm::AccumulatorStackHalfKA_hm;
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
    feature_set HalfKA_hm,
    l1 512,
    acc crate::nnue::network_halfka::AccumulatorHalfKA<512>,
    stack AccumulatorStackHalfKA_hm<512>,

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
        assert_eq!(spec.feature_set, FeatureSet::HalfKA_hm);
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

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKAL512::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKA_hm-512-"),
                "Architecture name should start with 'HalfKA_hm-512-', got: {name}"
            );
        }
    }

    /// マクロ生成: 活性化関数の output_dim_divisor テスト
    #[test]
    fn test_activation_output_dim_divisor() {
        for spec in HalfKAL512::SUPPORTED_SPECS {
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
            HalfKAL512::SUPPORTED_SPECS.iter().map(|s| (s.l2, s.l3)).collect();

        // L2=8, L3=96 と L2=32, L3=32 の2パターン
        assert!(combinations.contains(&(8, 96)), "Should support L2=8, L3=96");
        assert!(combinations.contains(&(32, 32)), "Should support L2=32, L3=32");
    }

    /// マクロ生成: すべての活性化タイプがサポートされていることを確認
    #[test]
    fn test_all_activations_present() {
        let activations: Vec<_> =
            HalfKAL512::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();

        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }
}
