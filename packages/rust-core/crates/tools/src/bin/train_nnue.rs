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
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use clap::{arg, Command};
use engine_core::{
    evaluation::nnue::features::{extract_features, FE_END},
    shogi::SHOGI_BOARD_SIZE,
    Color, Position,
};
use rand::rngs::StdRng;
use rand::{seq::SliceRandom, Rng, SeedableRng};
use serde::{Deserialize, Serialize};

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
    // Data filters (align with build_feature_cache)
    exclude_no_legal_move: bool,
    exclude_fallback: bool,
}

#[derive(Clone, Debug)]
struct Sample {
    features: Vec<u32>, // Active feature indices for both perspectives
    label: f32,
    weight: f32,
}

// CacheHeader schema is parsed ad-hoc (v1 only). No struct kept to avoid drift.

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

    #[allow(dead_code)]
    fn forward(&self, features: &[u32]) -> (f32, Vec<f32>) {
        // Accumulator = b0 + sum(W0[features])
        let mut acc = self.b0.clone();

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
            for (i, acc_val) in acc.iter_mut().enumerate() {
                *acc_val += self.w0[offset + i];
            }
        }

        // Apply clipped ReLU
        let mut activated = vec![0.0f32; self.acc_dim];
        for (i, &acc_val) in acc.iter().enumerate() {
            activated[i] = acc_val.max(0.0).min(self.relu_clip);
        }

        // Output layer
        let mut output = self.b2;
        for (w, act) in self.w2.iter().zip(activated.iter()) {
            output += w * act;
        }

        (output, activated)
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
    samples: Arc<Vec<Sample>>,
    indices: Vec<usize>,
    batch_size: usize,
    position: usize,
    epoch: usize,
}

impl BatchLoader {
    fn new(samples: Arc<Vec<Sample>>, batch_size: usize, shuffle: bool, rng: &mut StdRng) -> Self {
        let n_samples = samples.len();
        let mut indices: Vec<usize> = (0..n_samples).collect();

        if shuffle {
            indices.shuffle(rng);
        }

        BatchLoader {
            samples,
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

    #[allow(dead_code)]
    fn get_samples(&self, indices: &[usize]) -> Vec<Sample> {
        indices.iter().map(|&idx| self.samples[idx].clone()).collect()
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
            beta1: 0.9,
            beta2: 0.999,
            epsilon: 1e-8,
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
        .arg(arg!(-l --label <TYPE> "Label type: wdl, cp").default_value("wdl"))
        .arg(arg!(--scale <N> "Scale for cp->wdl conversion").default_value("600"))
        .arg(arg!(--"cp-clip" <N> "Clip CP values to this range").default_value("1200"))
        .arg(arg!(--"acc-dim" <N> "Accumulator dimension").default_value("256"))
        .arg(arg!(--"relu-clip" <N> "ReLU clipping value").default_value("127"))
        .arg(arg!(--shuffle "Shuffle training data"))
        .arg(arg!(--"exclude-no-legal-move" "Exclude positions with no legal moves (JSONL input)"))
        .arg(arg!(--"exclude-fallback" "Exclude positions where fallback was used (JSONL input)"))
        .arg(arg!(--"save-every" <N> "Save checkpoint every N batches"))
        .arg(arg!(--quantized "Save quantized (int8) version of the model"))
        .arg(arg!(--seed <SEED> "Random seed for reproducibility"))
        .arg(arg!(-o --out <DIR> "Output directory"))
        .get_matches();

    let config = Config {
        epochs: app.get_one::<String>("epochs").unwrap().parse()?,
        batch_size: app.get_one::<String>("batch-size").unwrap().parse()?,
        learning_rate: app.get_one::<String>("lr").unwrap().parse()?,
        optimizer: app.get_one::<String>("opt").unwrap().to_string(),
        l2_reg: app.get_one::<String>("l2").unwrap().parse()?,
        label_type: app.get_one::<String>("label").unwrap().to_string(),
        scale: app.get_one::<String>("scale").unwrap().parse()?,
        cp_clip: app.get_one::<String>("cp-clip").unwrap().parse()?,
        accumulator_dim: app.get_one::<String>("acc-dim").unwrap().parse()?,
        relu_clip: app.get_one::<String>("relu-clip").unwrap().parse()?,
        shuffle: app.get_flag("shuffle"),
        exclude_no_legal_move: app.get_flag("exclude-no-legal-move"),
        exclude_fallback: app.get_flag("exclude-fallback"),
    };

    let input_path = app.get_one::<String>("input").unwrap();
    let validation_path = app.get_one::<String>("validation");
    let save_every: Option<usize> =
        app.get_one::<String>("save-every").map(|s| s.parse()).transpose()?;

    let timestamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    let out_dir = app
        .get_one::<String>("out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("runs/nnue_{}", timestamp)));

    println!("Configuration:");
    println!("  Input: {}", input_path);
    if let Some(val_path) = validation_path {
        println!("  Validation: {}", val_path);
    }
    println!("  Output: {}", out_dir.display());
    println!("  Settings: {:?}", config);
    println!("  Feature dimension (input): {} (HalfKP)", SHOGI_BOARD_SIZE * FE_END);
    println!("  Network: {} -> {} -> 1", SHOGI_BOARD_SIZE * FE_END, config.accumulator_dim);

    // Load training data
    let start_time = Instant::now();
    println!("\nLoading training data...");

    // Check if input is cache file or JSONL
    let is_cache = input_path.ends_with(".bin") || input_path.ends_with(".cache");
    let mut train_samples = if is_cache {
        println!("Loading from cache file...");
        load_samples_from_cache(input_path)?
    } else {
        load_samples(input_path, &config)?
    };

    println!(
        "Loaded {} samples in {:.2}s",
        train_samples.len(),
        start_time.elapsed().as_secs_f32()
    );

    // Load validation data if provided
    let validation_samples = if let Some(val_path) = validation_path {
        println!("\nLoading validation data...");
        let start_val = Instant::now();

        let is_val_cache = val_path.ends_with(".bin") || val_path.ends_with(".cache");
        let samples = if is_val_cache {
            println!("Loading validation from cache file...");
            load_samples_from_cache(val_path)?
        } else {
            load_samples(val_path, &config)?
        };

        println!(
            "Loaded {} validation samples in {:.2}s",
            samples.len(),
            start_val.elapsed().as_secs_f32()
        );
        Some(samples)
    } else {
        None
    };

    // Initialize RNG with seed if provided
    let mut rng: StdRng = if let Some(seed_str) = app.get_one::<String>("seed") {
        let seed: u64 = seed_str.parse().expect("Invalid seed value");
        println!("Using random seed: {}", seed);
        StdRng::seed_from_u64(seed)
    } else {
        let seed_bytes: [u8; 32] = rand::rng().random();
        let seed_str = seed_bytes.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        println!("Generated random seed: {}", seed_str);
        StdRng::from_seed(seed_bytes)
    };

    // Initialize network
    let mut network = Network::new(config.accumulator_dim, config.relu_clip, &mut rng);

    // Train the model
    println!("\nTraining...");
    create_dir_all(&out_dir)?;

    // Use BatchLoader if training with cache files
    if is_cache {
        train_model_with_loader(
            &mut network,
            train_samples,
            &validation_samples,
            &config,
            &out_dir,
            save_every,
            &mut rng,
        )?;
    } else {
        train_model(
            &mut network,
            &mut train_samples,
            &validation_samples,
            &config,
            &out_dir,
            save_every,
            &mut rng,
        )?;
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

    println!("\nModel saved to: {}", out_dir.display());

    Ok(())
}

fn load_samples(path: &str, config: &Config) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut samples = Vec::new();
    let mut skipped = 0;

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let pos_data: TrainingPosition = match serde_json::from_str(&line) {
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
                "cp" => (cp_black.clamp(-config.cp_clip, config.cp_clip) as f32) / 100.0,
                _ => continue,
            };
            samples.push(Sample {
                features,
                label,
                weight,
            });
        }

        // White perspective sample
        {
            let feats = extract_features(&position, white_king, Color::White);
            let features: Vec<u32> = feats.as_slice().iter().map(|&f| f as u32).collect();
            let label = match config.label_type.as_str() {
                "wdl" => cp_to_wdl(cp_white, config.scale),
                "cp" => (cp_white.clamp(-config.cp_clip, config.cp_clip) as f32) / 100.0,
                _ => continue,
            };
            samples.push(Sample {
                features,
                label,
                weight,
            });
        }
    }

    if skipped > 0 {
        println!("Skipped {} positions", skipped);
    }

    Ok(samples)
}

fn cp_to_wdl(cp: i32, scale: f32) -> f32 {
    1.0 / (1.0 + (-cp as f32 / scale).exp())
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
        weight *= (gap as f32 / 50.0).min(1.0);
    }

    // Bound-based weight
    let both_exact = is_exact_opt(&pos_data.bound1) && is_exact_opt(&pos_data.bound2);
    weight *= if both_exact { 1.0 } else { 0.7 };

    // Mate boundary weight
    if pos_data.mate_boundary.unwrap_or(false) {
        weight *= 0.5;
    }

    // Depth-based weight
    if let (Some(depth), Some(seldepth)) = (pos_data.depth, pos_data.seldepth) {
        if seldepth < depth.saturating_add(6) {
            weight *= 0.8;
        }
    }

    weight
}

fn load_samples_from_cache(path: &str) -> Result<Vec<Sample>, Box<dyn std::error::Error>> {
    let mut f = File::open(path)?;

    // Read header - matches build_feature_cache.rs v1 extended header
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic)?;
    if &magic != b"NNFC" {
        return Err(format!("Invalid cache file: bad magic (file {})", path).into());
    }

    let mut u32b = [0u8; 4];
    let mut u64b = [0u8; 8];

    // Version (v1 only)
    f.read_exact(&mut u32b)?;
    let version = u32::from_le_bytes(u32b);
    if version != 1 {
        return Err(format!(
            "Unsupported cache version: {} (v1 required) for file {}",
            version, path
        )
        .into());
    }

    // feature set id, num samples, chunk size
    f.read_exact(&mut u32b)?;
    let feature_set_id = u32::from_le_bytes(u32b);
    const FEATURE_SET_ID_HALF: u32 = 0x48414C46; // "HALF"
    if feature_set_id != FEATURE_SET_ID_HALF {
        return Err(format!(
            "Unsupported feature_set_id: 0x{:08x} for file {}",
            feature_set_id, path
        )
        .into());
    }

    f.read_exact(&mut u64b)?;
    let num_samples = u64::from_le_bytes(u64b);

    f.read_exact(&mut u32b)?;
    let _chunk_size = u32::from_le_bytes(u32b);

    // header_size
    f.read_exact(&mut u32b)?; // header_size
    let header_size = u32::from_le_bytes(u32b);
    if !(40..=4096).contains(&header_size) {
        return Err(format!("Unreasonable header_size: {} for file {}", header_size, path).into());
    }
    // endianness
    let mut b = [0u8; 1];
    f.read_exact(&mut b)?;
    let endianness = b[0];
    if endianness != 0 {
        return Err(format!(
            "Unsupported endianness in cache header (expected LE=0) for file {}",
            path
        )
        .into());
    }
    // payload_encoding
    f.read_exact(&mut b)?;
    let payload_encoding = b[0];
    // reserved16
    let mut _r16 = [0u8; 2];
    f.read_exact(&mut _r16)?;
    // payload_offset
    f.read_exact(&mut u64b)?;
    let payload_offset = u64::from_le_bytes(u64b);
    // sample_flags_mask
    f.read_exact(&mut u32b)?;
    let flags_mask = u32::from_le_bytes(u32b);
    // Skip to payload_offset if header had extra bytes
    let current = f.stream_position()?;
    if current < payload_offset {
        f.seek(SeekFrom::Start(payload_offset))?;
    }

    // Prepare reader for payload (gzip/zstd)
    let reader: Box<dyn Read> = match payload_encoding {
        0 => Box::new(f),
        1 => {
            use flate2::read::GzDecoder;
            Box::new(GzDecoder::new(f))
        }
        2 => {
            #[cfg(feature = "zstd")]
            {
                Box::new(zstd::Decoder::new(f)?)
            }
            #[cfg(not(feature = "zstd"))]
            {
                return Err(format!(
                    "zstd payload requires building with 'zstd' feature (file {})",
                    path
                )
                .into());
            }
        }
        other => {
            return Err(format!("Unknown payload encoding: {} for file {}", other, path).into())
        }
    };

    let mut r = BufReader::new(reader);

    println!("Loading cache: {num_samples} samples");

    let mut samples = Vec::with_capacity(num_samples as usize);
    let mut unknown_flag_samples: u64 = 0;
    let mut unknown_flag_bits_accum: u32 = 0;

    for i in 0..num_samples {
        if i % 100000 == 0 && i > 0 {
            println!("  Loaded {i}/{num_samples} samples...");
        }

        // Read number of features
        let mut nb = [0u8; 4];
        r.read_exact(&mut nb)?;
        let n_features = u32::from_le_bytes(nb) as usize;

        // Read features in bulk
        let mut buf = vec![0u8; n_features * 4];
        r.read_exact(&mut buf)?;
        let features: Vec<u32> = buf
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

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
        weight *= (gap as f32 / 50.0).min(1.0);

        // Exact bound weight
        let both_exact = (flags & 1) != 0;
        weight *= if both_exact { 1.0 } else { 0.7 };

        // Mate boundary weight
        if (flags & 2) != 0 {
            weight *= 0.5;
        }

        // Depth-based weight
        if seldepth < depth.saturating_add(6) {
            weight *= 0.8;
        }

        samples.push(Sample {
            features,
            label,
            weight,
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

fn train_model(
    network: &mut Network,
    train_samples: &mut [Sample],
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    out_dir: &Path,
    save_every: Option<usize>,
    rng: &mut StdRng,
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

        // Shuffle training data
        if config.shuffle {
            train_samples.shuffle(rng);
        }

        let mut total_loss = 0.0;
        let mut total_weight = 0.0;

        // Training
        for batch_idx in 0..n_batches {
            let start = batch_idx * config.batch_size;
            let end = (start + config.batch_size).min(n_samples);
            let batch = &train_samples[start..end];

            let batch_indices: Vec<usize> = (0..batch.len()).collect();
            let loss =
                train_batch_by_indices(network, batch, &batch_indices, config, &mut adam_state);
            let batch_weight: f32 = batch.iter().map(|s| s.weight).sum();
            total_loss += loss * batch_weight;
            total_weight += batch_weight;

            total_batches += 1;

            // Save checkpoint if requested
            if let Some(interval) = save_every {
                if total_batches % interval == 0 {
                    let checkpoint_path =
                        out_dir.join(format!("checkpoint_batch_{}.fp32.bin", total_batches));
                    save_network(network, &checkpoint_path)?;
                    println!("Saved checkpoint: {}", checkpoint_path.display());
                }
            }
        }

        let avg_loss = if total_weight > 0.0 {
            total_loss / total_weight
        } else {
            0.0
        };

        // Validation
        let val_loss = if let Some(val_samples) = validation_samples {
            compute_validation_loss(network, val_samples, config)
        } else {
            0.0
        };

        println!(
            "Epoch {}/{}: train_loss={:.4} val_loss={:.4} time={:.2}s",
            epoch + 1,
            config.epochs,
            avg_loss,
            val_loss,
            epoch_start.elapsed().as_secs_f32()
        );
    }

    Ok(())
}

fn train_model_with_loader(
    network: &mut Network,
    train_samples: Vec<Sample>,
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    out_dir: &Path,
    save_every: Option<usize>,
    rng: &mut StdRng,
) -> Result<(), Box<dyn std::error::Error>> {
    let train_samples_arc = Arc::new(train_samples);
    let mut batch_loader =
        BatchLoader::new(train_samples_arc.clone(), config.batch_size, config.shuffle, rng);

    let mut adam_state = if config.optimizer == "adam" {
        Some(AdamState::new(network))
    } else {
        None
    };

    let mut total_batches = 0;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        batch_loader.reset(config.shuffle, rng);

        let mut total_loss = 0.0;
        let mut total_weight = 0.0;
        let mut batch_count = 0;

        while let Some(indices) = batch_loader.next_batch() {
            let loss = train_batch_by_indices(
                network,
                &train_samples_arc,
                &indices,
                config,
                &mut adam_state,
            );
            let batch_weight: f32 = indices.iter().map(|&idx| train_samples_arc[idx].weight).sum();
            total_loss += loss * batch_weight;
            total_weight += batch_weight;

            batch_count += 1;
            total_batches += 1;

            // Save checkpoint if requested
            if let Some(interval) = save_every {
                if total_batches % interval == 0 {
                    let checkpoint_path =
                        out_dir.join(format!("checkpoint_batch_{total_batches}.fp32.bin"));
                    save_network(network, &checkpoint_path)?;
                    println!("Saved checkpoint: {}", checkpoint_path.display());
                }
            }
        }

        let avg_loss = if total_weight > 0.0 {
            total_loss / total_weight
        } else {
            0.0
        };

        // Validation
        let val_loss = if let Some(val_samples) = validation_samples {
            compute_validation_loss(network, val_samples, config)
        } else {
            0.0
        };

        println!(
            "Epoch {}/{}: train_loss={:.4} val_loss={:.4} batches={} time={:.2}s",
            epoch + 1,
            config.epochs,
            avg_loss,
            val_loss,
            batch_count,
            epoch_start.elapsed().as_secs_f32()
        );
    }

    Ok(())
}

fn train_batch_by_indices(
    network: &mut Network,
    samples: &[Sample],
    indices: &[usize],
    config: &Config,
    adam_state: &mut Option<AdamState>,
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
        config.learning_rate * (1.0 - adam.beta2.powf(t)).sqrt() / (1.0 - adam.beta1.powf(t))
    } else {
        config.learning_rate
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
        let zero_point = (-min_val / scale - 128.0).round().clamp(-128.0, 127.0) as i32;

        Self { scale, zero_point }
    }
}

// Quantize weights to int8
fn quantize_weights(weights: &[f32], params: &QuantizationParams) -> Vec<i8> {
    weights
        .iter()
        .map(|&w| {
            let quantized = (w / params.scale + params.zero_point as f32).round();
            quantized.clamp(-128.0, 127.0) as i8
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
    let quantized_size = (network.w0.len() + network.b0.len() + network.w2.len()) + 3 * 8 + 4;
    println!(
        "Quantized model saved. Size: {:.1} MB -> {:.1} MB ({:.1}% reduction)",
        original_size as f32 / 1024.0 / 1024.0,
        quantized_size as f32 / 1024.0 / 1024.0,
        (1.0 - quantized_size as f32 / original_size as f32) * 100.0
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

    fn write_v1_header(
        f: &mut File,
        feature_set_id: u32,
        num_samples: u64,
        chunk_size: u32,
        header_size: u32,
        endianness: u8,
        payload_encoding: u8,
        sample_flags_mask: u32,
    ) -> u64 {
        // Magic
        f.write_all(b"NNFC").unwrap();
        // version
        f.write_all(&1u32.to_le_bytes()).unwrap();
        // feature_set_id
        f.write_all(&feature_set_id.to_le_bytes()).unwrap();
        // num_samples
        f.write_all(&num_samples.to_le_bytes()).unwrap();
        // chunk_size
        f.write_all(&chunk_size.to_le_bytes()).unwrap();
        // header_size
        f.write_all(&header_size.to_le_bytes()).unwrap();
        // endianness
        f.write_all(&[endianness]).unwrap();
        // payload_encoding
        f.write_all(&[payload_encoding]).unwrap();
        // reserved16
        f.write_all(&[0u8; 2]).unwrap();
        // payload_offset = after magic (4 bytes) + header_size
        let payload_offset = 4u64 + header_size as u64;
        f.write_all(&payload_offset.to_le_bytes()).unwrap();
        // sample_flags_mask
        f.write_all(&sample_flags_mask.to_le_bytes()).unwrap();
        // pad header tail to header_size
        let written = 40usize; // fields after magic
        let pad = (header_size as usize).saturating_sub(written);
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
                &mut f, 0x48414C46, 0, 1024, 48, 1, // BE
                0, 0,
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
            let _off = write_v1_header(&mut f, 0x48414C46, 0, 1024, 48, 0, 3, 0);
            f.flush().unwrap();
            let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
            assert!(format!("{}", err).contains("Unknown payload encoding"));
        }

        // feature_set_id mismatch
        {
            let td = tempdir().unwrap();
            let path = td.path().join("featureset.cache");
            let mut f = File::create(&path).unwrap();
            let _off = write_v1_header(&mut f, 0x00000000, 0, 1024, 48, 0, 0, 0);
            f.flush().unwrap();
            let err = load_samples_from_cache(path.to_str().unwrap()).unwrap_err();
            assert!(format!("{}", err).contains("Unsupported feature_set_id"));
        }

        // header_size 極端値（0/8/4097）でエラー
        for bad_size in [0u32, 8u32, 4097u32] {
            let td = tempdir().unwrap();
            let path = td.path().join(format!("bad_hs_{bad_size}.cache"));
            let mut f = File::create(&path).unwrap();
            let _off = write_v1_header(&mut f, 0x48414C46, 0, 1024, bad_size, 0, 0, 0);
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
            let _off = write_v1_header(&mut f, 0x48414C46, 0, 1024, 64, 0, 0, 0);
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
            relu_clip: 127,
            shuffle: false,
            exclude_no_legal_move: false,
            exclude_fallback: false,
        };
        let json_samples = load_samples(json_path.to_str().unwrap(), &cfg).unwrap();
        // Two-sample orientation -> take first weight
        let w_json = json_samples[0].weight;

        // Build cache with a single sample (n_features=0) carrying same meta
        let cache_path = td.path().join("w.cache");
        {
            let mut f = File::create(&cache_path).unwrap();
            let payload_offset = write_v1_header(&mut f, 0x48414C46, 1, 1024, 48, 0, 0, 1u8 as u32);
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
        let off = write_v1_header(&mut f, 0x48414C46, 1, 1024, 48, 0, 0, 0);
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
            relu_clip: 127,
            shuffle: false,
            exclude_no_legal_move: false,
            exclude_fallback: false,
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
            relu_clip: 127,
            shuffle: false,
            exclude_no_legal_move: false,
            exclude_fallback: false,
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
        train_model(&mut net1, &mut samples1, &None, &cfg, out_dir, None, &mut dummy_rng1)
            .unwrap();
        train_model(&mut net2, &mut samples2, &None, &cfg, out_dir, None, &mut dummy_rng2)
            .unwrap();

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
        assert!(
            (net1.b2 - net2.b2).abs() <= eps,
            "b2 diff: {} vs {}",
            net1.b2,
            net2.b2
        );
    }
}
