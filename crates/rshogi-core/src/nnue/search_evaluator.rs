//! 探索開始時に重み・スタック・Finny cache を対応付ける評価器。
//!
//! 重みは Arc で所有し、探索中のグローバル network 参照を不要にする。
//! 探索の制御フローは評価器の種類に依存しない。

#[cfg(feature = "halfkx-arch")]
use super::AccumulatorCacheGeneric;
#[cfg(feature = "halfkx-arch")]
use super::halfka_hm_merged::{HalfKaHmMergedNetwork, HalfKaHmMergedStack};
#[cfg(feature = "halfkx-arch")]
use super::halfka_hm_split::{HalfKaHmSplitNetwork, HalfKaHmSplitStack};
#[cfg(feature = "halfkx-arch")]
use super::halfka_merged::{HalfKaMergedNetwork, HalfKaMergedStack};
#[cfg(feature = "halfkx-arch")]
use super::halfka_split::{HalfKaSplitNetwork, HalfKaSplitStack};
#[cfg(feature = "halfkx-arch")]
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
#[cfg(any(
    feature = "halfkx-arch",
    feature = "layerstack-arch",
    feature = "nnue-runtime-dimensions"
))]
use std::sync::Arc;

/// ロードと探索開始で共有する、探索可能な重みへの型付き参照。
pub(crate) enum SearchNetwork {
    #[cfg(feature = "halfkx-arch")]
    HalfKP(Arc<HalfKPNetwork>),
    #[cfg(feature = "halfkx-arch")]
    HalfKaSplit(Arc<HalfKaSplitNetwork>),
    #[cfg(feature = "halfkx-arch")]
    HalfKaHmMerged(Arc<HalfKaHmMergedNetwork>),
    #[cfg(feature = "halfkx-arch")]
    HalfKaMerged(Arc<HalfKaMergedNetwork>),
    #[cfg(feature = "halfkx-arch")]
    HalfKaHmSplit(Arc<HalfKaHmSplitNetwork>),
    #[cfg(feature = "layerstack-arch")]
    LayerStacks(Arc<LayerStacksNetwork>),
    #[cfg(feature = "nnue-runtime-dimensions")]
    DynamicHalfKx(Arc<DynamicHalfKxNetwork>),
    #[cfg(feature = "nnue-runtime-dimensions")]
    DynamicLayerStacks(Arc<DynamicLayerStacksNetwork>),
}

impl TryFrom<&NNUENetwork> for SearchNetwork {
    type Error = std::io::Error;

    fn try_from(network: &NNUENetwork) -> Result<Self, Self::Error> {
        match network {
            #[cfg(feature = "halfkx-arch")]
            NNUENetwork::HalfKP(net) => Ok(Self::HalfKP(Arc::clone(net))),
            #[cfg(feature = "halfkx-arch")]
            NNUENetwork::HalfKaSplit(net) => Ok(Self::HalfKaSplit(Arc::clone(net))),
            #[cfg(feature = "halfkx-arch")]
            NNUENetwork::HalfKaHmMerged(net) => Ok(Self::HalfKaHmMerged(Arc::clone(net))),
            #[cfg(feature = "halfkx-arch")]
            NNUENetwork::HalfKaMerged(net) => Ok(Self::HalfKaMerged(Arc::clone(net))),
            #[cfg(feature = "halfkx-arch")]
            NNUENetwork::HalfKaHmSplit(net) => Ok(Self::HalfKaHmSplit(Arc::clone(net))),
            #[cfg(feature = "layerstack-arch")]
            NNUENetwork::LayerStacks(net) => Ok(Self::LayerStacks(Arc::clone(net))),
            #[cfg(feature = "nnue-runtime-dimensions")]
            NNUENetwork::DynamicHalfKx(net) => Ok(Self::DynamicHalfKx(Arc::clone(net))),
            #[cfg(feature = "nnue-runtime-dimensions")]
            NNUENetwork::DynamicLayerStacks(net) => Ok(Self::DynamicLayerStacks(Arc::clone(net))),
            #[cfg(not(feature = "halfkx-arch"))]
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                format!(
                    "NNUE model {} requires the `halfkx-arch` feature for search; use edition-halfkx or edition-universal",
                    network.architecture_name()
                ),
            )),
        }
    }
}

pub(crate) enum SearchEvaluator {
    Uninitialized,
    Material {
        level: MaterialLevel,
    },
    #[cfg(feature = "halfkx-arch")]
    HalfKP {
        net: Arc<HalfKPNetwork>,
        stack: HalfKPStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(feature = "halfkx-arch")]
    HalfKaSplit {
        net: Arc<HalfKaSplitNetwork>,
        stack: HalfKaSplitStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(feature = "halfkx-arch")]
    HalfKaHmMerged {
        net: Arc<HalfKaHmMergedNetwork>,
        stack: HalfKaHmMergedStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(feature = "halfkx-arch")]
    HalfKaMerged {
        net: Arc<HalfKaMergedNetwork>,
        stack: HalfKaMergedStack,
        cache: Option<AccumulatorCacheGeneric>,
    },
    #[cfg(feature = "halfkx-arch")]
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
    pub(crate) fn prepare(&mut self) {
        let network = get_network();
        // 静的 LayerStacks の探索はロード済み net を MaterialLevel より優先する。
        #[cfg(feature = "layerstack-arch")]
        if let Some(net) = &network
            && matches!(&**net, NNUENetwork::LayerStacks(_))
        {
            self.prepare_network(net);
            return;
        }
        if material::is_material_enabled() {
            *self = Self::Material {
                level: material::get_material_level(),
            };
        } else if let Some(net) = network {
            self.prepare_network(&net);
        } else {
            *self = Self::Uninitialized;
        }
    }

    fn prepare_network(&mut self, network: &NNUENetwork) {
        // 同じ重みの探索では領域を再利用し、計算済みフラグだけを無効化する。
        // Arc が変わった場合は形状が同じでも cache ごと作り直す。
        match (&mut *self, network) {
            #[cfg(feature = "halfkx-arch")]
            (Self::HalfKP { net, stack, cache }, NNUENetwork::HalfKP(next))
                if Arc::ptr_eq(net, next) =>
            {
                stack.reset();
                if let Some(cache) = cache {
                    cache.invalidate();
                }
            }
            #[cfg(feature = "halfkx-arch")]
            (Self::HalfKaSplit { net, stack, cache }, NNUENetwork::HalfKaSplit(next))
                if Arc::ptr_eq(net, next) =>
            {
                stack.reset();
                if let Some(cache) = cache {
                    cache.invalidate();
                }
            }
            #[cfg(feature = "halfkx-arch")]
            (Self::HalfKaHmMerged { net, stack, cache }, NNUENetwork::HalfKaHmMerged(next))
                if Arc::ptr_eq(net, next) =>
            {
                stack.reset();
                if let Some(cache) = cache {
                    cache.invalidate();
                }
            }
            #[cfg(feature = "halfkx-arch")]
            (Self::HalfKaMerged { net, stack, cache }, NNUENetwork::HalfKaMerged(next))
                if Arc::ptr_eq(net, next) =>
            {
                stack.reset();
                if let Some(cache) = cache {
                    cache.invalidate();
                }
            }
            #[cfg(feature = "halfkx-arch")]
            (Self::HalfKaHmSplit { net, stack, cache }, NNUENetwork::HalfKaHmSplit(next))
                if Arc::ptr_eq(net, next) =>
            {
                stack.reset();
                if let Some(cache) = cache {
                    cache.invalidate();
                }
            }
            #[cfg(feature = "layerstack-arch")]
            (Self::LayerStacks { net, stack, cache }, NNUENetwork::LayerStacks(next))
                if Arc::ptr_eq(net, next) =>
            {
                stack.reset();
                if let Some(cache) = cache {
                    cache.invalidate();
                }
            }
            #[cfg(feature = "nnue-runtime-dimensions")]
            (Self::DynamicHalfKx { net, stack }, NNUENetwork::DynamicHalfKx(next))
                if Arc::ptr_eq(net, next) =>
            {
                stack.reset();
            }
            #[cfg(feature = "nnue-runtime-dimensions")]
            (Self::DynamicLayerStacks { net, stack }, NNUENetwork::DynamicLayerStacks(next))
                if Arc::ptr_eq(net, next) =>
            {
                stack.reset();
            }
            _ => *self = Self::from_network(network),
        }
    }

    pub(crate) fn from_network(network: &NNUENetwork) -> Self {
        match SearchNetwork::try_from(network) {
            #[cfg(feature = "halfkx-arch")]
            Ok(SearchNetwork::HalfKP(net)) => Self::HalfKP {
                stack: HalfKPStack::from_network(&net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
                net,
            },
            #[cfg(feature = "halfkx-arch")]
            Ok(SearchNetwork::HalfKaSplit(net)) => Self::HalfKaSplit {
                stack: HalfKaSplitStack::from_network(&net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
                net,
            },
            #[cfg(feature = "halfkx-arch")]
            Ok(SearchNetwork::HalfKaHmMerged(net)) => Self::HalfKaHmMerged {
                stack: HalfKaHmMergedStack::from_network(&net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
                net,
            },
            #[cfg(feature = "halfkx-arch")]
            Ok(SearchNetwork::HalfKaMerged(net)) => Self::HalfKaMerged {
                stack: HalfKaMergedStack::from_network(&net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
                net,
            },
            #[cfg(feature = "halfkx-arch")]
            Ok(SearchNetwork::HalfKaHmSplit(net)) => Self::HalfKaHmSplit {
                stack: HalfKaHmSplitStack::from_network(&net),
                cache: super::halfkx_finny_enabled(net.l1_size())
                    .then(|| AccumulatorCacheGeneric::new(net.l1_size())),
                net,
            },
            #[cfg(feature = "layerstack-arch")]
            Ok(SearchNetwork::LayerStacks(net)) => Self::LayerStacks {
                stack: net.new_acc_stack(),
                cache: Some(net.new_acc_cache()),
                net,
            },
            #[cfg(feature = "nnue-runtime-dimensions")]
            Ok(SearchNetwork::DynamicHalfKx(net)) => Self::DynamicHalfKx {
                stack: Box::new(DynamicHalfKxStack::new(&net)),
                net,
            },
            #[cfg(feature = "nnue-runtime-dimensions")]
            Ok(SearchNetwork::DynamicLayerStacks(net)) => Self::DynamicLayerStacks {
                stack: Box::new(net.new_stack()),
                net,
            },
            Err(error) => panic!("loader must validate search support: {error}"),
        }
    }

    #[inline]
    pub(crate) fn evaluate(&mut self, pos: &Position) -> Value {
        match self {
            Self::Uninitialized => panic!(
                "NNUE network not loaded and MaterialLevel not set. Use 'setoption name EvalFile' or 'setoption name MaterialLevel'."
            ),
            Self::Material { level } => material::evaluate_material_at_level(pos, *level),
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKP { net, stack, cache } => {
                super::network::update_and_evaluate_halfkp(net, pos, stack, cache)
            }
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaSplit { net, stack, cache } => {
                super::network::update_and_evaluate_halfka(net, pos, stack, cache)
            }
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaHmMerged { net, stack, cache } => {
                super::network::update_and_evaluate_halfka_hm(net, pos, stack, cache)
            }
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaMerged { net, stack, cache } => {
                super::network::update_and_evaluate_halfka_merged(net, pos, stack, cache)
            }
            #[cfg(feature = "halfkx-arch")]
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
    #[cfg_attr(
        not(any(
            feature = "halfkx-arch",
            feature = "layerstack-arch",
            feature = "nnue-runtime-dimensions"
        )),
        allow(unused_variables)
    )]
    pub(crate) fn push(&mut self, dirty: DirtyPiece) {
        match self {
            Self::Uninitialized | Self::Material { .. } => {}
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKP { stack, .. } => stack.push(dirty),
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaSplit { stack, .. } => stack.push(dirty),
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaHmMerged { stack, .. } => stack.push(dirty),
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaMerged { stack, .. } => stack.push(dirty),
            #[cfg(feature = "halfkx-arch")]
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
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKP { stack, .. } => stack.pop(),
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaSplit { stack, .. } => stack.pop(),
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaHmMerged { stack, .. } => stack.pop(),
            #[cfg(feature = "halfkx-arch")]
            Self::HalfKaMerged { stack, .. } => stack.pop(),
            #[cfg(feature = "halfkx-arch")]
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
