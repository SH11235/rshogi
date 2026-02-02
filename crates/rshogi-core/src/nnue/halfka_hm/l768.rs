//! HalfKA_hm L1=768 のアーキテクチャバリアント
// NOTE: 公式表記(HalfKA_hm)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm::AccumulatorStackHalfKA_hm;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::HalfKA_hm768CReLU;

crate::define_l1_variants!(
    enum HalfKA_hm_L768,
    feature_set HalfKA_hm,
    l1 768,
    acc crate::nnue::network_halfka_hm::AccumulatorHalfKA_hm<768>,
    stack AccumulatorStackHalfKA_hm<768>,

    variants {
        // L2=16, L3=64 バリアント
        (16, 64, CReLU,         "CReLU")    => CReLU16x64    : HalfKA_hm768CReLU,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKA_hm_L768::SUPPORTED_SPECS.len(), 1);

        // 16-64 CReLU
        let spec = &HalfKA_hm_L768::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKA_hm);
        assert_eq!(spec.l1, 768);
        assert_eq!(spec.l2, 16);
        assert_eq!(spec.l3, 64);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKA_hm_L768::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 768);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKA_hm_L768::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKA_hm-768-"),
                "Architecture name should start with 'HalfKA_hm-768-', got: {name}"
            );
        }
    }

    /// マクロ生成: 活性化関数の output_dim_divisor テスト
    #[test]
    fn test_activation_output_dim_divisor() {
        for spec in HalfKA_hm_L768::SUPPORTED_SPECS {
            assert_eq!(spec.activation, Activation::CReLU);
            assert_eq!(spec.activation.output_dim_divisor(), 1);
        }
    }
}
