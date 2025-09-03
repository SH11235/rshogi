//! NNUE (Efficiently Updatable Neural Network) trainer
//!
//! This tool trains NNUE models directly from JSONL training data.
//! It supports HalfKP features and row-sparse updates for efficient training.

use std::fs::{create_dir_all, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use clap::{arg, Command};
use engine_core::{
    evaluation::nnue::features::{extract_features, FE_END},
    Color, Position,
};
use rand::{seq::SliceRandom, Rng};
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
}

#[derive(Clone)]
struct Sample {
    features: Vec<usize>, // Active feature indices for both perspectives
    label: f32,
    weight: f32,
}

#[derive(Clone)]
struct Network {
    // Input layer: HalfKP features -> accumulator
    w0: Vec<f32>, // [FE_END * ACC_DIM]
    b0: Vec<f32>, // [ACC_DIM]

    // Output layer: accumulator -> score
    w2: Vec<f32>, // [ACC_DIM]
    b2: f32,

    // Hyperparameters
    acc_dim: usize,
    relu_clip: f32,
}

impl Network {
    fn new(acc_dim: usize, relu_clip: i32) -> Self {
        let mut rng = rand::rng();

        // Initialize weights with small random values
        let w0_size = FE_END * acc_dim;
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
            acc_dim,
            relu_clip: relu_clip as f32,
        }
    }

    fn forward(&self, features: &[usize]) -> (f32, Vec<f32>) {
        // Accumulator = b0 + sum(W0[features])
        let mut acc = self.b0.clone();

        for &feat_idx in features {
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
        .arg(arg!(--"save-every" <N> "Save checkpoint every N batches"))
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
        shuffle: app.contains_id("shuffle"),
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
    println!("  Feature dimension: {} (HalfKP)", FE_END);
    println!("  Network: {} -> {} -> 1", FE_END, config.accumulator_dim);

    // Load training data
    let start_time = Instant::now();
    println!("\nLoading training data...");
    let mut train_samples = load_samples(input_path, &config)?;
    println!(
        "Loaded {} samples in {:.2}s",
        train_samples.len(),
        start_time.elapsed().as_secs_f32()
    );

    // Load validation data if provided
    let validation_samples = if let Some(val_path) = validation_path {
        println!("\nLoading validation data...");
        let start_val = Instant::now();
        let samples = load_samples(val_path, &config)?;
        println!(
            "Loaded {} validation samples in {:.2}s",
            samples.len(),
            start_val.elapsed().as_secs_f32()
        );
        Some(samples)
    } else {
        None
    };

    // Initialize network
    let mut network = Network::new(config.accumulator_dim, config.relu_clip);

    // Train the model
    println!("\nTraining...");
    create_dir_all(&out_dir)?;
    train_model(
        &mut network,
        &mut train_samples,
        &validation_samples,
        &config,
        &out_dir,
        save_every,
    )?;

    // Save final model
    save_network(&network, &out_dir.join("nn.fp32.bin"))?;

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

        // Skip positions with no legal moves or fallback
        if pos_data.no_legal_move.unwrap_or(false) || pos_data.fallback_used.unwrap_or(false) {
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
        let black_king = position.board.king_square(Color::Black).unwrap();
        let white_king = position.board.king_square(Color::White).unwrap();

        let mut features = Vec::new();
        // Extract features from black's perspective
        let black_features = extract_features(&position, black_king, Color::Black);
        features.extend_from_slice(black_features.as_slice());
        // Extract features from white's perspective
        let white_features = extract_features(&position, white_king, Color::White);
        features.extend_from_slice(white_features.as_slice());

        // Calculate label
        let label = match config.label_type.as_str() {
            "wdl" => cp_to_wdl(cp, config.scale),
            "cp" => (cp.clamp(-config.cp_clip, config.cp_clip) as f32) / 100.0,
            _ => continue,
        };

        // Calculate sample weight
        let weight = calculate_weight(&pos_data, config);

        samples.push(Sample {
            features,
            label,
            weight,
        });
    }

    if skipped > 0 {
        println!("Skipped {} positions", skipped);
    }

    Ok(samples)
}

fn cp_to_wdl(cp: i32, scale: f32) -> f32 {
    1.0 / (1.0 + (-cp as f32 / scale).exp())
}

fn calculate_weight(pos_data: &TrainingPosition, _config: &Config) -> f32 {
    let mut weight = 1.0;

    // Gap-based weight
    if let Some(gap) = pos_data.best2_gap_cp {
        weight *= (gap as f32 / 50.0).min(1.0);
    }

    // Bound-based weight
    let both_exact =
        pos_data.bound1.as_deref() == Some("Exact") && pos_data.bound2.as_deref() == Some("Exact");
    weight *= if both_exact { 1.0 } else { 0.7 };

    // Mate boundary weight
    if pos_data.mate_boundary.unwrap_or(false) {
        weight *= 0.5;
    }

    // Depth-based weight
    if let (Some(depth), Some(seldepth)) = (pos_data.depth, pos_data.seldepth) {
        if seldepth < depth + 6 {
            weight *= 0.8;
        }
    }

    weight
}

fn train_model(
    network: &mut Network,
    train_samples: &mut [Sample],
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
    out_dir: &Path,
    save_every: Option<usize>,
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
            train_samples.shuffle(&mut rand::rng());
        }

        let mut total_loss = 0.0;

        // Training
        for batch_idx in 0..n_batches {
            let start = batch_idx * config.batch_size;
            let end = (start + config.batch_size).min(n_samples);
            let batch = &train_samples[start..end];

            let loss = train_batch(network, batch, config, &mut adam_state);
            total_loss += loss * batch.len() as f32;

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

        let avg_loss = total_loss / n_samples as f32;

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

fn train_batch(
    network: &mut Network,
    batch: &[Sample],
    config: &Config,
    adam_state: &mut Option<AdamState>,
) -> f32 {
    let batch_size = batch.len() as f32;
    let mut total_loss = 0.0;

    // Gradients
    let mut grad_w0 = vec![0.0f32; network.w0.len()];
    let mut grad_b0 = vec![0.0f32; network.b0.len()];
    let mut grad_w2 = vec![0.0f32; network.w2.len()];
    let mut grad_b2 = 0.0f32;

    for sample in batch {
        // Forward pass
        let (output, activated) = network.forward(&sample.features);

        // Compute loss and output gradient
        let (loss, grad_output) = match config.label_type.as_str() {
            "wdl" => {
                let pred = 1.0 / (1.0 + (-output).exp());
                let loss = -(sample.label * pred.ln() + (1.0 - sample.label) * (1.0 - pred).ln());
                let grad = pred - sample.label;
                (loss, grad)
            }
            "cp" => {
                let error = output - sample.label;
                let loss = 0.5 * error * error;
                (loss, error)
            }
            _ => unreachable!(),
        };

        total_loss += loss * sample.weight;
        let weighted_grad = grad_output * sample.weight;

        // Backward pass - output layer
        grad_b2 += weighted_grad;
        for i in 0..network.acc_dim {
            grad_w2[i] += weighted_grad * activated[i];
        }

        // Backward pass - hidden layer (row-sparse update)
        for (i, grad_b0_val) in grad_b0.iter_mut().enumerate().take(network.acc_dim) {
            let grad_act = weighted_grad * network.w2[i];

            // ReLU derivative
            let acc_value = network.b0[i]
                + sample
                    .features
                    .iter()
                    .map(|&f| network.w0[f * network.acc_dim + i])
                    .sum::<f32>();

            if acc_value > 0.0 && acc_value < network.relu_clip {
                *grad_b0_val += grad_act;

                // Update only active features
                for &feat_idx in &sample.features {
                    let idx = feat_idx * network.acc_dim + i;
                    grad_w0[idx] += grad_act;
                }
            }
        }
    }

    // Average gradients
    for g in grad_w0.iter_mut() {
        *g /= batch_size;
    }
    for g in grad_b0.iter_mut() {
        *g /= batch_size;
    }
    for g in grad_w2.iter_mut() {
        *g /= batch_size;
    }
    grad_b2 /= batch_size;

    // Add L2 regularization
    for (i, grad) in grad_w0.iter_mut().enumerate() {
        *grad += config.l2_reg * network.w0[i];
    }
    for (i, grad) in grad_w2.iter_mut().enumerate() {
        *grad += config.l2_reg * network.w2[i];
    }

    // Update parameters
    if let Some(adam) = adam_state {
        // Adam optimizer
        adam.t += 1;
        let t = adam.t as f32;
        let lr_t =
            config.learning_rate * (1.0 - adam.beta2.powf(t)).sqrt() / (1.0 - adam.beta1.powf(t));

        // Update w0 (sparse)
        for (i, &grad) in grad_w0.iter().enumerate() {
            if grad != 0.0 {
                adam.m_w0[i] = adam.beta1 * adam.m_w0[i] + (1.0 - adam.beta1) * grad;
                adam.v_w0[i] = adam.beta2 * adam.v_w0[i] + (1.0 - adam.beta2) * grad * grad;
                network.w0[i] -= lr_t * adam.m_w0[i] / (adam.v_w0[i].sqrt() + adam.epsilon);
            }
        }

        // Update b0
        for (i, &grad) in grad_b0.iter().enumerate() {
            adam.m_b0[i] = adam.beta1 * adam.m_b0[i] + (1.0 - adam.beta1) * grad;
            adam.v_b0[i] = adam.beta2 * adam.v_b0[i] + (1.0 - adam.beta2) * grad * grad;
            network.b0[i] -= lr_t * adam.m_b0[i] / (adam.v_b0[i].sqrt() + adam.epsilon);
        }

        // Update w2
        for (i, &grad) in grad_w2.iter().enumerate() {
            adam.m_w2[i] = adam.beta1 * adam.m_w2[i] + (1.0 - adam.beta1) * grad;
            adam.v_w2[i] = adam.beta2 * adam.v_w2[i] + (1.0 - adam.beta2) * grad * grad;
            network.w2[i] -= lr_t * adam.m_w2[i] / (adam.v_w2[i].sqrt() + adam.epsilon);
        }

        // Update b2
        adam.m_b2 = adam.beta1 * adam.m_b2 + (1.0 - adam.beta1) * grad_b2;
        adam.v_b2 = adam.beta2 * adam.v_b2 + (1.0 - adam.beta2) * grad_b2 * grad_b2;
        network.b2 -= lr_t * adam.m_b2 / (adam.v_b2.sqrt() + adam.epsilon);
    } else {
        // SGD optimizer
        for (i, &grad) in grad_w0.iter().enumerate() {
            if grad != 0.0 {
                network.w0[i] -= config.learning_rate * grad;
            }
        }
        for (i, &grad) in grad_b0.iter().enumerate() {
            network.b0[i] -= config.learning_rate * grad;
        }
        for (i, &grad) in grad_w2.iter().enumerate() {
            network.w2[i] -= config.learning_rate * grad;
        }
        network.b2 -= config.learning_rate * grad_b2;
    }

    total_loss / batch_size
}

fn compute_validation_loss(network: &Network, samples: &[Sample], config: &Config) -> f32 {
    let mut total_loss = 0.0;
    let mut total_weight = 0.0;

    for sample in samples {
        let (output, _) = network.forward(&sample.features);

        let loss = match config.label_type.as_str() {
            "wdl" => {
                let pred = 1.0 / (1.0 + (-output).exp());
                -(sample.label * pred.ln() + (1.0 - sample.label) * (1.0 - pred).ln())
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

fn save_network(network: &Network, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = File::create(path)?;

    // Write header
    writeln!(file, "NNUE")?;
    writeln!(file, "VERSION 1")?;
    writeln!(file, "FEATURES HALFKP")?;
    writeln!(file, "ACC_DIM {}", network.acc_dim)?;
    writeln!(file, "RELU_CLIP {}", network.relu_clip)?;
    writeln!(file, "WEIGHTS")?;

    // Write weights (in binary format in real implementation)
    // For now, just save as text for debugging
    for w in &network.w0 {
        writeln!(file, "{}", w)?;
    }
    for b in &network.b0 {
        writeln!(file, "{}", b)?;
    }
    for w in &network.w2 {
        writeln!(file, "{}", w)?;
    }
    writeln!(file, "{}", network.b2)?;

    Ok(())
}
