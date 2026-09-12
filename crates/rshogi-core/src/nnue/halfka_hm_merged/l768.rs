//! HalfKaHmMerged L1=768 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm_merged::AccumulatorStackHalfKaHmMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaHmMerged768CReLU, HalfKaHmMerged768Pairwise, HalfKaHmMerged768SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaHmMergedL768,
    feature_set HalfKaHmMerged,
    l1 768,
    acc crate::nnue::network_halfka_hm_merged::AccumulatorHalfKaHmMerged<768>,
    stack AccumulatorStackHalfKaHmMerged<768>,

    variants {
        // L2=16, L3=64 バリアント
        (16, 64, CReLU)         => CReLU16x64    : HalfKaHmMerged768CReLU,
        (16, 64, SCReLU)        => SCReLU16x64   : HalfKaHmMerged768SCReLU,
        (16, 64, PairwiseCReLU) => Pairwise16x64 : HalfKaHmMerged768Pairwise,
    }
);
