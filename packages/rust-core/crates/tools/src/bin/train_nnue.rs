//! NNUE (Efficiently Updatable Neural Network) trainer
//!
//! This tool trains NNUE models directly from JSONL training data.
//! It supports HalfKP features and row-sparse updates for efficient training.
//!
//! ## Architecture
//!
//! This implementation uses a **single-channel accumulator design** where features
//! from both perspectives (Black and White) are accumulated into a single vector.
//!
//! ### Network Structure:
//! - Input: HalfKP features (King position × Piece position/type)
//! - Hidden Layer: Single accumulator of size `acc_dim` (default: 256)
//! - Output: Single evaluation score
//!
//! ### Feature Extraction:
//! - Black perspective features: Indexed directly as `king_sq * FE_END + piece_index`
//! - White perspective features: Uses the same index space, accumulated into the same vector
//! - Total feature space: `81 * FE_END` (81 king positions × feature types)
//!
//! ### Design Rationale:
//! The single-channel design simplifies the implementation while maintaining good
//! performance. For higher accuracy, a dual-channel architecture (separate accumulators
//! for each perspective) could be implemented by:
//! 1. Doubling the accumulator dimension
//! 2. Adding an offset of `81 * FE_END` to White perspective features
//! 3. Concatenating both accumulators before the output layer
//!
//! ### Row-Sparse Updates:
//! The training uses immediate Adam updates with row-sparse gradients, updating only
//! the weights corresponding to active features. This significantly improves training
//! speed for sparse feature sets like HalfKP.

use std::fs::{create_dir_all, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Instant, SystemTime};
use tools::nnfc_v1::{
    open_payload_reader as open_cache_payload_reader_shared, FEATURE_SET_ID_HALF,
};

use clap::{arg, Command};
use engine_core::game_phase::{detect_game_phase, GamePhase, Profile};
use engine_core::{
    evaluation::nnue::features::{extract_features, FE_END},
    shogi::SHOGI_BOARD_SIZE,
    Color, Position,
};
use rand::rngs::StdRng;
use rand::{seq::SliceRandom, Rng, SeedableRng};
use serde::{Deserialize, Serialize};
use tools::nnfc_v1::flags as fc_flags;
use tools::stats::{
    binary_metrics, calibration_bins, ece_from_bins, regression_metrics, roc_auc_weighted,
};

// Training configuration constants
const DEFAULT_ACC_DIM: &str = "256";
const DEFAULT_RELU_CLIP: &str = "127";
const MAX_PREFETCH_BATCHES: usize = 1024;

// Weight scaling constants
const GAP_WEIGHT_DIVISOR: f32 = 50.0;
const SELECTIVE_DEPTH_WEIGHT: f32 = 0.8;
const NON_EXACT_BOUND_WEIGHT: f32 = 0.7;
const SELECTIVE_DEPTH_MARGIN: i32 = 6;

// Numeric conversion constants
const PERCENTAGE_DIVISOR: f32 = 100.0;
const CP_TO_FLOAT_DIVISOR: f32 = 100.0;
const CP_CLAMP_LIMIT: f32 = 20.0;
const NANOSECONDS_PER_SECOND: f64 = 1e9;
const BYTES_PER_MB: usize = 1024 * 1024;
const KB_TO_MB_DIVISOR: f32 = 1024.0;

// Buffer sizes
const LINE_BUFFER_CAPACITY: usize = 64 * 1024;

// Adam optimizer constants
const ADAM_BETA1: f32 = 0.9;
const ADAM_BETA2: f32 = 0.999;
const ADAM_EPSILON: f32 = 1e-8;

// Safety constants
const MIN_ELAPSED_TIME: f64 = 1e-6;

// Quantization constants
const QUANTIZATION_MIN: f32 = -128.0;
const QUANTIZATION_MAX: f32 = 127.0;
const QUANTIZATION_METADATA_SIZE: usize = 3 * 8 + 4;

#[derive(Debug, Deserialize)]
struct TrainingPosition {
    sfen: String,
    #[serde(default)]
    lines: Vec<LineInfo>,
    #[serde(default)]
    best2_gap_cp: Option<i32>,
    #[serde(default)]
    bound1: Option<String>,
    #[serde(default)]
    bound2: Option<String>,
    #[serde(default)]
    mate_boundary: Option<bool>,
    #[serde(default)]
    no_legal_move: Option<bool>,
    #[serde(default)]
    fallback_used: Option<bool>,
    #[serde(default)]
    eval: Option<i32>,
    #[serde(default)]
    depth: Option<u8>,
    #[serde(default)]
    seldepth: Option<u8>,
}

#[derive(Debug, Deserialize)]
struct LineInfo {
    #[serde(default)]
    score_cp: Option<i32>,
}

#[derive(Clone, Debug, Serialize)]
struct Config {
    epochs: usize,
    batch_size: usize,
    learning_rate: f32,
    optimizer: String,
    l2_reg: f32,
    label_type: String,
    scale: f32,
    cp_clip: i32,
    accumulator_dim: usize,
    relu_clip: i32,
    shuffle: bool,
    // Prefetch/metrics
    prefetch_batches: usize,
    throughput_interval_sec: f32,
    // Streaming cache mode (no preloading)
    stream_cache: bool,
    // Optional cap for prefetch memory usage (bytes). 0 or None = unlimited
    prefetch_bytes: Option<usize>,
    // Estimated active feature count per sample (for memory cap estimation)
    estimated_features_per_sample: usize,
    // Data filters (align with build_feature_cache)
    exclude_no_legal_move: bool,
    exclude_fallback: bool,
    // LR scheduler (Spec #11)
    lr_schedule: String,          // constant|step|cosine
    lr_warmup_epochs: u32,        // >=0
    lr_decay_epochs: Option<u32>, // mutually exclusive with steps
    lr_decay_steps: Option<u64>,  // mutually exclusive with epochs
    lr_plateau_patience: Option<u32>,
}

#[derive(Clone, Debug)]
struct Sample {
    features: Vec<u32>, // Active feature indices for both perspectives
    label: f32,
    weight: f32,
    // Dashboard-only (JSONL validation path). None for cache-loaded samples.
    cp: Option<i32>,
    phase: Option<GamePhase>,
}

// CacheHeader schema is parsed ad-hoc (v1 only). No struct kept to avoid drift.

// Structured JSONL logger (shared by training paths)
struct StructuredLogger {
    to_stdout: bool,
    file: Option<std::sync::Mutex<std::io::BufWriter<File>>>,
}
impl StructuredLogger {
    fn new(path: &str) -> std::io::Result<Self> {
        if path == "-" {
            Ok(Self {
                to_stdout: true,
                file: None,
            })
        } else {
            if let Some(parent) = std::path::Path::new(path).parent() {
                std::fs::create_dir_all(parent)?;
            }
            let f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            let bw = std::io::BufWriter::with_capacity(1 << 20, f);
            Ok(Self {
                to_stdout: false,
                file: Some(std::sync::Mutex::new(bw)),
            })
        }
    }
    fn write_json(&self, v: &serde_json::Value) {
        if self.to_stdout {
            println!("{}", v);
        } else if let Some(ref file) = self.file {
            if let Ok(mut w) = file.lock() {
                let _ = writeln!(w, "{}", v);
            }
        }
    }
}

/// NNUE Network structure with single-channel accumulator
///
/// The network uses a simple 2-layer architecture:
/// 1. Input layer: Maps HalfKP features to accumulator
/// 2. Output layer: Maps accumulator to evaluation score
///
/// Both Black and White perspective features are accumulated into the
/// same accumulator vector, effectively sharing the same weight space.
#[derive(Clone)]
struct Network {
    // Input layer: HalfKP features -> accumulator
    // Dimensions: [N * FE_END, acc_dim] flattened to 1D
    w0: Vec<f32>, // [(SHOGI_BOARD_SIZE * FE_END) * acc_dim]
    b0: Vec<f32>, // [acc_dim]

    // Output layer: accumulator -> score
    w2: Vec<f32>, // [acc_dim]
    b2: f32,

    // Hyperparameters
    input_dim: usize, // SHOGI_BOARD_SIZE * FE_END
    acc_dim: usize,
    relu_clip: f32,
}

impl Network {
    fn new(acc_dim: usize, relu_clip: i32, rng: &mut impl Rng) -> Self {
        // Initialize weights with small random values
        let input_dim = SHOGI_BOARD_SIZE * FE_END;
        let w0_size = input_dim * acc_dim;
        let mut w0 = vec![0.0f32; w0_size];
        for w in w0.iter_mut() {
            *w = rng.random_range(-0.01..0.01);
        }

        let b0 = vec![0.0f32; acc_dim];

        let mut w2 = vec![0.0f32; acc_dim];
        for w in w2.iter_mut() {
            *w = rng.random_range(-0.01..0.01);
        }

        Network {
            w0,
            b0,
            w2,
            b2: 0.0,
            input_dim,
            acc_dim,
            relu_clip: relu_clip as f32,
        }
    }

    // Forward pass with pre-allocated buffers to avoid allocations
    fn forward_with_buffers(
        &self,
        features: &[u32],
        acc_buffer: &mut Vec<f32>,
        activated_buffer: &mut Vec<f32>,
    ) -> f32 {
        // Initialize accumulator from bias
        acc_buffer.clear();
        acc_buffer.extend_from_slice(&self.b0);

        // Add feature contributions
        for &feat_idx in features {
            let feat_idx = feat_idx as usize;
            #[cfg(debug_assertions)]
            debug_assert!(
                feat_idx < self.input_dim,
                "feat_idx={} out of range {}",
                feat_idx,
                self.input_dim
            );

            let offset = feat_idx * self.acc_dim;
            for (i, acc_val) in acc_buffer.iter_mut().enumerate() {
                *acc_val += self.w0[offset + i];
            }
        }

        // Apply clipped ReLU
        activated_buffer.resize(self.acc_dim, 0.0);
        for (i, &x) in acc_buffer.iter().enumerate() {
            activated_buffer[i] = x.max(0.0).min(self.relu_clip);
        }

        // Output layer
        let mut output = self.b2;
        for (w, &act) in self.w2.iter().zip(activated_buffer.iter()) {
            output += w * act;
        }

        output
    }

    // Forward pass into pre-allocated buffers (for training)
    #[inline]
    fn forward_into(&self, features: &[u32], acc: &mut [f32], act: &mut [f32]) -> f32 {
        // Initialize accumulator from bias
        acc.copy_from_slice(&self.b0);

        // Add feature contributions
        for &f in features {
            let f = f as usize;
            #[cfg(debug_assertions)]
            debug_assert!(f < self.input_dim);

            let off = f * self.acc_dim;
            for (i, acc_val) in acc.iter_mut().enumerate() {
                *acc_val += self.w0[off + i];
            }
        }

        // Apply clipped ReLU
        for (i, &x) in acc.iter().enumerate() {
            act[i] = x.max(0.0).min(self.relu_clip);
        }

        // Output layer
        let mut out = self.b2;
        for (i, &act_val) in act.iter().enumerate() {
            out += self.w2[i] * act_val;
        }
        out
    }
}

// Helper function for training to use Network::forward_into
#[inline]
fn forward_into(network: &Network, features: &[u32], acc: &mut [f32], act: &mut [f32]) -> f32 {
    network.forward_into(features, acc, act)
}

struct BatchLoader {
    indices: Vec<usize>,
    batch_size: usize,
    position: usize,
    epoch: usize,
}

impl BatchLoader {
    fn new(num_samples: usize, batch_size: usize, shuffle: bool, rng: &mut StdRng) -> Self {
        let mut indices: Vec<usize> = (0..num_samples).collect();

        if shuffle {
            indices.shuffle(rng);
        }

        BatchLoader {
            indices,
            batch_size,
            position: 0,
            epoch: 0,
        }
    }

    fn next_batch(&mut self) -> Option<Vec<usize>> {
        if self.position >= self.indices.len() {
            return None;
        }

        let end = (self.position + self.batch_size).min(self.indices.len());
        let batch_indices: Vec<usize> = self.indices[self.position..end].to_vec();
        self.position = end;

        Some(batch_indices)
    }

    fn reset(&mut self, shuffle: bool, rng: &mut StdRng) {
        self.position = 0;
        self.epoch += 1;

        if shuffle {
            self.indices.shuffle(rng);
        }
    }
}

// Async prefetching batch loader (indices only)
struct AsyncBatchLoader {
    num_samples: usize,
    batch_size: usize,
    prefetch_batches: usize,
    rx: Option<Receiver<Vec<usize>>>,
    worker: Option<JoinHandle<()>>,
    epoch: usize,
}

impl AsyncBatchLoader {
    fn new(num_samples: usize, batch_size: usize, prefetch_batches: usize) -> Self {
        Self {
            num_samples,
            batch_size,
            prefetch_batches,
            rx: None,
            worker: None,
            epoch: 0,
        }
    }

    fn start_epoch(&mut self, shuffle: bool, seed: u64) {
        // Ensure previous worker has finished
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
        self.epoch += 1;

        let (tx, rx) = sync_channel::<Vec<usize>>(self.prefetch_batches);
        let num_samples = self.num_samples;
        let batch_size = self.batch_size;

        let handle = std::thread::spawn(move || {
            // Prepare indices
            let mut indices: Vec<usize> = (0..num_samples).collect();
            if shuffle {
                let mut srng = StdRng::seed_from_u64(seed);
                indices.shuffle(&mut srng);
            }
            // Stream batches into the channel
            let mut pos = 0;
            while pos < indices.len() {
                let end = (pos + batch_size).min(indices.len());
                // Copy indices slice (small object)
                let batch = indices[pos..end].to_vec();
                if tx.send(batch).is_err() {
                    break; // receiver dropped
                }
                pos = end;
            }
        });

        self.rx = Some(rx);
        self.worker = Some(handle);
    }

    fn next_batch_with_wait(&self) -> (Option<Vec<usize>>, std::time::Duration) {
        if let Some(rx) = &self.rx {
            let t0 = Instant::now();
            match rx.recv() {
                Ok(v) => (Some(v), t0.elapsed()),
                Err(_) => (None, t0.elapsed()),
            }
        } else {
            (None, std::time::Duration::ZERO)
        }
    }

    fn finish(&mut self) {
        // Drain and join worker if any
        self.rx.take();
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for AsyncBatchLoader {
    fn drop(&mut self) {
        self.finish();
    }
}

// Streaming cache loader: reads cache file on a worker thread and sends Vec<Sample>
enum BatchMsg {
    Ok(Vec<Sample>),
    Err(String),
}

struct StreamCacheLoader {
    path: String,
    batch_size: usize,
    prefetch_batches: usize,
    rx: Option<Receiver<BatchMsg>>,
    worker: Option<JoinHandle<()>>,
}

impl StreamCacheLoader {
    fn new(path: String, batch_size: usize, prefetch_batches: usize) -> Self {
        Self {
            path,
            batch_size,
            prefetch_batches,
            rx: None,
            worker: None,
        }
    }

    fn start_epoch(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        // Join any previous worker
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
        let path = self.path.clone();
        let batch_size = self.batch_size;
        let (tx, rx) = sync_channel::<BatchMsg>(self.prefetch_batches.max(1));

        let handle = std::thread::spawn(move || {
            // Use shared nnfc_v1 reader
            let (reader, header) = match tools::nnfc_v1::open_payload_reader(&path) {
                Ok(v) => v,
                Err(e) => {
                    let _ = tx.send(BatchMsg::Err(format!("Failed to open cache {}: {}", path, e)));
                    return;
                }
            };
            if header.feature_set_id != tools::nnfc_v1::FEATURE_SET_ID_HALF {
                let _ = tx.send(BatchMsg::Err(format!(
                    "Unsupported feature_set_id: 0x{:08x} (file {})",
                    header.feature_set_id, path
                )));
                return;
            }
            let num_samples = header.num_samples;
            let flags_mask = header.flags_mask;
            let mut r = reader; // BufReader<Box<dyn Read>>

            let mut loaded: u64 = 0;
            let mut batch = Vec::with_capacity(batch_size);
            let mut unknown_flag_samples: u64 = 0;
            let mut unknown_flag_bits_accum: u32 = 0;

            while loaded < num_samples {
                // Read one sample
                // n_features
                let mut nb = [0u8; 4];
                if let Err(e) = r.read_exact(&mut nb) {
                    let _ = tx.send(BatchMsg::Err(format!(
                        "Read error at sample {} in {}: {}",
                        loaded, path, e
                    )));
                    return;
                }
                let n_features = u32::from_le_bytes(nb) as usize;
                const MAX_FEATURES_PER_SAMPLE: usize = SHOGI_BOARD_SIZE * FE_END;
                if n_features > MAX_FEATURES_PER_SAMPLE {
                    let _ = tx.send(BatchMsg::Err(format!(
                        "n_features={} exceeds sane limit {} in {}",
                        n_features, MAX_FEATURES_PER_SAMPLE, path
                    )));
                    return;
                }
                let mut features: Vec<u32> = vec![0u32; n_features];
                #[cfg(target_endian = "little")]
                {
                    use bytemuck::cast_slice_mut;
                    if let Err(e) = r.read_exact(cast_slice_mut::<u32, u8>(&mut features)) {
                        let _ = tx.send(BatchMsg::Err(format!(
                            "Read features failed at {}: {}",
                            loaded, e
                        )));
                        return;
                    }
                }
                #[cfg(target_endian = "big")]
                {
                    let mut buf = vec![0u8; n_features * 4];
                    if let Err(e) = r.read_exact(&mut buf) {
                        let _ = tx.send(BatchMsg::Err(format!(
                            "Read features failed at {}: {}",
                            loaded, e
                        )));
                        return;
                    }
                    for (dst, chunk) in features.iter_mut().zip(buf.chunks_exact(4)) {
                        *dst = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                    }
                }

                // label
                let mut lb = [0u8; 4];
                if let Err(e) = r.read_exact(&mut lb) {
                    let _ =
                        tx.send(BatchMsg::Err(format!("Read label failed at {}: {}", loaded, e)));
                    return;
                }
                let label = f32::from_le_bytes(lb);

                // meta
                let mut gapb = [0u8; 2];
                if let Err(e) = r.read_exact(&mut gapb) {
                    let _ = tx.send(BatchMsg::Err(format!("Read gap failed at {}: {}", loaded, e)));
                    return;
                }
                let gap = u16::from_le_bytes(gapb);

                let mut depth = [0u8; 1];
                if let Err(e) = r.read_exact(&mut depth) {
                    let _ =
                        tx.send(BatchMsg::Err(format!("Read depth failed at {}: {}", loaded, e)));
                    return;
                }
                let depth = depth[0];

                let mut seldepth = [0u8; 1];
                if let Err(e) = r.read_exact(&mut seldepth) {
                    let _ = tx
                        .send(BatchMsg::Err(format!("Read seldepth failed at {}: {}", loaded, e)));
                    return;
                }
                let seldepth = seldepth[0];

                let mut flags = [0u8; 1];
                if let Err(e) = r.read_exact(&mut flags) {
                    let _ =
                        tx.send(BatchMsg::Err(format!("Read flags failed at {}: {}", loaded, e)));
                    return;
                }
                let flags = flags[0];
                let unknown = (flags as u32) & !flags_mask;
                if unknown != 0 {
                    unknown_flag_samples += 1;
                    unknown_flag_bits_accum |= unknown;
                }

                // weight policy
                let mut weight = 1.0f32;
                weight *= (gap as f32 / GAP_WEIGHT_DIVISOR).min(1.0);
                let both_exact = (flags & fc_flags::BOTH_EXACT) != 0;
                weight *= if both_exact {
                    1.0
                } else {
                    NON_EXACT_BOUND_WEIGHT
                };
                if (flags & fc_flags::MATE_BOUNDARY) != 0 {
                    weight *= 0.5;
                }
                if seldepth < depth.saturating_add(SELECTIVE_DEPTH_MARGIN as u8) {
                    weight *= SELECTIVE_DEPTH_WEIGHT;
                }

                batch.push(Sample {
                    features,
                    label,
                    weight,
                    cp: None,
                    phase: None,
                });
                loaded += 1;

                if batch.len() >= batch_size {
                    if tx.send(BatchMsg::Ok(std::mem::take(&mut batch))).is_err() {
                        break;
                    }
                    batch = Vec::with_capacity(batch_size);
                }

                // Progress log is omitted in worker to avoid log interleaving with training side
            }

            if !batch.is_empty() {
                let _ = tx.send(BatchMsg::Ok(batch));
            }

            if unknown_flag_samples > 0 {
                eprintln!(
                    "Warning: {} samples contained unknown flag bits (mask=0x{:08x}, seen=0x{:08x})",
                    unknown_flag_samples, flags_mask, unknown_flag_bits_accum
                );
            }
        });

        self.rx = Some(rx);
        self.worker = Some(handle);
        Ok(())
    }

    fn next_batch_with_wait(&self) -> (Option<Result<Vec<Sample>, String>>, std::time::Duration) {
        if let Some(rx) = &self.rx {
            let t0 = Instant::now();
            match rx.recv() {
                Ok(BatchMsg::Ok(v)) => (Some(Ok(v)), t0.elapsed()),
                Ok(BatchMsg::Err(msg)) => (Some(Err(msg)), t0.elapsed()),
                Err(_) => (None, t0.elapsed()),
            }
        } else {
            (None, std::time::Duration::ZERO)
        }
    }

    fn finish(&mut self) {
        self.rx.take();
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

impl Drop for StreamCacheLoader {
    fn drop(&mut self) {
        self.finish();
    }
}

// Adam optimizer state
struct AdamState {
    m_w0: Vec<f32>,
    v_w0: Vec<f32>,
    m_b0: Vec<f32>,
    v_b0: Vec<f32>,
    m_w2: Vec<f32>,
    v_w2: Vec<f32>,
    m_b2: f32,
    v_b2: f32,
    beta1: f32,
    beta2: f32,
    epsilon: f32,
    t: usize,
}

impl AdamState {
    fn new(network: &Network) -> Self {
        AdamState {
            m_w0: vec![0.0; network.w0.len()],
            v_w0: vec![0.0; network.w0.len()],
            m_b0: vec![0.0; network.b0.len()],
            v_b0: vec![0.0; network.b0.len()],
            m_w2: vec![0.0; network.w2.len()],
            v_w2: vec![0.0; network.w2.len()],
            m_b2: 0.0,
            v_b2: 0.0,
            beta1: ADAM_BETA1,
            beta2: ADAM_BETA2,
            epsilon: ADAM_EPSILON,
            t: 0,
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Command::new("train_nnue")
        .about("Train NNUE model from JSONL data")
        .arg(arg!(-i --input <FILE> "Input JSONL file").required(true))
        .arg(arg!(-v --validation <FILE> "Validation JSONL file"))
        .arg(arg!(-e --epochs <N> "Number of epochs").default_value("2"))
        .arg(arg!(-b --"batch-size" <N> "Batch size").default_value("8192"))
        .arg(arg!(--lr <RATE> "Learning rate").default_value("0.001"))
        .arg(arg!(--opt <TYPE> "Optimizer: sgd, adam").default_value("adam"))
        .arg(arg!(--l2 <RATE> "L2 regularization").default_value("0.000001"))
        .arg(
            arg!(-l --label <TYPE> "Label type: wdl, cp")
                .value_parser(["wdl", "cp"]) // strict accepted values
                .default_value("wdl"),
        )
        .arg(
            arg!(--scale <N> "Scale for cp->wdl conversion")
                .value_parser(clap::value_parser!(f32))
                .default_value("600"),
        )
        .arg(
            arg!(--"cp-clip" <N> "Clip CP values to this range")
                .value_parser(clap::value_parser!(i32).range(0..))
                .default_value("1200"),
        )
        .arg(arg!(--"acc-dim" <N> "Accumulator dimension").default_value(DEFAULT_ACC_DIM))
        .arg(arg!(--"relu-clip" <N> "ReLU clipping value").default_value(DEFAULT_RELU_CLIP))
        .arg(arg!(--shuffle "Shuffle training data"))
        .arg(arg!(--"exclude-no-legal-move" "Exclude positions with no legal moves (JSONL input)"))
        .arg(arg!(--"exclude-fallback" "Exclude positions where fallback was used (JSONL input)"))
        .arg(arg!(--"save-every" <N> "Save checkpoint every N batches"))
        .arg(arg!(--"stream-cache" "Stream cache input without preloading (disables shuffle)"))
        .arg(
            arg!(--"prefetch-batches" <N> "Async prefetch queue depth (cache/stream-cache input)")
                .default_value("2")
                .value_parser(clap::value_parser!(usize)),
        )
        .arg(
            arg!(--"throughput-interval" <SECS> "Seconds between throughput reports")
                .default_value("2.0")
                .value_parser(clap::value_parser!(f32)),
        )
        .arg(
            arg!(--"prefetch-bytes" <BYTES> "Approximate memory cap for prefetched batches (bytes)")
                .value_parser(clap::value_parser!(usize))
        )
        .arg(
            arg!(--"estimated-features-per-sample" <N> "Estimated active features per sample (for prefetch memory cap)")
                .default_value("64")
                .value_parser(clap::value_parser!(usize))
        )
        .arg(arg!(--metrics "Emit per-epoch metrics CSV").action(clap::ArgAction::SetTrue))
        .arg(
            arg!(--"calibration-bins" <N> "Bins for cp calibration (JSONL validation)")
                .value_parser(clap::value_parser!(usize))
                .default_value("40"),
        )
        .arg(
            arg!(--"plots" "Emit PNG plots (requires features=plots)")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            arg!(--"gate-val-loss-non-increase" "Fail if best val_loss not at last epoch")
                .action(clap::ArgAction::SetTrue),
        )
        .arg(
            arg!(--"gate-min-auc" <N> "Minimum AUC to pass (wdl only)")
                .value_parser(clap::value_parser!(f64)),
        )
        .arg(
            arg!(--"gate-mode" <MODE> "Gate behavior")
                .value_parser(["warn", "fail"]) 
                .default_value("warn"),
        )
        // LR scheduler (Spec #11)
        .arg(
            arg!(--"lr-schedule" <KIND> "LR scheduler: constant|step|cosine")
                .value_parser(["constant", "step", "cosine"]) 
                .default_value("constant"),
        )
        .arg(
            arg!(--"lr-warmup-epochs" <N> "Warmup epochs for LR")
                .value_parser(clap::value_parser!(u32))
                .default_value("0"),
        )
        .arg(
            arg!(--"lr-decay-epochs" <N> "Decay interval in epochs (step/cosine)")
                .value_parser(clap::value_parser!(u32))
                .conflicts_with("lr-decay-steps"),
        )
        .arg(
            arg!(--"lr-decay-steps" <N> "Decay interval in steps (step/cosine)")
                .value_parser(clap::value_parser!(u64))
                .conflicts_with("lr-decay-epochs"),
        )
        .arg(
            arg!(--"lr-plateau-patience" <N> "Plateau patience in epochs (optional, step)")
                .value_parser(clap::value_parser!(u32)),
        )
        .arg(
            arg!(--"structured-log" <PATH> "Structured JSONL log path ('-' for STDOUT)")
        )
        .arg(arg!(--quantized "Save quantized (int8) version of the model"))
        .arg(arg!(--seed <SEED> "Random seed for reproducibility"))
        .arg(arg!(-o --out <DIR> "Output directory"))
        .get_matches();

    // Prepare structured logger early for stdout/stderr routing decisions
    let structured_logger: Option<StructuredLogger> = app
        .get_one::<String>("structured-log")
        .and_then(|p| match StructuredLogger::new(p) {
            Ok(lg) => Some(lg),
            Err(e) => {
                eprintln!("Warning: failed to open structured log '{}': {}", p, e);
                None
            }
        });
    let human_to_stderr = structured_logger.as_ref().map(|lg| lg.to_stdout).unwrap_or(false);

    let config = Config {
        epochs: app.get_one::<String>("epochs").unwrap().parse()?,
        batch_size: app.get_one::<String>("batch-size").unwrap().parse()?,
        learning_rate: app.get_one::<String>("lr").unwrap().parse()?,
        optimizer: app.get_one::<String>("opt").unwrap().to_string(),
        l2_reg: app.get_one::<String>("l2").unwrap().parse()?,
        label_type: app.get_one::<String>("label").unwrap().to_string(),
        scale: *app.get_one::<f32>("scale").unwrap(),
        cp_clip: *app.get_one::<i32>("cp-clip").unwrap(),
        accumulator_dim: app.get_one::<String>("acc-dim").unwrap().parse()?,
        relu_clip: app.get_one::<String>("relu-clip").unwrap().parse()?,
        shuffle: app.get_flag("shuffle"),
        prefetch_batches: *app.get_one::<usize>("prefetch-batches").unwrap(),
        throughput_interval_sec: *app.get_one::<f32>("throughput-interval").unwrap(),
        stream_cache: app.get_flag("stream-cache"),
        prefetch_bytes: app.get_one::<usize>("prefetch-bytes").copied(),
        estimated_features_per_sample: *app
            .get_one::<usize>("estimated-features-per-sample")
            .unwrap(),
        exclude_no_legal_move: app.get_flag("exclude-no-legal-move"),
        exclude_fallback: app.get_flag("exclude-fallback"),
        lr_schedule: app
            .get_one::<String>("lr-schedule")
            .map(|s| s.to_string())
            .unwrap_or_else(|| "constant".to_string()),
        lr_warmup_epochs: *app.get_one::<u32>("lr-warmup-epochs").unwrap_or(&0u32),
        lr_decay_epochs: app.get_one::<u32>("lr-decay-epochs").copied(),
        lr_decay_steps: app.get_one::<u64>("lr-decay-steps").copied(),
        lr_plateau_patience: app.get_one::<u32>("lr-plateau-patience").copied(),
    };

    if config.scale <= 0.0 {
        return Err("Invalid --scale: must be > 0".into());
    }
    if config.throughput_interval_sec <= 0.0 {
        return Err("Invalid --throughput-interval: must be > 0".into());
    }
    if config.prefetch_batches > MAX_PREFETCH_BATCHES {
        return Err(format!("Invalid --prefetch-batches: must be <= {MAX_PREFETCH_BATCHES}").into());
    }
    if let Some(0) = config.lr_decay_epochs {
        eprintln!("Error: --lr-decay-epochs must be > 0");
        std::process::exit(2);
    }
    if let Some(0) = config.lr_decay_steps {
        eprintln!("Error: --lr-decay-steps must be > 0");
        std::process::exit(2);
    }

    let input_path = app.get_one::<String>("input").unwrap();
    let validation_path = app.get_one::<String>("validation");
    let emit_metrics = app.get_flag("metrics");
    let calib_bins_n = *app.get_one::<usize>("calibration-bins").unwrap_or(&40usize);
    let do_plots = app.get_flag("plots");
    let gate_last_epoch_best = app.get_flag("gate-val-loss-non-increase");
    let gate_min_auc = app.get_one::<f64>("gate-min-auc").copied();
    let gate_mode_fail = app.get_one::<String>("gate-mode").map(|s| s == "fail").unwrap_or(false);
    let save_every: Option<usize> =
        app.get_one::<String>("save-every").map(|s| s.parse()).transpose()?;

    let timestamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    let out_dir = app
        .get_one::<String>("out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("runs/nnue_{}", timestamp)));

    if human_to_stderr {
        eprintln!("Configuration:");
    } else {
        println!("Configuration:");
    }
    if human_to_stderr {
        eprintln!("  Input: {}", input_path);
    } else {
        println!("  Input: {}", input_path);
    }
    if let Some(val_path) = validation_path {
        if human_to_stderr {
            eprintln!("  Validation: {}", val_path);
        } else {
            println!("  Validation: {}", val_path);
        }
    }
    if human_to_stderr {
        eprintln!("  Output: {}", out_dir.display());
    } else {
        println!("  Output: {}", out_dir.display());
    }
    if human_to_stderr {
        eprintln!("  Settings: {:?}", config);
    } else {
        println!("  Settings: {:?}", config);
    }
    if human_to_stderr {
        eprintln!("  Feature dimension (input): {} (HalfKP)", SHOGI_BOARD_SIZE * FE_END);
    } else {
        println!("  Feature dimension (input): {} (HalfKP)", SHOGI_BOARD_SIZE * FE_END);
    }
    if human_to_stderr {
        eprintln!("  Network: {} -> {} -> 1", SHOGI_BOARD_SIZE * FE_END, config.accumulator_dim);
    } else {
        println!("  Network: {} -> {} -> 1", SHOGI_BOARD_SIZE * FE_END, config.accumulator_dim);
    }

    // Decide input mode
    // Robustly detect NNFC cache (raw/gzip/zstd) by attempting to parse the header.
    // This avoids misclassifying compressed caches as JSONL.
    fn is_cache_file(path: &str) -> bool {
        match open_cache_payload_reader_shared(path) {
            Ok((_r, header)) => header.feature_set_id == FEATURE_SET_ID_HALF,
            Err(_) => false,
        }
    }

    let is_cache = is_cache_file(input_path);
    if config.stream_cache && !is_cache {
        eprintln!("Warning: --stream-cache was set but input is not a cache file; ignoring.");
    }

    // Load training data only when not streaming
    let mut train_samples: Vec<Sample> = Vec::new();
    if !(is_cache && config.stream_cache) {
        let start_time = Instant::now();
        if human_to_stderr {
            eprintln!("\nLoading training data...");
        } else {
            println!("\nLoading training data...");
        }
        train_samples = if is_cache {
            if human_to_stderr {
                eprintln!("Loading from cache file...");
            } else {
                println!("Loading from cache file...");
            }
            load_samples_from_cache(input_path)?
        } else {
            load_samples(input_path, &config)?
        };
        if human_to_stderr {
            eprintln!(
                "Loaded {} samples in {:.2}s",
                train_samples.len(),
                start_time.elapsed().as_secs_f32()
            );
        } else {
            println!(
                "Loaded {} samples in {:.2}s",
                train_samples.len(),
                start_time.elapsed().as_secs_f32()
            );
        }
    } else {
        if human_to_stderr {
            eprintln!("\nStreaming training data from cache (no preloading)...");
        } else {
            println!("\nStreaming training data from cache (no preloading)...");
        }
        if config.shuffle {
            eprintln!("Note: shuffle is disabled in --stream-cache mode.");
        }
    }

    // Load validation data if provided
    let mut val_is_jsonl = false;
    let validation_samples = if let Some(val_path) = validation_path {
        if human_to_stderr {
            eprintln!("\nLoading validation data...");
        } else {
            println!("\nLoading validation data...");
        }
        let start_val = Instant::now();

        let is_val_cache = is_cache_file(val_path);
        let samples = if is_val_cache {
            if human_to_stderr {
                eprintln!("Loading validation from cache file...");
            } else {
                println!("Loading validation from cache file...");
            }
            load_samples_from_cache(val_path)?
        } else {
            val_is_jsonl = true;
            load_samples(val_path, &config)?
        };

        if human_to_stderr {
            eprintln!(
                "Loaded {} validation samples in {:.2}s",
                samples.len(),
                start_val.elapsed().as_secs_f32()
            );
        } else {
            println!(
                "Loaded {} validation samples in {:.2}s",
                samples.len(),
                start_val.elapsed().as_secs_f32()
            );
        }
        Some(samples)
    } else {
        None
    };

    // Initialize RNG with seed if provided
    let seed_u64_opt: Option<u64> =
        app.get_one::<String>("seed").and_then(|s| s.parse::<u64>().ok());
    let mut rng: StdRng = if let Some(seed) = seed_u64_opt {
        if human_to_stderr {
            eprintln!("Using random seed (u64): {}", seed);
        } else {
            println!("Using random seed (u64): {}", seed);
        }
        StdRng::seed_from_u64(seed)
    } else {
        let seed_bytes: [u8; 32] = rand::rng().random();
        let seed_hex = seed_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        let u64_proj = u64::from_le_bytes(seed_bytes[0..8].try_into().unwrap());
        if human_to_stderr {
            eprintln!("Generated random seed (32B hex): {} | (u64 proj): {}", seed_hex, u64_proj);
        } else {
            println!("Generated random seed (32B hex): {} | (u64 proj): {}", seed_hex, u64_proj);
        }
        StdRng::from_seed(seed_bytes)
    };

    // Initialize network
    let mut network = Network::new(config.accumulator_dim, config.relu_clip, &mut rng);

    // Train the model
    if human_to_stderr {
        eprintln!("\nTraining...");
    } else {
        println!("\nTraining...");
    }
    create_dir_all(&out_dir)?;
    if emit_metrics {
        let mut w = csv::Writer::from_path(out_dir.join("metrics.csv"))?;
        w.write_record([
            "epoch",
            "train_loss",
            "val_loss",
            "val_auc",
            "val_ece",
            "time_sec",
            "train_weight_sum",
            "val_weight_sum",
            "is_best",
        ])?;
        w.flush()?;
        let mut wp = csv::Writer::from_path(out_dir.join("phase_metrics.csv"))?;
        wp.write_record([
            "epoch",
            "phase",
            "count",
            "weighted_count",
            "logloss",
            "brier",
            "accuracy",
            "mae",
            "mse",
        ])?;
        wp.flush()?;
    }

    // Dashboard options
    let dash = DashboardOpts {
        emit: emit_metrics,
        calib_bins_n,
        do_plots,
        val_is_jsonl,
    };

    // structured_logger is already created above

    // Track best/last for gates and best saving
    let mut best_network: Option<Network> = None;
    let mut best_val_loss: f32 = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_epoch: Option<usize> = None;

    // Training mode dispatch (scope to release borrows when done)
    {
        let mut ctx = TrainContext {
            out_dir: &out_dir,
            save_every,
            dash,
            trackers: TrainTrackers {
                best_network: &mut best_network,
                best_val_loss: &mut best_val_loss,
                last_val_loss: &mut last_val_loss,
                best_epoch: &mut best_epoch,
            },
            structured: structured_logger,
            global_step: 0,
        };
        if is_cache && config.stream_cache {
            train_model_stream_cache(
                &mut network,
                input_path,
                &validation_samples,
                &config,
                &mut rng,
                &mut ctx,
            )?;
        } else if is_cache {
            train_model_with_loader(
                &mut network,
                train_samples,
                &validation_samples,
                &config,
                &mut rng,
                &mut ctx,
            )?;
        } else {
            train_model(
                &mut network,
                &mut train_samples,
                &validation_samples,
                &config,
                &mut rng,
                &mut ctx,
            )?;
        }
    }

    // Save final model
    save_network(&network, &out_dir.join("nn.fp32.bin"))?;

    // Save quantized version if requested
    if app.get_flag("quantized") {
        save_network_quantized(&network, &out_dir.join("nn.i8.bin"))?;
    }

    // Save config
    let mut config_file = File::create(out_dir.join("config.json"))?;
    writeln!(config_file, "{}", serde_json::to_string_pretty(&config)?)?;

    // Save best network and meta when validation present
    if let Some(val_samples) = &validation_samples {
        if let Some(best_net) = &best_network {
            save_network(best_net, &out_dir.join("nn_best.fp32.bin"))?;
            #[derive(serde::Serialize)]
            struct BestMeta {
                best_epoch: usize,
                best_val_loss: f32,
                best_val_auc: Option<f64>,
                best_val_ece: Option<f64>,
                // Repro metadata for reproducibility
                seed: Option<u64>,
                optimizer: String,
                lr: f32,
                l2: f32,
                acc_dim: usize,
                relu_clip: i32,
                label_type: String,
                scale: f32,
                cp_clip: i32,
            }
            let (best_val_auc, best_val_ece) =
                compute_val_auc_and_ece(best_net, val_samples, &config, &dash);
            let meta = BestMeta {
                best_epoch: best_epoch.unwrap_or(0),
                best_val_loss,
                best_val_auc,
                best_val_ece,
                seed: seed_u64_opt,
                optimizer: config.optimizer.clone(),
                lr: config.learning_rate,
                l2: config.l2_reg,
                acc_dim: config.accumulator_dim,
                relu_clip: config.relu_clip,
                label_type: config.label_type.clone(),
                scale: config.scale,
                cp_clip: config.cp_clip,
            };
            let mut mf = File::create(out_dir.join("nn_best.meta.json"))?;
            writeln!(mf, "{}", serde_json::to_string_pretty(&meta)?)?;
            if human_to_stderr {
                eprintln!(
                    "Saved best validation network to {}",
                    out_dir.join("nn_best.fp32.bin").display()
                );
            } else {
                println!(
                    "Saved best validation network to {}",
                    out_dir.join("nn_best.fp32.bin").display()
                );
            }
        }
    }

    // Gating
    if gate_last_epoch_best {
        match (last_val_loss, best_val_loss.is_finite(), validation_samples.is_some()) {
            (Some(last), true, true) => {
                let pass = last <= best_val_loss + 1e-6;
                if human_to_stderr {
                    eprintln!(
                        "GATE val_loss_last_is_best: {} (last={:.6}, best={:.6})",
                        if pass { "PASS" } else { "FAIL" },
                        last,
                        best_val_loss
                    );
                } else {
                    println!(
                        "GATE val_loss_last_is_best: {} (last={:.6}, best={:.6})",
                        if pass { "PASS" } else { "FAIL" },
                        last,
                        best_val_loss
                    );
                }
                if !pass && gate_mode_fail {
                    std::process::exit(1);
                }
            }
            _ => {
                if human_to_stderr {
                    eprintln!("GATE val_loss_last_is_best: SKIP (no validation)")
                } else {
                    println!("GATE val_loss_last_is_best: SKIP (no validation)")
                }
            }
        }
    }
    if let (Some(th), Some(val_samples)) = (gate_min_auc, validation_samples.as_ref()) {
        if config.label_type == "wdl" {
            let auc = compute_val_auc(&network, val_samples, &config);
            match auc {
                Some(v) => {
                    let pass = v >= th;
                    if human_to_stderr {
                        eprintln!(
                            "GATE min_auc {:.4} >= {:.4}: {}",
                            v,
                            th,
                            if pass { "PASS" } else { "FAIL" }
                        );
                    } else {
                        println!(
                            "GATE min_auc {:.4} >= {:.4}: {}",
                            v,
                            th,
                            if pass { "PASS" } else { "FAIL" }
                        );
                    }
                    if !pass && gate_mode_fail {
                        std::process::exit(1);
                    }
                }
                None => {
                    if human_to_stderr {
                        eprintln!("GATE min_auc: SKIP (insufficient positive/negative)")
                    } else {
                        println!("GATE min_auc: SKIP (insufficient positive/negative)")
                    }
                }
            }
        } else if human_to_stderr {
            eprintln!("GATE min_auc: SKIP (label_type!=wdl)");
        } else {
            println!("GATE min_auc: SKIP (label_type!=wdl)");
        }
    }

    if human_to_stderr {
        eprintln!("\nModel saved to: {}", out_dir.display());
    } else {
        println!("\nModel saved to: {}", out_dir.display());
    }

    Ok(())
}

fn open_jsonl_reader(path: &str) -> Result<Box<dyn BufRead>, Box<dyn std::error::Error>> {
    const BUF_MB: usize = 4;
    tools::io_detect::open_maybe_compressed_reader(path, BUF_MB * BYTES_PER_MB)
}

fn load_samples(path: &str, config: &Config) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let mut reader = open_jsonl_reader(path)?;
    let mut samples = Vec::new();
    let mut skipped = 0;
    let mut line_buf: Vec<u8> = Vec::with_capacity(LINE_BUFFER_CAPACITY);

    loop {
        line_buf.clear();
        let n = reader.read_until(b'\n', &mut line_buf)?;
        if n == 0 {
            break;
        }
        if line_buf.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }

        let pos_data: TrainingPosition = match serde_json::from_slice(&line_buf) {
            Ok(data) => data,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        // Optional filters (align with build_feature_cache)
        if config.exclude_no_legal_move && pos_data.no_legal_move.unwrap_or(false) {
            skipped += 1;
            continue;
        }
        if config.exclude_fallback && pos_data.fallback_used.unwrap_or(false) {
            skipped += 1;
            continue;
        }

        // Get evaluation score
        let cp = if let Some(eval) = pos_data.eval {
            eval
        } else if let Some(line) = pos_data.lines.first() {
            line.score_cp.unwrap_or(0)
        } else {
            skipped += 1;
            continue;
        };

        // Create position and extract features
        let position = match Position::from_sfen(&pos_data.sfen) {
            Ok(pos) => pos,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        // Extract HalfKP features for both perspectives
        let (Some(black_king), Some(white_king)) = (
            position.board.king_square(Color::Black),
            position.board.king_square(Color::White),
        ) else {
            skipped += 1;
            continue;
        };
        // Oriented CP (black perspective)
        let stm = position.side_to_move;
        let cp_black = if stm == Color::Black { cp } else { -cp };
        let cp_white = -cp_black;

        // Calculate sample weight once (shared)
        let weight = calculate_weight(&pos_data, config);

        // Black perspective sample
        {
            let feats = extract_features(&position, black_king, Color::Black);
            let features: Vec<u32> = feats.as_slice().iter().map(|&f| f as u32).collect();
            let label = match config.label_type.as_str() {
                "wdl" => cp_to_wdl(cp_black, config.scale),
                "cp" => {
                    (cp_black.clamp(-config.cp_clip, config.cp_clip) as f32) / CP_TO_FLOAT_DIVISOR
                }
                _ => continue,
            };
            samples.push(Sample {
                features,
                label,
                weight,
                cp: Some(cp_black),
                phase: Some(detect_game_phase(&position, position.ply as u32, Profile::Search)),
            });
        }

        // White perspective sample
        {
            let feats = extract_features(&position, white_king, Color::White);
            let features: Vec<u32> = feats.as_slice().iter().map(|&f| f as u32).collect();
            let label = match config.label_type.as_str() {
                "wdl" => cp_to_wdl(cp_white, config.scale),
                "cp" => {
                    (cp_white.clamp(-config.cp_clip, config.cp_clip) as f32) / CP_TO_FLOAT_DIVISOR
                }
                _ => continue,
            };
            samples.push(Sample {
                features,
                label,
                weight,
                cp: Some(cp_white),
                phase: Some(detect_game_phase(&position, position.ply as u32, Profile::Search)),
            });
        }
    }

    if skipped > 0 {
        eprintln!("Skipped {} positions (invalid/filtered)", skipped);
    }

    Ok(samples)
}

fn cp_to_wdl(cp: i32, scale: f32) -> f32 {
    let x = (cp as f32 / scale).clamp(-CP_CLAMP_LIMIT, CP_CLAMP_LIMIT);
    1.0 / (1.0 + (-x).exp())
}

#[inline]
fn is_exact_opt(s: &Option<String>) -> bool {
    s.as_deref()
        .map(|t| t.trim())
        .map(|t| t.eq_ignore_ascii_case("Exact"))
        .unwrap_or(false)
}

fn calculate_weight(pos_data: &TrainingPosition, _config: &Config) -> f32 {
    let mut weight = 1.0;

    // Gap-based weight
    if let Some(gap) = pos_data.best2_gap_cp {
        weight *= (gap as f32 / GAP_WEIGHT_DIVISOR).min(1.0);
    }

    // Bound-based weight
    let both_exact = is_exact_opt(&pos_data.bound1) && is_exact_opt(&pos_data.bound2);
    weight *= if both_exact {
        1.0
    } else {
        NON_EXACT_BOUND_WEIGHT
    };

    // Mate boundary weight
    if pos_data.mate_boundary.unwrap_or(false) {
        weight *= 0.5;
    }

    // Depth-based weight
    if let (Some(depth), Some(seldepth)) = (pos_data.depth, pos_data.seldepth) {
        if seldepth < depth.saturating_add(SELECTIVE_DEPTH_MARGIN as u8) {
            weight *= SELECTIVE_DEPTH_WEIGHT;
        }
    }

    weight
}

// Type alias to keep function signatures simple
type CachePayload = (BufReader<Box<dyn Read>>, u64, u32);

// Common helper: open a v1 cache file and return a BufReader over the payload,
// along with (num_samples, flags_mask). Handles raw/gzip/zstd based on header.
fn open_cache_payload_reader(path: &str) -> Result<CachePayload, Box<dyn std::error::Error>> {
    let (r, header) = open_cache_payload_reader_shared(path)?;
    if header.feature_set_id != FEATURE_SET_ID_HALF {
        return Err(format!(
            "Unsupported feature_set_id: 0x{:08x} for file {}",
            header.feature_set_id, path
        )
        .into());
    }
    Ok((r, header.num_samples, header.flags_mask))
}

fn load_samples_from_cache(path: &str) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let (mut r, num_samples, flags_mask) = open_cache_payload_reader(path)?;

    eprintln!("Loading cache: {num_samples} samples");

    let mut samples = Vec::with_capacity(num_samples as usize);
    let mut unknown_flag_samples: u64 = 0;
    let mut unknown_flag_bits_accum: u32 = 0;

    for i in 0..num_samples {
        if i % 100000 == 0 && i > 0 {
            eprintln!("  Loaded {i}/{num_samples} samples...");
        }

        // Read number of features
        let mut nb = [0u8; 4];
        r.read_exact(&mut nb)?;
        let n_features = u32::from_le_bytes(nb) as usize;
        // Guard against unreasonable feature counts (OOM/corruption)
        const MAX_FEATURES_PER_SAMPLE: usize = SHOGI_BOARD_SIZE * FE_END;
        if n_features > MAX_FEATURES_PER_SAMPLE {
            return Err(format!(
                "n_features={} exceeds sane limit {}; file {} may be corrupted",
                n_features, MAX_FEATURES_PER_SAMPLE, path
            )
            .into());
        }

        // Read features in bulk (bytemuck fast path on LE; safe fallback on BE)
        let mut features: Vec<u32> = vec![0u32; n_features];
        #[cfg(target_endian = "little")]
        {
            use bytemuck::cast_slice_mut;
            r.read_exact(cast_slice_mut::<u32, u8>(&mut features))?;
        }
        #[cfg(target_endian = "big")]
        {
            let mut buf = vec![0u8; n_features * 4];
            r.read_exact(&mut buf)?;
            for (dst, chunk) in features.iter_mut().zip(buf.chunks_exact(4)) {
                *dst = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            }
        }
        #[cfg(debug_assertions)]
        {
            let max_dim = (SHOGI_BOARD_SIZE * FE_END) as u32;
            debug_assert!(features.iter().all(|&f| f < max_dim), "feature index OOB");
        }

        // Read label
        let mut lb = [0u8; 4];
        r.read_exact(&mut lb)?;
        let label = f32::from_le_bytes(lb);

        // Read metadata: gap, depth, seldepth, flags, padding
        let mut gapb = [0u8; 2];
        r.read_exact(&mut gapb)?;
        let gap = u16::from_le_bytes(gapb);

        let mut depth = [0u8; 1];
        r.read_exact(&mut depth)?;
        let depth = depth[0];

        let mut seldepth = [0u8; 1];
        r.read_exact(&mut seldepth)?;
        let seldepth = seldepth[0];

        let mut flags = [0u8; 1];
        r.read_exact(&mut flags)?;
        let flags = flags[0];
        let unknown = (flags as u32) & !flags_mask;
        if unknown != 0 {
            unknown_flag_samples += 1;
            unknown_flag_bits_accum |= unknown;
        }

        // Calculate weight using same policy as JSONL loading
        let mut weight = 1.0;

        // Gap-based weight
        weight *= (gap as f32 / GAP_WEIGHT_DIVISOR).min(1.0);

        // Exact bound weight
        let both_exact = (flags & fc_flags::BOTH_EXACT) != 0;
        weight *= if both_exact {
            1.0
        } else {
            NON_EXACT_BOUND_WEIGHT
        };

        // Mate boundary weight
        if (flags & fc_flags::MATE_BOUNDARY) != 0 {
            weight *= 0.5;
        }

        // Depth-based weight
        if seldepth < depth.saturating_add(SELECTIVE_DEPTH_MARGIN as u8) {
            weight *= SELECTIVE_DEPTH_WEIGHT;
        }

        samples.push(Sample {
            features,
            label,
            weight,
            cp: None,
            phase: None,
        });
    }

    if unknown_flag_samples > 0 {
        eprintln!(
            "Warning: {} samples contained unknown flag bits (mask=0x{:08x}, seen=0x{:08x})",
            unknown_flag_samples, flags_mask, unknown_flag_bits_accum
        );
    }

    Ok(samples)
}

#[derive(Clone, Copy)]
struct DashboardOpts {
    emit: bool,
    calib_bins_n: usize,
    do_plots: bool,
    val_is_jsonl: bool,
}

impl DashboardValKind for DashboardOpts {
    fn is_jsonl(&self) -> bool {
        self.val_is_jsonl
    }
    fn calib_bins(&self) -> usize {
        self.calib_bins_n
    }
}

struct TrainTrackers<'a> {
    best_network: &'a mut Option<Network>,
    best_val_loss: &'a mut f32,
    last_val_loss: &'a mut Option<f32>,
    best_epoch: &'a mut Option<usize>,
}

struct TrainContext<'a> {
    out_dir: &'a Path,
    save_every: Option<usize>,
    dash: DashboardOpts,
    trackers: TrainTrackers<'a>,
    structured: Option<StructuredLogger>,
    global_step: u64,
}

fn train_model(
    network: &mut Network,
    train_samples: &mut [Sample],
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    let n_samples = train_samples.len();
    let n_batches = n_samples.div_ceil(config.batch_size);
    let mut adam_state = if config.optimizer == "adam" {
        Some(AdamState::new(network))
    } else {
        None
    };

    let mut total_batches = 0;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        let mut last_lr_base = config.learning_rate;

        // Shuffle training data
        if config.shuffle {
            train_samples.shuffle(rng);
        }

        let mut total_loss = 0.0;
        let mut total_weight = 0.0;

        // Training
        let mut last_report = Instant::now();
        let mut samples_since = 0usize;
        let mut batches_since = 0usize;
        let mut zero_weight_batches = 0usize;
        for batch_idx in 0..n_batches {
            let start = batch_idx * config.batch_size;
            let end = (start + config.batch_size).min(n_samples);
            let batch = &train_samples[start..end];

            let batch_indices: Vec<usize> = (0..batch.len()).collect();
            // LR scheduling per spec #11 (epoch-based warmup, optional step/cosine decay)
            let mut lr_factor = 1.0f32;
            if config.lr_warmup_epochs > 0 {
                let e = epoch as u32;
                if e < config.lr_warmup_epochs {
                    lr_factor = ((e + 1) as f32) / (config.lr_warmup_epochs as f32);
                }
            }
            match config.lr_schedule.as_str() {
                "constant" => {}
                "step" => {
                    let step_gamma: f32 = 0.5;
                    if let Some(de) = config.lr_decay_epochs {
                        if de > 0 {
                            let k = ((epoch as u32) / de) as i32;
                            lr_factor *= step_gamma.powi(k);
                        }
                    }
                    if let Some(ds) = config.lr_decay_steps {
                        if ds > 0 {
                            let k = (ctx.global_step / ds) as i32;
                            lr_factor *= step_gamma.powi(k);
                        }
                    }
                }
                "cosine" => {
                    let mut p = 0.0f32;
                    if let Some(de) = config.lr_decay_epochs {
                        if de > 0 {
                            p = ((epoch as f32) / (de as f32)).clamp(0.0, 1.0);
                        }
                    }
                    if let Some(ds) = config.lr_decay_steps {
                        if ds > 0 {
                            p = ((ctx.global_step as f32) / (ds as f32)).clamp(0.0, 1.0);
                        }
                    }
                    lr_factor *= 0.5 * (1.0 + (std::f32::consts::PI * p).cos());
                }
                _ => {}
            }
            let lr_base = (config.learning_rate * lr_factor).max(0.0);
            let loss = train_batch_by_indices(
                network,
                batch,
                &batch_indices,
                config,
                &mut adam_state,
                lr_base,
            );
            let batch_weight: f32 = batch.iter().map(|s| s.weight).sum();
            total_loss += loss * batch_weight;
            total_weight += batch_weight;
            last_lr_base = lr_base;

            total_batches += 1;
            ctx.global_step += 1;
            samples_since += batch.len();
            batches_since += 1;
            if batch_weight == 0.0 {
                zero_weight_batches += 1;
            }

            // Periodic throughput report
            if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                && batches_since > 0
            {
                let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                let sps = samples_since as f32 / secs;
                let bps = batches_since as f32 / secs;
                let avg_bs = samples_since as f32 / batches_since as f32;
                if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                    eprintln!(
                        "[throughput] mode=inmem epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1}",
                        epoch + 1,
                        batch_idx + 1,
                        sps,
                        bps,
                        avg_bs
                    );
                } else {
                    println!(
                        "[throughput] mode=inmem epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1}",
                        epoch + 1,
                        batch_idx + 1,
                        sps,
                        bps,
                        avg_bs
                    );
                }
                if let Some(ref lg) = ctx.structured {
                    let rec = serde_json::json!({
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "phase": "train",
                        "global_step": ctx.global_step as i64,
                        "epoch": (epoch + 1) as i64,
                        "lr": lr_base as f64,
                        "train_loss": loss as f64,
                        "examples_sec": sps as f64,
                        "loader_ratio": 0.0f64,
                        "wall_time": secs as f64,
                    });
                    lg.write_json(&rec);
                }
                last_report = Instant::now();
                samples_since = 0;
                batches_since = 0;
            }

            // Save checkpoint if requested
            if let Some(interval) = ctx.save_every {
                if total_batches % interval == 0 {
                    let checkpoint_path =
                        ctx.out_dir.join(format!("checkpoint_batch_{}.fp32.bin", total_batches));
                    save_network(network, &checkpoint_path)?;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!("Saved checkpoint: {}", checkpoint_path.display());
                    } else {
                        println!("Saved checkpoint: {}", checkpoint_path.display());
                    }
                }
            }
        }

        let avg_loss = if total_weight > 0.0 {
            total_loss / total_weight
        } else {
            0.0
        };

        // Validation/metrics
        let mut val_loss = None;
        let mut val_auc: Option<f64> = None;
        let mut val_ece: Option<f64> = None;
        let mut val_wsum: Option<f64> = None;
        if let Some(val_samples) = validation_samples {
            let vl = compute_validation_loss(network, val_samples, config);
            val_loss = Some(vl);
            val_auc = compute_val_auc(network, val_samples, config);
            if ctx.dash.val_is_jsonl && config.label_type == "wdl" {
                // Build bins and write CSV/PNG
                let mut cps = Vec::with_capacity(val_samples.len());
                let mut probs = Vec::with_capacity(val_samples.len());
                let mut labels = Vec::with_capacity(val_samples.len());
                let mut wts = Vec::with_capacity(val_samples.len());
                let mut acc_buffer = vec![0.0f32; network.acc_dim];
                let mut activated_buffer = Vec::with_capacity(network.acc_dim);
                for s in val_samples.iter() {
                    if let Some(cp) = s.cp {
                        let out = network.forward_with_buffers(
                            &s.features,
                            &mut acc_buffer,
                            &mut activated_buffer,
                        );
                        let p = 1.0 / (1.0 + (-out).exp());
                        cps.push(cp);
                        probs.push(p);
                        labels.push(s.label);
                        wts.push(s.weight);
                    }
                }
                if !cps.is_empty() {
                    let bins = calibration_bins(
                        &cps,
                        &probs,
                        &labels,
                        &wts,
                        config.cp_clip,
                        ctx.dash.calib_bins_n,
                    );
                    val_ece = ece_from_bins(&bins);
                    if ctx.dash.emit {
                        // Write calibration CSV
                        let mut w = csv::Writer::from_path(
                            ctx.out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
                        )?;
                        w.write_record([
                            "bin_left",
                            "bin_right",
                            "bin_center",
                            "count",
                            "weighted_count",
                            "mean_pred",
                            "mean_label",
                        ])?;
                        for b in &bins {
                            w.write_record([
                                b.left.to_string(),
                                b.right.to_string(),
                                format!("{:.1}", b.center),
                                b.count.to_string(),
                                format!("{:.3}", b.weighted_count),
                                format!("{:.6}", b.mean_pred),
                                format!("{:.6}", b.mean_label),
                            ])?;
                        }
                        w.flush()?;
                        if ctx.dash.do_plots {
                            let points: Vec<(i32, i32, f32, f64, f64)> = bins
                                .iter()
                                .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                                .collect();
                            if let Err(e) = tools::plot::plot_calibration_png(
                                ctx.out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                                &points,
                            ) {
                                eprintln!("plot_calibration_png failed: {}", e);
                            }
                        }
                    }
                }
            }
            // Phase metrics (JSONL only)
            if ctx.dash.val_is_jsonl && ctx.dash.emit {
                // buckets per phase
                let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                #[inline]
                fn idx_of(phase: GamePhase) -> usize {
                    match phase {
                        GamePhase::Opening => 0,
                        GamePhase::MiddleGame => 1,
                        GamePhase::EndGame => 2,
                    }
                }
                let mut acc_buffer = vec![0.0f32; network.acc_dim];
                let mut activated_buffer = Vec::with_capacity(network.acc_dim);
                match config.label_type.as_str() {
                    "wdl" => {
                        for s in val_samples.iter() {
                            if let Some(ph) = s.phase {
                                let out = network.forward_with_buffers(
                                    &s.features,
                                    &mut acc_buffer,
                                    &mut activated_buffer,
                                );
                                let p = 1.0 / (1.0 + (-out).exp());
                                let b = &mut probs_buckets[idx_of(ph)];
                                b.0.push(p);
                                b.1.push(s.label);
                                b.2.push(s.weight);
                            }
                        }
                    }
                    "cp" => {
                        for s in val_samples.iter() {
                            if let Some(ph) = s.phase {
                                let out = network.forward_with_buffers(
                                    &s.features,
                                    &mut acc_buffer,
                                    &mut activated_buffer,
                                );
                                let b = &mut cp_buckets[idx_of(ph)];
                                b.0.push(out);
                                b.1.push(s.label);
                                b.2.push(s.weight);
                            }
                        }
                    }
                    _ => {}
                }
                let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(ctx.out_dir.join("phase_metrics.csv"))?,
                );
                let phases = [
                    GamePhase::Opening,
                    GamePhase::MiddleGame,
                    GamePhase::EndGame,
                ];
                for (i, ph) in phases.iter().enumerate() {
                    match config.label_type.as_str() {
                        "wdl" => {
                            let (ref probs, ref labels, ref wts) = probs_buckets[i];
                            if !probs.is_empty() {
                                let cnt = probs.len();
                                let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                if let Some(m) = binary_metrics(probs, labels, wts) {
                                    wpm.write_record([
                                        (epoch + 1).to_string(),
                                        format!("{:?}", ph),
                                        cnt.to_string(),
                                        format!("{:.3}", wsum),
                                        format!("{:.6}", m.logloss),
                                        format!("{:.6}", m.brier),
                                        format!("{:.6}", m.accuracy),
                                        String::new(),
                                        String::new(),
                                    ])?;
                                }
                            }
                        }
                        "cp" => {
                            let (ref preds, ref labels, ref wts) = cp_buckets[i];
                            if !preds.is_empty() {
                                let cnt = preds.len();
                                let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                if let Some(r) = regression_metrics(preds, labels, wts) {
                                    wpm.write_record([
                                        (epoch + 1).to_string(),
                                        format!("{:?}", ph),
                                        cnt.to_string(),
                                        format!("{:.3}", wsum),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        format!("{:.6}", r.mae),
                                        format!("{:.6}", r.mse),
                                    ])?;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                wpm.flush()?;
            }
            val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
        }

        let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
        let epoch_sps = (n_samples as f32) / epoch_secs;
        if zero_weight_batches > 0 {
            eprintln!(
                "[debug] epoch {} had {} zero-weight batches",
                epoch + 1,
                zero_weight_batches
            );
        }
        // Update best trackers
        if let Some(vl) = val_loss {
            if vl < *ctx.trackers.best_val_loss {
                *ctx.trackers.best_val_loss = vl;
                *ctx.trackers.best_network = Some(network.clone());
                *ctx.trackers.best_epoch = Some(epoch + 1);
            }
            *ctx.trackers.last_val_loss = Some(vl);
        }
        // Emit metrics.csv
        if ctx.dash.emit {
            let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(ctx.out_dir.join("metrics.csv"))?,
            );
            w.write_record([
                (epoch + 1).to_string(),
                format!("{:.6}", avg_loss),
                val_loss.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                format!("{:.3}", epoch_secs),
                format!("{:.3}", total_weight),
                val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                if Some(epoch + 1) == *ctx.trackers.best_epoch {
                    "1".into()
                } else {
                    "0".into()
                },
            ])?;
            w.flush()?;
        }
        // Structured per-epoch logs (train/val)
        if let Some(ref lg) = ctx.structured {
            let rec_train = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "phase": "train",
                "global_step": ctx.global_step as i64,
                "epoch": (epoch + 1) as i64,
                "lr": last_lr_base as f64,
                "train_loss": avg_loss as f64,
                "examples_sec": epoch_sps as f64,
                "loader_ratio": 0.0f64,
                "wall_time": epoch_secs as f64,
            });
            lg.write_json(&rec_train);
            if let Some(vl) = val_loss {
                let mut rec_val = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "val",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "val_loss": vl as f64,
                    "wall_time": epoch_secs as f64,
                });
                if let Some(a) = val_auc {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_auc".to_string(), serde_json::json!(a));
                }
                lg.write_json(&rec_val);
            }
        }
        // Console log summary
        if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
            eprintln!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} time={:.2}s sps={:.0}",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
                epoch_secs,
                epoch_sps
            );
        } else {
            println!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} time={:.2}s sps={:.0}",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
                epoch_secs,
                epoch_sps
            );
        }
    }

    Ok(())
}

fn train_model_stream_cache(
    network: &mut Network,
    cache_path: &str,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    _rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    // Use ctx fields directly in this function to avoid borrow confusion
    // If prefetch=0, run synchronous streaming in the training thread (no background worker)
    if config.prefetch_batches == 0 {
        let mut adam_state = if config.optimizer == "adam" {
            Some(AdamState::new(network))
        } else {
            None
        };
        // Open and parse header via helper
        for epoch in 0..config.epochs {
            let epoch_start = Instant::now();
            let (mut r, num_samples, flags_mask) = open_cache_payload_reader(cache_path)?;

            // Epoch loop
            let mut total_loss = 0.0f32;
            let mut total_weight = 0.0f32;
            let mut batch_count = 0usize;
            let mut total_samples_epoch = 0usize;

            let mut last_report = Instant::now();
            let mut samples_since = 0usize;
            let mut batches_since = 0usize;
            let mut read_ns_since: u128 = 0;
            let mut read_ns_epoch: u128 = 0;

            let mut loaded: u64 = 0;
            let mut last_lr_base = config.learning_rate;
            while loaded < num_samples {
                // Read up to batch_size samples synchronously
                let mut batch = Vec::with_capacity(config.batch_size);
                let t_read0 = Instant::now();
                for _ in 0..config.batch_size {
                    if loaded >= num_samples {
                        break;
                    }
                    // n_features
                    let mut nb = [0u8; 4];
                    if let Err(e) = r.read_exact(&mut nb) {
                        return Err(format!("Read error at sample {}: {}", loaded, e).into());
                    }
                    let n_features = u32::from_le_bytes(nb) as usize;
                    const MAX_FEATURES_PER_SAMPLE: usize = SHOGI_BOARD_SIZE * FE_END;
                    if n_features > MAX_FEATURES_PER_SAMPLE {
                        return Err("n_features exceeds sane limit".into());
                    }
                    let mut features: Vec<u32> = vec![0u32; n_features];
                    #[cfg(target_endian = "little")]
                    {
                        use bytemuck::cast_slice_mut;
                        if let Err(e) = r.read_exact(cast_slice_mut::<u32, u8>(&mut features)) {
                            return Err(format!("Read features failed at {}: {}", loaded, e).into());
                        }
                    }
                    #[cfg(target_endian = "big")]
                    {
                        let mut buf = vec![0u8; n_features * 4];
                        if let Err(e) = r.read_exact(&mut buf) {
                            return Err(format!("Read features failed at {}: {}", loaded, e).into());
                        }
                        for (dst, chunk) in features.iter_mut().zip(buf.chunks_exact(4)) {
                            *dst = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                        }
                    }
                    let mut lb = [0u8; 4];
                    if let Err(e) = r.read_exact(&mut lb) {
                        return Err(format!("Read label failed at {}: {}", loaded, e).into());
                    }
                    let label = f32::from_le_bytes(lb);
                    let mut gapb = [0u8; 2];
                    if let Err(e) = r.read_exact(&mut gapb) {
                        return Err(format!("Read gap failed at {}: {}", loaded, e).into());
                    }
                    let gap = u16::from_le_bytes(gapb);
                    let mut depth = [0u8; 1];
                    if let Err(e) = r.read_exact(&mut depth) {
                        return Err(format!("Read depth failed at {}: {}", loaded, e).into());
                    }
                    let depth = depth[0];
                    let mut seldepth = [0u8; 1];
                    if let Err(e) = r.read_exact(&mut seldepth) {
                        return Err(format!("Read seldepth failed at {}: {}", loaded, e).into());
                    }
                    let seldepth = seldepth[0];
                    let mut flags = [0u8; 1];
                    if let Err(e) = r.read_exact(&mut flags) {
                        return Err(format!("Read flags failed at {}: {}", loaded, e).into());
                    }
                    let flags = flags[0];
                    let _unknown = (flags as u32) & !flags_mask; // ignore warn in sync path
                    let mut weight = 1.0f32;
                    weight *= (gap as f32 / GAP_WEIGHT_DIVISOR).min(1.0);
                    let both_exact = (flags & fc_flags::BOTH_EXACT) != 0;
                    weight *= if both_exact {
                        1.0
                    } else {
                        NON_EXACT_BOUND_WEIGHT
                    };
                    if (flags & fc_flags::MATE_BOUNDARY) != 0 {
                        weight *= 0.5;
                    }
                    if seldepth < depth.saturating_add(SELECTIVE_DEPTH_MARGIN as u8) {
                        weight *= SELECTIVE_DEPTH_WEIGHT;
                    }

                    batch.push(Sample {
                        features,
                        label,
                        weight,
                        cp: None,
                        phase: None,
                    });
                    loaded += 1;
                }
                let t_read1 = Instant::now();
                let read_ns = t_read1.duration_since(t_read0).as_nanos();
                read_ns_since += read_ns;
                read_ns_epoch += read_ns;

                if batch.is_empty() {
                    break;
                }

                let indices: Vec<usize> = (0..batch.len()).collect();
                // LR scheduling
                let mut lr_factor = 1.0f32;
                if config.lr_warmup_epochs > 0 {
                    let e = epoch as u32;
                    if e < config.lr_warmup_epochs {
                        lr_factor = ((e + 1) as f32) / (config.lr_warmup_epochs as f32);
                    }
                }
                match config.lr_schedule.as_str() {
                    "constant" => {}
                    "step" => {
                        let step_gamma: f32 = 0.5;
                        if let Some(de) = config.lr_decay_epochs {
                            if de > 0 {
                                let k = ((epoch as u32) / de) as i32;
                                lr_factor *= step_gamma.powi(k);
                            }
                        }
                        if let Some(ds) = config.lr_decay_steps {
                            if ds > 0 {
                                let k = (ctx.global_step / ds) as i32;
                                lr_factor *= step_gamma.powi(k);
                            }
                        }
                    }
                    "cosine" => {
                        let mut p = 0.0f32;
                        if let Some(de) = config.lr_decay_epochs {
                            if de > 0 {
                                p = ((epoch as f32) / (de as f32)).clamp(0.0, 1.0);
                            }
                        }
                        if let Some(ds) = config.lr_decay_steps {
                            if ds > 0 {
                                p = ((ctx.global_step as f32) / (ds as f32)).clamp(0.0, 1.0);
                            }
                        }
                        lr_factor *= 0.5 * (1.0 + (std::f32::consts::PI * p).cos());
                    }
                    _ => {}
                }
                let lr_base = (config.learning_rate * lr_factor).max(0.0);
                let loss = train_batch_by_indices(
                    network,
                    &batch,
                    &indices,
                    config,
                    &mut adam_state,
                    lr_base,
                );
                let batch_weight: f32 = batch.iter().map(|s| s.weight).sum();
                total_loss += loss * batch_weight;
                total_weight += batch_weight;
                last_lr_base = lr_base;

                total_samples_epoch += batch.len();
                batch_count += 1;
                batches_since += 1;
                samples_since += batch.len();

                if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                    && batches_since > 0
                {
                    let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                    let sps = samples_since as f32 / secs;
                    let bps = batches_since as f32 / secs;
                    let avg_bs = samples_since as f32 / batches_since as f32;
                    let loader_ratio = ((read_ns_since as f64)
                        / (secs as f64 * NANOSECONDS_PER_SECOND))
                        .clamp(0.0, 1.0)
                        * PERCENTAGE_DIVISOR as f64;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!(
                            "[throughput] mode=stream-sync epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                            epoch + 1, batch_count, sps, bps, avg_bs, loader_ratio
                        );
                    } else {
                        println!(
                            "[throughput] mode=stream-sync epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                            epoch + 1, batch_count, sps, bps, avg_bs, loader_ratio
                        );
                    }
                    if let Some(ref lg) = ctx.structured {
                        let rec = serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "phase": "train",
                            "global_step": ctx.global_step as i64,
                            "epoch": (epoch + 1) as i64,
                            "lr": lr_base as f64,
                            "train_loss": loss as f64,
                            "examples_sec": sps as f64,
                            "loader_ratio": loader_ratio/100.0,
                            "wall_time": secs as f64,
                        });
                        lg.write_json(&rec);
                    }
                    last_report = Instant::now();
                    samples_since = 0;
                    batches_since = 0;
                    read_ns_since = 0;
                }
                ctx.global_step += 1;
            }

            let avg_loss = if total_weight > 0.0 {
                total_loss / total_weight
            } else {
                0.0
            };
            let has_val = validation_samples.is_some();
            let val_loss = if let Some(val_samples) = validation_samples {
                compute_validation_loss(network, val_samples, config)
            } else {
                0.0
            };
            let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
            let loader_ratio_epoch = ((read_ns_epoch as f64)
                / (epoch_secs as f64 * NANOSECONDS_PER_SECOND))
                .clamp(0.0, 1.0)
                * PERCENTAGE_DIVISOR as f64;
            let epoch_sps = (total_samples_epoch as f32) / epoch_secs;
            if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                eprintln!(
                    "Epoch {}/{}: train_loss={:.4} val_loss={:.4} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                    epoch + 1, config.epochs, avg_loss, val_loss, batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
                );
            } else {
                println!(
                    "Epoch {}/{}: train_loss={:.4} val_loss={:.4} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                    epoch + 1, config.epochs, avg_loss, val_loss, batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
                );
            }
            if let Some(ref lg) = ctx.structured {
                let rec_train = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "train",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "lr": last_lr_base as f64,
                    "train_loss": avg_loss as f64,
                    "examples_sec": epoch_sps as f64,
                    "loader_ratio": (loader_ratio_epoch/100.0) ,
                    "wall_time": epoch_secs as f64,
                });
                lg.write_json(&rec_train);
                let mut rec_val = serde_json::json!({
                    "ts": chrono::Utc::now().to_rfc3339(),
                    "phase": "val",
                    "global_step": ctx.global_step as i64,
                    "epoch": (epoch + 1) as i64,
                    "wall_time": epoch_secs as f64,
                });
                if has_val {
                    rec_val
                        .as_object_mut()
                        .unwrap()
                        .insert("val_loss".to_string(), serde_json::json!(val_loss as f64));
                }
                lg.write_json(&rec_val);
            }
        }

        return Ok(());
    }

    // Async streaming loader path
    // Optionally cap prefetch by bytes (rough estimate)
    let mut effective_prefetch = config.prefetch_batches.max(1);
    if let Some(bytes_cap) = config.prefetch_bytes.filter(|&b| b > 0) {
        // Estimate per-sample bytes: header/meta (~32B) + 4B * estimated_features
        let est_sample_bytes =
            32usize.saturating_add(4usize.saturating_mul(config.estimated_features_per_sample));
        let est_batch_bytes = config.batch_size.saturating_mul(est_sample_bytes);
        if est_batch_bytes > 0 {
            let max_batches = (bytes_cap / est_batch_bytes).max(1);
            if effective_prefetch > max_batches {
                if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                    eprintln!(
                        "Capping prefetch-batches from {} to {} by --prefetch-bytes={} (~{} bytes/batch; est_feats/sample={})",
                        effective_prefetch, max_batches, bytes_cap, est_batch_bytes, config.estimated_features_per_sample
                    );
                } else {
                    println!(
                        "Capping prefetch-batches from {} to {} by --prefetch-bytes={} (~{} bytes/batch; est_feats/sample={})",
                        effective_prefetch, max_batches, bytes_cap, est_batch_bytes, config.estimated_features_per_sample
                    );
                }
                effective_prefetch = max_batches;
            }
        }
    }
    let mut loader =
        StreamCacheLoader::new(cache_path.to_string(), config.batch_size, effective_prefetch);
    let mut adam_state = if config.optimizer == "adam" {
        Some(AdamState::new(network))
    } else {
        None
    };

    let mut total_batches = 0usize;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        loader.start_epoch()?;

        let mut total_loss = 0.0f32;
        let mut total_weight = 0.0f32;
        let mut batch_count = 0usize;
        let mut total_samples_epoch = 0usize;

        let mut last_report = Instant::now();
        let mut samples_since = 0usize;
        let mut batches_since = 0usize;
        let mut wait_ns_since: u128 = 0;
        let mut wait_ns_epoch: u128 = 0;

        let mut last_lr_base = config.learning_rate;
        loop {
            let (maybe_batch, wait_dur) = loader.next_batch_with_wait();
            let Some(batch_res) = maybe_batch else { break };
            let batch = match batch_res {
                Ok(b) => b,
                Err(msg) => return Err(msg.into()),
            };
            let indices: Vec<usize> = (0..batch.len()).collect();
            // LR scheduling
            let mut lr_factor = 1.0f32;
            if config.lr_warmup_epochs > 0 {
                let e = epoch as u32;
                if e < config.lr_warmup_epochs {
                    lr_factor = ((e + 1) as f32) / (config.lr_warmup_epochs as f32);
                }
            }
            match config.lr_schedule.as_str() {
                "constant" => {}
                "step" => {
                    let step_gamma: f32 = 0.5;
                    if let Some(de) = config.lr_decay_epochs {
                        if de > 0 {
                            let k = ((epoch as u32) / de) as i32;
                            lr_factor *= step_gamma.powi(k);
                        }
                    }
                    if let Some(ds) = config.lr_decay_steps {
                        if ds > 0 {
                            let k = (ctx.global_step / ds) as i32;
                            lr_factor *= step_gamma.powi(k);
                        }
                    }
                }
                "cosine" => {
                    let mut p = 0.0f32;
                    if let Some(de) = config.lr_decay_epochs {
                        if de > 0 {
                            p = ((epoch as f32) / (de as f32)).clamp(0.0, 1.0);
                        }
                    }
                    if let Some(ds) = config.lr_decay_steps {
                        if ds > 0 {
                            p = ((ctx.global_step as f32) / (ds as f32)).clamp(0.0, 1.0);
                        }
                    }
                    lr_factor *= 0.5 * (1.0 + (std::f32::consts::PI * p).cos());
                }
                _ => {}
            }
            let lr_base = (config.learning_rate * lr_factor).max(0.0);
            let loss =
                train_batch_by_indices(network, &batch, &indices, config, &mut adam_state, lr_base);
            let batch_weight: f32 = batch.iter().map(|s| s.weight).sum();
            total_loss += loss * batch_weight;
            total_weight += batch_weight;
            last_lr_base = lr_base;

            total_samples_epoch += batch.len();
            batch_count += 1;
            total_batches += 1;
            samples_since += batch.len();
            batches_since += 1;
            wait_ns_since += wait_dur.as_nanos();
            wait_ns_epoch += wait_dur.as_nanos();

            if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                && batches_since > 0
            {
                let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                let sps = samples_since as f32 / secs;
                let bps = batches_since as f32 / secs;
                let avg_bs = samples_since as f32 / batches_since as f32;
                let wait_secs = (wait_ns_since as f64) / NANOSECONDS_PER_SECOND;
                let loader_ratio = if secs > 0.0 {
                    (wait_secs / secs as f64) * PERCENTAGE_DIVISOR as f64
                } else {
                    0.0
                };
                if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                    eprintln!(
                    "[throughput] mode=stream epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                    epoch + 1,
                    batch_count,
                    sps,
                    bps,
                    avg_bs,
                    loader_ratio
                    );
                } else {
                    println!(
                    "[throughput] mode=stream epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                    epoch + 1,
                    batch_count,
                    sps,
                    bps,
                    avg_bs,
                    loader_ratio
                    );
                }
                if let Some(ref lg) = ctx.structured {
                    let rec = serde_json::json!({
                        "ts": chrono::Utc::now().to_rfc3339(),
                        "phase": "train",
                        "global_step": ctx.global_step as i64,
                        "epoch": (epoch + 1) as i64,
                        "lr": lr_base as f64,
                        "train_loss": loss as f64,
                        "examples_sec": sps as f64,
                        "loader_ratio": (loader_ratio )/100.0,
                        "wall_time": secs as f64,
                    });
                    lg.write_json(&rec);
                }
                last_report = Instant::now();
                samples_since = 0;
                batches_since = 0;
                wait_ns_since = 0;
            }
            ctx.global_step += 1;

            if let Some(interval) = ctx.save_every {
                if total_batches % interval == 0 {
                    let checkpoint_path =
                        ctx.out_dir.join(format!("checkpoint_batch_{total_batches}.fp32.bin"));
                    save_network(network, &checkpoint_path)?;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!("Saved checkpoint: {}", checkpoint_path.display());
                    } else {
                        println!("Saved checkpoint: {}", checkpoint_path.display());
                    }
                }
            }
        }

        let avg_loss = if total_weight > 0.0 {
            total_loss / total_weight
        } else {
            0.0
        };
        let mut val_loss = None;
        let mut val_auc: Option<f64> = None;
        let mut val_ece: Option<f64> = None;
        let mut val_wsum: Option<f64> = None;
        if let Some(val_samples) = validation_samples {
            let vl = compute_validation_loss(network, val_samples, config);
            val_loss = Some(vl);
            val_auc = compute_val_auc(network, val_samples, config);
            if ctx.dash.val_is_jsonl && config.label_type == "wdl" {
                // Calibration CSV/PNG
                let mut cps = Vec::new();
                let mut probs = Vec::new();
                let mut labels = Vec::new();
                let mut wts = Vec::new();
                let mut acc_buffer = vec![0.0f32; network.acc_dim];
                let mut activated_buffer = Vec::with_capacity(network.acc_dim);
                for s in val_samples.iter() {
                    if let Some(cp) = s.cp {
                        let out = network.forward_with_buffers(
                            &s.features,
                            &mut acc_buffer,
                            &mut activated_buffer,
                        );
                        let p = 1.0 / (1.0 + (-out).exp());
                        cps.push(cp);
                        probs.push(p);
                        labels.push(s.label);
                        wts.push(s.weight);
                    }
                }
                if !cps.is_empty() {
                    let bins = calibration_bins(
                        &cps,
                        &probs,
                        &labels,
                        &wts,
                        config.cp_clip,
                        ctx.dash.calib_bins_n,
                    );
                    val_ece = ece_from_bins(&bins);
                    if ctx.dash.emit {
                        let mut w = csv::Writer::from_path(
                            ctx.out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
                        )?;
                        w.write_record([
                            "bin_left",
                            "bin_right",
                            "bin_center",
                            "count",
                            "weighted_count",
                            "mean_pred",
                            "mean_label",
                        ])?;
                        for b in &bins {
                            w.write_record([
                                b.left.to_string(),
                                b.right.to_string(),
                                format!("{:.1}", b.center),
                                b.count.to_string(),
                                format!("{:.3}", b.weighted_count),
                                format!("{:.6}", b.mean_pred),
                                format!("{:.6}", b.mean_label),
                            ])?;
                        }
                        w.flush()?;
                        if ctx.dash.do_plots {
                            let points: Vec<(i32, i32, f32, f64, f64)> = bins
                                .iter()
                                .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                                .collect();
                            if let Err(e) = tools::plot::plot_calibration_png(
                                ctx.out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                                &points,
                            ) {
                                eprintln!("plot_calibration_png failed: {}", e);
                            }
                        }
                    }
                }
            }
            // Phase metrics (JSONL only)
            if ctx.dash.val_is_jsonl && ctx.dash.emit {
                let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                #[inline]
                fn idx_of(phase: GamePhase) -> usize {
                    match phase {
                        GamePhase::Opening => 0,
                        GamePhase::MiddleGame => 1,
                        GamePhase::EndGame => 2,
                    }
                }
                let mut acc_buffer = vec![0.0f32; network.acc_dim];
                let mut activated_buffer = Vec::with_capacity(network.acc_dim);
                match config.label_type.as_str() {
                    "wdl" => {
                        for s in val_samples.iter() {
                            if let Some(ph) = s.phase {
                                let out = network.forward_with_buffers(
                                    &s.features,
                                    &mut acc_buffer,
                                    &mut activated_buffer,
                                );
                                let p = 1.0 / (1.0 + (-out).exp());
                                let b = &mut probs_buckets[idx_of(ph)];
                                b.0.push(p);
                                b.1.push(s.label);
                                b.2.push(s.weight);
                            }
                        }
                    }
                    "cp" => {
                        for s in val_samples.iter() {
                            if let Some(ph) = s.phase {
                                let out = network.forward_with_buffers(
                                    &s.features,
                                    &mut acc_buffer,
                                    &mut activated_buffer,
                                );
                                let b = &mut cp_buckets[idx_of(ph)];
                                b.0.push(out);
                                b.1.push(s.label);
                                b.2.push(s.weight);
                            }
                        }
                    }
                    _ => {}
                }
                let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(ctx.out_dir.join("phase_metrics.csv"))?,
                );
                let phases = [
                    GamePhase::Opening,
                    GamePhase::MiddleGame,
                    GamePhase::EndGame,
                ];
                for (i, ph) in phases.iter().enumerate() {
                    match config.label_type.as_str() {
                        "wdl" => {
                            let (ref probs, ref labels, ref wts) = probs_buckets[i];
                            if !probs.is_empty() {
                                let cnt = probs.len();
                                let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                if let Some(m) = binary_metrics(probs, labels, wts) {
                                    wpm.write_record([
                                        (epoch + 1).to_string(),
                                        format!("{:?}", ph),
                                        cnt.to_string(),
                                        format!("{:.3}", wsum),
                                        format!("{:.6}", m.logloss),
                                        format!("{:.6}", m.brier),
                                        format!("{:.6}", m.accuracy),
                                        String::new(),
                                        String::new(),
                                    ])?;
                                }
                            }
                        }
                        "cp" => {
                            let (ref preds, ref labels, ref wts) = cp_buckets[i];
                            if !preds.is_empty() {
                                let cnt = preds.len();
                                let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                if let Some(r) = regression_metrics(preds, labels, wts) {
                                    wpm.write_record([
                                        (epoch + 1).to_string(),
                                        format!("{:?}", ph),
                                        cnt.to_string(),
                                        format!("{:.3}", wsum),
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        format!("{:.6}", r.mae),
                                        format!("{:.6}", r.mse),
                                    ])?;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                wpm.flush()?;
            }
            val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
        }
        let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
        let wait_secs_epoch = (wait_ns_epoch as f64) / NANOSECONDS_PER_SECOND;
        let loader_ratio_epoch = if epoch_secs > 0.0 {
            ((wait_secs_epoch / epoch_secs as f64) * PERCENTAGE_DIVISOR as f64) as f32
        } else {
            0.0
        };
        let epoch_sps = (total_samples_epoch as f32) / epoch_secs;
        if let Some(vl) = val_loss {
            if vl < *ctx.trackers.best_val_loss {
                *ctx.trackers.best_val_loss = vl;
                *ctx.trackers.best_network = Some(network.clone());
                *ctx.trackers.best_epoch = Some(epoch + 1);
            }
            *ctx.trackers.last_val_loss = Some(vl);
        }
        if ctx.dash.emit {
            let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(ctx.out_dir.join("metrics.csv"))?,
            );
            w.write_record([
                (epoch + 1).to_string(),
                format!("{:.6}", avg_loss),
                val_loss.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                format!("{:.3}", epoch_secs),
                format!("{:.3}", total_weight),
                val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                if Some(epoch + 1) == *ctx.trackers.best_epoch {
                    "1".into()
                } else {
                    "0".into()
                },
            ])?;
            w.flush()?;
        }
        println!(
            "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
            epoch + 1, config.epochs, avg_loss,
            val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
            batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
        );
        if let Some(ref lg) = ctx.structured {
            let rec_train = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "phase": "train",
                "global_step": ctx.global_step as i64,
                "epoch": (epoch + 1) as i64,
                "lr": last_lr_base as f64,
                "train_loss": avg_loss as f64,
                "examples_sec": epoch_sps as f64,
                "loader_ratio": (loader_ratio_epoch as f64)/100.0,
                "wall_time": epoch_secs as f64,
            });
            lg.write_json(&rec_train);
            let mut rec_val = serde_json::json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "phase": "val",
                "global_step": ctx.global_step as i64,
                "epoch": (epoch + 1) as i64,
                "wall_time": epoch_secs as f64,
            });
            if let Some(vl) = val_loss {
                rec_val
                    .as_object_mut()
                    .unwrap()
                    .insert("val_loss".to_string(), serde_json::json!(vl as f64));
            }
            if let Some(a) = val_auc {
                rec_val
                    .as_object_mut()
                    .unwrap()
                    .insert("val_auc".to_string(), serde_json::json!(a));
            }
            lg.write_json(&rec_val);
        }

        loader.finish();
    }

    Ok(())
}
fn train_model_with_loader(
    network: &mut Network,
    train_samples: Vec<Sample>,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    rng: &mut StdRng,
    ctx: &mut TrainContext,
) -> Result<(), Box<dyn std::error::Error>> {
    // Local aliases to minimize code churn (avoid double references)
    let out_dir = ctx.out_dir;
    let dash = &ctx.dash;
    let save_every = ctx.save_every;
    let best_network: &mut Option<Network> = ctx.trackers.best_network;
    let best_val_loss: &mut f32 = ctx.trackers.best_val_loss;
    let last_val_loss: &mut Option<f32> = ctx.trackers.last_val_loss;
    let best_epoch: &mut Option<usize> = ctx.trackers.best_epoch;
    let train_samples_arc = Arc::new(train_samples);
    let mut adam_state = if config.optimizer == "adam" {
        Some(AdamState::new(network))
    } else {
        None
    };

    let mut total_batches = 0;

    if config.prefetch_batches > 0 {
        // Async prefetch path
        let mut async_loader = AsyncBatchLoader::new(
            train_samples_arc.len(),
            config.batch_size,
            config.prefetch_batches,
        );

        for epoch in 0..config.epochs {
            let epoch_start = Instant::now();
            let seed: u64 = rng.random();
            async_loader.start_epoch(config.shuffle, seed);

            let mut total_loss = 0.0;
            let mut total_weight = 0.0;
            let mut batch_count = 0usize;

            let mut last_report = Instant::now();
            let mut samples_since = 0usize;
            let mut batches_since = 0usize;
            let mut wait_ns_since: u128 = 0;
            let mut wait_ns_epoch: u128 = 0;
            let mut _last_lr_base = config.learning_rate;

            loop {
                let (maybe_indices, wait_dur) = async_loader.next_batch_with_wait();
                let Some(indices) = maybe_indices else { break };
                // LR scheduling
                let mut lr_factor = 1.0f32;
                if config.lr_warmup_epochs > 0 {
                    let e = epoch as u32;
                    if e < config.lr_warmup_epochs {
                        lr_factor = ((e + 1) as f32) / (config.lr_warmup_epochs as f32);
                    }
                }
                match config.lr_schedule.as_str() {
                    "constant" => {}
                    "step" => {
                        let step_gamma: f32 = 0.5;
                        if let Some(de) = config.lr_decay_epochs {
                            if de > 0 {
                                let k = ((epoch as u32) / de) as i32;
                                lr_factor *= step_gamma.powi(k);
                            }
                        }
                        if let Some(ds) = config.lr_decay_steps {
                            if ds > 0 {
                                let k = (ctx.global_step / ds) as i32;
                                lr_factor *= step_gamma.powi(k);
                            }
                        }
                    }
                    "cosine" => {
                        let mut p = 0.0f32;
                        if let Some(de) = config.lr_decay_epochs {
                            if de > 0 {
                                p = ((epoch as f32) / (de as f32)).clamp(0.0, 1.0);
                            }
                        }
                        if let Some(ds) = config.lr_decay_steps {
                            if ds > 0 {
                                p = ((ctx.global_step as f32) / (ds as f32)).clamp(0.0, 1.0);
                            }
                        }
                        lr_factor *= 0.5 * (1.0 + (std::f32::consts::PI * p).cos());
                    }
                    _ => {}
                }
                let lr_base = (config.learning_rate * lr_factor).max(0.0);
                let loss = train_batch_by_indices(
                    network,
                    &train_samples_arc,
                    &indices,
                    config,
                    &mut adam_state,
                    lr_base,
                );
                let batch_weight: f32 =
                    indices.iter().map(|&idx| train_samples_arc[idx].weight).sum();
                total_loss += loss * batch_weight;
                total_weight += batch_weight;

                let batch_len = indices.len();
                batch_count += 1;
                total_batches += 1;
                samples_since += batch_len;
                batches_since += 1;
                wait_ns_since += wait_dur.as_nanos();
                wait_ns_epoch += wait_dur.as_nanos();
                // Approximate compute time as time taken by train_batch (dominant)
                // Note: train_batch_by_indices already executed; we estimate by subtracting wait from interval wall time on print, but here we track per-batch compute as 0.
                // Instead, measure explicitly around forward+backward: do a local timing.
                // For minimal invasiveness, we cannot re-run compute; so we estimate compute time using throughput interval wall time at print.

                // Periodic throughput report
                if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                    && batches_since > 0
                {
                    let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                    let sps = samples_since as f32 / secs;
                    let bps = batches_since as f32 / secs;
                    let avg_bs = samples_since as f32 / batches_since as f32;
                    // compute_ns_since is not directly tracked; approximate as (secs - wait) * 1e9
                    let wait_secs = (wait_ns_since as f64) / NANOSECONDS_PER_SECOND;
                    let loader_ratio = if secs > 0.0 {
                        (wait_secs / secs as f64) * PERCENTAGE_DIVISOR as f64
                    } else {
                        0.0
                    };
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!(
                        "[throughput] mode=inmem loader=async epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                        epoch + 1,
                        batch_count,
                        sps,
                        bps,
                        avg_bs,
                        loader_ratio
                        );
                    } else {
                        println!(
                        "[throughput] mode=inmem loader=async epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio={:.1}%",
                        epoch + 1,
                        batch_count,
                        sps,
                        bps,
                        avg_bs,
                        loader_ratio
                        );
                    }
                    if let Some(ref lg) = ctx.structured {
                        let rec = serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "phase": "train",
                            "global_step": ctx.global_step as i64,
                            "epoch": (epoch + 1) as i64,
                            "lr": lr_base as f64,
                            "train_loss": loss as f64,
                            "examples_sec": sps as f64,
                            "loader_ratio": (loader_ratio )/100.0,
                            "wall_time": secs as f64,
                        });
                        lg.write_json(&rec);
                    }
                    last_report = Instant::now();
                    samples_since = 0;
                    batches_since = 0;
                    wait_ns_since = 0;
                }

                // Save checkpoint if requested
                if let Some(interval) = save_every {
                    if total_batches % interval == 0 {
                        let checkpoint_path =
                            out_dir.join(format!("checkpoint_batch_{total_batches}.fp32.bin"));
                        save_network(network, &checkpoint_path)?;
                        println!("Saved checkpoint: {}", checkpoint_path.display());
                    }
                }
                ctx.global_step += 1;
            }

            let avg_loss = if total_weight > 0.0 {
                total_loss / total_weight
            } else {
                0.0
            };
            let mut val_loss = None;
            let mut val_auc: Option<f64> = None;
            let mut val_ece: Option<f64> = None;
            let mut val_wsum: Option<f64> = None;
            if let Some(val_samples) = validation_samples {
                let vl = compute_validation_loss(network, val_samples, config);
                val_loss = Some(vl);
                val_auc = compute_val_auc(network, val_samples, config);
                if dash.val_is_jsonl && config.label_type == "wdl" {
                    let mut cps = Vec::new();
                    let mut probs = Vec::new();
                    let mut labels = Vec::new();
                    let mut wts = Vec::new();
                    let mut acc_buffer = vec![0.0f32; network.acc_dim];
                    let mut activated_buffer = Vec::with_capacity(network.acc_dim);
                    for s in val_samples.iter() {
                        if let Some(cp) = s.cp {
                            let out = network.forward_with_buffers(
                                &s.features,
                                &mut acc_buffer,
                                &mut activated_buffer,
                            );
                            let p = 1.0 / (1.0 + (-out).exp());
                            cps.push(cp);
                            probs.push(p);
                            labels.push(s.label);
                            wts.push(s.weight);
                        }
                    }
                    if !cps.is_empty() {
                        let bins = calibration_bins(
                            &cps,
                            &probs,
                            &labels,
                            &wts,
                            config.cp_clip,
                            dash.calib_bins_n,
                        );
                        val_ece = ece_from_bins(&bins);
                        if dash.emit {
                            let mut w = csv::Writer::from_path(
                                out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
                            )?;
                            w.write_record([
                                "bin_left",
                                "bin_right",
                                "bin_center",
                                "count",
                                "weighted_count",
                                "mean_pred",
                                "mean_label",
                            ])?;
                            for b in &bins {
                                w.write_record([
                                    b.left.to_string(),
                                    b.right.to_string(),
                                    format!("{:.1}", b.center),
                                    b.count.to_string(),
                                    format!("{:.3}", b.weighted_count),
                                    format!("{:.6}", b.mean_pred),
                                    format!("{:.6}", b.mean_label),
                                ])?;
                            }
                            w.flush()?;
                            if dash.do_plots {
                                let points: Vec<(i32, i32, f32, f64, f64)> = bins
                                    .iter()
                                    .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                                    .collect();
                                if let Err(e) = tools::plot::plot_calibration_png(
                                    out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                                    &points,
                                ) {
                                    eprintln!("plot_calibration_png failed: {}", e);
                                }
                            }
                        }
                    }
                }
                // Phase metrics
                if ctx.dash.val_is_jsonl && ctx.dash.emit {
                    let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                    let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                    #[inline]
                    fn idx_of(phase: GamePhase) -> usize {
                        match phase {
                            GamePhase::Opening => 0,
                            GamePhase::MiddleGame => 1,
                            GamePhase::EndGame => 2,
                        }
                    }
                    let mut acc_buffer = vec![0.0f32; network.acc_dim];
                    let mut activated_buffer = Vec::with_capacity(network.acc_dim);
                    match config.label_type.as_str() {
                        "wdl" => {
                            for s in val_samples.iter() {
                                if let Some(ph) = s.phase {
                                    let out = network.forward_with_buffers(
                                        &s.features,
                                        &mut acc_buffer,
                                        &mut activated_buffer,
                                    );
                                    let p = 1.0 / (1.0 + (-out).exp());
                                    let b = &mut probs_buckets[idx_of(ph)];
                                    b.0.push(p);
                                    b.1.push(s.label);
                                    b.2.push(s.weight);
                                }
                            }
                        }
                        "cp" => {
                            for s in val_samples.iter() {
                                if let Some(ph) = s.phase {
                                    let out = network.forward_with_buffers(
                                        &s.features,
                                        &mut acc_buffer,
                                        &mut activated_buffer,
                                    );
                                    let b = &mut cp_buckets[idx_of(ph)];
                                    b.0.push(out);
                                    b.1.push(s.label);
                                    b.2.push(s.weight);
                                }
                            }
                        }
                        _ => {}
                    }
                    let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(ctx.out_dir.join("phase_metrics.csv"))?,
                    );
                    let phases = [
                        GamePhase::Opening,
                        GamePhase::MiddleGame,
                        GamePhase::EndGame,
                    ];
                    for (i, ph) in phases.iter().enumerate() {
                        match config.label_type.as_str() {
                            "wdl" => {
                                let (ref probs, ref labels, ref wts) = probs_buckets[i];
                                if !probs.is_empty() {
                                    let cnt = probs.len();
                                    let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                    if let Some(m) = binary_metrics(probs, labels, wts) {
                                        wpm.write_record([
                                            (epoch + 1).to_string(),
                                            format!("{:?}", ph),
                                            cnt.to_string(),
                                            format!("{:.3}", wsum),
                                            format!("{:.6}", m.logloss),
                                            format!("{:.6}", m.brier),
                                            format!("{:.6}", m.accuracy),
                                            String::new(),
                                            String::new(),
                                        ])?;
                                    }
                                }
                            }
                            "cp" => {
                                let (ref preds, ref labels, ref wts) = cp_buckets[i];
                                if !preds.is_empty() {
                                    let cnt = preds.len();
                                    let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                    if let Some(r) = regression_metrics(preds, labels, wts) {
                                        wpm.write_record([
                                            (epoch + 1).to_string(),
                                            format!("{:?}", ph),
                                            cnt.to_string(),
                                            format!("{:.3}", wsum),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            format!("{:.6}", r.mae),
                                            format!("{:.6}", r.mse),
                                        ])?;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    wpm.flush()?;
                }
                val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
            }

            let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
            let loader_ratio_epoch = if epoch_secs > 0.0 {
                let wait_secs = (wait_ns_epoch as f64) / NANOSECONDS_PER_SECOND;
                ((wait_secs / epoch_secs as f64) * PERCENTAGE_DIVISOR as f64) as f32
            } else {
                0.0
            };
            let epoch_sps = (train_samples_arc.len() as f32) / epoch_secs;
            if let Some(vl) = val_loss {
                if vl < *best_val_loss {
                    *best_val_loss = vl;
                    *best_network = Some(network.clone());
                    *best_epoch = Some(epoch + 1);
                }
                *last_val_loss = Some(vl);
            }
            if dash.emit {
                let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(out_dir.join("metrics.csv"))?,
                );
                w.write_record([
                    (epoch + 1).to_string(),
                    format!("{:.6}", avg_loss),
                    val_loss.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    format!("{:.3}", epoch_secs),
                    format!("{:.3}", total_weight),
                    val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                    if Some(epoch + 1) == *best_epoch {
                        "1".into()
                    } else {
                        "0".into()
                    },
                ])?;
                w.flush()?;
            }
            if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                eprintln!(
                    "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                    epoch + 1, config.epochs, avg_loss, val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()), batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
                );
            } else {
                println!(
                    "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio={:.1}%",
                    epoch + 1, config.epochs, avg_loss, val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()), batch_count, epoch_secs, epoch_sps, loader_ratio_epoch
                );
            }
        }
        // Ensure worker fully finished at end
        async_loader.finish();
    } else {
        // Original synchronous loader path (still with throughput reporting)
        let mut batch_loader =
            BatchLoader::new(train_samples_arc.len(), config.batch_size, config.shuffle, rng);

        for epoch in 0..config.epochs {
            let epoch_start = Instant::now();
            batch_loader.reset(config.shuffle, rng);

            let mut total_loss = 0.0;
            let mut total_weight = 0.0;
            let mut batch_count = 0usize;

            let mut last_report = Instant::now();
            let mut samples_since = 0usize;
            let mut batches_since = 0usize;
            let _last_lr_base = config.learning_rate;

            while let Some(indices) = {
                let t0 = Instant::now();
                let next = batch_loader.next_batch();
                // We treat the time spent fetching indices as loader time in sync path
                let _wait = t0.elapsed();
                if let Some(ref _idxs) = next {
                    // Accumulate local variables by capturing outer mutable state via closures is cumbersome here.
                    // We will measure throughput window at print time similar to async path.
                }
                next
            } {
                // LR scheduling
                let mut lr_factor = 1.0f32;
                if config.lr_warmup_epochs > 0 {
                    let e = epoch as u32;
                    if e < config.lr_warmup_epochs {
                        lr_factor = ((e + 1) as f32) / (config.lr_warmup_epochs as f32);
                    }
                }
                match config.lr_schedule.as_str() {
                    "constant" => {}
                    "step" => {
                        let step_gamma: f32 = 0.5;
                        if let Some(de) = config.lr_decay_epochs {
                            if de > 0 {
                                let k = ((epoch as u32) / de) as i32;
                                lr_factor *= step_gamma.powi(k);
                            }
                        }
                        if let Some(ds) = config.lr_decay_steps {
                            if ds > 0 {
                                let k = (ctx.global_step / ds) as i32;
                                lr_factor *= step_gamma.powi(k);
                            }
                        }
                    }
                    "cosine" => {
                        let mut p = 0.0f32;
                        if let Some(de) = config.lr_decay_epochs {
                            if de > 0 {
                                p = ((epoch as f32) / (de as f32)).clamp(0.0, 1.0);
                            }
                        }
                        if let Some(ds) = config.lr_decay_steps {
                            if ds > 0 {
                                p = ((ctx.global_step as f32) / (ds as f32)).clamp(0.0, 1.0);
                            }
                        }
                        lr_factor *= 0.5 * (1.0 + (std::f32::consts::PI * p).cos());
                    }
                    _ => {}
                }
                let lr_base = (config.learning_rate * lr_factor).max(0.0);
                let loss = train_batch_by_indices(
                    network,
                    &train_samples_arc,
                    &indices,
                    config,
                    &mut adam_state,
                    lr_base,
                );
                let batch_weight: f32 =
                    indices.iter().map(|&idx| train_samples_arc[idx].weight).sum();
                total_loss += loss * batch_weight;
                total_weight += batch_weight;

                let batch_len = indices.len();
                batch_count += 1;
                total_batches += 1;
                samples_since += batch_len;
                batches_since += 1;

                if last_report.elapsed().as_secs_f32() >= config.throughput_interval_sec
                    && batches_since > 0
                {
                    let secs = last_report.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
                    let sps = samples_since as f32 / secs;
                    let bps = batches_since as f32 / secs;
                    let avg_bs = samples_since as f32 / batches_since as f32;
                    if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                        eprintln!(
                        "[throughput] mode=inmem loader=sync epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio=~0.0%",
                        epoch + 1,
                        batch_count,
                        sps,
                        bps,
                        avg_bs
                        );
                    } else {
                        println!(
                        "[throughput] mode=inmem loader=sync epoch={} batches={} sps={:.0} bps={:.2} avg_batch={:.1} loader_ratio=~0.0%",
                        epoch + 1,
                        batch_count,
                        sps,
                        bps,
                        avg_bs
                        );
                    }
                    if let Some(ref lg) = ctx.structured {
                        let rec = serde_json::json!({
                            "ts": chrono::Utc::now().to_rfc3339(),
                            "phase": "train",
                            "global_step": ctx.global_step as i64,
                            "epoch": (epoch + 1) as i64,
                            "lr": lr_base as f64,
                            "train_loss": loss as f64,
                            "examples_sec": sps as f64,
                            "loader_ratio": 0.0f64,
                            "wall_time": secs as f64,
                        });
                        lg.write_json(&rec);
                    }
                    last_report = Instant::now();
                    samples_since = 0;
                    batches_since = 0;
                }

                // Save checkpoint if requested
                if let Some(interval) = save_every {
                    if total_batches % interval == 0 {
                        let checkpoint_path =
                            out_dir.join(format!("checkpoint_batch_{total_batches}.fp32.bin"));
                        save_network(network, &checkpoint_path)?;
                        if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                            eprintln!("Saved checkpoint: {}", checkpoint_path.display());
                        } else {
                            println!("Saved checkpoint: {}", checkpoint_path.display());
                        }
                    }
                }
                // advance global step per batch
                ctx.global_step += 1;
                // record last lr used (unused in sync loader path)
            }

            let avg_loss = if total_weight > 0.0 {
                total_loss / total_weight
            } else {
                0.0
            };
            let mut val_loss = None;
            let mut val_auc: Option<f64> = None;
            let mut val_ece: Option<f64> = None;
            let mut val_wsum: Option<f64> = None;
            if let Some(val_samples) = validation_samples {
                let vl = compute_validation_loss(network, val_samples, config);
                val_loss = Some(vl);
                val_auc = compute_val_auc(network, val_samples, config);
                if dash.val_is_jsonl && config.label_type == "wdl" {
                    let mut cps = Vec::new();
                    let mut probs = Vec::new();
                    let mut labels = Vec::new();
                    let mut wts = Vec::new();
                    let mut acc_buffer = vec![0.0f32; network.acc_dim];
                    let mut activated_buffer = Vec::with_capacity(network.acc_dim);
                    for s in val_samples.iter() {
                        if let Some(cp) = s.cp {
                            let out = network.forward_with_buffers(
                                &s.features,
                                &mut acc_buffer,
                                &mut activated_buffer,
                            );
                            let p = 1.0 / (1.0 + (-out).exp());
                            cps.push(cp);
                            probs.push(p);
                            labels.push(s.label);
                            wts.push(s.weight);
                        }
                    }
                    if !cps.is_empty() {
                        let bins = calibration_bins(
                            &cps,
                            &probs,
                            &labels,
                            &wts,
                            config.cp_clip,
                            dash.calib_bins_n,
                        );
                        val_ece = ece_from_bins(&bins);
                        if dash.emit {
                            let mut w = csv::Writer::from_path(
                                out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
                            )?;
                            w.write_record([
                                "bin_left",
                                "bin_right",
                                "bin_center",
                                "count",
                                "weighted_count",
                                "mean_pred",
                                "mean_label",
                            ])?;
                            for b in &bins {
                                w.write_record([
                                    b.left.to_string(),
                                    b.right.to_string(),
                                    format!("{:.1}", b.center),
                                    b.count.to_string(),
                                    format!("{:.3}", b.weighted_count),
                                    format!("{:.6}", b.mean_pred),
                                    format!("{:.6}", b.mean_label),
                                ])?;
                            }
                            w.flush()?;
                            if dash.do_plots {
                                let points: Vec<(i32, i32, f32, f64, f64)> = bins
                                    .iter()
                                    .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                                    .collect();
                                if let Err(e) = tools::plot::plot_calibration_png(
                                    out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                                    &points,
                                ) {
                                    eprintln!("plot_calibration_png failed: {}", e);
                                }
                            }
                        }
                    }
                }
                // Phase metrics
                if dash.val_is_jsonl && dash.emit {
                    let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                    let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
                    #[inline]
                    fn idx_of(phase: GamePhase) -> usize {
                        match phase {
                            GamePhase::Opening => 0,
                            GamePhase::MiddleGame => 1,
                            GamePhase::EndGame => 2,
                        }
                    }
                    let mut acc_buffer = vec![0.0f32; network.acc_dim];
                    let mut activated_buffer = Vec::with_capacity(network.acc_dim);
                    match config.label_type.as_str() {
                        "wdl" => {
                            for s in val_samples.iter() {
                                if let Some(ph) = s.phase {
                                    let out = network.forward_with_buffers(
                                        &s.features,
                                        &mut acc_buffer,
                                        &mut activated_buffer,
                                    );
                                    let p = 1.0 / (1.0 + (-out).exp());
                                    let b = &mut probs_buckets[idx_of(ph)];
                                    b.0.push(p);
                                    b.1.push(s.label);
                                    b.2.push(s.weight);
                                }
                            }
                        }
                        "cp" => {
                            for s in val_samples.iter() {
                                if let Some(ph) = s.phase {
                                    let out = network.forward_with_buffers(
                                        &s.features,
                                        &mut acc_buffer,
                                        &mut activated_buffer,
                                    );
                                    let b = &mut cp_buckets[idx_of(ph)];
                                    b.0.push(out);
                                    b.1.push(s.label);
                                    b.2.push(s.weight);
                                }
                            }
                        }
                        _ => {}
                    }
                    let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                        std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(out_dir.join("phase_metrics.csv"))?,
                    );
                    let phases = [
                        GamePhase::Opening,
                        GamePhase::MiddleGame,
                        GamePhase::EndGame,
                    ];
                    for (i, ph) in phases.iter().enumerate() {
                        match config.label_type.as_str() {
                            "wdl" => {
                                let (ref probs, ref labels, ref wts) = probs_buckets[i];
                                if !probs.is_empty() {
                                    let cnt = probs.len();
                                    let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                    if let Some(m) = binary_metrics(probs, labels, wts) {
                                        wpm.write_record([
                                            (epoch + 1).to_string(),
                                            format!("{:?}", ph),
                                            cnt.to_string(),
                                            format!("{:.3}", wsum),
                                            format!("{:.6}", m.logloss),
                                            format!("{:.6}", m.brier),
                                            format!("{:.6}", m.accuracy),
                                            String::new(),
                                            String::new(),
                                        ])?;
                                    }
                                }
                            }
                            "cp" => {
                                let (ref preds, ref labels, ref wts) = cp_buckets[i];
                                if !preds.is_empty() {
                                    let cnt = preds.len();
                                    let wsum: f64 = wts.iter().map(|&x| x as f64).sum();
                                    if let Some(r) = regression_metrics(preds, labels, wts) {
                                        wpm.write_record([
                                            (epoch + 1).to_string(),
                                            format!("{:?}", ph),
                                            cnt.to_string(),
                                            format!("{:.3}", wsum),
                                            String::new(),
                                            String::new(),
                                            String::new(),
                                            format!("{:.6}", r.mae),
                                            format!("{:.6}", r.mse),
                                        ])?;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    wpm.flush()?;
                }
                val_wsum = Some(val_samples.iter().map(|s| s.weight as f64).sum());
            }

            let epoch_secs = epoch_start.elapsed().as_secs_f32().max(MIN_ELAPSED_TIME as f32);
            let epoch_sps = (train_samples_arc.len() as f32) / epoch_secs;
            if let Some(vl) = val_loss {
                if vl < *best_val_loss {
                    *best_val_loss = vl;
                    *best_network = Some(network.clone());
                    *best_epoch = Some(epoch + 1);
                }
                *last_val_loss = Some(vl);
            }
            if dash.emit {
                let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                    std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(out_dir.join("metrics.csv"))?,
                );
                w.write_record([
                    (epoch + 1).to_string(),
                    format!("{:.6}", avg_loss),
                    val_loss.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_auc.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    val_ece.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                    format!("{:.3}", epoch_secs),
                    format!("{:.3}", total_weight),
                    val_wsum.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                    if Some(epoch + 1) == *best_epoch {
                        "1".into()
                    } else {
                        "0".into()
                    },
                ])?;
                w.flush()?;
            }
            if ctx.structured.as_ref().map(|lg| lg.to_stdout).unwrap_or(false) {
                eprintln!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio=~0.0%",
                epoch + 1, config.epochs, avg_loss, val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()), batch_count, epoch_secs, epoch_sps
                );
            } else {
                println!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} batches={} time={:.2}s sps={:.0} loader_ratio=~0.0%",
                epoch + 1, config.epochs, avg_loss, val_loss.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()), batch_count, epoch_secs, epoch_sps
                );
            }
        }
    }

    Ok(())
}

fn train_batch_by_indices(
    network: &mut Network,
    samples: &[Sample],
    indices: &[usize],
    config: &Config,
    adam_state: &mut Option<AdamState>,
    lr_base: f32,
) -> f32 {
    let mut total_loss = 0.0;
    let mut total_weight = 0.0;

    // Gradients for output layer (accumulated over batch)
    let mut grad_w2 = vec![0.0f32; network.w2.len()];
    let mut grad_b2 = 0.0f32;

    // Pre-allocate buffers for forward pass
    let mut acc_buffer = vec![0.0f32; network.acc_dim];
    let mut activated_buffer = vec![0.0f32; network.acc_dim];

    // Pre-compute learning rate for Adam
    let lr_t = if let Some(adam) = adam_state.as_mut() {
        adam.t += 1;
        let t = adam.t as f32;
        lr_base * (1.0 - adam.beta2.powf(t)).sqrt() / (1.0 - adam.beta1.powf(t))
    } else {
        lr_base
    };

    for &idx in indices {
        let sample = &samples[idx];
        // Forward pass with reused buffers
        let output =
            forward_into(network, &sample.features, &mut acc_buffer, &mut activated_buffer);

        // Compute loss and output gradient using numerically stable BCE
        let (loss, grad_output) = match config.label_type.as_str() {
            "wdl" => {
                let (l, g) = bce_with_logits(output, sample.label);
                (l * sample.weight, g * sample.weight)
            }
            "cp" => {
                let error = output - sample.label;
                (0.5 * error * error * sample.weight, error * sample.weight)
            }
            _ => unreachable!(),
        };

        total_loss += loss;
        total_weight += sample.weight;

        // Accumulate output layer gradients
        grad_b2 += grad_output;
        for i in 0..network.acc_dim {
            grad_w2[i] += grad_output * activated_buffer[i];
        }

        // Immediate update for input layer (row-sparse)
        if let Some(adam) = adam_state.as_mut() {
            // Adam updates
            for (i, &act_val) in activated_buffer.iter().enumerate() {
                // Check if neuron is in linear region (ReLU derivative)
                if act_val <= 0.0 || act_val >= network.relu_clip {
                    continue;
                }

                let grad_act = grad_output * network.w2[i];

                // Update bias b0[i]
                let grad_b = grad_act;
                adam.m_b0[i] = adam.beta1 * adam.m_b0[i] + (1.0 - adam.beta1) * grad_b;
                adam.v_b0[i] = adam.beta2 * adam.v_b0[i] + (1.0 - adam.beta2) * grad_b * grad_b;
                network.b0[i] -= lr_t * adam.m_b0[i] / (adam.v_b0[i].sqrt() + adam.epsilon);

                // Update weights w0 for active features only
                for &feat_idx in &sample.features {
                    let idx = feat_idx as usize * network.acc_dim + i;
                    let grad_w = grad_act + config.l2_reg * network.w0[idx];

                    adam.m_w0[idx] = adam.beta1 * adam.m_w0[idx] + (1.0 - adam.beta1) * grad_w;
                    adam.v_w0[idx] =
                        adam.beta2 * adam.v_w0[idx] + (1.0 - adam.beta2) * grad_w * grad_w;
                    network.w0[idx] -=
                        lr_t * adam.m_w0[idx] / (adam.v_w0[idx].sqrt() + adam.epsilon);
                }
            }
        } else {
            // SGD updates
            for (i, &act_val) in activated_buffer.iter().enumerate() {
                // Check if neuron is in linear region
                if act_val <= 0.0 || act_val >= network.relu_clip {
                    continue;
                }

                let grad_act = grad_output * network.w2[i];

                // Update bias
                network.b0[i] -= lr_t * grad_act;

                // Update weights for active features
                for &feat_idx in &sample.features {
                    let idx = feat_idx as usize * network.acc_dim + i;
                    let grad_w = grad_act + config.l2_reg * network.w0[idx];
                    network.w0[idx] -= lr_t * grad_w;
                }
            }
        }
    }

    // Update output layer (weighted average + L2 reg)
    // Note: L2 is applied online (per-feature) for w0 in the sparse inner loop,
    // while w2 applies L2 to the batch-averaged gradient. This asymmetry is
    // intentional for performance (row-sparse updates on w0), and is documented
    // to aid reproducibility when comparing training dynamics.
    let sum_w = total_weight.max(1e-8);
    let inv_sum_w = 1.0 / sum_w;

    if let Some(adam) = adam_state.as_mut() {
        // Update w2
        for (i, grad_sum) in grad_w2.iter().enumerate() {
            let grad = grad_sum * inv_sum_w + config.l2_reg * network.w2[i];
            adam.m_w2[i] = adam.beta1 * adam.m_w2[i] + (1.0 - adam.beta1) * grad;
            adam.v_w2[i] = adam.beta2 * adam.v_w2[i] + (1.0 - adam.beta2) * grad * grad;
            network.w2[i] -= lr_t * adam.m_w2[i] / (adam.v_w2[i].sqrt() + adam.epsilon);
        }

        // Update b2
        let grad_b2_avg = grad_b2 * inv_sum_w;
        adam.m_b2 = adam.beta1 * adam.m_b2 + (1.0 - adam.beta1) * grad_b2_avg;
        adam.v_b2 = adam.beta2 * adam.v_b2 + (1.0 - adam.beta2) * grad_b2_avg * grad_b2_avg;
        network.b2 -= lr_t * adam.m_b2 / (adam.v_b2.sqrt() + adam.epsilon);
    } else {
        // SGD updates for output layer
        for (i, grad_sum) in grad_w2.iter().enumerate() {
            let grad = grad_sum * inv_sum_w + config.l2_reg * network.w2[i];
            network.w2[i] -= lr_t * grad;
        }
        network.b2 -= lr_t * grad_b2 * inv_sum_w;
    }

    if total_weight > 0.0 {
        total_loss / total_weight
    } else {
        0.0
    }
}

// Numerically stable binary cross-entropy with logits
#[inline]
fn bce_with_logits(logit: f32, target: f32) -> (f32, f32) {
    let max_val = 0.0f32.max(logit);
    let loss = max_val - logit * target + ((-logit.abs()).exp() + 1.0).ln();
    let grad = 1.0 / (1.0 + (-logit).exp()) - target;
    (loss, grad)
}

fn compute_validation_loss(network: &Network, samples: &[Sample], config: &Config) -> f32 {
    let mut total_loss = 0.0;
    let mut total_weight = 0.0;

    // Pre-allocate buffers for forward pass
    let mut acc_buffer = vec![0.0f32; network.acc_dim];
    let mut activated_buffer = Vec::with_capacity(network.acc_dim);

    for sample in samples {
        let output =
            network.forward_with_buffers(&sample.features, &mut acc_buffer, &mut activated_buffer);

        let loss = match config.label_type.as_str() {
            "wdl" => {
                let (loss, _) = bce_with_logits(output, sample.label);
                loss
            }
            "cp" => {
                let error = output - sample.label;
                0.5 * error * error
            }
            _ => unreachable!(),
        };

        total_loss += loss * sample.weight;
        total_weight += sample.weight;
    }

    total_loss / total_weight
}

fn compute_val_auc(network: &Network, samples: &[Sample], config: &Config) -> Option<f64> {
    if config.label_type != "wdl" || samples.is_empty() {
        return None;
    }
    let mut probs: Vec<f32> = Vec::with_capacity(samples.len());
    let mut labels: Vec<f32> = Vec::with_capacity(samples.len());
    let mut weights: Vec<f32> = Vec::with_capacity(samples.len());

    let mut acc_buffer = vec![0.0f32; network.acc_dim];
    let mut activated_buffer = Vec::with_capacity(network.acc_dim);
    for s in samples {
        let out = network.forward_with_buffers(&s.features, &mut acc_buffer, &mut activated_buffer);
        let p = 1.0 / (1.0 + (-out).exp());
        // Treat strict positives/negatives only; skip exact boundary (label==0.5)
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

fn compute_val_auc_and_ece(
    network: &Network,
    samples: &[Sample],
    config: &Config,
    dash_val: &impl DashboardValKind,
) -> (Option<f64>, Option<f64>) {
    let auc = compute_val_auc(network, samples, config);
    if config.label_type != "wdl" || !dash_val.is_jsonl() {
        return (auc, None);
    }
    // Build cp-binned calibration and compute ECE
    let mut cps: Vec<i32> = Vec::new();
    let mut probs: Vec<f32> = Vec::new();
    let mut labels: Vec<f32> = Vec::new();
    let mut wts: Vec<f32> = Vec::new();
    let mut acc_buffer = vec![0.0f32; network.acc_dim];
    let mut activated_buffer = Vec::with_capacity(network.acc_dim);
    for s in samples {
        if let Some(cp) = s.cp {
            let out =
                network.forward_with_buffers(&s.features, &mut acc_buffer, &mut activated_buffer);
            let p = 1.0 / (1.0 + (-out).exp());
            cps.push(cp);
            probs.push(p);
            labels.push(s.label);
            wts.push(s.weight);
        }
    }
    if cps.is_empty() {
        return (auc, None);
    }
    let bins = calibration_bins(&cps, &probs, &labels, &wts, config.cp_clip, dash_val.calib_bins());
    let ece = ece_from_bins(&bins);
    (auc, ece)
}

// Small trait to pass validation kind info into helpers without threading many flags.
trait DashboardValKind {
    fn is_jsonl(&self) -> bool;
    fn calib_bins(&self) -> usize;
}

// Quantization parameters for int8 conversion
#[derive(Debug)]
struct QuantizationParams {
    scale: f32,
    zero_point: i32,
}

impl QuantizationParams {
    fn from_weights(weights: &[f32]) -> Self {
        let min_val = weights.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let max_val = weights.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));

        // Scale to map [min, max] to [-128, 127]
        let range = (max_val - min_val).max(1e-8);
        let scale = range / 255.0;
        let zero_point =
            (-min_val / scale - 128.0).round().clamp(QUANTIZATION_MIN, QUANTIZATION_MAX) as i32;

        Self { scale, zero_point }
    }
}

// Quantize weights to int8
fn quantize_weights(weights: &[f32], params: &QuantizationParams) -> Vec<i8> {
    weights
        .iter()
        .map(|&w| {
            let quantized = (w / params.scale + params.zero_point as f32).round();
            quantized.clamp(QUANTIZATION_MIN, QUANTIZATION_MAX) as i8
        })
        .collect()
}

// Save network in quantized int8 format
fn save_network_quantized(
    network: &Network,
    path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write;
    let mut file = std::io::BufWriter::new(File::create(path)?);

    // Write header
    writeln!(file, "NNUE")?;
    writeln!(file, "VERSION 3")?; // Version 3 for quantized format
    writeln!(file, "FEATURES HALFKP")?;
    writeln!(file, "ACC_DIM {}", network.acc_dim)?;
    writeln!(file, "RELU_CLIP {}", network.relu_clip)?;
    writeln!(file, "FORMAT QUANTIZED_I8")?;
    writeln!(file, "END_HEADER")?;

    // Quantize and write w0
    let params_w0 = QuantizationParams::from_weights(&network.w0);
    file.write_all(&params_w0.scale.to_le_bytes())?;
    file.write_all(&params_w0.zero_point.to_le_bytes())?;
    let quantized_w0 = quantize_weights(&network.w0, &params_w0);
    file.write_all(&quantized_w0.iter().map(|&x| x as u8).collect::<Vec<_>>())?;

    // Quantize and write b0
    let params_b0 = QuantizationParams::from_weights(&network.b0);
    file.write_all(&params_b0.scale.to_le_bytes())?;
    file.write_all(&params_b0.zero_point.to_le_bytes())?;
    let quantized_b0 = quantize_weights(&network.b0, &params_b0);
    file.write_all(&quantized_b0.iter().map(|&x| x as u8).collect::<Vec<_>>())?;

    // Quantize and write w2
    let params_w2 = QuantizationParams::from_weights(&network.w2);
    file.write_all(&params_w2.scale.to_le_bytes())?;
    file.write_all(&params_w2.zero_point.to_le_bytes())?;
    let quantized_w2 = quantize_weights(&network.w2, &params_w2);
    file.write_all(&quantized_w2.iter().map(|&x| x as u8).collect::<Vec<_>>())?;

    // Write b2 as float (single value, no need to quantize)
    file.write_all(&network.b2.to_le_bytes())?;

    file.flush()?;

    // Calculate size reduction
    let original_size = (network.w0.len() + network.b0.len() + network.w2.len() + 1) * 4;
    let quantized_size =
        (network.w0.len() + network.b0.len() + network.w2.len()) + QUANTIZATION_METADATA_SIZE;
    println!(
        "Quantized model saved. Size: {:.1} MB -> {:.1} MB ({:.1}% reduction)",
        original_size as f32 / KB_TO_MB_DIVISOR / KB_TO_MB_DIVISOR,
        quantized_size as f32 / KB_TO_MB_DIVISOR / KB_TO_MB_DIVISOR,
        (1.0 - quantized_size as f32 / original_size as f32) * PERCENTAGE_DIVISOR
    );

    Ok(())
}

fn save_network(network: &Network, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::{BufWriter, Write};
    let mut file = BufWriter::new(File::create(path)?);

    // Write header (text for readability)
    writeln!(file, "NNUE")?;
    writeln!(file, "VERSION 2")?; // Version 2 includes binary data
    writeln!(file, "FEATURES HALFKP")?;
    writeln!(file, "ARCHITECTURE SINGLE_CHANNEL")?; // Architecture type
    writeln!(file, "ACC_DIM {}", network.acc_dim)?;
    writeln!(file, "RELU_CLIP {}", network.relu_clip)?;
    writeln!(file, "FEATURE_DIM {}", SHOGI_BOARD_SIZE * FE_END)?; // Total feature dimension
    writeln!(file, "END_HEADER")?;

    // Write binary data for better compatibility
    // Matrix dimensions
    file.write_all(&(network.input_dim as u32).to_le_bytes())?;
    file.write_all(&(network.acc_dim as u32).to_le_bytes())?;

    // Write w0
    for &w in &network.w0 {
        file.write_all(&w.to_le_bytes())?;
    }

    // Write b0
    for &b in &network.b0 {
        file.write_all(&b.to_le_bytes())?;
    }

    // Write w2
    for &w in &network.w2 {
        file.write_all(&w.to_le_bytes())?;
    }

    // Write b2
    file.write_all(&network.b2.to_le_bytes())?;

    file.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};
    use tempfile::tempdir;

    const DEFAULT_RELU_CLIP_NUM: i32 = 127;
    const DEFAULT_CALIBRATION_BINS: usize = 40;
    const DEFAULT_CHUNK_SIZE: u32 = 1024;

    #[derive(Clone, Copy)]
    struct HeaderV1 {
        feature_set_id: u32,
        num_samples: u64,
        chunk_size: u32,
        header_size: u32,
        endianness: u8,
        payload_encoding: u8,
        sample_flags_mask: u32,
    }

    fn write_v1_header(f: &mut File, h: HeaderV1) -> u64 {
        // Magic
        f.write_all(b"NNFC").unwrap();
        // version
        f.write_all(&1u32.to_le_bytes()).unwrap();
        // feature_set_id
        f.write_all(&h.feature_set_id.to_le_bytes()).unwrap();
        // num_samples
        f.write_all(&h.num_samples.to_le_bytes()).unwrap();
        // chunk_size
        f.write_all(&h.chunk_size.to_le_bytes()).unwrap();
        // header_size
        f.write_all(&h.header_size.to_le_bytes()).unwrap();
        // endianness
        f.write_all(&[h.endianness]).unwrap();
        // payload_encoding
        f.write_all(&[h.payload_encoding]).unwrap();
        // reserved16
        f.write_all(&[0u8; 2]).unwrap();
        // payload_offset = after magic (4 bytes) + header_size
        let payload_offset = 4u64 + h.header_size as u64;
        f.write_all(&payload_offset.to_le_bytes()).unwrap();
        // sample_flags_mask
        f.write_all(&h.sample_flags_mask.to_le_bytes()).unwrap();
        // pad header tail to header_size
        let written = 40usize; // fields after magic
        let pad = (h.header_size as usize).saturating_sub(written);
        if pad > 0 {
            f.write_all(&vec![0u8; pad]).unwrap();
        }
        payload_offset
    }

    #[test]
    fn header_errors_and_ok_cases() {
        // bad magic
        {
            let td = tempdir().unwrap();
            let path = td.path().join("bad_magic.cache");
            let mut f = File::create(&path).unwrap();
            f.write_all(b"BAD!").unwrap();
            f.flush().unwrap();
            let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
            assert!(format!("{}", err).contains("bad magic"));
        }

        // unknown version
        {
            let td = tempdir().unwrap();
            let path = td.path().join("bad_version.cache");
            let mut f = File::create(&path).unwrap();
            f.write_all(b"NNFC").unwrap();
            f.write_all(&2u32.to_le_bytes()).unwrap(); // version=2 (unsupported)
                                                       // Fill rest with zeros to avoid EOF early
            f.write_all(&[0u8; 64]).unwrap();
            f.flush().unwrap();
            let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
            assert!(format!("{}", err).contains("v1 required"));
        }

        // endianness error
        {
            let td = tempdir().unwrap();
            let path = td.path().join("endianness.cache");
            let mut f = File::create(&path).unwrap();
            let _off = write_v1_header(
                &mut f,
                HeaderV1 {
                    feature_set_id: 0x48414C46,
                    num_samples: 0,
                    chunk_size: DEFAULT_CHUNK_SIZE,
                    header_size: 48,
                    endianness: 1, // BE
                    payload_encoding: 0,
                    sample_flags_mask: 0,
                },
            );
            f.flush().unwrap();
            let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
            assert!(format!("{}", err).contains("Unsupported endianness"));
        }

        // unknown encoding
        {
            let td = tempdir().unwrap();
            let path = td.path().join("encoding.cache");
            let mut f = File::create(&path).unwrap();
            let _off = write_v1_header(
                &mut f,
                HeaderV1 {
                    feature_set_id: 0x48414C46,
                    num_samples: 0,
                    chunk_size: DEFAULT_CHUNK_SIZE,
                    header_size: 48,
                    endianness: 0,
                    payload_encoding: 3,
                    sample_flags_mask: 0,
                },
            );
            f.flush().unwrap();
            let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
            assert!(format!("{}", err).contains("Unknown payload encoding"));
        }

        // feature_set_id mismatch
        {
            let td = tempdir().unwrap();
            let path = td.path().join("featureset.cache");
            let mut f = File::create(&path).unwrap();
            let _off = write_v1_header(
                &mut f,
                HeaderV1 {
                    feature_set_id: 0x00000000,
                    num_samples: 0,
                    chunk_size: DEFAULT_CHUNK_SIZE,
                    header_size: 48,
                    endianness: 0,
                    payload_encoding: 0,
                    sample_flags_mask: 0,
                },
            );
            f.flush().unwrap();
            let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
            assert!(format!("{}", err).contains("Unsupported feature_set_id"));
        }

        // header_size 極端値（0/8/4097）でエラー
        for bad_size in [0u32, 8u32, 4097u32] {
            let td = tempdir().unwrap();
            let path = td.path().join(format!("bad_hs_{bad_size}.cache"));
            let mut f = File::create(&path).unwrap();
            let _off = write_v1_header(
                &mut f,
                HeaderV1 {
                    feature_set_id: 0x48414C46,
                    num_samples: 0,
                    chunk_size: DEFAULT_CHUNK_SIZE,
                    header_size: bad_size,
                    endianness: 0,
                    payload_encoding: 0,
                    sample_flags_mask: 0,
                },
            );
            f.flush().unwrap();
            let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
            assert!(format!("{}", err).contains("header_size"));
        }

        // 破損 payload_offset（header_end より小さい）でエラー
        {
            let td = tempdir().unwrap();
            let path = td.path().join("broken_offset.cache");
            let mut f = File::create(&path).unwrap();
            // Magic + version + feature_set_id + num_samples + chunk_size
            f.write_all(b"NNFC").unwrap();
            f.write_all(&1u32.to_le_bytes()).unwrap();
            f.write_all(&0x48414C46u32.to_le_bytes()).unwrap();
            f.write_all(&1u64.to_le_bytes()).unwrap(); // num_samples
            f.write_all(&1024u32.to_le_bytes()).unwrap(); // chunk_size
                                                          // header_size=48, endianness=0, encoding=0, reserved16=0
            f.write_all(&48u32.to_le_bytes()).unwrap();
            f.write_all(&[0u8, 0u8]).unwrap();
            f.write_all(&[0u8; 2]).unwrap();
            // payload_offset を header_end より小さくする（壊れ）
            // header_end = magic(4) + header_size(48) = 52
            let bad_off = 36u64;
            f.write_all(&bad_off.to_le_bytes()).unwrap();
            // sample_flags_mask
            f.write_all(&0u32.to_le_bytes()).unwrap();
            // 余りを header_size まで埋める
            let written = 40usize; // after magic
            let pad = (48usize).saturating_sub(written);
            if pad > 0 {
                f.write_all(&vec![0u8; pad]).unwrap();
            }
            // payload 仮書き
            f.write_all(&0u32.to_le_bytes()).unwrap();
            f.flush().unwrap();

            let res = load_samples_from_cache(path.to_str().unwrap());
            assert!(res.is_err(), "broken payload_offset should error");
        }

        // header_size larger with payload_offset respected and num_samples=0
        {
            let td = tempdir().unwrap();
            let path = td.path().join("ok_zero.cache");
            let mut f = File::create(&path).unwrap();
            let _off = write_v1_header(
                &mut f,
                HeaderV1 {
                    feature_set_id: 0x48414C46,
                    num_samples: 0,
                    chunk_size: DEFAULT_CHUNK_SIZE,
                    header_size: 64,
                    endianness: 0,
                    payload_encoding: 0,
                    sample_flags_mask: 0,
                },
            );
            f.flush().unwrap();
            let v = load_samples_from_cache(path.to_str().unwrap()).unwrap();
            assert!(v.is_empty());
        }
    }

    #[test]
    fn weight_consistency_jsonl_vs_cache() {
        // JSONL with both_exact, gap=50, depth=20, seldepth=30
        let td = tempdir().unwrap();
        let json_path = td.path().join("w.jsonl");
        let mut jf = File::create(&json_path).unwrap();
        writeln!(
            jf,
            "{{\"sfen\":\"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1\",\"eval\":0,\"depth\":20,\"seldepth\":30,\"bound1\":\"Exact\",\"bound2\":\"Exact\",\"best2_gap_cp\":50}}"
        )
        .unwrap();
        jf.flush().unwrap();

        let cfg = Config {
            epochs: 1,
            batch_size: 1,
            learning_rate: 0.001,
            optimizer: "sgd".to_string(),
            l2_reg: 0.0,
            label_type: "cp".to_string(),
            scale: 600.0,
            cp_clip: 1200,
            accumulator_dim: 8,
            relu_clip: DEFAULT_RELU_CLIP_NUM,
            shuffle: false,
            prefetch_batches: 0,
            throughput_interval_sec: 10_000.0,
            stream_cache: false,
            prefetch_bytes: None,
            estimated_features_per_sample: 64,
            exclude_no_legal_move: false,
            exclude_fallback: false,
            lr_schedule: "constant".to_string(),
            lr_warmup_epochs: 0,
            lr_decay_epochs: None,
            lr_decay_steps: None,
            lr_plateau_patience: None,
        };
        let json_samples = load_samples(json_path.to_str().unwrap(), &cfg).unwrap();
        // Two-sample orientation -> take first weight
        let w_json = json_samples[0].weight;

        // Build cache with a single sample (n_features=0) carrying same meta
        let cache_path = td.path().join("w.cache");
        {
            let mut f = File::create(&cache_path).unwrap();
            let payload_offset = write_v1_header(
                &mut f,
                HeaderV1 {
                    feature_set_id: 0x48414C46,
                    num_samples: 1,
                    chunk_size: DEFAULT_CHUNK_SIZE,
                    header_size: 48,
                    endianness: 0,
                    payload_encoding: 0,
                    sample_flags_mask: 1u8 as u32,
                },
            );
            // seek to payload_offset
            f.seek(SeekFrom::Start(payload_offset)).unwrap();
            // n_features = 0
            f.write_all(&0u32.to_le_bytes()).unwrap();
            // no features body
            // label (cp irrelevant for weight)
            f.write_all(&0.0f32.to_le_bytes()).unwrap();
            // gap=50
            f.write_all(&(50u16).to_le_bytes()).unwrap();
            // depth=20, seldepth=30
            f.write_all(&[20u8]).unwrap();
            f.write_all(&[30u8]).unwrap();
            // flags: both_exact (bit0)
            f.write_all(&[1u8]).unwrap();
            f.flush().unwrap();
        }

        let cache_samples = load_samples_from_cache(cache_path.to_str().unwrap()).unwrap();
        let w_cache = cache_samples[0].weight;

        assert!(
            (w_json - w_cache).abs() < 1e-6,
            "weights should match: {} vs {}",
            w_json,
            w_cache
        );
    }

    #[test]
    fn unknown_flags_warning_and_continue() {
        let td = tempdir().unwrap();
        let path = td.path().join("unknown_flags.cache");
        let mut f = File::create(&path).unwrap();
        // mask に 0 を渡して「全bit未知扱い」にする
        let off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x48414C46,
                num_samples: 1,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 48,
                endianness: 0,
                payload_encoding: 0,
                sample_flags_mask: 0,
            },
        );
        f.seek(SeekFrom::Start(off)).unwrap();
        // n_features=0, label=0.0, gap=0, depth=0, seldepth=0, flags = 0x80 (未知bit)
        f.write_all(&0u32.to_le_bytes()).unwrap();
        f.write_all(&0.0f32.to_le_bytes()).unwrap();
        f.write_all(&0u16.to_le_bytes()).unwrap();
        f.write_all(&[0u8, 0u8, 0x80u8]).unwrap();
        f.flush().unwrap();

        let samples = load_samples_from_cache(path.to_str().unwrap()).unwrap();
        assert_eq!(samples.len(), 1);
    }

    #[test]
    fn auc_boundary_labels_skipped() {
        // Network with zero weights outputs 0.0 logits -> p=0.5
        let mut rng = rand::rngs::StdRng::seed_from_u64(123);
        let mut net = Network::new(8, DEFAULT_RELU_CLIP_NUM, &mut rng);
        // Zero out all weights/biases to make output exactly 0
        for w in net.w0.iter_mut() {
            *w = 0.0;
        }
        for b in net.b0.iter_mut() {
            *b = 0.0;
        }
        for w in net.w2.iter_mut() {
            *w = 0.0;
        }
        net.b2 = 0.0;

        // Samples all with label==0.5 (boundary) should be skipped and yield None AUC
        let samples = vec![
            Sample {
                features: vec![],
                label: 0.5,
                weight: 1.0,
                cp: None,
                phase: None,
            },
            Sample {
                features: vec![],
                label: 0.5,
                weight: 1.0,
                cp: None,
                phase: None,
            },
        ];
        let cfg = Config {
            epochs: 1,
            batch_size: 1,
            learning_rate: 0.001,
            optimizer: "sgd".to_string(),
            l2_reg: 0.0,
            label_type: "wdl".to_string(),
            scale: 600.0,
            cp_clip: 1200,
            accumulator_dim: 8,
            relu_clip: DEFAULT_RELU_CLIP_NUM,
            shuffle: false,
            prefetch_batches: 0,
            throughput_interval_sec: 10.0,
            stream_cache: false,
            prefetch_bytes: None,
            estimated_features_per_sample: 64,
            exclude_no_legal_move: false,
            exclude_fallback: false,
            lr_schedule: "constant".to_string(),
            lr_warmup_epochs: 0,
            lr_decay_epochs: None,
            lr_decay_steps: None,
            lr_plateau_patience: None,
        };
        let auc = super::compute_val_auc(&net, &samples, &cfg);
        assert!(auc.is_none(), "AUC should be None when all labels are 0.5 boundary");
    }

    #[test]
    fn clamp_gap_and_depth_saturation() {
        // JSONL with large gap and max depth/seldepth (u8 saturate)
        let td = tempdir().unwrap();
        let json_path = td.path().join("w2.jsonl");
        let mut jf = File::create(&json_path).unwrap();
        writeln!(
            jf,
            r#"{{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":0,"depth":255,"seldepth":255,"bound1":"Exact","bound2":"Exact","best2_gap_cp":70000}}"#
        )
        .unwrap();
        jf.flush().unwrap();

        let cfg = Config {
            epochs: 1,
            batch_size: 1,
            learning_rate: 0.001,
            optimizer: "sgd".to_string(),
            l2_reg: 0.0,
            label_type: "cp".to_string(),
            scale: 600.0,
            cp_clip: 1200,
            accumulator_dim: 8,
            relu_clip: DEFAULT_RELU_CLIP_NUM,
            shuffle: false,
            prefetch_batches: 0,
            throughput_interval_sec: 10_000.0,
            stream_cache: false,
            prefetch_bytes: None,
            estimated_features_per_sample: 64,
            exclude_no_legal_move: false,
            exclude_fallback: false,
            lr_schedule: "constant".to_string(),
            lr_warmup_epochs: 0,
            lr_decay_epochs: None,
            lr_decay_steps: None,
            lr_plateau_patience: None,
        };
        let json_samples = load_samples(json_path.to_str().unwrap(), &cfg).unwrap();
        assert!(!json_samples.is_empty());
        assert!(json_samples[0].weight <= 1.0);
    }

    // 再現性（seed指定）— test_training_reproducibility_with_seed
    #[test]
    fn test_training_reproducibility_with_seed() {
        use rand::SeedableRng;

        // JSONLを用意（2局面）
        let td = tempdir().unwrap();
        let json_path = td.path().join("repro.jsonl");
        let mut jf = File::create(&json_path).unwrap();
        writeln!(
            jf,
            r#"{{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1","eval":100,"depth":10,"seldepth":12,"bound1":"Exact","bound2":"Exact","best2_gap_cp":25}}"#
        )
        .unwrap();
        writeln!(
            jf,
            r#"{{"sfen":"lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1","eval":200,"depth":10,"seldepth":12,"bound1":"Exact","bound2":"Exact","best2_gap_cp":30}}"#
        )
        .unwrap();
        jf.flush().unwrap();

        // 設定（shuffle=false、optimizer=sgd、l2=0、accumulator_dim小さめ）
        let cfg = Config {
            epochs: 2,
            batch_size: 4,
            learning_rate: 0.001,
            optimizer: "sgd".to_string(),
            l2_reg: 0.0,
            label_type: "cp".to_string(),
            scale: 600.0,
            cp_clip: 1200,
            accumulator_dim: 8,
            relu_clip: DEFAULT_RELU_CLIP_NUM,
            shuffle: false,
            prefetch_batches: 0,
            throughput_interval_sec: 10_000.0,
            stream_cache: false,
            prefetch_bytes: None,
            estimated_features_per_sample: 64,
            exclude_no_legal_move: false,
            exclude_fallback: false,
            lr_schedule: "constant".to_string(),
            lr_warmup_epochs: 0,
            lr_decay_epochs: None,
            lr_decay_steps: None,
            lr_plateau_patience: None,
        };

        // サンプルを読み込み（2局面→2サンプル/局面 = 計4サンプル）
        let mut samples1 = load_samples(json_path.to_str().unwrap(), &cfg).unwrap();
        let mut samples2 = samples1.clone();
        assert_eq!(samples1.len(), samples2.len());
        assert_eq!(samples1.len(), 4);

        // 同じseedで2つのネットワークを初期化
        let mut rng1 = rand::rngs::StdRng::seed_from_u64(42);
        let mut rng2 = rand::rngs::StdRng::seed_from_u64(42);
        let mut net1 = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng1);
        let mut net2 = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng2);

        // 同じ条件・同じデータで学習
        let out_dir = td.path();
        let mut dummy_rng1 = rand::rngs::StdRng::seed_from_u64(123);
        let mut dummy_rng2 = rand::rngs::StdRng::seed_from_u64(123);
        let dash = super::DashboardOpts {
            emit: false,
            calib_bins_n: DEFAULT_CALIBRATION_BINS,
            do_plots: false,
            val_is_jsonl: false,
        };
        let mut bn1 = None;
        let mut bvl1 = f32::INFINITY;
        let mut ll1 = None;
        let mut be1 = None;
        let mut ctx1 = super::TrainContext {
            out_dir,
            save_every: None,
            dash,
            trackers: super::TrainTrackers {
                best_network: &mut bn1,
                best_val_loss: &mut bvl1,
                last_val_loss: &mut ll1,
                best_epoch: &mut be1,
            },
            structured: None,
            global_step: 0,
        };
        train_model(&mut net1, &mut samples1, &None, &cfg, &mut dummy_rng1, &mut ctx1).unwrap();
        let dash2 = super::DashboardOpts {
            emit: false,
            calib_bins_n: DEFAULT_CALIBRATION_BINS,
            do_plots: false,
            val_is_jsonl: false,
        };
        let mut bn2 = None;
        let mut bvl2 = f32::INFINITY;
        let mut ll2 = None;
        let mut be2 = None;
        let mut ctx2 = super::TrainContext {
            out_dir,
            save_every: None,
            dash: dash2,
            trackers: super::TrainTrackers {
                best_network: &mut bn2,
                best_val_loss: &mut bvl2,
                last_val_loss: &mut ll2,
                best_epoch: &mut be2,
            },
            structured: None,
            global_step: 0,
        };
        train_model(&mut net2, &mut samples2, &None, &cfg, &mut dummy_rng2, &mut ctx2).unwrap();

        // 重みの一致を確認（厳密一致 or 十分小さい誤差）
        assert_eq!(net1.w0.len(), net2.w0.len());
        assert_eq!(net1.b0.len(), net2.b0.len());
        assert_eq!(net1.w2.len(), net2.w2.len());
        let eps = 1e-7;
        for (a, b) in net1.w0.iter().zip(net2.w0.iter()) {
            assert!((a - b).abs() <= eps, "w0 diff: {} vs {}", a, b);
        }
        for (a, b) in net1.b0.iter().zip(net2.b0.iter()) {
            assert!((a - b).abs() <= eps, "b0 diff: {} vs {}", a, b);
        }
        for (a, b) in net1.w2.iter().zip(net2.w2.iter()) {
            assert!((a - b).abs() <= eps, "w2 diff: {} vs {}", a, b);
        }
        assert!((net1.b2 - net2.b2).abs() <= eps, "b2 diff: {} vs {}", net1.b2, net2.b2);
    }

    // 巨大な n_features を持つ壊れキャッシュが上限制約でエラーになること
    #[test]
    fn n_features_exceeds_limit_errors() {
        let td = tempdir().unwrap();
        let path = td.path().join("too_many_features.cache");
        let mut f = File::create(&path).unwrap();
        // 1サンプル・非圧縮・flags_mask=0
        let off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x48414C46,
                num_samples: 1,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 48,
                endianness: 0,
                payload_encoding: 0,
                sample_flags_mask: 0,
            },
        );
        f.seek(SeekFrom::Start(off)).unwrap();
        // 上限 (SHOGI_BOARD_SIZE*FE_END) + 1 を書く
        let max_allowed = (SHOGI_BOARD_SIZE * FE_END) as u32;
        let n_features = max_allowed + 1;
        f.write_all(&n_features.to_le_bytes()).unwrap();
        // 以降のボディは不要（n_features検証で即エラー）
        f.flush().unwrap();

        let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("exceeds"), "unexpected err msg: {}", msg);
    }

    // stream-sync と in-memory 経路での重み一致（決定論）
    #[test]
    fn stream_sync_vs_inmem_equivalence() {
        use tempfile::tempdir;
        // 小さな cache v1 を作成（3サンプル, n_features=0）
        let td = tempdir().unwrap();
        let path = td.path().join("tiny.cache");
        let mut f = File::create(&path).unwrap();
        // header: feature_set_id=HALF, num_samples=3, chunk_size=1024, header_size=48, LE, raw payload, flags_mask=0
        let payload_off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: 0x48414C46,
                num_samples: 3,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 48,
                endianness: 0,
                payload_encoding: 0,
                sample_flags_mask: 0,
            },
        );
        f.seek(SeekFrom::Start(payload_off)).unwrap();
        for _ in 0..3u32 {
            // n_features=0
            f.write_all(&0u32.to_le_bytes()).unwrap();
            // label
            f.write_all(&0.0f32.to_le_bytes()).unwrap();
            // gap=50
            f.write_all(&(50u16).to_le_bytes()).unwrap();
            // depth=10, seldepth=12
            f.write_all(&[10u8]).unwrap();
            f.write_all(&[12u8]).unwrap();
            // flags=both_exact
            f.write_all(&[1u8]).unwrap();
        }
        f.flush().unwrap();

        // 共通設定
        let cfg_inmem = Config {
            epochs: 1,
            batch_size: 2,
            learning_rate: 0.001,
            optimizer: "sgd".to_string(),
            l2_reg: 0.0,
            label_type: "cp".to_string(),
            scale: 600.0,
            cp_clip: 1200,
            accumulator_dim: 8,
            relu_clip: DEFAULT_RELU_CLIP_NUM,
            shuffle: false,
            prefetch_batches: 0,
            throughput_interval_sec: 10_000.0,
            stream_cache: false,
            prefetch_bytes: None,
            estimated_features_per_sample: 64,
            exclude_no_legal_move: false,
            exclude_fallback: false,
            lr_schedule: "constant".to_string(),
            lr_warmup_epochs: 0,
            lr_decay_epochs: None,
            lr_decay_steps: None,
            lr_plateau_patience: None,
        };
        let cfg_stream = Config {
            stream_cache: true,
            ..cfg_inmem.clone()
        };

        // サンプルを読み込み（in-mem）
        let mut samples = load_samples_from_cache(path.to_str().unwrap()).unwrap();

        // 同じseedで2つのネットを初期化
        let mut rng1 = rand::rngs::StdRng::seed_from_u64(42);
        let mut rng2 = rand::rngs::StdRng::seed_from_u64(42);
        let mut net_inmem = Network::new(cfg_inmem.accumulator_dim, cfg_inmem.relu_clip, &mut rng1);
        let mut net_stream =
            Network::new(cfg_stream.accumulator_dim, cfg_stream.relu_clip, &mut rng2);

        let out_dir = td.path();
        let mut dummy_rng = rand::rngs::StdRng::seed_from_u64(123);

        // in-mem 学習
        let dash_inmem = super::DashboardOpts {
            emit: false,
            calib_bins_n: DEFAULT_CALIBRATION_BINS,
            do_plots: false,
            val_is_jsonl: false,
        };
        let mut best_network: Option<Network> = None;
        let mut best_val_loss = f32::INFINITY;
        let mut last_val_loss: Option<f32> = None;
        let mut best_epoch: Option<usize> = None;
        let mut ctx_in = super::TrainContext {
            out_dir,
            save_every: None,
            dash: dash_inmem,
            trackers: super::TrainTrackers {
                best_network: &mut best_network,
                best_val_loss: &mut best_val_loss,
                last_val_loss: &mut last_val_loss,
                best_epoch: &mut best_epoch,
            },
            structured: None,
            global_step: 0,
        };
        train_model(&mut net_inmem, &mut samples, &None, &cfg_inmem, &mut dummy_rng, &mut ctx_in)
            .unwrap();
        // stream-sync 学習
        let dash_stream = super::DashboardOpts {
            emit: false,
            calib_bins_n: DEFAULT_CALIBRATION_BINS,
            do_plots: false,
            val_is_jsonl: false,
        };
        let mut best_network2: Option<Network> = None;
        let mut best_val_loss2 = f32::INFINITY;
        let mut last_val_loss2: Option<f32> = None;
        let mut best_epoch2: Option<usize> = None;
        let mut ctx_st = super::TrainContext {
            out_dir,
            save_every: None,
            dash: dash_stream,
            trackers: super::TrainTrackers {
                best_network: &mut best_network2,
                best_val_loss: &mut best_val_loss2,
                last_val_loss: &mut last_val_loss2,
                best_epoch: &mut best_epoch2,
            },
            structured: None,
            global_step: 0,
        };
        train_model_stream_cache(
            &mut net_stream,
            path.to_str().unwrap(),
            &None,
            &cfg_stream,
            &mut dummy_rng,
            &mut ctx_st,
        )
        .unwrap();

        // 重み一致（厳密一致 or 近傍）
        assert_eq!(net_inmem.w0.len(), net_stream.w0.len());
        assert_eq!(net_inmem.b0.len(), net_stream.b0.len());
        assert_eq!(net_inmem.w2.len(), net_stream.w2.len());
        let eps = 1e-7;
        for (a, b) in net_inmem.w0.iter().zip(net_stream.w0.iter()) {
            assert!((a - b).abs() <= eps, "w0 diff: {} vs {}", a, b);
        }
        for (a, b) in net_inmem.b0.iter().zip(net_stream.b0.iter()) {
            assert!((a - b).abs() <= eps, "b0 diff: {} vs {}", a, b);
        }
        for (a, b) in net_inmem.w2.iter().zip(net_stream.w2.iter()) {
            assert!((a - b).abs() <= eps, "w2 diff: {} vs {}", a, b);
        }
        assert!(
            (net_inmem.b2 - net_stream.b2).abs() <= eps,
            "b2 diff: {} vs {}",
            net_inmem.b2,
            net_stream.b2
        );
    }

    // 非同期ストリーム（prefetch>0）で破損キャッシュのエラーが上位に伝搬すること
    #[test]
    fn stream_async_propagates_errors() {
        let td = tempdir().unwrap();
        let path = td.path().join("bad_async.cache");

        // 1サンプル、raw（非圧縮）でヘッダを書き、payload に n_features = MAX+1 を書く
        let mut f = File::create(&path).unwrap();
        let payload_off = write_v1_header(
            &mut f,
            HeaderV1 {
                feature_set_id: FEATURE_SET_ID_HALF,
                num_samples: 1,
                chunk_size: DEFAULT_CHUNK_SIZE,
                header_size: 48,
                endianness: 0,
                payload_encoding: 0,
                sample_flags_mask: 0,
            },
        );
        f.seek(SeekFrom::Start(payload_off)).unwrap();
        let max_allowed = (SHOGI_BOARD_SIZE * FE_END) as u32;
        let n_features = max_allowed + 1;
        f.write_all(&n_features.to_le_bytes()).unwrap();
        f.flush().unwrap();

        let cfg = Config {
            epochs: 1,
            batch_size: 1024,
            learning_rate: 0.001,
            optimizer: "sgd".to_string(),
            l2_reg: 0.0,
            label_type: "cp".to_string(),
            scale: 600.0,
            cp_clip: 1200,
            accumulator_dim: 8,
            relu_clip: DEFAULT_RELU_CLIP_NUM,
            shuffle: false,
            prefetch_batches: 2, // async 経路
            throughput_interval_sec: 10_000.0,
            stream_cache: true,
            prefetch_bytes: None,
            estimated_features_per_sample: 64,
            exclude_no_legal_move: false,
            exclude_fallback: false,
            lr_schedule: "constant".to_string(),
            lr_warmup_epochs: 0,
            lr_decay_epochs: None,
            lr_decay_steps: None,
            lr_plateau_patience: None,
        };

        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let mut net = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng);
        let out_dir = td.path();
        let dash = super::DashboardOpts {
            emit: false,
            calib_bins_n: DEFAULT_CALIBRATION_BINS,
            do_plots: false,
            val_is_jsonl: false,
        };
        let mut best_network: Option<Network> = None;
        let mut best_val_loss = f32::INFINITY;
        let mut last_val_loss: Option<f32> = None;
        let mut best_epoch: Option<usize> = None;
        let mut ctx = super::TrainContext {
            out_dir,
            save_every: None,
            dash,
            trackers: super::TrainTrackers {
                best_network: &mut best_network,
                best_val_loss: &mut best_val_loss,
                last_val_loss: &mut last_val_loss,
                best_epoch: &mut best_epoch,
            },
            structured: None,
            global_step: 0,
        };
        let err = train_model_stream_cache(
            &mut net,
            path.to_str().unwrap(),
            &None,
            &cfg,
            &mut rng,
            &mut ctx,
        )
        .unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("exceeds sane limit"), "unexpected err msg: {}", msg);
    }
    // n_features=0 のサンプルのみで 1 epoch 学習し、NaN が発生しないことのスモーク
    #[test]
    fn train_one_batch_with_zero_feature_sample_smoke() {
        use rand::SeedableRng;

        // 単一サンプル（特徴なし、重み1.0、ラベル0.0）
        let mut samples = vec![Sample {
            features: Vec::new(),
            label: 0.0,
            weight: 1.0,
            cp: None,
            phase: None,
        }];

        let cfg = Config {
            epochs: 1,
            batch_size: 1,
            learning_rate: 0.001,
            optimizer: "sgd".to_string(),
            l2_reg: 0.0,
            label_type: "cp".to_string(),
            scale: 600.0,
            cp_clip: 1200,
            accumulator_dim: 8,
            relu_clip: DEFAULT_RELU_CLIP_NUM,
            shuffle: false,
            prefetch_batches: 0,
            throughput_interval_sec: 10_000.0,
            stream_cache: false,
            prefetch_bytes: None,
            estimated_features_per_sample: 64,
            exclude_no_legal_move: false,
            exclude_fallback: false,
            lr_schedule: "constant".to_string(),
            lr_warmup_epochs: 0,
            lr_decay_epochs: None,
            lr_decay_steps: None,
            lr_plateau_patience: None,
        };

        let td = tempfile::tempdir().unwrap();
        let out_dir = td.path();

        let mut rng = rand::rngs::StdRng::seed_from_u64(1);
        let mut net = Network::new(cfg.accumulator_dim, cfg.relu_clip, &mut rng);
        let dash = super::DashboardOpts {
            emit: false,
            calib_bins_n: DEFAULT_CALIBRATION_BINS,
            do_plots: false,
            val_is_jsonl: false,
        };
        let mut best_network: Option<Network> = None;
        let mut best_val_loss = f32::INFINITY;
        let mut last_val_loss: Option<f32> = None;
        let mut best_epoch: Option<usize> = None;
        let mut ctx = super::TrainContext {
            out_dir,
            save_every: None,
            dash,
            trackers: super::TrainTrackers {
                best_network: &mut best_network,
                best_val_loss: &mut best_val_loss,
                last_val_loss: &mut last_val_loss,
                best_epoch: &mut best_epoch,
            },
            structured: None,
            global_step: 0,
        };
        train_model(&mut net, &mut samples, &None, &cfg, &mut rng, &mut ctx).unwrap();

        // NaN が混入していないこと
        assert!(net.w0.iter().all(|v| v.is_finite()));
        assert!(net.b0.iter().all(|v| v.is_finite()));
        assert!(net.w2.iter().all(|v| v.is_finite()));
        assert!(net.b2.is_finite());
    }
}
