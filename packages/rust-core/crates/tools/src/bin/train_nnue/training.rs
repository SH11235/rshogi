use crate::classic::ClassicIntNetworkBundle;
use crate::dataset::open_cache_payload_reader;
use crate::logging::StructuredLogger;
use crate::model::{
    ClassicForwardScratch, ClassicNetwork, Network, SingleAdamState, SingleForwardScratch,
    SingleNetwork,
};
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
    use crate::export::save_single_network;
    use crate::logging::print_zero_weight_debug;
    use crate::params::PERCENTAGE_DIVISOR;
    use crate::training::ctx::lr_base_for;
    use crate::training::ctx::{DashboardValKind, TrainContext};
    use crate::training::loaders::{AsyncBatchLoader, BatchLoader, StreamCacheLoader};

    use rand::Rng;
    use tools::nnfc_v1::flags as fc_flags;

    include!("training_core.rs");
}

pub use core::{train_model, train_model_stream_cache, train_model_with_loader};
pub use ctx::{DashboardOpts, LrPlateauState, TrainContext, TrainTrackers};

pub fn compute_val_auc(network: &Network, samples: &[Sample], config: &Config) -> Option<f64> {
    match network {
        Network::Single(inner) => core::compute_val_auc_single(inner, samples, config),
        Network::Classic(inner) => core::compute_val_auc_classic(inner, samples, config),
    }
}

pub fn compute_val_auc_and_ece(
    network: &Network,
    samples: &[Sample],
    config: &Config,
    dash_val: &impl ctx::DashboardValKind,
) -> (Option<f64>, Option<f64>) {
    match network {
        Network::Single(inner) => {
            core::compute_val_auc_and_ece_single(inner, samples, config, dash_val)
        }
        Network::Classic(inner) => {
            core::compute_val_auc_and_ece_classic(inner, samples, config, dash_val)
        }
    }
}
