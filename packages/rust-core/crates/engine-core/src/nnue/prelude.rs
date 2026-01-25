//! NNUE 型の prelude（マクロから利用）
//!
//! `define_l1_variants!` マクロ内で使用する共通型を集約する。
//! 新しい型エイリアス追加時に l256.rs 等の import 更新が不要になる。

// 基本型
pub use crate::position::Position;
pub use crate::types::Value;

// NNUE 型
pub use crate::nnue::accumulator::DirtyPiece;
pub use crate::nnue::network_halfka_hm::{AccumulatorHalfKA_hm, AccumulatorStackHalfKA_hm};
pub use crate::nnue::network_halfkp::{AccumulatorHalfKP, AccumulatorStackHalfKP};
pub use crate::nnue::spec::{Activation, ArchitectureSpec, FeatureSet};

// 型エイリアスは aliases モジュールから re-export
pub use crate::nnue::aliases::*;
