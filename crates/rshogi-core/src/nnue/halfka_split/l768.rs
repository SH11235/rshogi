//! HalfKaSplit L1=768 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_split::AccumulatorStackHalfKaSplit;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKaSplit768CReLU, HalfKaSplit768Pairwise, HalfKaSplit768SCReLU};

crate::define_l1_variants!(
    enum HalfKaSplitL768,
    feature_set HalfKaSplit,
    l1 768,
    acc crate::nnue::network_halfka_split::AccumulatorHalfKaSplit<768>,
    stack AccumulatorStackHalfKaSplit<768>,

    variants {
        // L2=16, L3=64 バリアント
        (16, 64, CReLU)         => CReLU16x64    : HalfKaSplit768CReLU,
        (16, 64, SCReLU)        => SCReLU16x64   : HalfKaSplit768SCReLU,
        (16, 64, PairwiseCReLU) => Pairwise16x64 : HalfKaSplit768Pairwise,
    }
);
