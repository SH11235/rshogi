//! NNUE評価関数モジュール
//!
//! Efficiently Updatable Neural Network による局面評価。
//! YaneuraOu の HalfKP 256x2-32-32 アーキテクチャを Rust で実装する。
//!
//! サポートするアーキテクチャ:
//! - **HalfKP**: 従来のclassic NNUE（水匠/tanuki互換）
//! - **HalfKA_hm^**: nnue-pytorch互換（Half-Mirror + Factorization）
//!
//! # const generics 版統一実装
//!
//! `NetworkHalfKA<L1, L2, L3, A>` で複数のアーキテクチャに対応:
//! - L1: FT出力次元（256, 512, 1024）
//! - L2: 隠れ層1出力次元（8, 32）
//! - L3: 隠れ層2出力次元（32, 96）
//! - A: 活性化関数（CReLU, SCReLU, PairwiseCReLU）
//!
//! - ネットワーク構造の読み込み（`Network::load` / `init_nnue`）
//! - 入力特徴量（HalfKP: 自玉×駒配置）の計算と変換（`BonaPiece` / `FeatureTransformer`）
//! - Accumulator による差分更新可能な中間表現の保持（`diff::get_changed_features` を用いた増分更新 + フォールバック全計算）
//! - AffineTransform + ClippedReLU による 512→32→32→1 の多層パーセプトロン
//! - NNUE 未初期化時のフォールバック駒得評価

mod accumulator;
mod accumulator_layer_stacks;
mod accumulator_stack_variant;
pub mod activation;
mod bona_piece;
mod bona_piece_halfka;
mod constants;
mod diff;
mod feature_transformer;
mod feature_transformer_layer_stacks;
pub mod features;
mod layer_stacks;
mod layers;
mod leb128;
mod network;
pub mod network_halfka;
pub mod network_halfkp;
mod network_layer_stacks;

pub use accumulator::{
    Accumulator, AccumulatorStack, ChangedPiece, DirtyPiece, HandChange, StackEntry,
};
pub use accumulator_layer_stacks::{
    AccumulatorLayerStacks, AccumulatorStackLayerStacks, StackEntryLayerStacks,
};
pub use accumulator_stack_variant::AccumulatorStackVariant;
pub use bona_piece::{halfkp_index, BonaPiece, FE_END};
pub use bona_piece_halfka::{
    halfka_index, is_hm_mirror, king_bucket, pack_bonapiece, BonaPieceHalfKA, E_KING, FE_HAND_END,
    FE_OLD_END, F_KING, PIECE_INPUTS,
};
pub use constants::*;
pub use diff::get_changed_features;
pub use feature_transformer::FeatureTransformer;
pub use feature_transformer_layer_stacks::FeatureTransformerLayerStacks;
pub use features::{
    Feature, FeatureSet, HalfKA_hm, HalfKA_hmFeatureSet, HalfKP, HalfKPFeatureSet, TriggerEvent,
};
pub use layer_stacks::{
    compute_bucket_index, sqr_clipped_relu_transform, LayerStackBucket, LayerStacks,
};
pub use layers::{AffineTransform, ClippedReLU};
pub use network::{
    evaluate, evaluate_dispatch, evaluate_layer_stacks, get_fv_scale_override, get_network,
    init_nnue, init_nnue_from_bytes, is_halfka_1024_loaded, is_halfka_512_loaded,
    is_layer_stacks_loaded, is_nnue_initialized, set_fv_scale_override, NNUENetwork, Network,
};
pub use network_layer_stacks::NetworkLayerStacks;

// const generics 版統一実装
pub use activation::{detect_activation_from_arch, CReLU, FtActivation, PairwiseCReLU, SCReLU};
pub use network_halfka::{
    AccumulatorEntryHalfKA, AccumulatorHalfKA, AccumulatorStackHalfKA, AffineTransformHalfKA,
    FeatureTransformerHalfKA, HalfKA1024CReLU, HalfKA1024Pairwise, HalfKA1024SCReLU,
    HalfKA256CReLU, HalfKA256Pairwise, HalfKA256SCReLU, HalfKA512CReLU, HalfKA512Pairwise,
    HalfKA512SCReLU, NetworkHalfKA,
};
pub use network_halfkp::{
    AccumulatorEntryHalfKP, AccumulatorHalfKP, AccumulatorStackHalfKP, AffineTransformHalfKP,
    FeatureTransformerHalfKP, HalfKP256CReLU, HalfKP256Pairwise, HalfKP256SCReLU, HalfKP512CReLU,
    HalfKP512Pairwise, HalfKP512SCReLU, HalfKP512_32_32CReLU, NetworkHalfKP,
};
