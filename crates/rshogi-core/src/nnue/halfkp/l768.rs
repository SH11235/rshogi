//! HalfKP L1=768 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKP768CReLU, HalfKP768Pairwise, HalfKP768SCReLU};

crate::define_l1_variants!(
    enum HalfKPL768,
    feature_set HalfKP,
    l1 768,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<768>,
    stack AccumulatorStackHalfKP<768>,

    variants {
        // L2=16, L3=64 バリアント
        (16, 64, CReLU,         "CReLU")    => CReLU16x64    : HalfKP768CReLU,
        (16, 64, SCReLU,        "SCReLU")   => SCReLU16x64   : HalfKP768SCReLU,
        (16, 64, PairwiseCReLU, "Pairwise") => Pairwise16x64 : HalfKP768Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKPL768::SUPPORTED_SPECS.len(), 3);

        // 16-64 CReLU
        let spec = &HalfKPL768::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKP);
        assert_eq!(spec.l1, 768);
        assert_eq!(spec.l2, 16);
        assert_eq!(spec.l3, 64);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKPL768::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 768);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKPL768::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKP-768-"),
                "Architecture name should start with 'HalfKP-768-', got: {name}"
            );
        }
    }

    /// マクロ生成: 活性化関数の output_dim_divisor テスト
    #[test]
    fn test_activation_output_dim_divisor() {
        for spec in HalfKPL768::SUPPORTED_SPECS {
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

    /// マクロ生成: すべての活性化タイプがサポートされていることを確認
    #[test]
    fn test_all_activations_present() {
        let activations: Vec<_> =
            HalfKPL768::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();

        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }
}
