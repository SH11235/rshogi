use anyhow::{anyhow, Result};
use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimits;
use engine_core::Position;
use std::fs::File;
use std::io::{BufRead, BufReader};

/// Validate a CP-labeled dataset by sampling lines and re-evaluating with the engine.
/// Usage:
///   cargo run --release -p tools --bin validate_cp_dataset -- <dataset_file> [sample_size] [depth] [time_ms] [engine]
/// Example:
///   cargo run --release -p tools --bin validate_cp_dataset -- runs/full_.../dataset_cp/train.txt 200 1 500 enhanced
fn main() -> Result<()> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <dataset_file> [sample_size] [depth] [time_ms] [engine]", args[0]);
        eprintln!("  sample_size: default 200");
        eprintln!("  depth: default 1");
        eprintln!("  time_ms: default 500");
        eprintln!("  engine: material|enhanced|nnue|enhanced-nnue (default enhanced)");
        std::process::exit(1);
    }

    let path = &args[1];
    let sample_size = args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(200);
    let depth = args.get(3).and_then(|s| s.parse::<u8>().ok()).unwrap_or(1);
    let time_ms = args.get(4).and_then(|s| s.parse::<u64>().ok()).unwrap_or(500);
    let engine = match args.get(5).map(|s| s.to_ascii_lowercase()) {
        Some(ref s) if s == "material" => EngineType::Material,
        Some(ref s) if s == "nnue" => EngineType::Nnue,
        Some(ref s) if s == "enhanced-nnue" || s == "enhanced_nnue" => EngineType::EnhancedNnue,
        _ => EngineType::Enhanced,
    };

    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;
    if lines.is_empty() {
        return Err(anyhow!("Dataset is empty: {}", path));
    }

    // Downsample deterministically: take evenly spaced indices
    let n = lines.len();
    let take = sample_size.min(n);
    let mut sample = Vec::with_capacity(take);
    for i in 0..take {
        let idx = i * n / take;
        sample.push(std::mem::take(&mut lines[idx]));
    }

    println!("validate_cp_dataset");
    println!("====================");
    println!("file: {}", path);
    println!("lines: {}", n);
    println!("sample: {}", take);
    println!("engine: {:?}", engine);
    println!("depth: {} time_ms: {}", depth, time_ms);

    let mut engine = Engine::new(engine);
    engine.set_hash_size(16);

    let mut parsed = 0usize;
    let mut failed_parse = 0usize;
    let mut evaluated = 0usize;
    let mut sum_x = 0f64; // label
    let mut sum_y = 0f64; // re-eval
    let mut sum_x2 = 0f64;
    let mut sum_y2 = 0f64;
    let mut sum_xy = 0f64;
    let mut mse = 0f64;
    let mut mae = 0f64;

    for line in &sample {
        // Expect format: "<SFENish> eval <cp> ..."
        let Some(eval_idx) = line.find(" eval ") else {
            failed_parse += 1;
            continue;
        };
        let sfen_str = &line[..eval_idx].trim();
        let rest = &line[eval_idx + 6..];
        let cp_str = rest
            .split_whitespace()
            .next()
            .ok_or_else(|| anyhow!("no cp token"))
            .unwrap_or("");
        let Ok(cp) = cp_str.parse::<i32>() else {
            failed_parse += 1;
            continue;
        };
        parsed += 1;

        // Evaluate quickly
        let mut pos = match Position::from_sfen(sfen_str) {
            Ok(p) => p,
            Err(_) => {
                failed_parse += 1;
                continue;
            }
        };
        let limits = SearchLimits::builder().depth(depth).fixed_time_ms(time_ms).build();
        let res = engine.search(&mut pos, limits);
        let y = res.score as f64;
        let x = cp as f64;
        evaluated += 1;

        sum_x += x;
        sum_y += y;
        sum_x2 += x * x;
        sum_y2 += y * y;
        sum_xy += x * y;
        let diff = y - x;
        mse += diff * diff;
        mae += diff.abs();
    }

    if evaluated == 0 {
        return Err(anyhow!("No lines evaluated"));
    }

    let n_e = evaluated as f64;
    mse /= n_e;
    mae /= n_e;
    let cov = sum_xy / n_e - (sum_x / n_e) * (sum_y / n_e);
    let var_x = sum_x2 / n_e - (sum_x / n_e).powi(2);
    let var_y = sum_y2 / n_e - (sum_y / n_e).powi(2);
    let corr = if var_x > 0.0 && var_y > 0.0 {
        cov / (var_x.sqrt() * var_y.sqrt())
    } else {
        0.0
    };

    println!("parsed: {} (failed: {})", parsed, failed_parse);
    println!("evaluated: {}", evaluated);
    println!("MSE: {:.2}", mse);
    println!("MAE: {:.2}", mae);
    println!("Pearson r: {:.3}", corr);

    Ok(())
}
