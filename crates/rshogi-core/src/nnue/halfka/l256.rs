//! HalfKA L1=256 のアーキテクチャバリアント
// NOTE: 公式表記(HalfKA)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka::AccumulatorStackHalfKA;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::HalfKA256CReLU;

crate::define_l1_variants!(
    enum HalfKA_L256,
    feature_set HalfKA,
    l1 256,
    acc crate::nnue::network_halfka::AccumulatorHalfKA<256>,
    stack AccumulatorStackHalfKA<256>,

    variants {
        (32, 32, CReLU,         "CReLU")    => CReLU32x32        : HalfKA256CReLU,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKA_L256::SUPPORTED_SPECS.len(), 1);

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
            assert_eq!(spec.activation, Activation::CReLU);
            assert_eq!(spec.activation.output_dim_divisor(), 1);
        }
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
