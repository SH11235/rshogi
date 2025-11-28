//! NNUE評価関数モジュール
//!
//! Efficiently Updatable Neural Network による局面評価。
//!
//! - ネットワーク構造の読み込み
//! - 特徴量計算（HalfKP）
//! - Accumulator（差分更新）
//! - SIMD最適化

mod accumulator;
mod bona_piece;
mod constants;
mod feature_transformer;
mod layers;
mod network;

pub use accumulator::Accumulator;
pub use bona_piece::{BonaPiece, FE_END};
pub use constants::*;
pub use feature_transformer::FeatureTransformer;
pub use layers::{AffineTransform, ClippedReLU};
pub use network::{evaluate, init_nnue, Network};
