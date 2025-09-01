use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::common::{get_mate_distance, is_mate_score};
use engine_core::search::limits::SearchLimits;
use engine_core::Position;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LabelKind {
    Cp,
    Wdl,
    Hybrid,
}

#[derive(Clone, Debug)]
struct Opts {
    engine: EngineType,
    nnue_weights: Option<String>,
    label: LabelKind,
    wdl_scale: f64,
    hybrid_ply_cutoff: u32,
    time_limit_override_ms: Option<u64>,
    hash_mb: usize,
}
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// NNUE学習データ生成用のツール
/// 初期段階では浅い探索で高速にデータを集めることを優先
fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!(
            "Usage: {} <input_sfen_file> <output_training_data> [depth] [batch_size] [resume_from]",
            args[0]
        );
        eprintln!("  depth: Search depth (default: 2 for initial data collection)");
        eprintln!("  batch_size: Number of positions to process in parallel (default: 50)");
        eprintln!("  resume_from: Line number to resume from (default: 0)");
        eprintln!("\nOptional flags:");
        eprintln!("  --engine <material|enhanced|nnue|enhanced-nnue> (default: material)");
        eprintln!("  --nnue-weights <path> (required if engine is nnue/enhanced-nnue and weights not zero)");
        eprintln!("  --label <cp|wdl|hybrid> (default: cp)");
        eprintln!("  --wdl-scale <float> (default: 600.0)");
        eprintln!("  --hybrid-ply-cutoff <u32> (default: 100, ply<=cutoff use WDL else CP)");
        eprintln!("  --time-limit-ms <u64> (override per-position time budget)");
        eprintln!("  --hash-mb <usize> (TT size per engine instance, default: 16)");
        eprintln!("\nRecommended settings for initial NNUE data:");
        eprintln!("  - Depth 2: Fast collection, basic evaluation");
        eprintln!("  - Depth 3: Balanced speed/quality");
        eprintln!("  - Depth 4+: High quality but slower");
        std::process::exit(1);
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = PathBuf::from(&args[2]);
    let search_depth = args.get(3).and_then(|s| s.parse::<u8>().ok()).unwrap_or(2);
    let batch_size = args.get(4).and_then(|s| s.parse::<usize>().ok()).unwrap_or(50);
    let resume_from = args.get(5).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);

    // Parse optional flags (simple manual parser to avoid new deps)

    fn parse_engine(s: &str) -> Option<EngineType> {
        match s.to_ascii_lowercase().as_str() {
            "material" => Some(EngineType::Material),
            "enhanced" => Some(EngineType::Enhanced),
            "nnue" => Some(EngineType::Nnue),
            "enhanced-nnue" | "enhanced_nnue" | "ennue" => Some(EngineType::EnhancedNnue),
            _ => None,
        }
    }

    fn parse_label(s: &str) -> Option<LabelKind> {
        match s.to_ascii_lowercase().as_str() {
            "cp" | "eval" => Some(LabelKind::Cp),
            "wdl" => Some(LabelKind::Wdl),
            "hybrid" => Some(LabelKind::Hybrid),
            _ => None,
        }
    }

    let mut opts = Opts {
        engine: EngineType::Material,
        nnue_weights: None,
        label: LabelKind::Cp,
        wdl_scale: 600.0,
        hybrid_ply_cutoff: 100,
        time_limit_override_ms: None,
        hash_mb: 16,
    };

    let mut i = 6; // flags start after positional args
    while i < args.len() {
        match args[i].as_str() {
            "--engine" => {
                if let Some(v) = args.get(i + 1).and_then(|s| parse_engine(s)) {
                    opts.engine = v;
                    i += 2;
                } else {
                    eprintln!(
                        "Error: --engine requires one of material|enhanced|nnue|enhanced-nnue"
                    );
                    std::process::exit(1);
                }
            }
            "--nnue-weights" => {
                if let Some(path) = args.get(i + 1) {
                    opts.nnue_weights = Some(path.clone());
                    i += 2;
                } else {
                    eprintln!("Error: --nnue-weights requires a path");
                    std::process::exit(1);
                }
            }
            "--label" => {
                if let Some(v) = args.get(i + 1).and_then(|s| parse_label(s)) {
                    opts.label = v;
                    i += 2;
                } else {
                    eprintln!("Error: --label requires one of cp|wdl|hybrid");
                    std::process::exit(1);
                }
            }
            "--wdl-scale" => {
                if let Some(scale) = args.get(i + 1).and_then(|s| s.parse::<f64>().ok()) {
                    opts.wdl_scale = scale;
                    i += 2;
                } else {
                    eprintln!("Error: --wdl-scale requires a float value");
                    std::process::exit(1);
                }
            }
            "--hybrid-ply-cutoff" => {
                if let Some(cut) = args.get(i + 1).and_then(|s| s.parse::<u32>().ok()) {
                    opts.hybrid_ply_cutoff = cut;
                    i += 2;
                } else {
                    eprintln!("Error: --hybrid-ply-cutoff requires an integer value");
                    std::process::exit(1);
                }
            }
            "--time-limit-ms" => {
                if let Some(ms) = args.get(i + 1).and_then(|s| s.parse::<u64>().ok()) {
                    opts.time_limit_override_ms = Some(ms);
                    i += 2;
                } else {
                    eprintln!("Error: --time-limit-ms requires an integer value");
                    std::process::exit(1);
                }
            }
            "--hash-mb" => {
                if let Some(mb) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    opts.hash_mb = mb.max(1);
                    i += 2;
                } else {
                    eprintln!("Error: --hash-mb requires an integer value");
                    std::process::exit(1);
                }
            }
            other => {
                eprintln!("Unknown option: {}", other);
                std::process::exit(1);
            }
        }
    }

    // Create skipped positions output file path
    let skipped_path = {
        let stem = output_path.file_stem().unwrap_or_default().to_string_lossy();
        let ext = output_path.extension().unwrap_or_default().to_string_lossy();
        output_path.with_file_name(format!("{stem}_skipped.{ext}"))
    };

    // Validate depth
    if !(1..=10).contains(&search_depth) {
        eprintln!("Error: Depth must be between 1 and 10");
        std::process::exit(1);
    }

    // Count existing lines if resuming or output file exists
    let existing_lines = if output_path.exists() {
        let file = File::open(&output_path)?;
        let reader = BufReader::new(file);
        let count = reader.lines().count();
        if count > 0 {
            println!("Found existing output file with {count} lines");
            if resume_from > 0 && resume_from != count {
                println!(
                    "Warning: resume_from ({resume_from}) differs from existing lines ({count})"
                );
                println!("Using the larger value: {}", resume_from.max(count));
            }
        }
        count
    } else if resume_from > 0 {
        println!("Warning: Output file does not exist, but resume_from is set to {resume_from}");
        println!("Starting from position {resume_from} anyway");
        0
    } else {
        0
    };

    println!("NNUE Training Data Generator");
    println!("============================");
    println!("Search depth: {search_depth}");
    println!("Batch size: {batch_size}");
    println!("Engine: {:?}", opts.engine);
    if let Some(ref w) = opts.nnue_weights {
        println!("NNUE weights: {w}");
    }
    println!("Label: {:?}", opts.label);
    if matches!(opts.label, LabelKind::Wdl | LabelKind::Hybrid) {
        println!("WDL scale: {:.3}", opts.wdl_scale);
        if matches!(opts.label, LabelKind::Hybrid) {
            println!("Hybrid cutoff ply: {}", opts.hybrid_ply_cutoff);
        }
    }
    println!("Hash size (MB): {}", opts.hash_mb);
    println!("CPU cores: {:?}", std::thread::available_parallelism());
    if resume_from > 0 || existing_lines > 0 {
        println!("Resuming from position: {}", resume_from.max(existing_lines));
    }
    println!("Skipped positions will be saved to: {}", skipped_path.display());

    // Calculate time limit based on depth
    let time_limit_ms = opts.time_limit_override_ms.unwrap_or(match search_depth {
        1 => 50,
        2 => 100,
        3 => 200,
        4 => 400,
        _ => 800,
    });
    println!("Time limit per position: {time_limit_ms}ms");

    // Open files - append mode if resuming
    let output_file = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .write(true)
            .append(resume_from > 0 || existing_lines > 0)
            .truncate(resume_from == 0 && existing_lines == 0)
            .open(&output_path)?,
    ));

    // Open skipped positions file (always append mode to not lose data)
    let skipped_file =
        Arc::new(Mutex::new(OpenOptions::new().create(true).append(true).open(&skipped_path)?));

    let input_file = File::open(&input_path)?;
    let reader = BufReader::new(input_file);
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

    // Extract SFEN positions
    let sfen_positions: Vec<(usize, String)> = lines
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line = line.trim();
            if line.is_empty() || !line.contains("sfen") {
                return None;
            }

            if let Some(start_idx) = line.find("sfen ") {
                let sfen_part = line[start_idx + 5..].to_string();
                Some((idx, sfen_part))
            } else {
                None
            }
        })
        .collect();

    let total_positions = sfen_positions.len();
    println!("\nFound {total_positions} positions in input file");

    // Skip already processed positions
    let skip_count = resume_from.max(existing_lines);
    let sfen_positions = if skip_count > 0 {
        println!("Skipping first {skip_count} positions (already processed)");
        sfen_positions.into_iter().skip(skip_count).collect()
    } else {
        sfen_positions
    };

    // Create progress file path for tracking actual progress including skipped
    let progress_path = output_path.with_extension("progress");

    // Load actual progress (including skipped positions)
    let actual_progress = if progress_path.exists() {
        let content = std::fs::read_to_string(&progress_path)?;
        let progress: usize = content.trim().parse().unwrap_or(skip_count);
        if progress > skip_count {
            println!("Progress file shows {progress} positions attempted (including skipped)");
            println!("Output file has {skip_count} successful results");
            println!("Difference of {} positions were skipped/failed", progress - skip_count);
        }
        progress
    } else {
        skip_count
    };

    // Skip based on actual progress, not just output lines
    let sfen_positions = if actual_progress > 0 {
        println!("Skipping first {actual_progress} positions (already attempted)");
        sfen_positions.into_iter().skip(actual_progress).collect()
    } else {
        sfen_positions
    };

    let remaining_positions = sfen_positions.len();
    println!("Processing {remaining_positions} remaining positions");

    // Statistics - include already processed count
    let processed_count = Arc::new(AtomicUsize::new(skip_count));
    let error_count = Arc::new(AtomicUsize::new(0));
    let skipped_count = Arc::new(AtomicUsize::new(0));
    let total_attempted = Arc::new(AtomicUsize::new(actual_progress));

    // Process in batches
    for (batch_idx, chunk) in sfen_positions.chunks(batch_size).enumerate() {
        let batch_start = std::time::Instant::now();

        println!(
            "\nBatch {}/{}: Processing {} positions...",
            batch_idx + 1,
            remaining_positions.div_ceil(batch_size),
            chunk.len()
        );

        let batch_results: Vec<_> = chunk
            .par_iter()
            .map(|(idx, sfen)| {
                process_position(
                    *idx,
                    sfen,
                    search_depth,
                    time_limit_ms,
                    &opts,
                    &error_count,
                    &skipped_count,
                    &skipped_file,
                )
            })
            .collect();

        // Separate successful results from skipped ones
        let successful_results: Vec<_> = batch_results.into_iter().flatten().collect();

        // Write successful results
        {
            let mut file = output_file.lock().unwrap();
            for result in &successful_results {
                writeln!(file, "{result}")?;
            }
            file.flush()?;
        }

        // Update progress
        let new_processed = processed_count.fetch_add(successful_results.len(), Ordering::Relaxed)
            + successful_results.len();
        let new_attempted = total_attempted.fetch_add(chunk.len(), Ordering::Relaxed) + chunk.len();

        // Save progress to file (total positions attempted, including skipped)
        std::fs::write(&progress_path, new_attempted.to_string())?;

        let batch_time = batch_start.elapsed();
        let positions_per_sec = chunk.len() as f64 / batch_time.as_secs_f64();

        println!(
            "Batch complete: {} results in {:.1}s ({:.0} pos/sec)",
            successful_results.len(),
            batch_time.as_secs_f32(),
            positions_per_sec
        );
        println!(
            "Overall progress: {new_processed}/{total_positions} ({:.1}%)",
            (new_processed as f64 / total_positions as f64) * 100.0
        );
    }

    // Final statistics
    let total_processed = processed_count.load(Ordering::Relaxed);
    let total_errors = error_count.load(Ordering::Relaxed);
    let total_skipped = skipped_count.load(Ordering::Relaxed);
    let newly_processed = total_processed - skip_count;

    println!("\n{}", "=".repeat(60));
    println!("NNUE Training Data Generation Complete!");
    println!("Total positions in file: {total_positions}");
    println!("Previously processed: {skip_count}");
    println!("Newly processed: {newly_processed}");
    println!("Total processed: {total_processed}");
    println!("Errors: {total_errors}");
    println!("Skipped (timeout): {total_skipped}");

    if newly_processed > 0 {
        let success_rate = (newly_processed as f64
            / (newly_processed + total_errors + total_skipped) as f64)
            * 100.0;
        println!("Success rate (this run): {success_rate:.1}%");
    }

    if total_skipped > 0 {
        println!("\nSkipped positions saved to: {}", skipped_path.display());
        println!("Progress tracked in: {}", progress_path.display());
        println!("You can reprocess skipped positions with:");
        println!(
            "  cargo run --release --bin generate_nnue_training_data -- {} output_retry.txt {} {}",
            skipped_path.display(),
            search_depth + 1,
            batch_size / 2
        );
    }

    Ok(())
}

fn process_position(
    idx: usize,
    sfen: &str,
    depth: u8,
    time_limit_ms: u64,
    opts: &Opts,
    error_count: &Arc<AtomicUsize>,
    skipped_count: &Arc<AtomicUsize>,
    skipped_file: &Arc<Mutex<File>>,
) -> Option<String> {
    let position = match Position::from_sfen(sfen) {
        Ok(pos) => pos,
        Err(e) => {
            if idx < 10 || (idx + 1) % 1000 == 0 {
                eprintln!("Error parsing position {}: {}", idx + 1, e);
            }
            error_count.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };

    // Create engine according to options
    let mut engine = Engine::new(opts.engine);
    // Reduce TT to avoid high memory usage with parallel batches
    engine.set_hash_size(opts.hash_mb);
    // Load NNUE weights if requested
    if matches!(opts.engine, EngineType::Nnue | EngineType::EnhancedNnue) {
        if let Some(ref path) = opts.nnue_weights {
            if let Err(e) = engine.load_nnue_weights(path) {
                if idx < 10 {
                    eprintln!("Failed to load NNUE weights ({}): {}", path, e);
                }
                // Fall back: continue with zero weights
            }
        }
    }
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Simple timeout without complex threading
    let limits = SearchLimits::builder()
        .depth(depth)
        .fixed_time_ms(time_limit_ms)
        .stop_flag(stop_flag.clone())
        .build();

    let start = std::time::Instant::now();
    let mut pos_clone = position.clone();
    let result = engine.search(&mut pos_clone, limits);
    let elapsed = start.elapsed();

    // Check if we exceeded time limit significantly
    if elapsed.as_millis() > (time_limit_ms * 2) as u128 {
        if idx < 10 || (idx + 1) % 100 == 0 {
            eprintln!(
                "Position {} took too long ({:.1}s), skipping",
                idx + 1,
                elapsed.as_secs_f32()
            );
        }
        skipped_count.fetch_add(1, Ordering::Relaxed);

        // Write to skipped file with reason
        if let Ok(mut file) = skipped_file.lock() {
            writeln!(
                file,
                "sfen {} # position {} timeout {:.1}s depth_reached {}",
                sfen,
                idx + 1,
                elapsed.as_secs_f32(),
                result.stats.depth
            )
            .ok();
            file.flush().ok();
        }

        return None;
    }

    let eval = result.score;
    let depth_reached = result.stats.depth;

    // Compute labels
    let (label_kind, wdl_prob_opt) = match opts.label {
        LabelKind::Cp => ("cp", None),
        LabelKind::Wdl => ("wdl", Some(cp_to_wdl(eval, opts.wdl_scale))),
        LabelKind::Hybrid => {
            let use_wdl = position.ply as u32 <= opts.hybrid_ply_cutoff;
            if use_wdl {
                ("wdl", Some(cp_to_wdl(eval, opts.wdl_scale)))
            } else {
                ("cp", None)
            }
        }
    };

    // Metadata for quality tracking
    let mut meta = if elapsed.as_millis() > time_limit_ms as u128 {
        format!(" # timeout_d{depth_reached}")
    } else {
        format!(" # d{depth_reached}")
    };

    // Add label type
    meta.push_str(&format!(" label:{}", label_kind));

    // Mark mate scores explicitly
    if is_mate_score(eval) {
        if let Some(md) = get_mate_distance(eval) {
            meta.push_str(&format!(" mate:{}", md));
        }
    }

    // Always emit CP (eval) for compatibility; optionally emit WDL
    let mut line = format!("{sfen} eval {eval}");
    if let Some(p) = wdl_prob_opt {
        line.push_str(&format!(" wdl {:.6}", p));
    }
    line.push_str(&meta);

    Some(line)
}

#[inline]
fn cp_to_wdl(cp: i32, scale: f64) -> f64 {
    // Clamp CP to a reasonable range to avoid NaNs
    let x = (cp as f64).clamp(-32000.0, 32000.0) / scale;
    1.0 / (1.0 + (-x).exp())
}
