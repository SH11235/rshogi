//! HalfKaHmSplit L1=768 のアーキテクチャバリアント
// NOTE: 公式表記(HalfKaHmSplit)をenum名に保持するため、非CamelCaseを許可する。
#![allow(non_camel_case_types)]

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm_split::AccumulatorStackHalfKaHmSplit;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaHmSplit768CReLU, HalfKaHmSplit768Pairwise, HalfKaHmSplit768SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaHmSplit_L768,
    feature_set HalfKaHmSplit,
    l1 768,
    acc crate::nnue::network_halfka_hm_split::AccumulatorHalfKaHmSplit<768>,
    stack AccumulatorStackHalfKaHmSplit<768>,

    variants {
        // L2=16, L3=64 バリアント
        (16, 64, CReLU)         => CReLU16x64    : HalfKaHmSplit768CReLU,
        (16, 64, SCReLU)        => SCReLU16x64   : HalfKaHmSplit768SCReLU,
        (16, 64, PairwiseCReLU) => Pairwise16x64 : HalfKaHmSplit768Pairwise,
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_specs() {
        assert_eq!(HalfKaHmSplit_L768::SUPPORTED_SPECS.len(), 3);

        // 16-64 CReLU
        let spec = &HalfKaHmSplit_L768::SUPPORTED_SPECS[0];
        assert_eq!(spec.feature_set, FeatureSet::HalfKaHmSplit);
        assert_eq!(spec.l1, 768);
        assert_eq!(spec.l2, 16);
        assert_eq!(spec.l3, 64);
        assert_eq!(spec.activation, Activation::CReLU);
    }

    #[test]
    fn test_l1_size() {
        for spec in HalfKaHmSplit_L768::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 768);
        }
    }

    /// マクロ生成: architecture_name() の命名規則テスト
    #[test]
    fn test_architecture_name_format() {
        for spec in HalfKaHmSplit_L768::SUPPORTED_SPECS {
            let name = spec.name();
            assert!(
                name.starts_with("HalfKA_hm_split-768-"),
                "Architecture name should start with 'HalfKA_hm_split-768-', got: {name}"
            );
        }
    }

    /// マクロ生成: 3 種の活性化関数がすべて登録されていることを確認
    #[test]
    fn test_supported_activations() {
        let activations: Vec<_> =
            HalfKaHmSplit_L768::SUPPORTED_SPECS.iter().map(|s| s.activation).collect();
        assert!(activations.contains(&Activation::CReLU));
        assert!(activations.contains(&Activation::SCReLU));
        assert!(activations.contains(&Activation::PairwiseCReLU));
    }
}
