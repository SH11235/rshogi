//! NNUE評価関数モジュール
//!
//! Efficiently Updatable Neural Network による局面評価。
//! YaneuraOu の HalfKP 256x2-32-32 アーキテクチャを Rust で実装する。
//!
//! - ネットワーク構造の読み込み（`Network::load` / `init_nnue`）
//! - 入力特徴量（HalfKP: 自玉×駒配置）の計算と変換（`BonaPiece` / `FeatureTransformer`）
//! - Accumulator による差分更新可能な中間表現の保持（`diff::get_changed_features` を用いた増分更新 + フォールバック全計算）
//! - AffineTransform + ClippedReLU による 512→32→32→1 の多層パーセプトロン
//! - NNUE 未初期化時のフォールバック駒得評価

mod accumulator;
mod bona_piece;
mod constants;
mod diff;
mod feature_transformer;
pub mod features;
mod layers;
mod network;

pub use accumulator::{
    Accumulator, AccumulatorStack, ChangedPiece, DirtyPiece, HandChange, StackEntry,
};
pub use bona_piece::{BonaPiece, FE_END};
pub use constants::*;
pub use diff::get_changed_features;
pub use feature_transformer::FeatureTransformer;
pub use features::{Feature, FeatureSet, HalfKP, HalfKPFeatureSet, TriggerEvent};
pub use layers::{AffineTransform, ClippedReLU};
pub use network::{evaluate, init_nnue, init_nnue_from_bytes, is_nnue_initialized, Network};
