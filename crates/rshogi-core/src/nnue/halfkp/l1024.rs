//! HalfKP L1=1024 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKP1024_8_32CReLU, HalfKP1024_8_32Pairwise, HalfKP1024_8_32SCReLU, HalfKP1024_8_64CReLU,
    HalfKP1024_8_64Pairwise, HalfKP1024_8_64SCReLU,
};

crate::define_l1_variants!(
    enum HalfKPL1024,
    feature_set HalfKP,
    l1 1024,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<1024>,
    stack AccumulatorStackHalfKP<1024>,

    variants {
        // L2=8, L3=32 バリアント
        (8,  32, CReLU)         => CReLU8x32     : HalfKP1024_8_32CReLU,
        (8,  32, SCReLU)        => SCReLU8x32    : HalfKP1024_8_32SCReLU,
        (8,  32, PairwiseCReLU) => Pairwise8x32  : HalfKP1024_8_32Pairwise,
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKP1024_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKP1024_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKP1024_8_64Pairwise,
    }
);
