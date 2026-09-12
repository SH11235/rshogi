//! HalfKP L1=768 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfkp::AccumulatorStackHalfKP;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKP768CReLU, HalfKP768Pairwise, HalfKP768SCReLU};

crate::define_l1_variants!(
    enum HalfKPL768,
    feature_set HalfKP,
    l1 768,
    acc crate::nnue::network_halfkp::AccumulatorHalfKP<768>,
    stack AccumulatorStackHalfKP<768>,

    variants {
        // L2=16, L3=64 バリアント
        (16, 64, CReLU)         => CReLU16x64    : HalfKP768CReLU,
        (16, 64, SCReLU)        => SCReLU16x64   : HalfKP768SCReLU,
        (16, 64, PairwiseCReLU) => Pairwise16x64 : HalfKP768Pairwise,
    }
);
