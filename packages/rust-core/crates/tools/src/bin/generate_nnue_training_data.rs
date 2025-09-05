use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::common::{get_mate_distance, is_mate_score};
use engine_core::search::limits::SearchLimits;
use engine_core::search::types::TeacherProfile;
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
    multipv: u8,
    nodes: Option<u64>,
    teacher_profile: TeacherProfile,
    output_format: OutputFormat,
    min_depth: Option<u8>,
    nodes_autocalibrate_ms: Option<u64>,
    calibrate_sample: usize,
    reuse_tt: bool,
    skip_overrun_factor: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputFormat {
    Text,
    Jsonl,
}

#[derive(Clone)]
struct GenShared {
    error_count: Arc<AtomicUsize>,
    skipped_count: Arc<AtomicUsize>,
    errors_parse: Arc<AtomicUsize>,
    errors_timeout: Arc<AtomicUsize>,
    errors_nonexact_top1: Arc<AtomicUsize>,
    errors_empty_pv: Arc<AtomicUsize>,
    skipped_file: Arc<Mutex<File>>,
}

struct ProcEnv<'a> {
    depth: u8,
    time_limit_ms: u64,
    opts: &'a Opts,
    shared: &'a GenShared,
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
        eprintln!("  --reuse-tt (reuse transposition table across positions; default: false)");
        eprintln!("  --skip-overrun-factor <float> (timeout skip threshold factor; default: 2.0)");
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
        multipv: 1,
        nodes: None,
        teacher_profile: TeacherProfile::Balanced,
        output_format: OutputFormat::Text,
        min_depth: None,
        nodes_autocalibrate_ms: None,
        calibrate_sample: 200,
        reuse_tt: false,
        skip_overrun_factor: 2.0,
    };

    // Flags start after positional args (2 mandatory + up to 3 optional numerics)
    // Be robust to missing optional numerics by scanning for the first `--*` token from index 3
    let mut i = 3;
    while i < args.len() && !args[i].starts_with('-') {
        i += 1;
    }
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
            "--reuse-tt" => {
                opts.reuse_tt = true;
                i += 1;
            }
            "--multipv" => {
                if let Some(k) = args.get(i + 1).and_then(|s| s.parse::<u8>().ok()) {
                    opts.multipv = k.max(1);
                    i += 2;
                } else {
                    eprintln!("Error: --multipv requires an integer value");
                    std::process::exit(1);
                }
            }
            "--nodes" => {
                if let Some(n) = args.get(i + 1).and_then(|s| s.parse::<u64>().ok()) {
                    opts.nodes = Some(n);
                    i += 2;
                } else {
                    eprintln!("Error: --nodes requires an integer value");
                    std::process::exit(1);
                }
            }
            "--skip-overrun-factor" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<f64>().ok()) {
                    opts.skip_overrun_factor = v.max(1.0);
                    i += 2;
                } else {
                    eprintln!("Error: --skip-overrun-factor requires a float value");
                    std::process::exit(1);
                }
            }
            "--teacher-profile" => {
                if let Some(v) = args.get(i + 1) {
                    match v.to_ascii_lowercase().as_str() {
                        "safe" => opts.teacher_profile = TeacherProfile::Safe,
                        "balanced" => opts.teacher_profile = TeacherProfile::Balanced,
                        "aggressive" => opts.teacher_profile = TeacherProfile::Aggressive,
                        other => {
                            eprintln!(
                                "Error: unknown teacher profile '{}'. Use safe|balanced|aggressive",
                                other
                            );
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --teacher-profile requires a value");
                    std::process::exit(1);
                }
            }
            "--output-format" => {
                if let Some(v) = args.get(i + 1) {
                    match v.to_ascii_lowercase().as_str() {
                        "jsonl" => opts.output_format = OutputFormat::Jsonl,
                        "text" => opts.output_format = OutputFormat::Text,
                        other => {
                            eprintln!("Error: unknown output format '{}'. Use text|jsonl", other);
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --output-format requires a value");
                    std::process::exit(1);
                }
            }
            "--min-depth" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<u8>().ok()) {
                    opts.min_depth = Some(v);
                    i += 2;
                } else {
                    eprintln!("Error: --min-depth requires an integer value");
                    std::process::exit(1);
                }
            }
            "--nodes-autocalibrate-ms" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<u64>().ok()) {
                    opts.nodes_autocalibrate_ms = Some(v);
                    i += 2;
                } else {
                    eprintln!("Error: --nodes-autocalibrate-ms requires an integer value");
                    std::process::exit(1);
                }
            }
            "--calibrate-sample" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    opts.calibrate_sample = v.max(10);
                    i += 2;
                } else {
                    eprintln!("Error: --calibrate-sample requires an integer value");
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
    let effective_depth = opts.min_depth.map(|m| m.max(search_depth)).unwrap_or(search_depth);
    println!("Search depth: {effective_depth}");
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
    println!("MultiPV: {}", opts.multipv);
    println!("Teacher profile: {:?}", opts.teacher_profile);
    println!("Reuse TT: {}", opts.reuse_tt);
    if let Some(n) = opts.nodes {
        println!("Nodes (limit): {}", n);
    }
    println!("CPU cores: {:?}", std::thread::available_parallelism());
    if resume_from > 0 || existing_lines > 0 {
        println!("Resuming from position: {}", resume_from.max(existing_lines));
    }
    println!("Skipped positions will be saved to: {}", skipped_path.display());

    // Calculate time limit based on depth (used only if --nodes is not set)
    let time_limit_ms = opts.time_limit_override_ms.unwrap_or(match effective_depth {
        1 => 50,
        2 => 100,
        3 => 200,
        4 => 400,
        _ => 800,
    });
    if opts.nodes.is_none() {
        println!("Time limit per position: {time_limit_ms}ms");
    } else {
        println!("Nodes-based budget active; time limit ignored for search.");
    }

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

    fn extract_sfen(line: &str) -> Option<String> {
        let start = line.find("sfen ")? + 5;
        let rest = &line[start..];
        let end = rest.find(" moves").or_else(|| rest.find('#')).unwrap_or(rest.len());
        let sfen = rest[..end].trim();
        if sfen.is_empty() {
            None
        } else {
            Some(sfen.to_string())
        }
    }
    let sfen_positions: Vec<(usize, String)> = lines
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| extract_sfen(line.trim()).map(|s| (idx, s)))
        .collect();

    let total_positions = sfen_positions.len();
    println!("\nFound {total_positions} positions in input file");

    // Optional: calibrate nodes from NPS if requested and nodes not explicitly set
    if opts.nodes.is_none() {
        if let Some(target_ms) = opts.nodes_autocalibrate_ms {
            println!("Starting nodes auto-calibration: target {} ms", target_ms);
            let sample_n = opts.calibrate_sample.min(total_positions).max(10);
            // Prepare a reusable engine for calibration
            let mut engine = Engine::new(opts.engine);
            engine.set_hash_size(opts.hash_mb);
            engine.set_threads(1);
            engine.set_multipv(opts.multipv);
            engine.set_teacher_profile(opts.teacher_profile);
            if matches!(opts.engine, EngineType::Nnue | EngineType::EnhancedNnue) {
                if let Some(ref path) = opts.nnue_weights {
                    let _ = engine.load_nnue_weights(path);
                }
            }

            let mut total_nodes: u64 = 0;
            let mut total_ms: u64 = 0;
            for (i, (_idx, sfen)) in sfen_positions.iter().take(sample_n).enumerate() {
                if i % 25 == 0 {
                    println!("  calibrating {}/{}", i + 1, sample_n);
                }
                // Build position
                let mut pos = match engine_core::usi::parse_sfen(sfen) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                engine.reset_for_position();
                // Fixed-time calibration search (use min depth for stability)
                let depth = opts.min_depth.unwrap_or(2).max(2);
                let limits = SearchLimits::builder()
                    .depth(depth)
                    .fixed_time_ms(200)
                    .multipv(opts.multipv)
                    .build();
                let start = std::time::Instant::now();
                let result = engine.search(&mut pos, limits);
                let elapsed = start.elapsed();
                total_nodes = total_nodes.saturating_add(result.stats.nodes);
                total_ms = total_ms.saturating_add(elapsed.as_millis() as u64);
            }

            if total_ms > 0 && total_nodes > 0 {
                let nps = (total_nodes as f64) / (total_ms as f64 / 1000.0);
                let target_nodes = (nps * (target_ms as f64) / 1000.0) as u64;
                opts.nodes = Some(target_nodes.max(10_000));
                println!(
                    "Auto-calibration done: NPS≈{:.0}, nodes target={} ({} ms)",
                    nps, target_nodes, target_ms
                );
            } else {
                println!("Auto-calibration failed (zero ms or nodes). Using time budget.");
            }
        }
    }

    // Skip already processed positions
    let skip_count = resume_from.max(existing_lines);

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

    // Skip based on the maximum of skip_count and actual_progress to avoid double-skip
    let positions_to_skip = skip_count.max(actual_progress);
    let sfen_positions = if positions_to_skip > 0 {
        println!("Skipping first {positions_to_skip} positions (already attempted)");
        sfen_positions.into_iter().skip(positions_to_skip).collect()
    } else {
        sfen_positions
    };

    let remaining_positions = sfen_positions.len();
    println!("Processing {remaining_positions} remaining positions");

    // Statistics - include already processed count
    let processed_count = Arc::new(AtomicUsize::new(skip_count));
    let error_count = Arc::new(AtomicUsize::new(0));
    let skipped_count = Arc::new(AtomicUsize::new(0));
    let errors_parse = Arc::new(AtomicUsize::new(0));
    let errors_timeout = Arc::new(AtomicUsize::new(0));
    let errors_nonexact_top1 = Arc::new(AtomicUsize::new(0));
    let errors_empty_pv = Arc::new(AtomicUsize::new(0));
    let total_attempted = Arc::new(AtomicUsize::new(positions_to_skip));

    let shared = GenShared {
        error_count: error_count.clone(),
        skipped_count: skipped_count.clone(),
        errors_parse: errors_parse.clone(),
        errors_timeout: errors_timeout.clone(),
        errors_nonexact_top1: errors_nonexact_top1.clone(),
        errors_empty_pv: errors_empty_pv.clone(),
        skipped_file: skipped_file.clone(),
    };

    let env = ProcEnv {
        depth: effective_depth,
        time_limit_ms,
        opts: &opts,
        shared: &shared,
    };

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
            .map_init(
                || {
                    let mut eng = Engine::new(opts.engine);
                    eng.set_hash_size(opts.hash_mb);
                    eng.set_threads(1);
                    eng.set_multipv_persistent(opts.multipv);
                    eng.set_teacher_profile(opts.teacher_profile);
                    if matches!(opts.engine, EngineType::Nnue | EngineType::EnhancedNnue) {
                        if let Some(ref path) = opts.nnue_weights {
                            if let Err(e) = eng.load_nnue_weights(path) {
                                eprintln!("Failed to load NNUE weights ({}): {}", path, e);
                            }
                        }
                    }
                    eng
                },
                |eng, (idx, sfen)| process_position_with_engine(*idx, sfen, &env, eng),
            )
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
    let e_parse = errors_parse.load(Ordering::Relaxed);
    let e_nonexact = errors_nonexact_top1.load(Ordering::Relaxed);
    let e_empty_pv = errors_empty_pv.load(Ordering::Relaxed);
    let newly_processed = total_processed - skip_count;

    println!("\n{}", "=".repeat(60));
    println!("NNUE Training Data Generation Complete!");
    println!("Total positions in file: {total_positions}");
    println!("Previously processed: {skip_count}");
    println!("Newly processed: {newly_processed}");
    println!("Total processed: {total_processed}");
    println!("Errors (total): {total_errors}");
    println!("  - parse: {e_parse}");
    println!("  - nonexact_top1: {e_nonexact}");
    println!("  - empty_or_missing_pv: {e_empty_pv}");
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

fn process_position_with_engine(
    idx: usize,
    sfen: &str,
    env: &ProcEnv<'_>,
    eng: &mut Engine,
) -> Option<String> {
    let position = match Position::from_sfen(sfen) {
        Ok(pos) => pos,
        Err(e) => {
            if idx < 10 || (idx + 1) % 1000 == 0 {
                eprintln!("Error parsing position {}: {}", idx + 1, e);
            }
            env.shared.error_count.fetch_add(1, Ordering::Relaxed);
            env.shared.errors_parse.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };

    // Reset per position only when not reusing TT
    if !env.opts.reuse_tt {
        eng.reset_for_position();
    }
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Build limits: prefer nodes-based budget if provided
    let mut builder = SearchLimits::builder()
        .depth(env.depth)
        .stop_flag(stop_flag.clone())
        .multipv(env.opts.multipv);
    if let Some(n) = env.opts.nodes {
        builder = builder.fixed_nodes(n);
    } else {
        builder = builder.fixed_time_ms(env.time_limit_ms);
    }
    let limits = builder.build();

    let start = std::time::Instant::now();
    let mut pos_clone = position.clone();
    let result = eng.search(&mut pos_clone, limits);
    let elapsed = start.elapsed();

    // Check if we exceeded time limit significantly (only when using time-based budget)
    if env.opts.nodes.is_none()
        && (elapsed.as_millis() as f64) > (env.time_limit_ms as f64) * env.opts.skip_overrun_factor
    {
        if idx < 10 || (idx + 1) % 100 == 0 {
            eprintln!(
                "Position {} took too long ({:.1}s), skipping",
                idx + 1,
                elapsed.as_secs_f32()
            );
        }
        env.shared.skipped_count.fetch_add(1, Ordering::Relaxed);
        env.shared.errors_timeout.fetch_add(1, Ordering::Relaxed);

        // Write to skipped file with reason (JSON)
        if let Ok(mut file) = env.shared.skipped_file.lock() {
            let budget_ms = env.time_limit_ms;
            let overrun_factor = (elapsed.as_millis() as f64) / (budget_ms as f64);
            let obj = serde_json::json!({
                "sfen": sfen,
                "timeout": true,
                "elapsed_ms": elapsed.as_millis() as u64,
                "budget_ms": budget_ms,
                "overrun_factor": overrun_factor,
                "depth_reached": result.stats.depth,
                "reason": "time_overrun",
                "index": idx + 1,
            });
            writeln!(file, "{}", obj).ok();
            file.flush().ok();
        }

        return None;
    }

    let eval = result.score;
    let depth_reached = result.stats.depth;

    // Validate PV and top1 bound=Exact
    let mut has_lines = false;
    let mut top1_exact = false;
    if let Some(ref lines) = result.lines {
        if !lines.is_empty() {
            has_lines = true;
            if let Some(l0) = lines.first() {
                top1_exact = matches!(format!("{:?}", l0.bound).as_str(), "Exact");
            }
        }
    }
    if !has_lines {
        env.shared.error_count.fetch_add(1, Ordering::Relaxed);
        env.shared.errors_empty_pv.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut file) = env.shared.skipped_file.lock() {
            let obj = serde_json::json!({
                "sfen": sfen,
                "search_error": "empty_or_missing_pv",
                "depth_reached": depth_reached,
                "index": idx + 1,
            });
            writeln!(file, "{}", obj).ok();
            file.flush().ok();
        }
        return None;
    }
    if !top1_exact {
        env.shared.error_count.fetch_add(1, Ordering::Relaxed);
        env.shared.errors_nonexact_top1.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut file) = env.shared.skipped_file.lock() {
            let obj = serde_json::json!({
                "sfen": sfen,
                "search_error": "nonexact_top1",
                "depth_reached": depth_reached,
                "index": idx + 1,
            });
            writeln!(file, "{}", obj).ok();
            file.flush().ok();
        }
        return None;
    }

    // Compute best2 gap if MultiPV >= 2 and we have 2 lines
    let mut best2_gap_cp_meta = String::new();
    if env.opts.multipv >= 2 {
        if let Some(ref lines) = result.lines {
            if lines.len() >= 2 {
                let l0 = &lines[0];
                let l1 = &lines[1];
                // Prefer reporting gap even if non-Exact, but you may filter later
                let gap = (l0.score_cp - l1.score_cp).abs();
                best2_gap_cp_meta =
                    format!(" best2_gap_cp:{} bound0:{:?} bound1:{:?}", gap, l0.bound, l1.bound);
            }
        }
    }

    // Compute labels
    let (label_kind, wdl_prob_opt) = match env.opts.label {
        LabelKind::Cp => ("cp", None),
        LabelKind::Wdl => ("wdl", Some(cp_to_wdl(eval, env.opts.wdl_scale))),
        LabelKind::Hybrid => {
            let use_wdl = position.ply as u32 <= env.opts.hybrid_ply_cutoff;
            if use_wdl {
                ("wdl", Some(cp_to_wdl(eval, env.opts.wdl_scale)))
            } else {
                ("cp", None)
            }
        }
    };

    // Metadata for quality tracking
    let mut meta = if env.opts.nodes.is_none() && elapsed.as_millis() > env.time_limit_ms as u128 {
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

    // Output according to selected format
    match env.opts.output_format {
        OutputFormat::Text => {
            let mut line = format!("{sfen} eval {eval}");
            if let Some(p) = wdl_prob_opt {
                line.push_str(&format!(" wdl {:.6}", p));
            }
            line.push_str(&meta);
            if !best2_gap_cp_meta.is_empty() {
                line.push_str(&best2_gap_cp_meta);
            }
            Some(line)
        }
        OutputFormat::Jsonl => {
            use serde_json::json;
            let mut bound1 = None;
            let mut bound2 = None;
            let mut gap2: Option<i32> = None;
            let mut lines_json = Vec::new();
            if let Some(ref lines) = result.lines {
                for (i, l) in lines.iter().enumerate() {
                    if i == 0 {
                        bound1 = Some(format!("{:?}", l.bound));
                    }
                    if i == 1 {
                        bound2 = Some(format!("{:?}", l.bound));
                    }
                    lines_json.push(json!({
                        "multipv": l.multipv_index,
                        "move": engine_core::usi::move_to_usi(&l.root_move),
                        "score_internal": l.score_internal,
                        "score_cp": l.score_cp,
                        "bound": format!("{:?}", l.bound),
                        "depth": l.depth,
                        "seldepth": l.seldepth,
                        "pv": l.pv.iter().map(engine_core::usi::move_to_usi).collect::<Vec<_>>(),
                        "exact_exhausted": l.exact_exhausted,
                        "exhaust_reason": l.exhaust_reason,
                        "mate_distance": l.mate_distance,
                    }));
                }
                if lines.len() >= 2 {
                    let l0 = &lines[0];
                    let l1 = &lines[1];
                    gap2 = Some((l0.score_cp - l1.score_cp).abs());
                }
            }

            let tt_hit_rate = if result.stats.nodes > 0 {
                result.stats.tt_hits.map(|h| (h as f64) / (result.stats.nodes as f64))
            } else {
                None
            };

            let json_obj = json!({
                "sfen": sfen,
                "lines": lines_json,
                "depth": result.stats.depth,
                "seldepth": result.stats.seldepth,
                "nodes": result.stats.nodes,
                "nodes_q": result.stats.qnodes,
                "time_ms": result.stats.elapsed.as_millis() as u64,
                "aspiration_retries": result.stats.re_searches,
                "pv_changed": result.stats.pv_changed,
                "best2_gap_cp": gap2,
                "root_fail_high_count": result.stats.root_fail_high_count,
                "used_null": result.stats.null_cuts.map(|c| c > 0).unwrap_or(false),
                "lmr_applied": result.stats.lmr_count.unwrap_or(0),
                "bound1": bound1,
                "bound2": bound2,
                "tt_hit_rate": tt_hit_rate,
                "root_move_index": 0,
                "label": label_kind,
                "eval": eval,
                "wdl": wdl_prob_opt,
                "meta": meta.trim(),
            });
            Some(json_obj.to_string())
        }
    }
}

#[inline]
fn cp_to_wdl(cp: i32, scale: f64) -> f64 {
    // Clamp CP to a reasonable range to avoid NaNs
    let x = (cp as f64).clamp(-32000.0, 32000.0) / scale;
    1.0 / (1.0 + (-x).exp())
}
