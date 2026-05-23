//! HalfKaMerged L1=256 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_merged::AccumulatorStackHalfKaMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKaMerged256CReLU, HalfKaMerged256Pairwise, HalfKaMerged256SCReLU};

crate::define_l1_variants!(
    enum HalfKaMergedL256,
    feature_set HalfKaMerged,
    l1 256,
    acc crate::nnue::network_halfka_merged::AccumulatorHalfKaMerged<256>,
    stack AccumulatorStackHalfKaMerged<256>,

    variants {
        (32, 32, CReLU)         => CReLU32x32    : HalfKaMerged256CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKaMerged256SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKaMerged256Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKaMergedL256::SUPPORTED_SPECS.len(), 3);

        let spec = &HalfKaMergedL256::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKaMerged);
        assert_eq!(spec.l1, 256);
        assert_eq!(spec.l2, 32);
        assert_eq!(spec.l3, 32);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        // 静的メソッドでのテスト用にダミーのネットワークを読み込む必要があるが、
        // ファイルがないのでここではスペックの確認のみ
        for spec in HalfKaMergedL256::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 256);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKaMergedL256::SUPPORTED_SPECS {
            let name = spec.name();
            // HalfKaMerged-256-L2-L3-Activation 形式
            assert!(
                name.starts_with("HalfKaMerged-256-"),
                "Architecture name should start with 'HalfKaMerged-256-', got: {name}"
            );
        }
    }

    /// マクロ生成: 3 種の活性化関数がすべて登録されていることを確認
    #[test]
    fn test_supported_activations() {
        let activations: Vec<_> =
            HalfKaMergedL256::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();
        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }

    /// マクロ生成: L2/L3 の妥当な範囲チェック
    #[test]
    fn test_l2_l3_valid_range() {
        for spec in HalfKaMergedL256::SUPPORTED_SPECS {
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
