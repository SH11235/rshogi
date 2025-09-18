use crate::classic::ClassicIntNetworkBundle;
use crate::dataset::open_cache_payload_reader;
use crate::logging::StructuredLogger;
use crate::model::{Network, SingleAdamState, SingleForwardScratch, SingleNetwork};
use crate::params::{
    BASELINE_MIN_EPS, GAP_WEIGHT_DIVISOR, MIN_ELAPSED_TIME, NANOSECONDS_PER_SECOND,
    NON_EXACT_BOUND_WEIGHT, SELECTIVE_DEPTH_MARGIN, SELECTIVE_DEPTH_WEIGHT,
};
use crate::types::{Config, DistillOptions, ExportOptions, Sample};
use engine_core::evaluation::nnue::features::FE_END;
use engine_core::game_phase::GamePhase;
use engine_core::shogi::SHOGI_BOARD_SIZE;
use rand::rngs::StdRng;
use rand::{seq::SliceRandom, SeedableRng};
use std::io::Read;
use std::path::Path;
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;
use tools::common::weighting as wcfg;
use tools::stats::{
    binary_metrics, calibration_bins, ece_from_bins, regression_metrics, roc_auc_weighted,
};

mod loaders {
    use super::*;
    use tools::nnfc_v1::flags as fc_flags;

    include!("training_loaders.rs");
}

mod ctx {
    use super::*;
    include!("training_ctx.rs");
}

mod core {
    use super::*;
    use crate::export::{save_classic_network, save_single_network};
    use crate::logging::print_zero_weight_debug;
    use crate::params::{
        ADAM_BETA1, ADAM_BETA2, ADAM_EPSILON, CLASSIC_RELU_CLIP_F32, PERCENTAGE_DIVISOR,
    };
    use crate::training::ctx::lr_base_for;
    use crate::training::ctx::{DashboardValKind, TrainContext};
    use crate::training::loaders::{AsyncBatchLoader, BatchLoader, StreamCacheLoader};

    use crate::classic::ClassicFloatNetwork;
    use crate::model::{ClassicForwardScratch, ClassicNetwork};
    use engine_core::evaluation::nnue::features::flip_us_them;
    use rand::Rng;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tools::nnfc_v1::flags as fc_flags;

    include!("training_core.rs");
}

pub use core::{train_model, train_model_stream_cache, train_model_with_loader};
pub use ctx::{DashboardOpts, LrPlateauState, TrainContext, TrainTrackers};

pub fn compute_val_auc(network: &Network, samples: &[Sample], config: &Config) -> Option<f64> {
    if config.label_type != "wdl" || samples.is_empty() {
        return None;
    }

    let mut scratch = network.new_forward_scratch();
    let mut probs: Vec<f32> = Vec::with_capacity(samples.len());
    let mut labels: Vec<f32> = Vec::with_capacity(samples.len());
    let mut weights: Vec<f32> = Vec::with_capacity(samples.len());

    for s in samples {
        let out = network.forward_with_scratch(&s.features, &mut scratch);
        let p = sigmoid(out);
        if s.label > 0.5 {
            probs.push(p);
            labels.push(1.0);
            weights.push(s.weight);
        } else if s.label < 0.5 {
            probs.push(p);
            labels.push(0.0);
            weights.push(s.weight);
        }
    }

    if probs.is_empty() {
        None
    } else {
        roc_auc_weighted(&probs, &labels, &weights)
    }
}

pub fn compute_val_auc_and_ece(
    network: &Network,
    samples: &[Sample],
    config: &Config,
    dash_val: &impl ctx::DashboardValKind,
) -> (Option<f64>, Option<f64>) {
    let auc = compute_val_auc(network, samples, config);
    if config.label_type != "wdl" || !dash_val.is_jsonl() {
        return (auc, None);
    }

    let mut cps: Vec<i32> = Vec::new();
    let mut probs: Vec<f32> = Vec::new();
    let mut labels: Vec<f32> = Vec::new();
    let mut weights: Vec<f32> = Vec::new();
    let mut scratch = network.new_forward_scratch();

    for s in samples {
        if let Some(cp) = s.cp {
            let out = network.forward_with_scratch(&s.features, &mut scratch);
            let p = sigmoid(out);
            cps.push(cp);
            probs.push(p);
            labels.push(s.label);
            weights.push(s.weight);
        }
    }

    if cps.is_empty() {
        return (auc, None);
    }

    let bins =
        calibration_bins(&cps, &probs, &labels, &weights, config.cp_clip, dash_val.calib_bins());
    let ece = ece_from_bins(&bins);
    (auc, ece)
}

#[inline]
fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}
