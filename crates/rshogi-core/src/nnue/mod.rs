//! NNUE評価関数モジュール
//!
//! Efficiently Updatable Neural Network による局面評価。
//! YaneuraOu の HalfKP 256x2-32-32 アーキテクチャを Rust で実装する。
//!
//! サポートするアーキテクチャ:
//! - **HalfKP**: 従来のclassic NNUE（水匠/tanuki互換）
//! - **HalfKA**: nnue-pytorch互換（Non-mirror）
//! - **HalfKA_hm^**: nnue-pytorch互換（Half-Mirror + Factorization）
//!
//! # const generics 版統一実装
//!
//! `NetworkHalfKA_hm<L1, L2, L3, A>` で複数のアーキテクチャに対応:
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
pub mod aliases;
mod bona_piece;
mod bona_piece_halfka;
mod bona_piece_halfka_hm;
mod constants;
mod diff;
mod evaluator;
mod feature_transformer;
mod feature_transformer_layer_stacks;
pub mod features;
pub(crate) mod halfka;
pub(crate) mod halfka_hm;
pub(crate) mod halfkp;
mod layer_stacks;
mod layers;
mod leb128;
#[macro_use]
pub mod macros;
mod network;
pub(crate) mod network_halfka;
pub(crate) mod network_halfka_hm;
pub(crate) mod network_halfkp;
mod network_layer_stacks;
pub mod piece_list;
pub mod prelude;
pub mod spec;
pub mod stats;

pub use accumulator::{Accumulator, AccumulatorStack, ChangedBonaPiece, DirtyPiece, StackEntry};
pub use accumulator_layer_stacks::{
    AccumulatorCacheLayerStacks, AccumulatorLayerStacks, AccumulatorStackLayerStacks,
    StackEntryLayerStacks,
};
pub use accumulator_stack_variant::AccumulatorStackVariant;
pub use bona_piece::{BonaPiece, ExtBonaPiece, FE_END, halfkp_index};
pub use bona_piece_halfka_hm::{
    BonaPieceHalfKA_hm, E_KING, F_KING, FE_HAND_END, FE_OLD_END, PIECE_INPUTS, halfka_index,
    is_hm_mirror, king_bucket, pack_bonapiece,
};
pub use constants::*;
pub use diff::get_changed_features;
pub use feature_transformer::FeatureTransformer;
pub use feature_transformer_layer_stacks::FeatureTransformerLayerStacks;
pub use features::{
    Feature, FeatureSet, HalfKA, HalfKA_hm, HalfKA_hm_FeatureSet, HalfKAFeatureSet, HalfKP,
    HalfKPFeatureSet, TriggerEvent,
};
pub use layer_stacks::{
    LayerStackBucket, LayerStacks, compute_bucket_index, compute_king_ranks,
    sqr_clipped_relu_transform,
};
pub use layers::{AffineTransform, ClippedReLU};
pub use network::{
    LAYER_STACK_PLY9_DEFAULT_BOUNDS, LayerStackBucketMode, LayerStackProgressCoeff,
    LayerStackProgressCoeffGikouLite, NNUENetwork, NnueFormatInfo,
    SHOGI_PROGRESS_GIKOU_LITE_FEATURE_ORDER, SHOGI_PROGRESS_GIKOU_LITE_NUM_FEATURES,
    SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, SHOGI_PROGRESS8_FEATURE_ORDER, SHOGI_PROGRESS8_NUM_FEATURES,
    compute_layer_stack_ply9_bucket_index, compute_layer_stack_progress8_bucket_index,
    compute_layer_stack_progress8gikou_bucket_index,
    compute_layer_stack_progress8kpabs_bucket_index, compute_progress8kpabs_sum, detect_format,
    ensure_accumulator_computed, evaluate_dispatch, evaluate_layer_stacks,
    format_layer_stack_ply_bounds, get_fv_scale_override, get_layer_stack_bucket_mode,
    get_layer_stack_ply_bounds, get_layer_stack_progress_coeff,
    get_layer_stack_progress_coeff_gikou_lite, get_layer_stack_progress_kpabs_weights, get_network,
    init_nnue, init_nnue_from_bytes, is_halfka_256_loaded, is_halfka_512_loaded,
    is_halfka_1024_loaded, is_halfka_hm_256_loaded, is_halfka_hm_512_loaded,
    is_halfka_hm_1024_loaded, is_layer_stacks_loaded, is_nnue_initialized,
    parse_layer_stack_bucket_mode, parse_layer_stack_ply_bounds_csv, progress_sum_to_bucket,
    reset_layer_stack_progress_kpabs_weights, set_fv_scale_override, set_layer_stack_bucket_mode,
    set_layer_stack_ply_bounds, set_layer_stack_progress_coeff,
    set_layer_stack_progress_coeff_gikou_lite, set_layer_stack_progress_kpabs_weights,
};
pub use network_layer_stacks::NetworkLayerStacks;
pub use piece_list::{PieceList, PieceNumber};

// const generics 版統一実装（内部型は pub(crate) に隠蔽）
pub use activation::{CReLU, FtActivation, PairwiseCReLU, SCReLU, detect_activation_from_arch};
pub use spec::{Activation, ArchitectureSpec, FeatureSet as SpecFeatureSet};

// 型エイリアス（HalfKA*/HalfKP* の全バリアント）は pub(crate) に隠蔽
// 外部からは NNUEEvaluator を通じてのみ NNUE 評価を行う
// 内部モジュールは crate::nnue::aliases 経由で直接インポート

// Phase 2: 外部 API 統一
pub use evaluator::NNUEEvaluator;
pub use network::clear_nnue;

// 統計カウンタ（デバッグ・チューニング用）
pub use stats::{NnueStatsSnapshot, get_nnue_stats, print_nnue_stats, reset_nnue_stats};
