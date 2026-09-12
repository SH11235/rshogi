//! HalfKaHmMerged L1=256 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm_merged::AccumulatorStackHalfKaHmMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaHmMerged256CReLU, HalfKaHmMerged256Pairwise, HalfKaHmMerged256SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaHmMergedL256,
    feature_set HalfKaHmMerged,
    l1 256,
    acc crate::nnue::network_halfka_hm_merged::AccumulatorHalfKaHmMerged<256>,
    stack AccumulatorStackHalfKaHmMerged<256>,

    variants {
        (32, 32, CReLU)         => CReLU32x32    : HalfKaHmMerged256CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKaHmMerged256SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKaHmMerged256Pairwise,
    }
);
