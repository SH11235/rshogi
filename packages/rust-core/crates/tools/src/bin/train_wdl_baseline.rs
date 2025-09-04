//! WDL (Win/Draw/Loss) baseline trainer for NNUE training data
//!
//! This tool reads JSONL format training data directly and trains a simple linear model
//! using logistic regression for WDL prediction.

use std::fs::{create_dir_all, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use clap::{arg, Command};
use engine_core::Position;
use serde::{Deserialize, Serialize};

#[allow(dead_code)]
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
    nodes: Option<u64>,
    #[serde(default)]
    time_ms: Option<u64>,
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

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct LineInfo {
    #[serde(default)]
    idx: u8,
    #[serde(default)]
    score_cp: Option<i32>,
    #[serde(default)]
    bound: Option<String>,
    #[serde(default)]
    depth: Option<u8>,
    #[serde(default)]
    seldepth: Option<u8>,
}

#[derive(Clone, Debug, Serialize)]
struct Config {
    epochs: usize,
    batch_size: usize,
    learning_rate: f32,
    l2_reg: f32,
    label_type: String,
    scale: f32,
    cp_clip: i32,
    weight_gap_ref: f32,
    weight_exact: f32,
    weight_non_exact: f32,
    exclude_no_legal_move: bool,
    exclude_fallback: bool,
}

#[derive(Clone, Debug)]
struct Sample {
    features: Vec<f32>,
    label: f32,
    weight: f32,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Command::new("train_wdl_baseline")
        .about("Train WDL baseline model from JSONL data")
        .arg(arg!(-i --input <FILE> "Input JSONL file").required(true))
        .arg(arg!(-v --validation <FILE> "Validation JSONL file"))
        .arg(arg!(-e --epochs <N> "Number of epochs").default_value("3"))
        .arg(arg!(-b --"batch-size" <N> "Batch size").default_value("4096"))
        .arg(arg!(--lr <RATE> "Learning rate").default_value("0.001"))
        .arg(arg!(--l2 <RATE> "L2 regularization").default_value("0.000001"))
        .arg(arg!(-l --label <TYPE> "Label type: wdl, cp, hybrid").default_value("wdl"))
        .arg(arg!(--scale <N> "Scale for cp->wdl conversion").default_value("600"))
        .arg(arg!(--"cp-clip" <N> "Clip CP values to this range").default_value("1200"))
        .arg(
            arg!(--"weight-gap-ref" <N> "Reference gap for weight calculation").default_value("50"),
        )
        .arg(arg!(--"weight-exact" <N> "Weight for exact bounds").default_value("1.0"))
        .arg(arg!(--"weight-non-exact" <N> "Weight for non-exact bounds").default_value("0.7"))
        .arg(arg!(--"exclude-no-legal-move" "Exclude positions with no legal moves"))
        .arg(arg!(--"exclude-fallback" "Exclude positions where fallback was used"))
        .arg(arg!(-o --out <DIR> "Output directory"))
        .get_matches();

    let config = Config {
        epochs: app.get_one::<String>("epochs").unwrap().parse()?,
        batch_size: app.get_one::<String>("batch-size").unwrap().parse()?,
        learning_rate: app.get_one::<String>("lr").unwrap().parse()?,
        l2_reg: app.get_one::<String>("l2").unwrap().parse()?,
        label_type: app.get_one::<String>("label").unwrap().to_string(),
        scale: app.get_one::<String>("scale").unwrap().parse()?,
        cp_clip: app.get_one::<String>("cp-clip").unwrap().parse()?,
        weight_gap_ref: app.get_one::<String>("weight-gap-ref").unwrap().parse()?,
        weight_exact: app.get_one::<String>("weight-exact").unwrap().parse()?,
        weight_non_exact: app.get_one::<String>("weight-non-exact").unwrap().parse()?,
        exclude_no_legal_move: app.get_flag("exclude-no-legal-move"),
        exclude_fallback: app.get_flag("exclude-fallback"),
    };

    let input_path = app.get_one::<String>("input").unwrap();
    let validation_path = app.get_one::<String>("validation");

    let timestamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    let out_dir = app
        .get_one::<String>("out")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("runs/wdl_baseline_{}", timestamp)));

    println!("Configuration:");
    println!("  Input: {}", input_path);
    if let Some(val_path) = validation_path {
        println!("  Validation: {}", val_path);
    }
    println!("  Output: {}", out_dir.display());
    println!("  Settings: {:?}", config);

    // Load training data
    let start_time = Instant::now();
    println!("\nLoading training data...");
    let train_samples = load_samples(input_path, &config)?;
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

    // Initialize model (simple linear model for now)
    // Features: bias(1) + side_to_move(1) + material_balance(7*2) + king_safety(2) = 18
    const FEATURE_DIM: usize = 18;
    let mut weights = vec![0.0f32; FEATURE_DIM];

    // Train the model
    println!("\nTraining...");
    train_model(&mut weights, &train_samples, &validation_samples, &config)?;

    // Save model and config
    create_dir_all(&out_dir)?;

    let mut weights_file = File::create(out_dir.join("weights.json"))?;
    writeln!(weights_file, "{}", serde_json::to_string_pretty(&weights)?)?;

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
            Err(e) => {
                eprintln!("Failed to parse line: {}", e);
                skipped += 1;
                continue;
            }
        };

        // Skip based on config
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

        let features = extract_features(&position);

        // Calculate label based on type
        let label = match config.label_type.as_str() {
            "wdl" => cp_to_wdl(cp, config.scale),
            "cp" => (cp.clamp(-config.cp_clip, config.cp_clip) as f32) / 100.0,
            _ => {
                eprintln!("Unknown label type: {}", config.label_type);
                continue;
            }
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

fn extract_features(pos: &Position) -> Vec<f32> {
    let mut features = vec![0.0f32; 18];
    let mut idx = 0;

    // Bias term
    features[idx] = 1.0;
    idx += 1;

    // Side to move
    features[idx] = if pos.side_to_move == engine_core::Color::Black {
        1.0
    } else {
        -1.0
    };
    idx += 1;

    // Material balance (piece counts)
    use engine_core::{Color, PieceType};

    for &pt in &[
        PieceType::Pawn,
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Rook,
    ] {
        let black_count =
            pos.board.piece_bb[Color::Black as usize][pt as usize].count_ones() as f32;
        let white_count =
            pos.board.piece_bb[Color::White as usize][pt as usize].count_ones() as f32;
        features[idx] = black_count - white_count;
        idx += 1;
    }

    // Hand pieces balance
    for i in 0..7 {
        let black_hand = pos.hands[Color::Black as usize][i] as f32;
        let white_hand = pos.hands[Color::White as usize][i] as f32;
        features[idx] = black_hand - white_hand;
        idx += 1;
    }

    // Simple king safety features (king position)
    let black_king = pos.board.king_square(Color::Black);
    let white_king = pos.board.king_square(Color::White);

    // Normalize king positions to [-1, 1]
    if let Some(bk) = black_king {
        features[idx] = (bk.rank() as f32 - 4.0) / 4.0;
    }
    idx += 1;
    if let Some(wk) = white_king {
        features[idx] = (wk.rank() as f32 - 4.0) / 4.0;
    }

    features
}

fn cp_to_wdl(cp: i32, scale: f32) -> f32 {
    1.0 / (1.0 + (-cp as f32 / scale).exp())
}

fn bce_with_logits(logit: f32, target: f32) -> (f32, f32) {
    // Numerically stable binary cross-entropy with logits
    // Returns (loss, gradient)
    let max_val = 0.0_f32.max(logit);
    let loss = max_val - logit * target + ((-logit.abs()).exp() + 1.0).ln();
    let sigmoid = 1.0 / (1.0 + (-logit).exp());
    let grad = sigmoid - target;
    (loss, grad)
}

fn calculate_weight(pos_data: &TrainingPosition, config: &Config) -> f32 {
    let mut weight = 1.0;

    // Gap-based weight
    if let Some(gap) = pos_data.best2_gap_cp {
        weight *= (gap as f32 / config.weight_gap_ref).min(1.0);
    }

    // Bound-based weight
    let both_exact =
        pos_data.bound1.as_deref() == Some("Exact") && pos_data.bound2.as_deref() == Some("Exact");
    weight *= if both_exact {
        config.weight_exact
    } else {
        config.weight_non_exact
    };

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

fn train_model(
    weights: &mut [f32],
    train_samples: &[Sample],
    validation_samples: &Option<Vec<Sample>>,
    config: &Config,
) -> Result<(), Box<dyn std::error::Error>> {
    let n_samples = train_samples.len();
    let n_batches = n_samples.div_ceil(config.batch_size);

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        let mut total_loss = 0.0;

        // Training
        for batch_idx in 0..n_batches {
            let start = batch_idx * config.batch_size;
            let end = (start + config.batch_size).min(n_samples);
            let batch = &train_samples[start..end];

            let (loss, grad) = compute_batch_gradient(weights, batch, config);

            // Update weights
            for i in 0..weights.len() {
                weights[i] -= config.learning_rate * grad[i];
            }

            total_loss += loss * batch.len() as f32;
        }

        let avg_loss = total_loss / n_samples as f32;

        // Validation
        let val_loss = if let Some(val_samples) = validation_samples {
            compute_validation_loss(weights, val_samples, config)
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

fn compute_batch_gradient(weights: &[f32], batch: &[Sample], config: &Config) -> (f32, Vec<f32>) {
    let mut total_loss = 0.0;
    let mut gradient = vec![0.0f32; weights.len()];
    let mut total_weight = 0.0;

    for sample in batch {
        // Forward pass
        let logit: f32 = weights.iter().zip(sample.features.iter()).map(|(w, f)| w * f).sum();

        // Compute loss and gradient based on label type
        let (loss, grad_factor) = match config.label_type.as_str() {
            "wdl" => bce_with_logits(logit, sample.label),
            "cp" => {
                let error = logit - sample.label;
                let loss = 0.5 * error * error;
                (loss, error)
            }
            _ => unreachable!(),
        };

        total_loss += loss * sample.weight;
        total_weight += sample.weight;

        // Accumulate gradient
        for (grad, feat) in gradient.iter_mut().zip(sample.features.iter()) {
            *grad += grad_factor * feat * sample.weight;
        }
    }

    // Average gradient and add L2 regularization
    for (i, grad) in gradient.iter_mut().enumerate() {
        *grad = *grad / total_weight + config.l2_reg * weights[i];
    }

    (total_loss / total_weight, gradient)
}

fn compute_validation_loss(weights: &[f32], samples: &[Sample], config: &Config) -> f32 {
    let mut total_loss = 0.0;
    let mut total_weight = 0.0;

    for sample in samples {
        let logit: f32 = weights.iter().zip(sample.features.iter()).map(|(w, f)| w * f).sum();

        let loss = match config.label_type.as_str() {
            "wdl" => {
                let (loss, _) = bce_with_logits(logit, sample.label);
                loss
            }
            "cp" => {
                let error = logit - sample.label;
                0.5 * error * error
            }
            _ => unreachable!(),
        };

        total_loss += loss * sample.weight;
        total_weight += sample.weight;
    }

    total_loss / total_weight
}
