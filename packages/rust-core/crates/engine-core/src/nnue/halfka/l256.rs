//! HalfKA L1=256 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka::AccumulatorStackHalfKA;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKA256CReLU, HalfKA256Pairwise, HalfKA256SCReLU};

crate::define_l1_variants!(
    enum HalfKAL256,
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
        assert_eq!(HalfKAL256::SUPPORTED_SPECS.len(), 3);

        let spec = &HalfKAL256::SUPPORTED_SPECS[0];
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
        for spec in HalfKAL256::SUPPORTED_SPECS {
            assert_eq!(spec.l1, 256);
        }
    }
}
