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
use rand::rngs::StdRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use tools::stats::{
    binary_metrics, calibration_bins, ece_from_bins, regression_metrics, roc_auc_weighted,
};

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
            arg!(--"weight-gap-ref" <N> "Reference gap for weight calculation")
                .value_parser(clap::value_parser!(f32))
                .default_value("50"),
        )
        .arg(
            arg!(--"weight-exact" <N> "Weight for exact bounds")
                .value_parser(clap::value_parser!(f32))
                .default_value("1.0"),
        )
        .arg(
            arg!(--"weight-non-exact" <N> "Weight for non-exact bounds")
                .value_parser(clap::value_parser!(f32))
                .default_value("0.7"),
        )
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
            arg!(--"seed" <N> "Shuffle seed for reproducibility")
                .value_parser(clap::value_parser!(u64)),
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
        .get_matches();

    let config = Config {
        epochs: app.get_one::<String>("epochs").unwrap().parse()?,
        batch_size: app.get_one::<String>("batch-size").unwrap().parse()?,
        learning_rate: app.get_one::<String>("lr").unwrap().parse()?,
        l2_reg: app.get_one::<String>("l2").unwrap().parse()?,
        label_type: app.get_one::<String>("label").unwrap().to_string(),
        scale: *app.get_one::<f32>("scale").unwrap(),
        cp_clip: *app.get_one::<i32>("cp-clip").unwrap(),
        weight_gap_ref: *app.get_one::<f32>("weight-gap-ref").unwrap(),
        weight_exact: *app.get_one::<f32>("weight-exact").unwrap(),
        weight_non_exact: *app.get_one::<f32>("weight-non-exact").unwrap(),
        exclude_no_legal_move: app.get_flag("exclude-no-legal-move"),
        exclude_fallback: app.get_flag("exclude-fallback"),
    };

    let input_path = app.get_one::<String>("input").unwrap();
    let validation_path = app.get_one::<String>("validation");
    let emit_metrics = app.get_flag("metrics");
    let calib_bins_n = *app.get_one::<usize>("calibration-bins").unwrap_or(&40usize);
    let do_plots = app.get_flag("plots");
    let seed_opt = app.get_one::<u64>("seed").copied();
    let gate_last_epoch_best = app.get_flag("gate-val-loss-non-increase");
    let gate_min_auc = app.get_one::<f64>("gate-min-auc").copied();
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
        // Phase metrics header
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

    let dash = DashboardOpts {
        out_dir: &out_dir,
        emit_metrics,
        calib_bins_n,
        do_plots,
    };

    let best_is_last =
        train_model(&mut weights, &train_samples, &validation_samples, &config, &dash, seed_opt)?;

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

    // AUC threshold gate (wdl only)
    if let (Some(th), Some(val_samples)) = (gate_min_auc, validation_samples.as_ref()) {
        if config.label_type == "wdl" {
            // Compute a final AUC on validation with final weights
            let mut probs: Vec<f32> = Vec::with_capacity(val_samples.len());
            let mut labels: Vec<f32> = Vec::with_capacity(val_samples.len());
            let mut wts: Vec<f32> = Vec::with_capacity(val_samples.len());
            for s in val_samples.iter() {
                let logit: f32 = weights.iter().zip(s.features.iter()).map(|(w, f)| w * f).sum();
                let p = 1.0 / (1.0 + (-logit).exp());
                probs.push(p);
                labels.push(s.label);
                wts.push(s.weight);
            }
            let auc = roc_auc_weighted(&probs, &labels, &wts);
            match auc {
                Some(v) => {
                    let pass = v >= th;
                    println!(
                        "GATE min_auc {:.4} >= {:.4}: {}",
                        v,
                        th,
                        if pass { "PASS" } else { "FAIL" }
                    );
                    if !pass && gate_mode_fail {
                        std::process::exit(1);
                    }
                }
                None => {
                    println!("GATE min_auc: SKIP (insufficient positive/negative)");
                }
            }
        } else {
            println!("GATE min_auc: SKIP (label_type!=wdl)");
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
    seed_opt: Option<u64>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let n_samples = train_samples.len();
    let n_batches = n_samples.div_ceil(config.batch_size);
    let mut best_val_loss = f32::INFINITY;
    let mut last_val_loss: Option<f32> = None;
    let mut best_weights: Option<Vec<f32>> = None;

    for epoch in 0..config.epochs {
        let epoch_start = Instant::now();
        let mut total_loss = 0.0f64;
        let mut total_wsum = 0.0f64;
        // Shuffle order per epoch for SGD stability
        let mut order: Vec<usize> = (0..n_samples).collect();
        if let Some(seed) = seed_opt {
            let mut rng =
                StdRng::seed_from_u64(seed ^ (epoch as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
            order.as_mut_slice().shuffle(&mut rng);
        } else {
            let mut rng = rand::rng();
            order.as_mut_slice().shuffle(&mut rng);
        }

        // Training
        let mut zero_weight_batches = 0usize;
        for batch_idx in 0..n_batches {
            let start = batch_idx * config.batch_size;
            let end = (start + config.batch_size).min(n_samples);
            let batch_indices = &order[start..end];

            let (loss_avg, batch_wsum, grad) =
                compute_batch_gradient_over_indices(weights, train_samples, batch_indices, config);

            // Update weights
            for i in 0..weights.len() {
                weights[i] -= config.learning_rate * grad[i];
            }

            total_loss += (loss_avg as f64) * (batch_wsum as f64);
            total_wsum += batch_wsum as f64;
            if batch_wsum == 0.0 {
                zero_weight_batches += 1;
            }
        }

        let avg_loss = if total_wsum > 0.0 {
            (total_loss / total_wsum) as f32
        } else {
            0.0
        };

        // Validation
        let (val_loss_opt, val_auc_opt, val_wsum_opt) =
            if let Some(val_samples) = validation_samples {
                let (vl_opt, auc_opt, wsum_opt) =
                    compute_validation_metrics(weights, val_samples, config);
                (vl_opt, auc_opt, wsum_opt)
            } else {
                (None, None, None)
            };

        let mut is_epoch_best = false;
        if let Some(vl) = val_loss_opt {
            if vl < best_val_loss {
                best_val_loss = vl;
                best_weights = Some(weights.to_vec());
                is_epoch_best = true;
            }
            last_val_loss = Some(vl);
        }

        if let Some(auc) = val_auc_opt {
            println!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} val_auc={:.4} time={:.2}s",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss_opt.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
                auc,
                epoch_start.elapsed().as_secs_f32()
            );
        } else {
            println!(
                "Epoch {}/{}: train_loss={:.4} val_loss={} time={:.2}s",
                epoch + 1,
                config.epochs,
                avg_loss,
                val_loss_opt.map(|v| format!("{:.4}", v)).unwrap_or_else(|| "NA".into()),
                epoch_start.elapsed().as_secs_f32()
            );
        }

        // Calibration CSV/PNG for WDL when validation present
        let mut val_ece_opt: Option<f64> = None;
        if let (Some(val_samples), Some(_)) = (validation_samples, val_loss_opt) {
            if config.label_type == "wdl" {
                let mut cps = Vec::with_capacity(val_samples.len());
                let mut probs = Vec::with_capacity(val_samples.len());
                let mut labels = Vec::with_capacity(val_samples.len());
                let mut sample_w = Vec::with_capacity(val_samples.len());
                for s in val_samples.iter() {
                    let logit: f32 =
                        weights.iter().zip(s.features.iter()).map(|(w, f)| w * f).sum();
                    let p = 1.0 / (1.0 + (-logit).exp());
                    cps.push(s.cp);
                    probs.push(p);
                    labels.push(s.label);
                    sample_w.push(s.weight);
                }
                let bins = calibration_bins(
                    &cps,
                    &probs,
                    &labels,
                    &sample_w,
                    config.cp_clip,
                    dash.calib_bins_n,
                );
                val_ece_opt = ece_from_bins(&bins);
                // Write CSV
                let mut w = csv::Writer::from_path(
                    dash.out_dir.join(format!("calibration_epoch_{}.csv", epoch + 1)),
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
                    // map bins to tuple form expected by plot helper
                    let points: Vec<(i32, i32, f32, f64, f64)> = bins
                        .iter()
                        .map(|b| (b.left, b.right, b.center, b.mean_pred, b.mean_label))
                        .collect();
                    if let Err(e) = tools::plot::plot_calibration_png(
                        dash.out_dir.join(format!("calibration_epoch_{}.png", epoch + 1)),
                        &points,
                    ) {
                        eprintln!("plot_calibration_png failed: {}", e);
                    }
                }
            }
        }

        // Phase metrics (validation-based)
        if let Some(val_samples) = validation_samples {
            // Use fixed index buckets instead of maps to avoid extra trait bounds
            #[inline]
            fn idx_of(phase: GamePhase) -> usize {
                match phase {
                    GamePhase::Opening => 0,
                    GamePhase::MiddleGame => 1,
                    GamePhase::EndGame => 2,
                }
            }
            let mut probs_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
            let mut cp_buckets: [(Vec<f32>, Vec<f32>, Vec<f32>); 3] = Default::default();
            match config.label_type.as_str() {
                "wdl" => {
                    for s in val_samples.iter() {
                        let logit: f32 =
                            weights.iter().zip(s.features.iter()).map(|(w, f)| w * f).sum();
                        let p = 1.0 / (1.0 + (-logit).exp());
                        let b = &mut probs_buckets[idx_of(s.phase)];
                        b.0.push(p);
                        b.1.push(s.label);
                        b.2.push(s.weight);
                    }
                }
                "cp" => {
                    for s in val_samples.iter() {
                        let pred: f32 =
                            weights.iter().zip(s.features.iter()).map(|(w, f)| w * f).sum();
                        let b = &mut cp_buckets[idx_of(s.phase)];
                        b.0.push(pred);
                        b.1.push(s.label);
                        b.2.push(s.weight);
                    }
                }
                _ => {}
            }
            let mut wpm = csv::WriterBuilder::new().has_headers(false).from_writer(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dash.out_dir.join("phase_metrics.csv"))?,
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

        // Optional debug about zero-weight batches
        if zero_weight_batches > 0 {
            eprintln!(
                "[debug] epoch {} had {} zero-weight batches",
                epoch + 1,
                zero_weight_batches
            );
        }

        // Write metrics row (after computing optional ECE)
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
                val_ece_opt.map(|v| format!("{:.6}", v)).unwrap_or_else(|| "".into()),
                format!("{:.3}", epoch_start.elapsed().as_secs_f32()),
                format!("{:.3}", total_wsum),
                val_wsum_opt.map(|v| format!("{:.3}", v)).unwrap_or_else(|| "".into()),
                if is_epoch_best {
                    "1".into()
                } else {
                    "0".into()
                },
            ])?;
            w.flush()?;
        }
    }

    // Save best weights if available
    if let Some(wb) = best_weights {
        let mut f = File::create(dash.out_dir.join("weights_best.json"))?;
        writeln!(f, "{}", serde_json::to_string_pretty(&wb)?)?;
        println!(
            "Saved best validation weights to {}",
            dash.out_dir.join("weights_best.json").display()
        );
    }

    // Gate condition: last epoch should be best within small epsilon margin
    let eps = 1e-6_f32;
    let pass = match (last_val_loss, best_val_loss.is_finite()) {
        (Some(last), true) => last <= best_val_loss + eps,
        _ => true, // no validation -> skip gate
    };
    Ok(pass)
}

fn compute_batch_gradient_over_indices(
    weights: &[f32],
    samples: &[Sample],
    indices: &[usize],
    config: &Config,
) -> (f32, f32, Vec<f32>) {
    let mut total_loss = 0.0f32;
    let mut gradient = vec![0.0f32; weights.len()];
    let mut total_weight = 0.0f32;

    for &idx in indices {
        let sample = &samples[idx];
        let logit: f32 = weights.iter().zip(sample.features.iter()).map(|(w, f)| w * f).sum();
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
        for (g, feat) in gradient.iter_mut().zip(sample.features.iter()) {
            *g += grad_factor * feat * sample.weight;
        }
    }
    for (i, grad) in gradient.iter_mut().enumerate() {
        let wd = if i == 0 { 0.0 } else { config.l2_reg };
        *grad = if total_weight > 0.0 {
            *grad / total_weight + wd * weights[i]
        } else {
            wd * weights[i]
        };
    }
    let loss_avg = if total_weight > 0.0 {
        total_loss / total_weight
    } else {
        0.0
    };
    (loss_avg, total_weight, gradient)
}

fn compute_validation_metrics(
    weights: &[f32],
    samples: &[Sample],
    config: &Config,
) -> (Option<f32>, Option<f64>, Option<f64>) {
    if samples.is_empty() {
        return (None, None, None);
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
    (
        val_loss,
        val_auc,
        if total_weight > 0.0 {
            Some(total_weight)
        } else {
            None
        },
    )
}
