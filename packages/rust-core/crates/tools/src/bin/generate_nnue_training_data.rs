use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::common::{get_mate_distance, is_mate_score};
use engine_core::search::limits::SearchLimits;
use engine_core::search::types::{Bound, TeacherProfile};
use engine_core::Position;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LabelKind {
    Cp,
    Wdl,
    Hybrid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresetKind {
    Baseline,
    Balanced,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MateEntropyMode {
    Exclude,
    Saturate,
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
    no_recalib: bool,
    force_recalib: bool,
    jobs: Option<usize>,
    // Step 4: conditional MultiPV K=3 entropy
    amb_gap2_threshold: i32,
    amb_require_exact: bool,
    entropy_mate_mode: MateEntropyMode,
    entropy_scale: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestCalibration {
    nps: Option<f64>,
    target_nodes: Option<u64>,
    samples: Option<usize>,
    min_depth_used: Option<u8>,
    timestamp: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestBudget {
    mode: String,
    time_ms: Option<u64>,
    nodes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestOverrides {
    time: bool,
    nodes: bool,
    hash_mb: bool,
    multipv: bool,
    min_depth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    generated_at: String,
    git_commit: Option<String>,
    engine: String,
    nnue_weights: Option<String>,
    preset: Option<String>,
    overrides: Option<ManifestOverrides>,
    teacher_profile: String,
    multipv: u8,
    budget: ManifestBudget,
    min_depth: u8,
    hash_mb: usize,
    threads_per_engine: usize,
    jobs: Option<usize>,
    count: usize,
    cp_to_wdl_scale: f64,
    wdl_semantics: String,
    calibration: Option<ManifestCalibration>,
    // Added operational stats
    attempted: usize,
    skipped_timeout: usize,
    errors: serde_json::Value,
    reuse_tt: bool,
    skip_overrun_factor: f64,
    search_depth_arg: u8,
    effective_min_depth: u8,
    output_sha256: Option<String>,
    output_bytes: Option<u64>,
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
    errors_nonexact_top1: Arc<AtomicUsize>,
    errors_empty_pv: Arc<AtomicUsize>,
    skipped_file: Arc<Mutex<File>>,
}

struct ProcEnv<'a> {
    depth: u8,
    time_limit_ms: u64,
    opts: &'a Opts,
    shared: &'a GenShared,
    global_stop: Arc<AtomicBool>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    // Global cancellation flag (Ctrl-C)
    let global_stop = Arc::new(AtomicBool::new(false));
    {
        let stop = global_stop.clone();
        let installed = ctrlc::set_handler(move || {
            if !stop.swap(true, Ordering::Relaxed) {
                eprintln!("Received Ctrl-C: requesting graceful stop ...");
            }
        });
        if let Err(e) = installed {
            eprintln!("Warning: failed to install Ctrl-C handler: {}", e);
        }
    }
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
        eprintln!(
            "  --preset <baseline|balanced|high> (apply recommended time/hash/multipv/min-depth; time is ignored if --nodes is set)"
        );
        eprintln!("  --label <cp|wdl|hybrid> (default: cp)");
        eprintln!("  --wdl-scale <float> (default: 600.0)");
        eprintln!("  --entropy-scale <float> (default: 600.0; temperature for K=3 entropy)");
        eprintln!("  --hybrid-ply-cutoff <u32> (default: 100, ply<=cutoff use WDL else CP)");
        eprintln!("  --time-limit-ms <u64> (override per-position time budget)");
        eprintln!("  --hash-mb <usize> (TT size per engine instance, default: 16)");
        eprintln!("  --reuse-tt (reuse TT and heuristics across positions; faster but may bias; default: false)");
        eprintln!("  --skip-overrun-factor <float> (timeout skip threshold factor; default: 2.0)");
        eprintln!("  --jobs <n> (outer parallelism for positions; engine threads stay 1)");
        eprintln!("  --amb-gap2-threshold <cp> (default: 25; ambiguity threshold for gap2)");
        eprintln!(
            "  --amb-allow-inexact (allow non-Exact bounds for ambiguity; default requires Exact)"
        );
        eprintln!("  --entropy-mate-mode <exclude|saturate> (mate handling in entropy; default: saturate)");
        eprintln!("  --no-recalib (reuse manifest calibration if available)");
        eprintln!("  --force-recalib (force re-calibration even if manifest exists)");
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
        no_recalib: false,
        force_recalib: false,
        jobs: None,
        amb_gap2_threshold: 25,
        amb_require_exact: true,
        entropy_mate_mode: MateEntropyMode::Saturate,
        entropy_scale: 600.0,
    };

    // Flags start after positional args (2 mandatory + up to 3 optional numerics)
    // Be robust to missing optional numerics by scanning for the first `--*` token from index 3
    let mut i = 3;
    // Track explicit CLI overrides to apply after preset
    let mut cli_set_time = false;
    let mut cli_set_hash = false;
    let mut cli_set_multipv = false;
    let mut cli_set_min_depth = false;
    let mut cli_set_nodes = false;
    let mut preset_sel: Option<PresetKind> = None;
    while i < args.len() && !args[i].starts_with('-') {
        i += 1;
    }
    while i < args.len() {
        match args[i].as_str() {
            "--preset" => {
                if let Some(v) = args.get(i + 1) {
                    match v.to_ascii_lowercase().as_str() {
                        "baseline" => preset_sel = Some(PresetKind::Baseline),
                        "balanced" => preset_sel = Some(PresetKind::Balanced),
                        "high" => preset_sel = Some(PresetKind::High),
                        other => {
                            eprintln!(
                                "Error: unknown preset '{}'. Use baseline|balanced|high",
                                other
                            );
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --preset requires a value");
                    std::process::exit(1);
                }
            }
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
            "--entropy-scale" => {
                if let Some(scale) = args.get(i + 1).and_then(|s| s.parse::<f64>().ok()) {
                    opts.entropy_scale = scale;
                    i += 2;
                } else {
                    eprintln!("Error: --entropy-scale requires a float value");
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
                    cli_set_time = true;
                    i += 2;
                } else {
                    eprintln!("Error: --time-limit-ms requires an integer value");
                    std::process::exit(1);
                }
            }
            "--hash-mb" => {
                if let Some(mb) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    opts.hash_mb = mb.max(1);
                    cli_set_hash = true;
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
                    cli_set_multipv = true;
                    i += 2;
                } else {
                    eprintln!("Error: --multipv requires an integer value");
                    std::process::exit(1);
                }
            }
            "--nodes" => {
                if let Some(n) = args.get(i + 1).and_then(|s| s.parse::<u64>().ok()) {
                    opts.nodes = Some(n);
                    cli_set_nodes = true;
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
            "--amb-gap2-threshold" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<i32>().ok()) {
                    opts.amb_gap2_threshold = v.max(0);
                    i += 2;
                } else {
                    eprintln!("Error: --amb-gap2-threshold requires an integer value (centipawns)");
                    std::process::exit(1);
                }
            }
            "--amb-allow-inexact" => {
                opts.amb_require_exact = false;
                i += 1;
            }
            "--entropy-mate-mode" => {
                if let Some(v) = args.get(i + 1) {
                    match v.to_ascii_lowercase().as_str() {
                        "exclude" => opts.entropy_mate_mode = MateEntropyMode::Exclude,
                        "saturate" => opts.entropy_mate_mode = MateEntropyMode::Saturate,
                        other => {
                            eprintln!(
                                "Error: --entropy-mate-mode must be exclude|saturate (got '{}')",
                                other
                            );
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --entropy-mate-mode requires a value");
                    std::process::exit(1);
                }
            }
            "--jobs" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    opts.jobs = Some(v.max(1));
                    i += 2;
                } else {
                    eprintln!("Error: --jobs requires an integer value");
                    std::process::exit(1);
                }
            }
            "--no-recalib" => {
                opts.no_recalib = true;
                i += 1;
            }
            "--force-recalib" => {
                opts.force_recalib = true;
                i += 1;
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
                    cli_set_min_depth = true;
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

    // Apply preset after parsing, but allow explicit CLI overrides to win
    let mut preset_log: Option<String> = None;
    let mut preset_name: Option<String> = None;
    if let Some(p) = preset_sel {
        let (time_ms, hash_mb, multipv, min_depth) = match p {
            PresetKind::Baseline => (100u64, 16usize, 1u8, 2u8),
            PresetKind::Balanced => (200u64, 32usize, 2u8, 3u8),
            PresetKind::High => (400u64, 64usize, 3u8, 4u8),
        };
        preset_name = Some(
            match p {
                PresetKind::Baseline => "baseline",
                PresetKind::Balanced => "balanced",
                PresetKind::High => "high",
            }
            .to_string(),
        );
        // Apply unless overridden by CLI
        if !cli_set_hash {
            opts.hash_mb = hash_mb;
        }
        if !cli_set_multipv {
            opts.multipv = multipv;
        }
        if !cli_set_min_depth {
            opts.min_depth = Some(min_depth);
        }
        if !cli_set_time && !cli_set_nodes {
            opts.time_limit_override_ms = Some(time_ms);
        }
        // Build an informative preset log with effective mode/value and override hints
        let mode = if cli_set_nodes || opts.nodes.is_some() {
            "nodes"
        } else {
            "time"
        };
        let min_depth_log =
            opts.min_depth.map(|d| d.to_string()).unwrap_or_else(|| "<unset>".into());
        let eff = match (mode, opts.time_limit_override_ms, opts.nodes) {
            ("time", Some(ms), _) => format!("time_ms={}", ms),
            ("nodes", _, Some(n)) => format!("nodes={}", n),
            _ => format!("time_ms={}", time_ms),
        };
        preset_log = Some(format!(
            "Preset {:?} -> mode={}, {}{} hash_mb={} multipv={} min_depth={}{}{}{}",
            p,
            mode,
            eff,
            if cli_set_time || cli_set_nodes {
                " (overridden)"
            } else {
                ""
            },
            opts.hash_mb,
            opts.multipv,
            min_depth_log,
            if cli_set_hash {
                " (hash overridden)"
            } else {
                ""
            },
            if cli_set_multipv {
                " (multipv overridden)"
            } else {
                ""
            },
            if cli_set_min_depth {
                " (min_depth overridden)"
            } else {
                ""
            },
        ));
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
    if let Some(ref s) = preset_log {
        println!("Preset: {s}");
    }
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
    println!("Entropy scale: {:.3}", opts.entropy_scale);
    println!("Hash size (MB): {}", opts.hash_mb);
    println!("MultiPV: {}", opts.multipv);
    println!("Teacher profile: {:?}", opts.teacher_profile);
    println!("Reuse TT: {}", opts.reuse_tt);
    println!(
        "Ambiguity: gap2_th={}cp, require_exact={}, mate_mode={:?}",
        opts.amb_gap2_threshold, opts.amb_require_exact, opts.entropy_mate_mode
    );
    if let Some(n) = opts.nodes {
        println!("Nodes (limit): {}", n);
    }
    if let Some(j) = opts.jobs {
        println!("Jobs (outer parallelism): {}", j);
    }
    println!("CPU cores: {:?}", std::thread::available_parallelism());
    if resume_from > 0 || existing_lines > 0 {
        println!("Resuming from position: {}", resume_from.max(existing_lines));
    }
    println!("Skipped positions will be saved to: {}", skipped_path.display());
    println!("Note: skipped file contains timeouts and search errors (nonexact/empty PV)");
    println!("Note: TT memory usage scales with jobs: ~ hash_mb × jobs per process");

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

    // Resolve manifest path bound to output file name: <out>.manifest.json
    let manifest_path = {
        let stem = output_path.file_stem().unwrap_or_default().to_string_lossy();
        output_path.with_file_name(format!("{stem}.manifest.json"))
    };

    let input_file = File::open(&input_path)?;
    let reader = BufReader::new(input_file);
    let lines: Vec<String> = reader.lines().collect::<Result<_, _>>()?;

    fn normalize_sfen_tokens(sfen: &str) -> Option<String> {
        // Reconstruct first 4 tokens (board, side, hands, move count)
        let mut it = sfen.split_whitespace();
        let b = it.next()?;
        let s = it.next()?;
        let h = it.next()?;
        let m = it.next()?;
        Some(format!("{} {} {} {}", b, s, h, m))
    }

    fn extract_sfen(line: &str) -> Option<String> {
        let start = line.find("sfen ")? + 5;
        // normalize tabs to spaces to robustly detect " moves"
        let rest_raw = &line[start..];
        let rest_norm = rest_raw.replace('\t', " ");
        let end = rest_norm
            .find(" moves")
            .or_else(|| rest_norm.find('#'))
            .unwrap_or(rest_norm.len());
        let sfen = rest_norm[..end].trim();
        if sfen.is_empty() {
            return None;
        }
        normalize_sfen_tokens(sfen)
    }
    let sfen_positions: Vec<(usize, String)> = lines
        .into_iter()
        .enumerate()
        .filter_map(|(idx, line)| extract_sfen(line.trim()).map(|s| (idx, s)))
        .collect();

    let total_positions = sfen_positions.len();
    println!("\nFound {total_positions} positions in input file");

    // Optional: calibrate nodes from NPS if requested and nodes not explicitly set
    let mut manifest_existing: Option<Manifest> = None;
    if manifest_path.exists() {
        if let Ok(txt) = std::fs::read_to_string(&manifest_path) {
            if let Ok(m) = serde_json::from_str::<Manifest>(&txt) {
                manifest_existing = Some(m);
            }
        }
    }
    let mut calibration_to_write: Option<ManifestCalibration> = None;
    if opts.nodes.is_none() {
        if let Some(target_ms) = opts.nodes_autocalibrate_ms {
            // Reuse prior calibration if requested and available
            if opts.no_recalib && !opts.force_recalib {
                if let Some(ref man) = manifest_existing {
                    // Check compatibility
                    let engine_name = match opts.engine {
                        EngineType::Material => "material",
                        EngineType::Enhanced => "enhanced",
                        EngineType::Nnue => "nnue",
                        EngineType::EnhancedNnue => "enhanced-nnue",
                    };
                    let effective_depth =
                        opts.min_depth.map(|m| m.max(search_depth)).unwrap_or(search_depth);
                    let compat = man.engine == engine_name
                        && man.nnue_weights == opts.nnue_weights
                        && man.hash_mb == opts.hash_mb
                        && man.multipv == opts.multipv
                        && man.min_depth == effective_depth;
                    if compat {
                        if let Some(ref calib) = man.calibration {
                            if let Some(tn) = calib.target_nodes {
                                opts.nodes = Some(tn);
                                println!(
                                    "Reusing calibration from manifest: target_nodes={} (samples={:?}, min_depth={:?})",
                                    tn, calib.samples, calib.min_depth_used
                                );
                            }
                        }
                    } else {
                        println!("Existing calibration found but incompatible with current settings; recalibrating.");
                    }
                }
            }

            if opts.nodes.is_none() {
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
                    if global_stop.load(Ordering::Relaxed) {
                        break;
                    }
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
                        .stop_flag(global_stop.clone())
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
                    let target_nodes = target_nodes.max(10_000);
                    opts.nodes = Some(target_nodes);
                    println!(
                        "Auto-calibration done: NPS≈{:.0}, nodes target={} ({} ms)",
                        nps, target_nodes, target_ms
                    );
                    calibration_to_write = Some(ManifestCalibration {
                        nps: Some(nps),
                        target_nodes: Some(target_nodes),
                        samples: Some(sample_n),
                        min_depth_used: Some(opts.min_depth.unwrap_or(2).max(2)),
                        timestamp: Some(chrono::Utc::now().to_rfc3339()),
                    });
                } else {
                    println!("Auto-calibration failed (zero ms or nodes). Using time budget.");
                }
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
    let errors_nonexact_top1 = Arc::new(AtomicUsize::new(0));
    let errors_empty_pv = Arc::new(AtomicUsize::new(0));
    let total_attempted = Arc::new(AtomicUsize::new(positions_to_skip));

    let shared = GenShared {
        error_count: error_count.clone(),
        skipped_count: skipped_count.clone(),
        errors_parse: errors_parse.clone(),
        errors_nonexact_top1: errors_nonexact_top1.clone(),
        errors_empty_pv: errors_empty_pv.clone(),
        skipped_file: skipped_file.clone(),
    };

    let env = ProcEnv {
        depth: effective_depth,
        time_limit_ms,
        opts: &opts,
        shared: &shared,
        global_stop: global_stop.clone(),
    };

    // Process in batches (optionally inside a local rayon thread pool)
    let run_batches = || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for (batch_idx, chunk) in sfen_positions.chunks(batch_size).enumerate() {
            if global_stop.load(Ordering::Relaxed) {
                break;
            }
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
            let new_processed = processed_count
                .fetch_add(successful_results.len(), Ordering::Relaxed)
                + successful_results.len();
            let new_attempted =
                total_attempted.fetch_add(chunk.len(), Ordering::Relaxed) + chunk.len();

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
        Ok(())
    };

    if let Some(j) = opts.jobs {
        let pool = ThreadPoolBuilder::new().num_threads(j).build().expect("build rayon pool");
        let res = pool.install(run_batches);
        if let Err(e) = res {
            return Err(e as Box<dyn std::error::Error>);
        }
    } else if let Err(e) = run_batches() {
        return Err(e as Box<dyn std::error::Error>);
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
    println!("Errors (hard): {total_errors}");
    println!("  - parse: {e_parse}");
    println!("  - nonexact_top1: {e_nonexact}");
    println!("  - empty_or_missing_pv: {e_empty_pv}");
    println!("Skipped (timeout_overruns): {total_skipped}");

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

    // Build and write manifest.json
    let calibration_out = if calibration_to_write.is_some() {
        calibration_to_write
    } else if let Some(ref man) = manifest_existing {
        man.calibration.clone()
    } else {
        None
    };
    let engine_name = match opts.engine {
        EngineType::Material => "material",
        EngineType::Enhanced => "enhanced",
        EngineType::Nnue => "nnue",
        EngineType::EnhancedNnue => "enhanced-nnue",
    };
    let attempted_total = total_attempted.load(Ordering::Relaxed);
    // Compute output file SHA-256
    let (out_sha256, out_bytes) = (|| -> Option<(String, u64)> {
        use sha2::{Digest, Sha256};
        let mut f = File::open(&output_path).ok()?;
        let mut hasher = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        let mut total: u64 = 0;
        loop {
            let n = std::io::Read::read(&mut f, &mut buf).ok()?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            total += n as u64;
        }
        let hash = hasher.finalize();
        Some((hex::encode(hash), total))
    })()
    .unwrap_or((String::new(), 0));

    let manifest = Manifest {
        generated_at: chrono::Utc::now().to_rfc3339(),
        git_commit: std::env::var("GIT_COMMIT_HASH").ok(),
        engine: engine_name.to_string(),
        nnue_weights: opts.nnue_weights.clone(),
        preset: preset_name,
        overrides: Some(ManifestOverrides {
            time: cli_set_time,
            nodes: cli_set_nodes,
            hash_mb: cli_set_hash,
            multipv: cli_set_multipv,
            min_depth: cli_set_min_depth,
        }),
        teacher_profile: format!("{:?}", opts.teacher_profile),
        multipv: opts.multipv,
        budget: ManifestBudget {
            mode: if opts.nodes.is_some() {
                "nodes".to_string()
            } else {
                "time".to_string()
            },
            time_ms: if opts.nodes.is_some() {
                None
            } else {
                Some(time_limit_ms)
            },
            nodes: opts.nodes,
        },
        min_depth: effective_depth,
        hash_mb: opts.hash_mb,
        threads_per_engine: 1,
        jobs: opts.jobs,
        count: total_processed,
        cp_to_wdl_scale: opts.wdl_scale,
        wdl_semantics: "side_to_move".to_string(),
        calibration: calibration_out,
        attempted: attempted_total,
        skipped_timeout: total_skipped,
        errors: serde_json::json!({
            "parse": e_parse,
            "nonexact_top1": e_nonexact,
            "empty_or_missing_pv": e_empty_pv,
        }),
        reuse_tt: opts.reuse_tt,
        skip_overrun_factor: opts.skip_overrun_factor,
        search_depth_arg: search_depth,
        effective_min_depth: effective_depth,
        output_sha256: if out_sha256.is_empty() {
            None
        } else {
            Some(out_sha256)
        },
        output_bytes: if out_bytes == 0 {
            None
        } else {
            Some(out_bytes)
        },
    };
    if let Ok(txt) = serde_json::to_string_pretty(&manifest) {
        if let Err(e) = std::fs::write(&manifest_path, txt) {
            eprintln!(
                "Warning: failed to write manifest.json ({}): {}",
                manifest_path.display(),
                e
            );
        } else {
            println!("Manifest written: {}", manifest_path.display());
        }
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
    // Use global stop flag for graceful cancellation
    let stop_flag = env.global_stop.clone();

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

    let mut pos_clone = position.clone();
    let result = eng.search(&mut pos_clone, limits);
    let elapsed_stats_ms = result.stats.elapsed.as_millis() as u64;
    // Track which search stats we will report (K=2 by default; may switch to K=3)
    let mut result_used_time_ms = elapsed_stats_ms;
    let mut result_used_depth = result.stats.depth;
    let mut result_used_seldepth = result.stats.seldepth;
    let mut result_used_nodes = result.stats.nodes;
    let mut result_used_qnodes = result.stats.qnodes;
    let mut result_used_tt_hits = result.stats.tt_hits;
    let mut result_used_re_searches = result.stats.re_searches;
    let mut result_used_pv_changed = result.stats.pv_changed;
    let mut result_used_root_fh = result.stats.root_fail_high_count;
    let mut result_used_null_cuts = result.stats.null_cuts;
    let mut result_used_lmr_count = result.stats.lmr_count;
    let mut eval_used = result.score;
    let mut used_k3_research = false;

    // Determine time budget status in time mode
    let is_time_mode = env.opts.nodes.is_none();
    let timed_out = is_time_mode && (elapsed_stats_ms > env.time_limit_ms);
    let overrun = is_time_mode
        && (elapsed_stats_ms as f64) > (env.time_limit_ms as f64) * env.opts.skip_overrun_factor;

    // If overrun the skip threshold, record and skip this position
    if overrun {
        if idx < 10 || (idx + 1) % 100 == 0 {
            let elapsed_s = (elapsed_stats_ms as f64) / 1000.0;
            eprintln!("Position {} took too long ({:.1}s), skipping", idx + 1, elapsed_s);
        }
        env.shared.skipped_count.fetch_add(1, Ordering::Relaxed);

        // Write to skipped file with reason (JSON)
        if let Ok(mut file) = env.shared.skipped_file.lock() {
            let budget_ms = env.time_limit_ms;
            let overrun_factor = (elapsed_stats_ms as f64) / (budget_ms as f64);
            let obj = serde_json::json!({
                "sfen": sfen,
                "timeout": true,
                "elapsed_ms": elapsed_stats_ms,
                "budget_ms": budget_ms,
                "overrun_factor": overrun_factor,
                "depth_reached": result.stats.depth,
                "reason": "time_overrun",
                "mode": "time",
                "index": idx + 1,
            });
            writeln!(file, "{}", obj).ok();
            file.flush().ok();
        }

        return None;
    }

    let _eval = result.score; // superseded by eval_used
    let depth_reached = result.stats.depth;

    // Validate PV and top1 bound=Exact
    let mut has_lines = false;
    let mut top1_exact = false;
    if let Some(ref lines) = result.lines {
        if !lines.is_empty() {
            has_lines = true;
            if let Some(l0) = lines.first() {
                top1_exact = matches!(l0.bound, Bound::Exact);
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
                "mode": "search",
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
                "mode": "search",
                "index": idx + 1,
            });
            writeln!(file, "{}", obj).ok();
            file.flush().ok();
        }
        return None;
    }

    // Compute best2 gap, detect ambiguity, and optionally compute K=3 entropy
    let mut best2_gap_cp_meta = String::new();
    let mut ambiguous_k3 = false;
    let mut entropy_k3_opt: Option<f64> = None;
    let mut lines_used_opt: Option<Vec<engine_core::search::types::RootLine>> = None;
    if let Some(ref lines0) = result.lines {
        if lines0.len() >= 2 {
            let l0 = &lines0[0];
            let l1 = &lines0[1];
            let gap = (l0.score_cp - l1.score_cp).abs();
            best2_gap_cp_meta =
                format!(" best2_gap_cp:{} bound0:{:?} bound1:{:?}", gap, l0.bound, l1.bound);
            if is_ambiguous_for_k3(l0.bound, l1.bound, l0.score_cp, l1.score_cp, env.opts.amb_gap2_threshold, env.opts.amb_require_exact) {
                ambiguous_k3 = true;
                let mut have_three = lines0.len() >= 3;
                // lightweight copy: only first 3 lines
                let mut lines_k: Vec<_> = lines0.iter().take(3).cloned().collect();
                if !have_three {
                    // If we already exceeded time budget on K=2 in time mode, skip K=3 to avoid budget blow-up
                    if is_time_mode && timed_out {
                        have_three = false;
                    } else {
                        let mut pos2 = position.clone();
                        let mut builder2 = SearchLimits::builder()
                            .depth(env.depth)
                            .stop_flag(stop_flag.clone())
                            .multipv(3);
                        if let Some(n) = env.opts.nodes {
                            builder2 = builder2.fixed_nodes(n);
                        } else {
                            // Keep within remaining budget conservatively: at least 1/4 of time_limit
                            let rem = env.time_limit_ms.saturating_sub(elapsed_stats_ms);
                            let cap = rem.max(env.time_limit_ms / 4);
                            builder2 = builder2.fixed_time_ms(cap);
                        }
                        let limits2 = builder2.build();
                        let res2 = eng.search(&mut pos2, limits2);
                        if let Some(ref lines2) = res2.lines {
                            lines_k = lines2.iter().take(3).cloned().collect();
                            have_three = lines_k.len() >= 3;
                            // Switch reported stats to K=3 for consistency
                            result_used_time_ms = res2.stats.elapsed.as_millis() as u64;
                            result_used_depth = res2.stats.depth;
                            result_used_seldepth = res2.stats.seldepth;
                            result_used_nodes = res2.stats.nodes;
                            result_used_qnodes = res2.stats.qnodes;
                            result_used_tt_hits = res2.stats.tt_hits;
                            result_used_re_searches = res2.stats.re_searches;
                            result_used_pv_changed = res2.stats.pv_changed;
                            result_used_root_fh = res2.stats.root_fail_high_count;
                            result_used_null_cuts = res2.stats.null_cuts;
                            result_used_lmr_count = res2.stats.lmr_count;
                            eval_used = res2.score;
                            used_k3_research = true;
                        }
                    }
                }
                if have_three {
                    let ent = softmax_entropy_k3_from_lines(
                        &lines_k,
                        env.opts.entropy_scale,
                        env.opts.entropy_mate_mode,
                    );
                    entropy_k3_opt = ent;
                    lines_used_opt = Some(lines_k);
                }
            }
        }
    }

    // Compute labels
    let (label_kind, wdl_prob_opt) = match env.opts.label {
        LabelKind::Cp => ("cp", None),
        LabelKind::Wdl => ("wdl", Some(cp_to_wdl(eval_used, env.opts.wdl_scale))),
        LabelKind::Hybrid => {
            let use_wdl = position.ply as u32 <= env.opts.hybrid_ply_cutoff;
            if use_wdl {
                ("wdl", Some(cp_to_wdl(eval_used, env.opts.wdl_scale)))
            } else {
                ("cp", None)
            }
        }
    };

    // Metadata for quality tracking
    // Apply timeout mark if either K=2 or K=3 (if run) exceeded the time budget
    let timed_out_any = is_time_mode && (result_used_time_ms > env.time_limit_ms);
    let mut meta = if timed_out_any {
        format!(" # timeout_d{}", result_used_depth)
    } else {
        format!(" # d{}", result_used_depth)
    };

    // Add label type
    meta.push_str(&format!(" label:{}", label_kind));

    // Mark mate scores explicitly
    if is_mate_score(eval_used) {
        if let Some(md) = get_mate_distance(eval_used) {
            meta.push_str(&format!(" mate:{}", md));
        }
    }

    // Prefer adopted lines reference for output sections
    let fallback_small = result.lines.as_ref();
    let lines_ref_for_output: Option<&[engine_core::search::types::RootLine]> =
        if let Some(ref v) = lines_used_opt { Some(v.as_slice()) } else { fallback_small.map(|sv| sv.as_slice()) };

    // Output according to selected format
    match env.opts.output_format {
        OutputFormat::Text => {
            let mut line = format!("{sfen} eval {eval_used}");
            if let Some(p) = wdl_prob_opt {
                line.push_str(&format!(" wdl {:.6}", p));
            }
            line.push_str(&meta);
            // Recompute best2 gap on adopted lines if available
            if let Some(lines) = lines_ref_for_output {
                if lines.len() >= 2 {
                    let g = (lines[0].score_cp - lines[1].score_cp).abs();
                    line.push_str(&format!(
                        " best2_gap_cp:{} bound0:{:?} bound1:{:?}",
                        g, lines[0].bound, lines[1].bound
                    ));
                }
            } else if !best2_gap_cp_meta.is_empty() {
                line.push_str(&best2_gap_cp_meta);
            }
            if let Some(ent) = entropy_k3_opt {
                line.push_str(&format!(" ent_k3:{:.6}", ent));
            }
            if ambiguous_k3 {
                line.push_str(" ambiguous:true");
            }
            Some(line)
        }
        OutputFormat::Jsonl => {
            use serde_json::json;
            let mut bound1 = None;
            let mut bound2 = None;
            let mut gap2: Option<i32> = None;
            let mut lines_json = Vec::new();
            // Prefer adopted lines (from K=3 if present) else original
            let lines_ref: Option<&[engine_core::search::types::RootLine]> = lines_ref_for_output;
            if let Some(lines) = lines_ref {
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

            let tt_hit_rate = if result_used_nodes > 0 {
                result_used_tt_hits.map(|h| (h as f64) / (result_used_nodes as f64))
            } else {
                None
            };

            let root_idx_val: u32 = if let Some(lines) = lines_ref {
                lines.first().map(|l| (l.multipv_index.saturating_sub(1)) as u32).unwrap_or(0)
            } else {
                0
            };
            let lines_origin = if used_k3_research { "k3" } else { "k2" };
            let json_obj = json!({
                "sfen": sfen,
                "lines": lines_json,
                "depth": result_used_depth,
                "seldepth": result_used_seldepth,
                "nodes": result_used_nodes,
                "nodes_q": result_used_qnodes,
                "time_ms": result_used_time_ms,
                "aspiration_retries": result_used_re_searches,
                "pv_changed": result_used_pv_changed,
                "best2_gap_cp": gap2,
                "root_fail_high_count": result_used_root_fh,
                "used_null": result_used_null_cuts.map(|c| c > 0).unwrap_or(false),
                "lmr_applied": result_used_lmr_count.unwrap_or(0),
                "bound1": bound1,
                "bound2": bound2,
                "tt_hit_rate": tt_hit_rate,
                "root_move_index": root_idx_val,
                "lines_origin": lines_origin,
                "label": label_kind,
                "eval": eval_used,
                "wdl": wdl_prob_opt,
                "meta": meta.trim(),
                "ambiguous": ambiguous_k3,
                "softmax_entropy_k3": entropy_k3_opt,
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

#[inline]
fn is_ambiguous_for_k3(
    b0: Bound,
    b1: Bound,
    cp0: i32,
    cp1: i32,
    threshold: i32,
    require_exact: bool,
) -> bool {
    let exact_ok = !require_exact || (matches!(b0, Bound::Exact) && matches!(b1, Bound::Exact));
    let gap = (cp0 - cp1).abs();
    exact_ok && gap <= threshold
}

fn softmax_entropy_k3_from_lines(
    lines: &[engine_core::search::types::RootLine],
    scale: f64,
    mate_mode: MateEntropyMode,
) -> Option<f64> {
    if lines.len() < 3 {
        return None;
    }
    let mut cp: [i32; 3] = [0; 3];
    for i in 0..3 {
        let l = &lines[i];
        let is_mate = l.mate_distance.is_some();
        if is_mate {
            match mate_mode {
                MateEntropyMode::Exclude => return None,
                MateEntropyMode::Saturate => {
                    // Use the sign of score_cp to saturate
                    let sign = if l.score_cp >= 0 { 1 } else { -1 };
                    cp[i] = 32000 * sign;
                }
            }
        } else {
            cp[i] = l.score_cp;
        }
    }
    // logits = cp/scale, softmax, then entropy
    let logits = [
        cp[0] as f64 / scale,
        cp[1] as f64 / scale,
        cp[2] as f64 / scale,
    ];
    let max = logits.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let exps = [
        (logits[0] - max).exp(),
        (logits[1] - max).exp(),
        (logits[2] - max).exp(),
    ];
    let sum = exps[0] + exps[1] + exps[2];
    if sum == 0.0 || !sum.is_finite() {
        return None;
    }
    let probs = [exps[0] / sum, exps[1] / sum, exps[2] / sum];
    let entropy = -probs.iter().map(|&p| if p > 0.0 { p * p.ln() } else { 0.0 }).sum::<f64>();
    Some(entropy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::search::types::{Bound, RootLine};
    use smallvec::smallvec;

    fn mk_line(cp: i32) -> RootLine {
        RootLine {
            multipv_index: 1,
            root_move: engine_core::shogi::Move::null(),
            score_internal: cp,
            score_cp: cp,
            bound: Bound::Exact,
            depth: 1,
            seldepth: None,
            pv: smallvec![],
            nodes: None,
            time_ms: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        }
    }

    #[test]
    fn test_softmax_entropy_k3() {
        let lines = vec![mk_line(10), mk_line(9), mk_line(8)];
        let ent = softmax_entropy_k3_from_lines(&lines, 600.0, MateEntropyMode::Saturate);
        assert!(ent.is_some());
        let v = ent.unwrap();
        assert!(v > 0.0 && v.is_finite());

        let mut lines2 = lines.clone();
        lines2[0].mate_distance = Some(3);
        let ent2 = softmax_entropy_k3_from_lines(&lines2, 600.0, MateEntropyMode::Exclude);
        assert!(ent2.is_none());
    }

    #[test]
    fn test_timeout_vs_overrun_flags() {
        let (limit, factor) = (100_u64, 2.0_f64);
        // helper mirrors production logic
        let decide = |elapsed: u64| -> (bool, bool) {
            let timed_out = elapsed > limit;
            let overrun = (elapsed as f64) > (limit as f64) * factor;
            (timed_out, overrun)
        };
        assert_eq!(decide(90), (false, false));
        assert_eq!(decide(150), (true, false));
        assert_eq!(decide(250), (true, true));
    }

    #[test]
    fn test_is_ambiguous_for_k3() {
        // exact required, small gap
        assert!(is_ambiguous_for_k3(Bound::Exact, Bound::Exact, 10, 0, 25, true));
        // exact required, nonexact bound -> false
        assert!(!is_ambiguous_for_k3(Bound::UpperBound, Bound::Exact, 5, 0, 25, true));
        // allow inexact
        assert!(is_ambiguous_for_k3(Bound::UpperBound, Bound::Exact, 5, 0, 25, false));
        // gap too large
        assert!(!is_ambiguous_for_k3(Bound::Exact, Bound::Exact, 100, 0, 25, true));
    }
}
