//! HalfKaSplit L1=256 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_split::AccumulatorStackHalfKaSplit;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKaSplit256CReLU, HalfKaSplit256Pairwise, HalfKaSplit256SCReLU};

crate::define_l1_variants!(
    enum HalfKaSplitL256,
    feature_set HalfKaSplit,
    l1 256,
    acc crate::nnue::network_halfka_split::AccumulatorHalfKaSplit<256>,
    stack AccumulatorStackHalfKaSplit<256>,

    variants {
        (32, 32, CReLU)         => CReLU32x32    : HalfKaSplit256CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKaSplit256SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKaSplit256Pairwise,
    }
);
