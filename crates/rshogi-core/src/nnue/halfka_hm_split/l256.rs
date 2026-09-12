//! HalfKaHmSplit L1=256 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm_split::AccumulatorStackHalfKaHmSplit;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaHmSplit256CReLU, HalfKaHmSplit256Pairwise, HalfKaHmSplit256SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaHmSplitL256,
    feature_set HalfKaHmSplit,
    l1 256,
    acc crate::nnue::network_halfka_hm_split::AccumulatorHalfKaHmSplit<256>,
    stack AccumulatorStackHalfKaHmSplit<256>,

    variants {
        (32, 32, CReLU)         => CReLU32x32    : HalfKaHmSplit256CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKaHmSplit256SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKaHmSplit256Pairwise,
    }
);
