//! NNUE評価関数モジュール
//!
//! Efficiently Updatable Neural Network による局面評価。
//! YaneuraOu の HalfKP 256x2-32-32 アーキテクチャを Rust で実装する。
//!
//! サポートするアーキテクチャ:
//! - **HalfKP**: 従来のclassic NNUE（水匠/tanuki互換）
//! - **HalfKaSplit**: nnue-pytorch互換（Non-mirror）
//! - **HalfKaHmMerged^**: nnue-pytorch互換（Half-Mirror + Factorization）
//!
//! # const generics 版統一実装
//!
//! `NetworkHalfKaHmMerged<L1, L2, L3, A>` で複数のアーキテクチャに対応:
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
mod bona_piece_effect_bucket;
mod bona_piece_halfka_hm_merged;
mod bona_piece_halfka_hm_split;
mod bona_piece_halfka_merged;
mod bona_piece_halfka_split;
mod constants;
mod diff;
#[cfg(feature = "nnue-runtime-dimensions")]
mod dynamic_halfkx;
#[cfg(feature = "nnue-runtime-dimensions")]
mod dynamic_layer_stacks;
mod effect_bucket_features;
mod evaluator;
mod feature_transformer;
mod feature_transformer_layer_stacks;
pub mod features;
pub(crate) mod halfka_hm_merged;
pub(crate) mod halfka_hm_split;
pub(crate) mod halfka_merged;
pub(crate) mod halfka_split;
pub(crate) mod halfkp;
mod layer_stacks;
mod layers;
mod leb128;
mod ls_feature_spec;
pub mod net_bin_layout;
#[macro_use]
pub mod macros;
pub mod net_delta;
mod network;
pub(crate) mod network_halfka_hm_merged;
pub(crate) mod network_halfka_hm_split;
pub(crate) mod network_halfka_merged;
pub(crate) mod network_halfka_split;
pub(crate) mod network_halfkp;
mod network_layer_stacks;
pub mod piece_list;
pub mod prelude;
mod shared_weights;
pub mod spec;
pub mod stats;
#[cfg(feature = "nnue-threat")]
pub(crate) mod threat_exclusion;
#[cfg(feature = "nnue-threat")]
pub(crate) mod threat_features;

pub use accumulator::{Accumulator, AccumulatorStack, ChangedBonaPiece, DirtyPiece, StackEntry};
pub use accumulator_layer_stacks::{
    AccumulatorCacheLayerStacks, AccumulatorLayerStacks, AccumulatorStackLayerStacks,
    LayerStacksAccCache, LayerStacksAccStack, StackEntryLayerStacks,
};
pub use accumulator_stack_variant::AccumulatorStackVariant;
pub use bona_piece::{BonaPiece, ExtBonaPiece, FE_END, halfkp_index};
#[cfg(feature = "nnue-effect-bucket")]
pub use bona_piece_effect_bucket::{
    EFFECT_BUCKET_CONFIG, EFFECT_BUCKET_KING_BUCKETED, EFFECT_BUCKET_NB,
};
pub use bona_piece_effect_bucket::{
    EffectBucketConfig, effect_bucket, effect_bucket_index, packed_is_bucketed,
};
pub use bona_piece_halfka_hm_merged::{
    BonaPieceHalfKaHmMerged, E_KING, F_KING, FE_HAND_END, FE_OLD_END, PIECE_INPUTS, halfka_index,
    is_hm_mirror, king_bucket, pack_bonapiece,
};
pub use constants::*;
pub use diff::get_changed_features;
pub use effect_bucket_features::{
    append_active_effect_bucket, append_changed_effect_bucket_indices,
    effect_bucket_active_indices_for_sfen,
};
pub use feature_transformer::FeatureTransformer;
pub use feature_transformer_layer_stacks::FeatureTransformerLayerStacks;
pub use features::{
    Feature, FeatureSet, HalfKP, HalfKPFeatureSet, HalfKaHmMerged, HalfKaHmMergedFeatureSet,
    HalfKaSplit, HalfKaSplitFeatureSet, TriggerEvent,
};
pub use layer_stacks::{
    LayerStackBucket, LayerStacks, LsSaturationCounts, compute_bucket_index, compute_king_ranks,
    sqr_clipped_relu_transform,
};
pub use layers::{AffineTransform, ClippedReLU};
pub use ls_feature_spec::{
    HalfKaHmMergedSpec, HalfKaHmSplitSpec, HalfKaMergedSpec, HalfKaSplitSpec, HalfKpSpec,
    LsFeatureSpec,
};
pub use net_delta::{
    NET_DELTA_OPTION_PREFIX, NetCoefficientId, NetDelta, NetDeltaError, NetDeltaReport,
    NetTensorKind, NetTensorShape,
};
#[cfg(feature = "layerstack-arch")]
pub use network::evaluate_layer_stacks;
pub(crate) use network::nnue_requires_board_effects;
#[cfg(feature = "layerstack-arch")]
pub(crate) use network::update_and_evaluate_layer_stacks_cached;
pub use network::{
    LayerStackBucketMode, NNUENetwork, NnueFormatInfo, SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS,
    compute_layer_stack_progresskpabs_bucket_index, compute_progresskpabs_sum,
    configure_layer_stack_routing, detect_format, ensure_accumulator_computed, evaluate_dispatch,
    get_fv_scale_override, get_layer_stack_bucket_mode, get_layer_stack_progress_buckets,
    get_layer_stack_progress_kpabs_weights, get_network, init_nnue, init_nnue_from_bytes,
    init_nnue_from_bytes_with_deltas, init_nnue_with_deltas, is_halfka_256_loaded,
    is_halfka_512_loaded, is_halfka_1024_loaded, is_halfka_hm_256_loaded, is_halfka_hm_512_loaded,
    is_halfka_hm_1024_loaded, is_layer_stacks_loaded, is_nnue_initialized,
    layer_stack_progress_coeff_required, load_progress_coeff_kpabs,
    load_progress_coeff_kpabs_from_bytes, parse_layer_stack_bucket_mode, parse_nnue_architecture,
    progress_sum_to_bucket, reset_layer_stack_progress_buckets,
    reset_layer_stack_progress_kpabs_weights, set_fv_scale_override,
    set_layer_stack_progress_kpabs_weights, set_nnue_architecture_override,
    validate_layer_stack_routing_configuration,
};
#[cfg(all(feature = "layerstacks-512x16x32", feature = "ft-halfka_hm_merged"))]
pub use network_layer_stacks::NetworkLayerStacks512x16x32;
#[cfg(all(feature = "layerstacks-768x8x32", feature = "ft-halfka_hm_merged"))]
pub use network_layer_stacks::NetworkLayerStacks768x8x32;
#[cfg(all(feature = "layerstacks-768x16x32", feature = "ft-halfka_hm_merged"))]
pub use network_layer_stacks::NetworkLayerStacks768x16x32;
#[cfg(all(feature = "layerstacks-1024x16x32", feature = "ft-halfka_hm_merged"))]
pub use network_layer_stacks::NetworkLayerStacks1024x16x32;
#[cfg(all(feature = "layerstacks-1536x16x32", feature = "ft-halfka_hm_merged"))]
pub use network_layer_stacks::NetworkLayerStacks1536x16x32;
#[cfg(all(feature = "layerstacks-1536x32x32", feature = "ft-halfka_hm_merged"))]
pub use network_layer_stacks::NetworkLayerStacks1536x32x32;
#[cfg(all(feature = "layerstacks-3072x16x32", feature = "ft-halfka_hm_merged"))]
pub use network_layer_stacks::NetworkLayerStacks3072x16x32;
pub use network_layer_stacks::{
    LayerStacksNetwork, LsNetByFt, NetworkLayerStacks, compute_layer_stack_kingrank9_bucket_index,
};
// `#[macro_export]` で crate root に出る ls_dispatch_ft_size! を nnue:: パスからも参照可能にする。
pub use crate::ls_dispatch_ft_size;
pub use piece_list::{PieceList, PieceNumber};

// const generics 版統一実装（内部型は pub(crate) に隠蔽）
pub use activation::{
    CReLU, FtActivation, PairwiseCReLU, SCReLU, default_qa_for_arch, detect_activation_from_arch,
};
pub use spec::{Activation, ArchitectureSpec, FeatureSet as SpecFeatureSet};

// 型エイリアス（HalfKaSplit*/HalfKP* の全バリアント）は pub(crate) に隠蔽
// 外部からは NNUEEvaluator を通じてのみ NNUE 評価を行う
// 内部モジュールは crate::nnue::aliases 経由で直接インポート

// Phase 2: 外部 API 統一
pub use evaluator::NNUEEvaluator;
pub use network::clear_nnue;

// 統計カウンタ（デバッグ・チューニング用）
pub use stats::{NnueStatsSnapshot, get_nnue_stats, print_nnue_stats, reset_nnue_stats};

#[cfg(test)]
mod registry_tests {
    use super::*;
    use spec::{Activation, ArchitectureSpec, FeatureSet};
    use std::collections::HashSet;

    #[test]
    fn supported_architectures_match_registry() {
        let registries = [
            (FeatureSet::HalfKP, halfkp::HalfKPNetwork::supported_specs()),
            (FeatureSet::HalfKaSplit, halfka_split::HalfKaSplitNetwork::supported_specs()),
            (FeatureSet::HalfKaMerged, halfka_merged::HalfKaMergedNetwork::supported_specs()),
            (
                FeatureSet::HalfKaHmSplit,
                halfka_hm_split::HalfKaHmSplitNetwork::supported_specs(),
            ),
            (
                FeatureSet::HalfKaHmMerged,
                halfka_hm_merged::HalfKaHmMergedNetwork::supported_specs(),
            ),
        ];
        for (feature_set, actual) in registries {
            let dimensions = [
                (256, 32, 32),
                (512, 8, 64),
                (512, 8, 96),
                (512, 32, 32),
                (768, 16, 64),
                (1024, 8, 32),
                (1024, 8, 64),
                (1024, 8, 96),
            ];
            let expected: HashSet<_> = dimensions
                .into_iter()
                .filter(|&(l1, _, l3)| {
                    !(feature_set == FeatureSet::HalfKP && l1 == 1024 && l3 == 96)
                })
                .flat_map(|(l1, l2, l3)| {
                    [
                        Activation::CReLU,
                        Activation::SCReLU,
                        Activation::PairwiseCReLU,
                    ]
                    .map(move |activation| {
                        ArchitectureSpec::new(feature_set, l1, l2, l3, activation)
                    })
                })
                .collect();
            let unique: HashSet<_> = actual.iter().copied().collect();
            assert_eq!(unique.len(), actual.len(), "duplicate: {feature_set:?}");
            assert_eq!(unique, expected, "{feature_set:?}");
        }
    }
}
