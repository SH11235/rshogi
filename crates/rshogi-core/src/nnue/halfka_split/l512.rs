//! HalfKaSplit L1=512 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_split::AccumulatorStackHalfKaSplit;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaSplit512_8_64CReLU, HalfKaSplit512_8_64Pairwise, HalfKaSplit512_8_64SCReLU,
    HalfKaSplit512_32_32CReLU, HalfKaSplit512_32_32Pairwise, HalfKaSplit512_32_32SCReLU,
    HalfKaSplit512CReLU, HalfKaSplit512Pairwise, HalfKaSplit512SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaSplitL512,
    feature_set HalfKaSplit,
    l1 512,
    acc crate::nnue::network_halfka_split::AccumulatorHalfKaSplit<512>,
    stack AccumulatorStackHalfKaSplit<512>,

    variants {
        // L2=8, L3=64
        (8,  64, CReLU)         => CReLU8x64     : HalfKaSplit512_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKaSplit512_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKaSplit512_8_64Pairwise,
        // L2=8, L3=96
        (8,  96, CReLU)         => CReLU8x96     : HalfKaSplit512CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKaSplit512SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKaSplit512Pairwise,
        // L2=32, L3=32
        (32, 32, CReLU)         => CReLU32x32    : HalfKaSplit512_32_32CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKaSplit512_32_32SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKaSplit512_32_32Pairwise,
    }
);
