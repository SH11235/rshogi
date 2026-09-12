//! HalfKaMerged L1=768 のアーキテクチャバリアント

use crate::nnue::accumulator::DirtyPiece;
use crate::nnue::network_halfka_merged::AccumulatorStackHalfKaMerged;
use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};
use crate::position::Position;
use crate::types::Value;

// 型エイリアスを aliases 経由でインポート
use crate::nnue::aliases::{HalfKaMerged768CReLU, HalfKaMerged768Pairwise, HalfKaMerged768SCReLU};

crate::define_l1_variants!(
    enum HalfKaMergedL768,
    feature_set HalfKaMerged,
    l1 768,
    acc crate::nnue::network_halfka_merged::AccumulatorHalfKaMerged<768>,
    stack AccumulatorStackHalfKaMerged<768>,

    variants {
        // L2=16, L3=64 バリアント
        (16, 64, CReLU)         => CReLU16x64    : HalfKaMerged768CReLU,
        (16, 64, SCReLU)        => SCReLU16x64   : HalfKaMerged768SCReLU,
        (16, 64, PairwiseCReLU) => Pairwise16x64 : HalfKaMerged768Pairwise,
    }
);
