//! HalfKaHmMerged L1=512 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_hm_merged::AccumulatorStackHalfKaHmMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKaHmMerged512_8_64CReLU, HalfKaHmMerged512_8_64Pairwise, HalfKaHmMerged512_8_64SCReLU,
    HalfKaHmMerged512_32_32CReLU, HalfKaHmMerged512_32_32Pairwise, HalfKaHmMerged512_32_32SCReLU,
    HalfKaHmMerged512CReLU, HalfKaHmMerged512Pairwise, HalfKaHmMerged512SCReLU,
};

crate::define_l1_variants!(
    enum HalfKaHmMergedL512,
    feature_set HalfKaHmMerged,
    l1 512,
    acc crate::nnue::network_halfka_hm_merged::AccumulatorHalfKaHmMerged<512>,
    stack AccumulatorStackHalfKaHmMerged<512>,

    variants {
        // L2=8, L3=64
        (8,  64, CReLU)         => CReLU8x64     : HalfKaHmMerged512_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKaHmMerged512_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKaHmMerged512_8_64Pairwise,
        // L2=8, L3=96
        (8,  96, CReLU)         => CReLU8x96     : HalfKaHmMerged512CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKaHmMerged512SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKaHmMerged512Pairwise,
        // L2=32, L3=32
        (32, 32, CReLU)         => CReLU32x32    : HalfKaHmMerged512_32_32CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKaHmMerged512_32_32SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKaHmMerged512_32_32Pairwise,
    }
);
