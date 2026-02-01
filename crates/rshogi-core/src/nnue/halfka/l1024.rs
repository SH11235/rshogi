//! HalfKA L1=1024 のアーキテクチャバリアント
// NOTE: 公式表記(HalfKA)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka::AccumulatorStackHalfKA;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKA1024CReLU, HalfKA1024_8_32CReLU, HalfKA1024_8_64CReLU};

crate::define_l1_variants!(
    enum HalfKA_L1024,
    feature_set HalfKA,
    l1 1024,
    acc crate::nnue::network_halfka::AccumulatorHalfKA<1024>,
    stack AccumulatorStackHalfKA<1024>,

    variants {
        // L2=8, L3=64 バリアント
        (8,  64, CReLU,         "CReLU")    => CReLU8x64     : HalfKA1024_8_64CReLU,
        // L2=8, L3=96 バリアント
        (8,  96, CReLU,         "CReLU")    => CReLU8x96     : HalfKA1024CReLU,
        // L2=8, L3=32 バリアント
        (8,  32, CReLU,         "CReLU")    => CReLU8x32     : HalfKA1024_8_32CReLU,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKA_L1024::SUPPORTED_SPECS.len(), 3);

        // 8-64 CReLU
        let spec = &HalfKA_L1024::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKA);
        assert_eq!(spec.l1, 1024);
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 64);
        assert_eq!(spec.activation, Activation::CReLU);

        // 8-96 CReLU
        let spec = &HalfKA_L1024::SUPPORTED_SPECS[1];
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 96);

        // 8-32 CReLU
        let spec = &HalfKA_L1024::SUPPORTED_SPECS[2];
        assert_eq!(spec.l2, 8);
        assert_eq!(spec.l3, 32);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKA_L1024::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 1024);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKA_L1024::SUPPORTED_SPECS {
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
        for spec in HalfKA_L1024::SUPPORTED_SPECS {
            assert_eq!(spec.activation, Activation::CReLU);
            assert_eq!(spec.activation.output_dim_divisor(), 1);
        }
    }

    /// マクロ生成: L2/L3 の組み合わせが複数あることを確認
    #[test]
    fn test_multiple_l2_l3_combinations() {
        let combinations: Vec<_> =
            HalfKA_L1024::SUPPORTED_SPECS.iter().map(|s| (s.l2, s.l3)).collect();

        assert!(combinations.contains(&(8, 64)), "Should support L2=8, L3=64");
        assert!(combinations.contains(&(8, 96)), "Should support L2=8, L3=96");
        assert!(combinations.contains(&(8, 32)), "Should support L2=8, L3=32");
    }
}
