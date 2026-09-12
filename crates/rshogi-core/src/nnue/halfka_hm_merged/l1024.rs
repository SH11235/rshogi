//! HalfKaHmMerged L1=1024 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm_merged::AccumulatorStackHalfKaHmMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaHmMerged1024_8_32CReLU, HalfKaHmMerged1024_8_32Pairwise, HalfKaHmMerged1024_8_32SCReLU,
    HalfKaHmMerged1024_8_64CReLU, HalfKaHmMerged1024_8_64Pairwise, HalfKaHmMerged1024_8_64SCReLU,
    HalfKaHmMerged1024CReLU, HalfKaHmMerged1024Pairwise, HalfKaHmMerged1024SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaHmMergedL1024,
    feature_set HalfKaHmMerged,
    l1 1024,
    acc crate::nnue::network_halfka_hm_merged::AccumulatorHalfKaHmMerged<1024>,
    stack AccumulatorStackHalfKaHmMerged<1024>,

    variants {
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKaHmMerged1024_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKaHmMerged1024_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKaHmMerged1024_8_64Pairwise,
        // L2=8, L3=96 バリアント
        (8,  96, CReLU)         => CReLU8x96     : HalfKaHmMerged1024CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKaHmMerged1024SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKaHmMerged1024Pairwise,
        // L2=8, L3=32 バリアント
        (8,  32, CReLU)         => CReLU8x32     : HalfKaHmMerged1024_8_32CReLU,
        (8,  32, SCReLU)        => SCReLU8x32    : HalfKaHmMerged1024_8_32SCReLU,
        (8,  32, PairwiseCReLU) => Pairwise8x32  : HalfKaHmMerged1024_8_32Pairwise,
    }
);
