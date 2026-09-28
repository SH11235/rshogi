//! 探索開始時に重み・スタック・Finny cache を対応付ける評価器。
//!
//! 重みは Arc で所有し、探索中のグローバル network 参照を不要にする。
//! 探索の制御フローは評価器の種類に依存しない。

#[cfg(feature = "halfkx-arch")]
use super::AccumulatorCacheGeneric;
#[cfg(all(
    feature = "halfkx-arch",
    any(not(feature = "mode-specific"), feature = "ft-halfka_hm_merged")
))]
use super::halfka_hm_merged::{HalfKaHmMergedNetwork, HalfKaHmMergedStack};
#[cfg(all(
    feature = "halfkx-arch",
    any(not(feature = "mode-specific"), feature = "ft-halfka_hm_split")
))]
use super::halfka_hm_split::{HalfKaHmSplitNetwork, HalfKaHmSplitStack};
#[cfg(all(
    feature = "halfkx-arch",
    any(not(feature = "mode-specific"), feature = "ft-halfka_merged")
))]
use super::halfka_merged::{HalfKaMergedNetwork, HalfKaMergedStack};
#[cfg(all(
    feature = "halfkx-arch",
    any(not(feature = "mode-specific"), feature = "ft-halfka_split")
))]
use super::halfka_split::{HalfKaSplitNetwork, HalfKaSplitStack};
#[cfg(all(
    feature = "halfkx-arch",
    any(not(feature = "mode-specific"), feature = "ft-halfkp")
))]
use super::halfkp::{HalfKPNetwork, HalfKPStack};
use super::{DirtyPiece, NNUENetwork, get_network};
#[cfg(feature = "layerstack-arch")]
use super::{LayerStacksAccCache, LayerStacksAccStack, LayerStacksNetwork};
#[cfg(feature = "nnue-runtime-dimensions")]
use super::{
    dynamic_halfkx::{DynamicHalfKxNetwork, DynamicHalfKxStack},
    dynamic_layer_stacks::{DynamicLayerStacksNetwork, DynamicLayerStacksStack},
};
use crate::eval::material::{self, MaterialLevel};
use crate::position::Position;
use crate::types::Value;
use std::sync::Arc;

pub(crate) enum SearchEvaluator {
    Uninitialized,
    Material {
        level: MaterialLevel,
    },
    #[cfg(all(
        feature = "halfkx-arch",
        any(not(feature = "mode-specific"), feature = "ft-halfkp")
    ))]
    HalfKP {
        net: Arc<HalfKPNetwork>,
        stack: HalfKPStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(all(
        feature = "halfkx-arch",
        any(not(feature = "mode-specific"), feature = "ft-halfka_split")
    ))]
    HalfKaSplit {
        net: Arc<HalfKaSplitNetwork>,
        stack: HalfKaSplitStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(all(
        feature = "halfkx-arch",
        any(not(feature = "mode-specific"), feature = "ft-halfka_hm_merged")
    ))]
    HalfKaHmMerged {
        net: Arc<HalfKaHmMergedNetwork>,
        stack: HalfKaHmMergedStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(all(
        feature = "halfkx-arch",
        any(not(feature = "mode-specific"), feature = "ft-halfka_merged")
    ))]
    HalfKaMerged {
        net: Arc<HalfKaMergedNetwork>,
        stack: HalfKaMergedStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(all(
        feature = "halfkx-arch",
        any(not(feature = "mode-specific"), feature = "ft-halfka_hm_split")
    ))]
    HalfKaHmSplit {
        net: Arc<HalfKaHmSplitNetwork>,
        stack: HalfKaHmSplitStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(feature = "layerstack-arch")]
    LayerStacks {
        net: Arc<LayerStacksNetwork>,
        stack: LayerStacksAccStack,
        cache: Option<LayerStacksAccCache>,
    },
    #[cfg(feature = "nnue-runtime-dimensions")]
    DynamicHalfKx {
        net: Arc<DynamicHalfKxNetwork>,
        stack: Box<DynamicHalfKxStack>,
    },
    #[cfg(feature = "nnue-runtime-dimensions")]
    DynamicLayerStacks {
        net: Arc<DynamicLayerStacksNetwork>,
        stack: Box<DynamicLayerStacksStack>,
    },
}

impl SearchEvaluator {
    pub(crate) fn prepare() -> Self {
        let network = get_network();
        // 静的 LayerStacks の探索はロード済み net を MaterialLevel より優先する。
        #[cfg(feature = "layerstack-arch")]
        if let Some(net) = &network
            && matches!(&**net, NNUENetwork::LayerStacks(_))
        {
            return Self::from_network(net);
        }
        if material::is_material_enabled() {
            return Self::Material {
                level: material::get_material_level(),
            };
        }
        network.map_or(Self::Uninitialized, |net| Self::from_network(&net))
    }

    pub(crate) fn from_network(network: &NNUENetwork) -> Self {
        match network {
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfkp")
            ))]
            NNUENetwork::HalfKP(net) => Self::HalfKP {
                net: Arc::clone(net),
                stack: HalfKPStack::from_network(net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
            },
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_split")
            ))]
            NNUENetwork::HalfKaSplit(net) => Self::HalfKaSplit {
                net: Arc::clone(net),
                stack: HalfKaSplitStack::from_network(net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
            },
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_hm_merged")
            ))]
            NNUENetwork::HalfKaHmMerged(net) => Self::HalfKaHmMerged {
                net: Arc::clone(net),
                stack: HalfKaHmMergedStack::from_network(net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
            },
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_merged")
            ))]
            NNUENetwork::HalfKaMerged(net) => Self::HalfKaMerged {
                net: Arc::clone(net),
                stack: HalfKaMergedStack::from_network(net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
            },
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_hm_split")
            ))]
            NNUENetwork::HalfKaHmSplit(net) => Self::HalfKaHmSplit {
                net: Arc::clone(net),
                stack: HalfKaHmSplitStack::from_network(net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
            },
            #[cfg(feature = "layerstack-arch")]
            NNUENetwork::LayerStacks(net) => Self::LayerStacks {
                net: Arc::clone(net),
                stack: net.new_acc_stack(),
                cache: Some(net.new_acc_cache()),
            },
            #[cfg(feature = "nnue-runtime-dimensions")]
            NNUENetwork::DynamicHalfKx(net) => Self::DynamicHalfKx {
                net: Arc::clone(net),
                stack: Box::new(DynamicHalfKxStack::new(net)),
            },
            #[cfg(feature = "nnue-runtime-dimensions")]
            NNUENetwork::DynamicLayerStacks(net) => Self::DynamicLayerStacks {
                net: Arc::clone(net),
                stack: Box::new(net.new_stack()),
            },
            #[cfg(any(not(feature = "halfkx-arch"), feature = "mode-specific"))]
            _ => panic!("NNUE architecture is not enabled for search in this build"),
        }
    }

    #[inline]
    pub(crate) fn evaluate(&mut self, pos: &Position) -> Value {
        match self {
            Self::Uninitialized => panic!(
                "NNUE network not loaded and MaterialLevel not set. Use 'setoption name EvalFile' or 'setoption name MaterialLevel'."
            ),
            Self::Material { level } => material::evaluate_material_at_level(pos, *level),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfkp")
            ))]
            Self::HalfKP { net, stack, cache } => {
                super::network::update_and_evaluate_halfkp(net, pos, stack, cache)
            }
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_split")
            ))]
            Self::HalfKaSplit { net, stack, cache } => {
                super::network::update_and_evaluate_halfka(net, pos, stack, cache)
            }
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_hm_merged")
            ))]
            Self::HalfKaHmMerged { net, stack, cache } => {
                super::network::update_and_evaluate_halfka_hm(net, pos, stack, cache)
            }
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_merged")
            ))]
            Self::HalfKaMerged { net, stack, cache } => {
                super::network::update_and_evaluate_halfka_merged(net, pos, stack, cache)
            }
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_hm_split")
            ))]
            Self::HalfKaHmSplit { net, stack, cache } => {
                super::network::update_and_evaluate_halfka_hm_split(net, pos, stack, cache)
            }
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks { net, stack, cache } => {
                super::network::update_and_evaluate_layer_stacks_cached(net, pos, stack, cache)
            }
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicHalfKx { net, stack } => {
                net.ensure(pos, stack);
                net.evaluate(pos, stack)
            }
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicLayerStacks { net, stack } => {
                net.ensure(pos, stack);
                net.evaluate(pos, stack)
            }
        }
    }

    #[inline]
    pub(crate) fn push(&mut self, dirty: DirtyPiece) {
        match self {
            Self::Uninitialized | Self::Material { .. } => {}
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfkp")
            ))]
            Self::HalfKP { stack, .. } => stack.push(dirty),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_split")
            ))]
            Self::HalfKaSplit { stack, .. } => stack.push(dirty),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_hm_merged")
            ))]
            Self::HalfKaHmMerged { stack, .. } => stack.push(dirty),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_merged")
            ))]
            Self::HalfKaMerged { stack, .. } => stack.push(dirty),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_hm_split")
            ))]
            Self::HalfKaHmSplit { stack, .. } => stack.push(dirty),
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicHalfKx { stack, .. } => stack.push(dirty),
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicLayerStacks { stack, .. } => stack.push(dirty),
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks { stack, .. } => {
                stack.push();
                stack.set_current_dirty_piece(dirty);
            }
        }
    }

    #[inline]
    pub(crate) fn pop(&mut self) {
        match self {
            Self::Uninitialized | Self::Material { .. } => {}
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfkp")
            ))]
            Self::HalfKP { stack, .. } => stack.pop(),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_split")
            ))]
            Self::HalfKaSplit { stack, .. } => stack.pop(),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_hm_merged")
            ))]
            Self::HalfKaHmMerged { stack, .. } => stack.pop(),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_merged")
            ))]
            Self::HalfKaMerged { stack, .. } => stack.pop(),
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfka_hm_split")
            ))]
            Self::HalfKaHmSplit { stack, .. } => stack.pop(),
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicHalfKx { stack, .. } => stack.pop(),
            #[cfg(feature = "nnue-runtime-dimensions")]
            Self::DynamicLayerStacks { stack, .. } => stack.pop(),
            #[cfg(feature = "layerstack-arch")]
            Self::LayerStacks { stack, .. } => stack.pop(),
        }
    }
}

#[cfg(test)]
#[path = "search_evaluator_tests.rs"]
mod tests;
