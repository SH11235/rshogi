//! HalfKP L1=256 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKP256CReLU, HalfKP256Pairwise, HalfKP256SCReLU};

crate::define_l1_variants!(
    enum HalfKPL256,
    feature_set HalfKP,
    l1 256,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<256>,
    stack AccumulatorStackHalfKP<256>,

    variants {
        (32, 32, CReLU,         "CReLU")    => CReLU32x32       : HalfKP256CReLU,
        (32, 32, SCReLU,        "SCReLU")   => SCReLU32x32      : HalfKP256SCReLU,
        (32, 32, PairwiseCReLU, "Pairwise") => Pairwise32x32    : HalfKP256Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKPL256::SUPPORTED_SPECS.len(), 3);

        let spec = &HalfKPL256::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKP);
        assert_eq!(spec.l1, 256);
        assert_eq!(spec.l2, 32);
        assert_eq!(spec.l3, 32);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKPL256::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 256);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKPL256::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKP-256-"),
                "Architecture name should start with 'HalfKP-256-', got: {name}"
            );
        }
    }

    /// マクロ生成: 活性化関数の output_dim_divisor テスト
    #[test]
    fn test_activation_output_dim_divisor() {
        for spec in HalfKPL256::SUPPORTED_SPECS {
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
            HalfKPL256::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();

        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }

    /// マクロ生成: L2/L3 の妥当な範囲チェック
    #[test]
    fn test_l2_l3_valid_range() {
        for spec in HalfKPL256::SUPPORTED_SPECS {
            assert!(spec.l2 > 0 && spec.l2 <= 128, "L2 should be in range (0, 128]");
            assert!(spec.l3 > 0 && spec.l3 <= 128, "L3 should be in range (0, 128]");
        }
    }
}
