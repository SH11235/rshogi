//! WDL (Win/Draw/Loss) baseline trainer for NNUE training data
//!
//! This tool reads JSONL format training data directly and trains a simple linear model
//! using logistic regression for WDL prediction.

use std::fs::{create_dir_all, File};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use clap::{arg, Command};
use engine_core::game_phase::{detect_game_phase, GamePhase, Profile};
use engine_core::Position;
use serde::{Deserialize, Serialize};
use tools::stats::{calibration_bins, roc_auc_weighted};

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
    cp: i32,
    #[allow(dead_code)]
    phase: GamePhase,
}

struct DashboardOpts<'a> {
    out_dir: &'a std::path::Path,
    emit_metrics: bool,
    calib_bins_n: usize,
    do_plots: bool,
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
                .value_parser(clap::value_parser!(i32))
                .default_value("1200"),
        )
        .arg(
            arg!(--"weight-gap-ref" <N> "Reference gap for weight calculation").default_value("50"),
        )
        .arg(arg!(--"weight-exact" <N> "Weight for exact bounds").default_value("1.0"))
        .arg(arg!(--"weight-non-exact" <N> "Weight for non-exact bounds").default_value("0.7"))
        .arg(arg!(--"exclude-no-legal-move" "Exclude positions with no legal moves"))
        .arg(arg!(--"exclude-fallback" "Exclude positions where fallback was used"))
        .arg(arg!(-o --out <DIR> "Output directory"))
        .arg(arg!(--"metrics" "Emit per-epoch metrics CSV").action(clap::ArgAction::SetTrue))
        .arg(
            arg!(--"calibration-bins" <N> "Bins for cp calibration")
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
            arg!(--"gate-mode" <MODE> "Gate behavior")
                .value_parser(["warn", "fail"])
                .default_value("warn"),
        )
        .get_matches();

    let config = Config {
        epochs: app.get_one::<String>("epochs").unwrap().parse()?,
        batch_size: app.get_one::<String>("batch-size").unwrap().parse()?,
        learning_rate: app.get_one::<String>("lr").unwrap().parse()?,
        l2_reg: app.get_one::<String>("l2").unwrap().parse()?,
        label_type: app.get_one::<String>("label").unwrap().to_string(),
        scale: *app.get_one::<f32>("scale").unwrap(),
        cp_clip: *app.get_one::<i32>("cp-clip").unwrap(),
        weight_gap_ref: app.get_one::<String>("weight-gap-ref").unwrap().parse()?,
        weight_exact: app.get_one::<String>("weight-exact").unwrap().parse()?,
        weight_non_exact: app.get_one::<String>("weight-non-exact").unwrap().parse()?,
        exclude_no_legal_move: app.get_flag("exclude-no-legal-move"),
        exclude_fallback: app.get_flag("exclude-fallback"),
    };

    let input_path = app.get_one::<String>("input").unwrap();
    let validation_path = app.get_one::<String>("validation");
    let emit_metrics = app.get_flag("metrics");
    let calib_bins_n = *app.get_one::<usize>("calibration-bins").unwrap_or(&40usize);
    let do_plots = app.get_flag("plots");
    let gate_last_epoch_best = app.get_flag("gate-val-loss-non-increase");
    let gate_mode_fail = app.get_one::<String>("gate-mode").map(|s| s == "fail").unwrap_or(false);

    // Sanity checks for numeric args
    if config.scale <= 0.0 {
        return Err("Invalid --scale: must be > 0".into());
    }
    if config.cp_clip < 0 {
        return Err("Invalid --cp-clip: must be >= 0".into());
    }
    if config.epochs == 0 || config.batch_size == 0 {
        return Err("Invalid --epochs/--batch-size: must be >= 1".into());
    }
    if !config.learning_rate.is_finite() || config.learning_rate <= 0.0 {
        return Err("Invalid --lr: must be > 0".into());
    }
    if !config.l2_reg.is_finite() || config.l2_reg < 0.0 {
        return Err("Invalid --l2: must be >= 0".into());
    }

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

    // Ensure output dir exists early
    create_dir_all(&out_dir)?;

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

    // Prepare metrics.csv if requested
    if emit_metrics {
        let mut w = csv::Writer::from_path(out_dir.join("metrics.csv"))?;
        w.write_record(["epoch", "train_loss", "val_loss", "val_auc", "time_sec"])?;
        w.flush()?;
    }

    let dash = DashboardOpts { out_dir: &out_dir, emit_metrics, calib_bins_n, do_plots };

    let best_is_last = train_model(
        &mut weights,
        &train_samples,
        &validation_samples,
        &config,
        &dash,
    )?;

    if gate_last_epoch_best {
        match (best_is_last, validation_samples.is_some()) {
            (true, true) => {
                println!("GATE val_loss_last_is_best: PASS");
            }
            (false, true) => {
                println!("GATE val_loss_last_is_best: FAIL");
                if gate_mode_fail {
                    std::process::exit(1);
                }
            }
            (_, false) => {
                println!("GATE val_loss_last_is_best: SKIP (no validation)");
            }
        }
    }

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
        let phase = detect_game_phase(&position, position.ply as u32, Profile::Search);

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
            cp,
            phase,
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
    dash: &DashboardOpts,
) -> Result<bool, Box<dyn std::error::Error>> {
    let n_samples = train_samples.len();
    let n_batches = n_samples.div_ceil(config.batch_size);
    let mut best_val_loss = f32::INFINITY;
    let mut best_epoch = 0usize;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        let mut total_loss = 0.0f64;
        let mut total_wsum = 0.0f64;

        // Training
        for batch_idx in 0..n_batches {
            let start = batch_idx * config.batch_size;
            let end = (start + config.batch_size).min(n_samples);
            let batch = &train_samples[start..end];

            let (loss_avg, batch_wsum, grad) = compute_batch_gradient(weights, batch, config);

            // Update weights
            for i in 0..weights.len() {
                weights[i] -= config.learning_rate * grad[i];
            }

            total_loss += (loss_avg as f64) * (batch_wsum as f64);
            total_wsum += batch_wsum as f64;
        }

        let avg_loss = if total_wsum > 0.0 {
            (total_loss / total_wsum) as f32
        } else {
            0.0
        };

        // Validation
        let (val_loss_opt, val_auc_opt) = if let Some(val_samples) = validation_samples {
            let (vl_opt, auc_opt) = compute_validation_metrics(weights, val_samples, config);
            (vl_opt, auc_opt)
        } else {
            (None, None)
        };

        if let Some(vl) = val_loss_opt {
            if vl < best_val_loss {
                best_val_loss = vl;
                best_epoch = epoch + 1;
            }
        }

        println!(
            "Epoch {}/{}: train_loss={:.4} val_loss={} time={:.2}s",
            epoch + 1,
            config.epochs,
            avg_loss,
            val_loss_opt.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
            epoch_start.elapsed().as_secs_f32()
        );

        if dash.emit_metrics {
            let mut w = csv::WriterBuilder::new().has_headers(false).from_writer(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dash.out_dir.join("metrics.csv"))?,
            );
            w.write_record([
                (epoch + 1).to_string(),
                format!("{:.6}", avg_loss),
                val_loss_opt.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                val_auc_opt.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                format!("{:.3}", epoch_start.elapsed().as_secs_f32()),
            ])?;
            w.flush()?;
        }

        // Calibration CSV/PNG for WDL when validation present
        if let (Some(val_samples), Some(_)) = (validation_samples, val_loss_opt) {
            if config.label_type == "wdl" {
                let mut cps = Vec::with_capacity(val_samples.len());
                let mut probs = Vec::with_capacity(val_samples.len());
                let mut labels = Vec::with_capacity(val_samples.len());
                let mut weights = Vec::with_capacity(val_samples.len());
                for s in val_samples.iter() {
                    let logit: f32 =
                        weights.iter().zip(s.features.iter()).map(|(w, f)| w * f).sum();
                    let p = 1.0 / (1.0 + (-logit).exp());
                    cps.push(s.cp);
                    probs.push(p);
                    labels.push(s.label);
                    weights.push(s.weight);
                }
                let bins = calibration_bins(
                    &cps,
                    &probs,
                    &labels,
                    &weights,
                    config.cp_clip,
                    dash.calib_bins_n,
                );
                // Write CSV
                let mut w = csv::Writer::from_path(dash.out_dir.join(format!(
                    "calibration_epoch_{}.csv",
                    epoch + 1
                )))?;
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
                    // map bins to tuple form expected by plot helper
                    let points: Vec<(i32, i32, f32, f64, f64)> = bins
                        .iter()
                        .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                        .collect();
                    let _ = tools::plot::plot_calibration_png(
                        dash.out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                        &points,
                    );
                }
            }
        }
    }

    Ok(best_epoch == config.epochs)
}

fn compute_batch_gradient(
    weights: &[f32],
    batch: &[Sample],
    config: &Config,
) -> (f32, f32, Vec<f32>) {
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

    (total_loss / total_weight, total_weight, gradient)
}

fn compute_validation_metrics(
    weights: &[f32],
    samples: &[Sample],
    config: &Config,
) -> (Option<f32>, Option<f64>) {
    if samples.is_empty() {
        return (None, None);
    }
    let mut total_loss = 0.0f64;
    let mut total_weight = 0.0f64;

    let mut probs: Vec<f32> = Vec::new();
    let mut labels: Vec<f32> = Vec::new();
    let mut weights_v: Vec<f32> = Vec::new();

    for sample in samples {
        let logit: f32 = weights.iter().zip(sample.features.iter()).map(|(w, f)| w * f).sum();

        let loss = match config.label_type.as_str() {
            "wdl" => {
                let (loss, _) = bce_with_logits(logit, sample.label);
                let p = 1.0 / (1.0 + (-logit).exp());
                probs.push(p);
                labels.push(sample.label);
                weights_v.push(sample.weight);
                loss
            }
            "cp" => {
                let error = logit - sample.label;
                0.5 * error * error
            }
            _ => unreachable!(),
        };

        total_loss += (loss as f64) * (sample.weight as f64);
        total_weight += sample.weight as f64;
    }

    let val_loss = if total_weight > 0.0 {
        Some((total_loss / total_weight) as f32)
    } else {
        None
    };
    let val_auc = if config.label_type == "wdl" {
        roc_auc_weighted(&probs, &labels, &weights_v)
    } else {
        None
    };
    (val_loss, val_auc)
}
