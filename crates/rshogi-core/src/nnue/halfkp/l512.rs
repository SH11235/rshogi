//! HalfKP L1=512 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{
    HalfKP512_8_64CReLU, HalfKP512_8_64Pairwise, HalfKP512_8_64SCReLU, HalfKP512_32_32CReLU,
    HalfKP512_32_32Pairwise, HalfKP512_32_32SCReLU, HalfKP512CReLU, HalfKP512Pairwise,
    HalfKP512SCReLU,
};

crate::define_l1_variants!(
    enum HalfKPL512,
    feature_set HalfKP,
    l1 512,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<512>,
    stack AccumulatorStackHalfKP<512>,

    variants {
        // L2=8, L3=64 バリアント
        (8,  64, CReLU)         => CReLU8x64     : HalfKP512_8_64CReLU,
        (8,  64, SCReLU)        => SCReLU8x64    : HalfKP512_8_64SCReLU,
        (8,  64, PairwiseCReLU) => Pairwise8x64  : HalfKP512_8_64Pairwise,
        // L2=8, L3=96 バリアント
        (8,  96, CReLU)         => CReLU8x96     : HalfKP512CReLU,
        (8,  96, SCReLU)        => SCReLU8x96    : HalfKP512SCReLU,
        (8,  96, PairwiseCReLU) => Pairwise8x96  : HalfKP512Pairwise,
        // L2=32, L3=32 バリアント
        (32, 32, CReLU)         => CReLU32x32    : HalfKP512_32_32CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKP512_32_32SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKP512_32_32Pairwise,
    }
);
