//! HalfKA L1=256 のアーキテクチャバリアント
// NOTE: 公式表記(HalfKA)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka::AccumulatorStackHalfKA;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKA256CReLU, HalfKA256Pairwise, HalfKA256SCReLU};

crate::define_l1_variants!(
    enum HalfKA_L256,
    feature_set HalfKA,
    l1 256,
    acc crate::nnue::network_halfka::AccumulatorHalfKA<256>,
    stack AccumulatorStackHalfKA<256>,

    variants {
        (32, 32, CReLU,         "CReLU")    => CReLU32x32        : HalfKA256CReLU,
        (32, 32, SCReLU,        "SCReLU")   => SCReLU32x32       : HalfKA256SCReLU,
        (32, 32, PairwiseCReLU, "Pairwise") => Pairwise32x32     : HalfKA256Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKA_L256::SUPPORTED_SPECS.len(), 3);

        let spec = &HalfKA_L256::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKA);
        assert_eq!(spec.l1, 256);
        assert_eq!(spec.l2, 32);
        assert_eq!(spec.l3, 32);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        // 静的メソッドでのテスト用にダミーのネットワークを読み込む必要があるが、
        // ファイルがないのでここではスペックの確認のみ
        for spec in HalfKA_L256::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 256);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKA_L256::SUPPORTED_SPECS {
            let name = spec.name();
            // HalfKA-256-L2-L3-Activation 形式
            assert!(
                name.starts_with("HalfKA-256-"),
                "Architecture name should start with 'HalfKA-256-', got: {name}"
            );
        }
    }

    /// マクロ生成: 活性化関数の output_dim_divisor が正しく設定されているかテスト
    #[test]
    fn test_activation_output_dim_divisor() {
        for spec in HalfKA_L256::SUPPORTED_SPECS {
            match spec.activation {
                Activation::CReLU | Activation::SCReLU => {
                    assert_eq!(
                        spec.activation.output_dim_divisor(),
                        1,
                        "CReLU/SCReLU should have divisor 1"
                    );
                }
                Activation::PairwiseCReLU => {
                    assert_eq!(
                        spec.activation.output_dim_divisor(),
                        2,
                        "PairwiseCReLU should have divisor 2"
                    );
                }
            }
        }
    }

    /// マクロ生成: すべての活性化タイプがサポートされていることを確認
    #[test]
    fn test_all_activations_present() {
        let activations: Vec<_> =
            HalfKA_L256::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();

        assert!(activations.contains(&Activation::CReLU), "CReLU should be supported");
        assert!(activations.contains(&Activation::SCReLU), "SCReLU should be supported");
        assert!(
            activations.contains(&Activation::PairwiseCReLU),
            "PairwiseCReLU should be supported"
        );
    }

    /// マクロ生成: L2/L3 の妥当な範囲チェック
    #[test]
    fn test_l2_l3_valid_range() {
        for spec in HalfKA_L256::SUPPORTED_SPECS {
            assert!(
                spec.l2 > 0 && spec.l2 <= 128,
                "L2 should be in range (0, 128], got: {}",
                spec.l2
            );
            assert!(
                spec.l3 > 0 && spec.l3 <= 128,
                "L3 should be in range (0, 128], got: {}",
                spec.l3
            );
        }
    }
}
