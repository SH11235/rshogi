//! HalfKaMerged L1=256 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_merged::AccumulatorStackHalfKaMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKaMerged256CReLU, HalfKaMerged256Pairwise, HalfKaMerged256SCReLU};

crate::define_l1_variants!(
    enum HalfKaMergedL256,
    feature_set HalfKaMerged,
    l1 256,
    acc crate::nnue::network_halfka_merged::AccumulatorHalfKaMerged<256>,
    stack AccumulatorStackHalfKaMerged<256>,

    variants {
        (32, 32, CReLU)         => CReLU32x32    : HalfKaMerged256CReLU,
        (32, 32, SCReLU)        => SCReLU32x32   : HalfKaMerged256SCReLU,
        (32, 32, PairwiseCReLU) => Pairwise32x32 : HalfKaMerged256Pairwise,
    }
);
