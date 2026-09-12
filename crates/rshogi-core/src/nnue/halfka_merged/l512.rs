//! HalfKaMerged L1=512 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_merged::AccumulatorStackHalfKaMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaMerged512_8_64CReLU, HalfKaMerged512_8_64Pairwise, HalfKaMerged512_8_64SCReLU,
    HalfKaMerged512_32_32CReLU, HalfKaMerged512_32_32Pairwise, HalfKaMerged512_32_32SCReLU,
    HalfKaMerged512CReLU, HalfKaMerged512Pairwise, HalfKaMerged512SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaMergedL512,
    feature_set HalfKaMerged,
    l1 512,
    acc crate::nnue::network_halfka_merged::AccumulatorHalfKaMerged<512>,
    stack AccumulatorStackHalfKaMerged<512>,

    variants {
        // L2=8, L3=64
        (8,  64, CReLU)         => CReLU8x64     : HalfKaMerged512_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKaMerged512_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKaMerged512_8_64Pairwise,
        // L2=8, L3=96
        (8,  96, CReLU)         => CReLU8x96     : HalfKaMerged512CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKaMerged512SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKaMerged512Pairwise,
        // L2=32, L3=32
        (32, 32, CReLU)         => CReLU32x32    : HalfKaMerged512_32_32CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKaMerged512_32_32SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKaMerged512_32_32Pairwise,
    }
);
