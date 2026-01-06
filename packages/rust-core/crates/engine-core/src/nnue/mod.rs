//! NNUE評価関数モジュール
//!
//! Efficiently Updatable Neural Network による局面評価。
//! YaneuraOu の HalfKP 256x2-32-32 アーキテクチャを Rust で実装する。
//!
//! サポートするアーキテクチャ:
//! - **HalfKP**: 従来のclassic NNUE（水匠/tanuki互換）
//! - **HalfKA_hm^**: nnue-pytorch互換（Half-Mirror + Factorization）
//!
//! - ネットワーク構造の読み込み（`Network::load` / `init_nnue`）
//! - 入力特徴量（HalfKP: 自玉×駒配置）の計算と変換（`BonaPiece` / `FeatureTransformer`）
//! - Accumulator による差分更新可能な中間表現の保持（`diff::get_changed_features` を用いた増分更新 + フォールバック全計算）
//! - AffineTransform + ClippedReLU による 512→32→32→1 の多層パーセプトロン
//! - NNUE 未初期化時のフォールバック駒得評価

mod accumulator;
mod bona_piece;
mod bona_piece_halfka;
mod constants;
mod diff;
mod feature_transformer;
mod feature_transformer_halfka;
pub mod features;
mod layers;
mod leb128;
mod network;

pub use accumulator::{
    Accumulator, AccumulatorStack, ChangedPiece, DirtyPiece, HandChange, StackEntry,
};
pub use bona_piece::{halfkp_index, BonaPiece, FE_END};
pub use bona_piece_halfka::{
    factorized_index, halfka_index, is_hm_mirror, king_bucket, pack_bonapiece, BonaPieceHalfKA,
    E_KING, FE_HAND_END, FE_OLD_END, F_KING, PIECE_INPUTS,
};
pub use constants::*;
pub use diff::get_changed_features;
pub use feature_transformer::FeatureTransformer;
pub use features::{
    Feature, FeatureSet, HalfKA_hm, HalfKA_hmFeatureSet, HalfKP, HalfKPFeatureSet, TriggerEvent,
};
pub use layers::{AffineTransform, ClippedReLU};
pub use network::{
    evaluate, init_nnue, init_nnue_from_bytes, is_nnue_initialized, NNUENetwork, Network,
    NetworkHalfKA,
};
