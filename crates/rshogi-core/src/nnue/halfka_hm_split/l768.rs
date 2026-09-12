//! HalfKaHmSplit L1=768 のアーキテクチャバリアント

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
    enum HalfKaHmSplitL768,
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
