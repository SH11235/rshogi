//! HalfKaMerged L1=1024 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_merged::AccumulatorStackHalfKaMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaMerged1024_8_32CReLU, HalfKaMerged1024_8_32Pairwise, HalfKaMerged1024_8_32SCReLU,
    HalfKaMerged1024_8_64CReLU, HalfKaMerged1024_8_64Pairwise, HalfKaMerged1024_8_64SCReLU,
    HalfKaMerged1024CReLU, HalfKaMerged1024Pairwise, HalfKaMerged1024SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaMergedL1024,
    feature_set HalfKaMerged,
    l1 1024,
    acc crate::nnue::network_halfka_merged::AccumulatorHalfKaMerged<1024>,
    stack AccumulatorStackHalfKaMerged<1024>,

    variants {
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKaMerged1024_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKaMerged1024_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKaMerged1024_8_64Pairwise,
        // L2=8, L3=96 バリアント
        (8,  96, CReLU)         => CReLU8x96     : HalfKaMerged1024CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKaMerged1024SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKaMerged1024Pairwise,
        // L2=8, L3=32 バリアント
        (8,  32, CReLU)         => CReLU8x32     : HalfKaMerged1024_8_32CReLU,
        (8,  32, SCReLU)        => SCReLU8x32    : HalfKaMerged1024_8_32SCReLU,
        (8,  32, PairwiseCReLU) => Pairwise8x32  : HalfKaMerged1024_8_32Pairwise,
    }
);
