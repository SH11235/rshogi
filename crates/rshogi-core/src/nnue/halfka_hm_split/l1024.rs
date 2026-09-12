//! HalfKaHmSplit L1=1024 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm_split::AccumulatorStackHalfKaHmSplit;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaHmSplit1024_8_32CReLU, HalfKaHmSplit1024_8_32Pairwise, HalfKaHmSplit1024_8_32SCReLU,
    HalfKaHmSplit1024_8_64CReLU, HalfKaHmSplit1024_8_64Pairwise, HalfKaHmSplit1024_8_64SCReLU,
    HalfKaHmSplit1024CReLU, HalfKaHmSplit1024Pairwise, HalfKaHmSplit1024SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaHmSplitL1024,
    feature_set HalfKaHmSplit,
    l1 1024,
    acc crate::nnue::network_halfka_hm_split::AccumulatorHalfKaHmSplit<1024>,
    stack AccumulatorStackHalfKaHmSplit<1024>,

    variants {
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKaHmSplit1024_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKaHmSplit1024_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKaHmSplit1024_8_64Pairwise,
        // L2=8, L3=96 バリアント
        (8,  96, CReLU)         => CReLU8x96     : HalfKaHmSplit1024CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKaHmSplit1024SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKaHmSplit1024Pairwise,
        // L2=8, L3=32 バリアント
        (8,  32, CReLU)         => CReLU8x32     : HalfKaHmSplit1024_8_32CReLU,
        (8,  32, SCReLU)        => SCReLU8x32    : HalfKaHmSplit1024_8_32SCReLU,
        (8,  32, PairwiseCReLU) => Pairwise8x32  : HalfKaHmSplit1024_8_32Pairwise,
    }
);
