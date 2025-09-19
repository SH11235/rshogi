use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::common::{get_mate_distance, is_mate_score};
use engine_core::search::limits::SearchLimits;
use engine_core::search::types::{Bound, TeacherProfile};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tools::common::io::open_reader;

fn derive_skipped_path(out: &std::path::Path) -> std::path::PathBuf {
    let stem = out.file_stem().unwrap_or_default().to_string_lossy().to_string();
    let ext = out.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.is_empty() {
        out.with_file_name(format!("{stem}_skipped"))
    } else {
        out.with_file_name(format!("{stem}_skipped.{ext}"))
    }
}

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
enum CompressionKind {
    None,
    Gz,
    Zst,
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
    split_every: Option<usize>,
    compress: CompressionKind,
    structured_log: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ManifestAmbiguity {
    gap2_threshold_cp: i32,
    require_exact: bool,
    mate_mode: String,
    entropy_scale: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestErrorsSummary {
    parse: usize,
    nonexact_top1: usize,
    empty_or_missing_pv: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestCountsSummary {
    attempted: usize,
    success: usize,
    skipped_timeout: usize,
    errors: ManifestErrorsSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestThroughputSummary {
    attempted_sps: f64,
    success_sps: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestRatesSummary {
    timeout: f64,
    top1_exact: f64,
    both_exact: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestAmbiguousSummary {
    threshold_cp: i32,
    require_exact: bool,
    count: usize,
    denom: usize,
    rate: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reran: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    with_entropy: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestDepthSummary {
    histogram: Vec<usize>,
    min: u8,
    max: u8,
    p50: u8,
    p90: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestSummary {
    elapsed_sec: f64,
    throughput: ManifestThroughputSummary,
    rates: ManifestRatesSummary,
    ambiguous: ManifestAmbiguousSummary,
    depth: ManifestDepthSummary,
    counts: ManifestCountsSummary,
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
    #[serde(default)]
    manifest_scope: Option<String>,
    // Manifest v2 provenance (required)
    #[serde(default)]
    teacher_engine: TeacherEngineInfo,
    #[serde(default)]
    generation_command: String,
    #[serde(default)]
    seed: u64,
    #[serde(default)]
    manifest_version: String,
    #[serde(default)]
    input: ManifestInputInfo,
    #[serde(default)]
    nnue_weights_sha256: Option<String>,
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
    part_index: Option<usize>,
    part_count: Option<usize>,
    count_in_part: Option<usize>,
    compression: Option<String>,
    ambiguity: Option<ManifestAmbiguity>,
    #[serde(default)]
    summary: Option<ManifestSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TeacherEngineInfo {
    name: String,
    version: String,
    commit: Option<String>,
    usi_opts: TeacherUsiOpts,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TeacherUsiOpts {
    hash_mb: usize,
    multipv: u8,
    threads: usize,
    teacher_profile: String,
    min_depth: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ManifestInputInfo {
    path: String,
    sha256: Option<String>,
    bytes: Option<u64>,
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
    skipped_file: Arc<Mutex<BufWriter<File>>>,
    // extra counters for summary
    lines_ge2: Arc<AtomicUsize>,
    both_exact: Arc<AtomicUsize>,
    ambiguous_k3: Arc<AtomicUsize>,
    depth_hist: Arc<Mutex<Vec<usize>>>,
    k3_reran: Arc<AtomicUsize>,
    k3_entropy: Arc<AtomicUsize>,
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
        eprintln!("  --split <N> (rotate output every N lines as <out>.part-0001...)");
        eprintln!("  --compress <gz|zst> (compress output parts; zst requires 'zstd' feature)");
        eprintln!("  --structured-log <PATH|-> (emit JSONL structured logs to file or STDOUT)");
        eprintln!("  --amb-gap2-threshold <cp> (default: 25; ambiguity threshold for gap2)");
        eprintln!(
            "  --amb-allow-inexact (allow non-Exact bounds for ambiguity; default requires Exact)"
        );
        eprintln!("  --entropy-mate-mode <exclude|saturate> (mate handling in entropy; default: saturate)");
        eprintln!("  --no-recalib (reuse manifest calibration if available)");
        eprintln!("  --force-recalib (force re-calibration even if manifest exists)");
        eprintln!("  (Input) You can pass '-' to read SFEN from STDIN");
        eprintln!("\nRecommended settings for initial NNUE data:");
        eprintln!("  - Depth 2: Fast collection, basic evaluation");
        eprintln!("  - Depth 3: Balanced speed/quality");
        eprintln!("  - Depth 4+: High quality but slower");
        std::process::exit(1);
    }

    // Optimized line counting (non-UTF8 aware, counts '\n' bytes)
    fn fast_count_lines(path: &Path) -> std::io::Result<usize> {
        let mut f = File::open(path)?;
        let mut buf = [0u8; 1 << 20]; // 1 MiB
        let mut cnt: usize = 0;
        let mut last_byte: Option<u8> = None;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            for &b in &buf[..n] {
                if b == b'\n' {
                    cnt += 1;
                }
            }
            last_byte = Some(buf[n - 1]);
        }
        // Count the final line if file doesn't end with a newline
        if last_byte.is_some() && last_byte != Some(b'\n') {
            cnt += 1;
        }
        Ok(cnt)
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
        split_every: None,
        compress: CompressionKind::None,
        structured_log: None,
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
            "--split" => {
                if let Some(v) = args.get(i + 1).and_then(|s| s.parse::<usize>().ok()) {
                    if v == 0 {
                        eprintln!("Error: --split requires N > 0");
                        std::process::exit(1);
                    }
                    opts.split_every = Some(v);
                    i += 2;
                } else {
                    eprintln!("Error: --split requires an integer value");
                    std::process::exit(1);
                }
            }
            "--compress" => {
                if let Some(v) = args.get(i + 1) {
                    match v.to_ascii_lowercase().as_str() {
                        "gz" => opts.compress = CompressionKind::Gz,
                        "zst" => opts.compress = CompressionKind::Zst,
                        other => {
                            eprintln!("Error: --compress must be gz|zst (got '{}')", other);
                            std::process::exit(1);
                        }
                    }
                    i += 2;
                } else {
                    eprintln!("Error: --compress requires a value");
                    std::process::exit(1);
                }
            }
            "--structured-log" => {
                if let Some(p) = args.get(i + 1) {
                    opts.structured_log = Some(p.clone());
                    i += 2;
                } else {
                    eprintln!("Error: --structured-log requires a path or '-' for STDOUT");
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
    let skipped_path = derive_skipped_path(&output_path);

    // Validate depth
    if !(1..=10).contains(&search_depth) {
        eprintln!("Error: Depth must be between 1 and 10");
        std::process::exit(1);
    }

    // existing_lines will be computed later (only for non-split/non-compress mode)
    let mut existing_lines: usize = 0;

    // Route human-readable logs to STDERR when structured JSON goes to STDOUT
    let human_to_stderr = matches!(opts.structured_log.as_deref(), Some("-"));
    macro_rules! human_log {
        ($($arg:tt)*) => {
            if human_to_stderr { eprintln!($($arg)*); } else { println!($($arg)*); }
        }
    }

    human_log!("NNUE Training Data Generator");
    human_log!("============================");
    let effective_depth = opts.min_depth.map(|m| m.max(search_depth)).unwrap_or(search_depth);
    human_log!("Search depth: {effective_depth}");
    if let Some(ref s) = preset_log {
        human_log!("Preset: {s}");
    }
    human_log!("Batch size: {batch_size}");
    human_log!("Engine: {:?}", opts.engine);
    if let Some(ref w) = opts.nnue_weights {
        human_log!("NNUE weights: {w}");
    }
    human_log!("Label: {:?}", opts.label);
    if matches!(opts.label, LabelKind::Wdl | LabelKind::Hybrid) {
        human_log!("WDL scale: {:.3}", opts.wdl_scale);
        if matches!(opts.label, LabelKind::Hybrid) {
            human_log!("Hybrid cutoff ply: {}", opts.hybrid_ply_cutoff);
        }
    }
    human_log!("Entropy scale: {:.3}", opts.entropy_scale);
    human_log!("Hash size (MB): {}", opts.hash_mb);
    human_log!("MultiPV: {}", opts.multipv);
    human_log!("Teacher profile: {:?}", opts.teacher_profile);
    human_log!("Reuse TT: {}", opts.reuse_tt);
    human_log!(
        "Ambiguity: gap2_th={}cp, require_exact={}, mate_mode={:?}",
        opts.amb_gap2_threshold,
        opts.amb_require_exact,
        opts.entropy_mate_mode
    );
    if let Some(n) = opts.nodes {
        let cap_actual = std::cmp::max(n / 4, 10_000);
        human_log!("Nodes (limit): {} (K=3 cap: max(n/4, 10000) = {})", n, cap_actual);
    }
    if let Some(j) = opts.jobs {
        human_log!("Jobs (outer parallelism): {}", j);
    }
    human_log!("CPU cores: {:?}", std::thread::available_parallelism());
    human_log!("Skipped positions will be saved to: {}", skipped_path.display());
    human_log!("Note: skipped file contains timeouts and search errors (nonexact/empty PV)");
    human_log!("Note: TT memory usage scales with jobs: ~ hash_mb × jobs per process");
    if let Some(n) = opts.split_every {
        human_log!("Split every: {} lines", n);
    }
    human_log!(
        "Compression: {}",
        match opts.compress {
            CompressionKind::None => "none",
            CompressionKind::Gz => "gz",
            CompressionKind::Zst => "zst",
        }
    );
    if let Some(ref p) = opts.structured_log {
        human_log!("Structured log: {}", p);
    }

    // Early feature guard for zstd compression for better UX
    if matches!(opts.compress, CompressionKind::Zst) {
        #[cfg(not(feature = "zstd"))]
        {
            eprintln!("Error: --compress zst requires building with `--features zstd`");
            std::process::exit(1);
        }
    }

    // Calculate time limit based on depth (used only if --nodes is not set)
    let time_limit_ms = opts.time_limit_override_ms.unwrap_or(match effective_depth {
        1 => 50,
        2 => 100,
        3 => 200,
        4 => 400,
        _ => 800,
    });
    if opts.nodes.is_none() {
        human_log!("Time limit per position: {time_limit_ms}ms");
    } else {
        human_log!("Nodes-based budget active; time limit ignored for search.");
    }

    // Decide if we use split/compressed output
    let use_parted_output =
        opts.split_every.is_some() || !matches!(opts.compress, CompressionKind::None);

    // Count existing lines only when writing to a single non-parted file
    if !use_parted_output {
        if output_path.exists() {
            let count = fast_count_lines(&output_path)?;
            if count > 0 {
                human_log!("Found existing output file with {count} lines");
                if resume_from > 0 && resume_from != count {
                    human_log!(
                        "Warning: resume_from ({resume_from}) differs from existing lines ({count})"
                    );
                    human_log!("Using the larger value: {}", resume_from.max(count));
                }
            }
            existing_lines = count;
        } else if resume_from > 0 {
            human_log!(
                "Warning: Output file does not exist, but resume_from is set to {resume_from}"
            );
            human_log!("Starting from position {resume_from} anyway");
        }
    }

    // Now that existing_lines is known (for non-parted mode), print resume info
    if resume_from > 0 || existing_lines > 0 {
        human_log!("Resuming from position: {}", resume_from.max(existing_lines));
    }

    // Open files - append mode if resuming (only when not using split/compress)
    // Ensure trailing newline if we will append to an existing non-parted file
    fn ensure_trailing_newline(path: &std::path::Path) -> std::io::Result<()> {
        use std::io::{Read, Seek, SeekFrom, Write};
        if !path.exists() {
            return Ok(());
        }
        let mut f = OpenOptions::new().read(true).open(path)?;
        let len = f.seek(SeekFrom::End(0))?;
        if len == 0 {
            return Ok(());
        }
        f.seek(SeekFrom::End(-1))?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last)?;
        drop(f);
        if last[0] != b'\n' {
            let mut w = OpenOptions::new().append(true).open(path)?;
            w.write_all(b"\n")?;
            w.flush()?;
        }
        Ok(())
    }

    let output_file: Option<Arc<Mutex<BufWriter<File>>>> = if !use_parted_output {
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .append(resume_from > 0 || existing_lines > 0)
            .truncate(resume_from == 0 && existing_lines == 0)
            .open(&output_path)?;
        if resume_from > 0 || existing_lines > 0 {
            // We've opened the file (may be in append mode). Ensure trailing newline before writing more.
            let _ = ensure_trailing_newline(&output_path);
        }
        let bw = BufWriter::with_capacity(1 << 20, f);
        Some(Arc::new(Mutex::new(bw)))
    } else {
        None
    };

    // Open skipped positions file (always append mode to not lose data)
    let skipped_raw = OpenOptions::new().create(true).append(true).open(&skipped_path)?;
    let skipped_file = Arc::new(Mutex::new(BufWriter::with_capacity(1 << 20, skipped_raw)));

    // Resolve manifest path bound to output file name: <out>.manifest.json
    let manifest_path = {
        let stem = output_path.file_stem().unwrap_or_default().to_string_lossy();
        output_path.with_file_name(format!("{stem}.manifest.json"))
    };

    // Split/compress part manager
    // Best-effort atomic write helper for small text files (progress snapshots)
    fn write_atomic_best_effort(path: &std::path::Path, s: &str) {
        let tmp = path.with_extension("tmp");
        if let Ok(mut f) = std::fs::File::create(&tmp) {
            if f.write_all(s.as_bytes()).is_ok() && f.sync_all().is_ok() {
                if std::fs::rename(&tmp, path).is_err() {
                    let _ = std::fs::remove_file(path);
                    let _ = std::fs::rename(&tmp, path);
                }
                return;
            }
        }
        let _ = std::fs::remove_file(&tmp);
    }

    // Stable 64-bit seed from argv[1..] using SHA-256
    fn stable_seed_from_args(args: &[String]) -> u64 {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for a in args.iter().skip(1) {
            hasher.update(a.as_bytes());
            hasher.update([0]); // delimiter
        }
        let digest = hasher.finalize();
        u64::from_le_bytes(digest[0..8].try_into().unwrap())
    }

    // Compute SHA-256 and byte size for a path
    fn compute_sha_and_bytes(path: &std::path::Path) -> Option<(String, u64)> {
        use sha2::{Digest, Sha256};
        let mut f = std::fs::File::open(path).ok()?;
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
    }

    struct PartManifestInfo {
        path: std::path::PathBuf,
        count_in_part: usize,
    }
    // Writers for part files with explicit finalization semantics
    trait PartWrite: Write + Send {
        fn finalize(&mut self) -> std::io::Result<()>;
    }
    struct PlainPartWriter {
        inner: BufWriter<File>,
    }
    impl Write for PlainPartWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    impl PartWrite for PlainPartWriter {
        fn finalize(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    struct GzPartWriter {
        inner: flate2::write::GzEncoder<BufWriter<File>>,
    }
    impl Write for GzPartWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }
    impl PartWrite for GzPartWriter {
        fn finalize(&mut self) -> std::io::Result<()> {
            self.inner.try_finish()?;
            // Ensure underlying BufWriter<File> is flushed to the OS
            self.inner.get_mut().flush()
        }
    }
    #[cfg(feature = "zstd")]
    struct ZstPartWriter {
        inner: Option<zstd::stream::write::Encoder<'static, BufWriter<File>>>,
    }
    #[cfg(feature = "zstd")]
    impl Write for ZstPartWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.inner.as_mut().unwrap().write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.as_mut().unwrap().flush()
        }
    }
    #[cfg(feature = "zstd")]
    impl PartWrite for ZstPartWriter {
        fn finalize(&mut self) -> std::io::Result<()> {
            if let Some(enc) = self.inner.take() {
                // finish() returns the underlying BufWriter<File>; flush it to the OS
                let mut bw = enc.finish()?;
                bw.flush()?;
            }
            Ok(())
        }
    }
    struct PartManager {
        base_stem: String,
        base_ext: String,
        dir: std::path::PathBuf,
        split_every: usize,
        compress: CompressionKind,
        current_part: usize,
        lines_in_part: usize,
        writer: Option<Box<dyn PartWrite>>,
        part_manifests: Vec<PartManifestInfo>,
    }
    impl PartManager {
        fn new(
            output_path: &std::path::Path,
            split_every: usize,
            compress: CompressionKind,
        ) -> Self {
            let dir =
                output_path.parent().unwrap_or_else(|| std::path::Path::new(".")).to_path_buf();
            let stem = output_path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            let ext = output_path.extension().unwrap_or_default().to_string_lossy().to_string();
            Self {
                base_stem: stem,
                base_ext: ext,
                dir,
                split_every,
                compress,
                current_part: 0,
                lines_in_part: 0,
                writer: None,
                part_manifests: Vec::new(),
            }
        }
        fn make_part_paths(&self, idx: usize) -> (std::path::PathBuf, std::path::PathBuf) {
            let part_name = if self.base_ext.is_empty() {
                format!("{}.part-{:04}", self.base_stem, idx)
            } else {
                format!("{}.part-{:04}.{}", self.base_stem, idx, self.base_ext)
            };
            let mut file_name = part_name.clone();
            match self.compress {
                CompressionKind::None => {}
                CompressionKind::Gz => file_name.push_str(".gz"),
                CompressionKind::Zst => file_name.push_str(".zst"),
            }
            let out_path = self.dir.join(file_name);
            let prog_path = self.dir.join(format!("{}.part-{:04}.progress", self.base_stem, idx));
            (out_path, prog_path)
        }
        fn open_next(&mut self) -> std::io::Result<(std::path::PathBuf, std::path::PathBuf)> {
            self.current_part += 1;
            self.lines_in_part = 0;
            let (out_path, prog_path) = self.make_part_paths(self.current_part);
            let file = File::create(&out_path)?;
            let buf = BufWriter::with_capacity(1 << 20, file);
            let w: Box<dyn PartWrite> = match self.compress {
                CompressionKind::None => Box::new(PlainPartWriter { inner: buf }),
                CompressionKind::Gz => {
                    let enc = flate2::write::GzEncoder::new(buf, flate2::Compression::default());
                    Box::new(GzPartWriter { inner: enc })
                }
                CompressionKind::Zst => {
                    #[cfg(feature = "zstd")]
                    {
                        let enc = zstd::stream::write::Encoder::new(buf, 0)
                            .map_err(std::io::Error::other)?;
                        Box::new(ZstPartWriter { inner: Some(enc) })
                    }
                    #[cfg(not(feature = "zstd"))]
                    {
                        return Err(std::io::Error::other(
                            "zstd compression requires 'zstd' feature",
                        ));
                    }
                }
            };
            self.writer = Some(w);
            Ok((out_path, prog_path))
        }
        fn write_lines(&mut self, lines: &[String]) -> std::io::Result<()> {
            for line in lines {
                if self.current_part == 0 || (self.lines_in_part >= self.split_every) {
                    self.finish_part().ok();
                    self.open_next()?;
                }
                let w = self.writer.as_mut().unwrap();
                writeln!(w, "{}", line)?;
                self.lines_in_part += 1;
            }
            Ok(())
        }
        fn finish_part(&mut self) -> std::io::Result<()> {
            let was_open = self.writer.is_some();
            if let Some(mut w) = self.writer.take() {
                w.finalize()?;
            }
            if was_open && self.current_part > 0 {
                let (out_path, prog) = self.make_part_paths(self.current_part);
                // Write final progress snapshot for the part (best effort, atomic)
                write_atomic_best_effort(&prog, &self.lines_in_part.to_string());
                self.part_manifests.push(PartManifestInfo {
                    path: out_path,
                    count_in_part: self.lines_in_part,
                });
            }
            Ok(())
        }
        fn update_part_progress(&self) -> std::io::Result<()> {
            if self.current_part == 0 {
                return Ok(());
            }
            let (_out, prog) = self.make_part_paths(self.current_part);
            write_atomic_best_effort(&prog, &self.lines_in_part.to_string());
            Ok(())
        }
    }

    impl Drop for PartManager {
        fn drop(&mut self) {
            let _ = self.finish_part();
        }
    }

    // Always create PartManager when using split or any compression
    let mut part_mgr = if use_parted_output {
        let n = opts.split_every.unwrap_or(usize::MAX);
        Some(PartManager::new(&output_path, n, opts.compress))
    } else {
        None
    };

    // Optional structured JSONL logger
    struct StructuredLogger {
        to_stdout: bool,
        file: Option<Mutex<BufWriter<File>>>,
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
                let f = OpenOptions::new().create(true).append(true).open(path)?;
                let bw = BufWriter::with_capacity(1 << 20, f);
                Ok(Self {
                    to_stdout: false,
                    file: Some(Mutex::new(bw)),
                })
            }
        }
        fn write_json(&self, v: &serde_json::Value) {
            if self.to_stdout {
                println!("{}", v);
            } else if let Some(ref file) = self.file {
                if let Ok(mut w) = file.lock() {
                    let _ = writeln!(w, "{}", v);
                    let _ = w.flush();
                }
            }
        }
    }
    let structured_logger: Option<StructuredLogger> = match opts.structured_log.as_deref() {
        Some(path) => match StructuredLogger::new(path) {
            Ok(lg) => Some(lg),
            Err(e) => {
                eprintln!("Warning: failed to open structured log '{}': {}", path, e);
                None
            }
        },
        None => None,
    };

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
        tools::common::sfen::normalize_4t(sfen)
    }
    // First pass: count positions and optionally collect samples for calibration
    let mut total_positions: usize = 0;
    let mut calib_samples: Vec<String> = Vec::new();
    let need_calib_samples = opts.nodes.is_none() && opts.nodes_autocalibrate_ms.is_some();
    let sample_cap = if need_calib_samples {
        opts.calibrate_sample.max(10)
    } else {
        0
    };
    // When reading from stdin ("-"), we tee the stream into a temporary file during pass-1
    // so that pass-2 can re-read the same data. This keeps memory bounded while preserving
    // the two-pass behavior (counting + processing).
    struct TeeGuard(Option<PathBuf>);
    impl Drop for TeeGuard {
        fn drop(&mut self) {
            if let Some(p) = self.0.take() {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    let is_stdin = input_path.to_string_lossy() == "-";
    let mut tee_tmp_path: Option<PathBuf> = None;
    let mut tee_writer: Option<BufWriter<File>> = None;
    let mut tee_guard = TeeGuard(None);
    if is_stdin {
        // Try a small number of unique suffixes to avoid leftovers blocking creation
        let base = format!("generate_nnue_training_data.stdin.{}.tmp", std::process::id());
        let mut opened: Option<(File, PathBuf)> = None;
        for attempt in 0..5 {
            let cand = if attempt == 0 {
                std::env::temp_dir().join(&base)
            } else {
                std::env::temp_dir().join(format!("{base}.{}", attempt))
            };
            let mut opts = OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                opts.mode(0o600);
            }
            match opts.open(&cand) {
                Ok(f) => {
                    opened = Some((f, cand));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    continue;
                }
                Err(e) => {
                    eprintln!(
                        "Error: failed to create temporary tee file for STDIN: {}. Cannot safely re-read in pass-2.",
                        e
                    );
                    eprintln!(
                        "Hint: set TMPDIR to a writable filesystem or provide an input file instead of '-'"
                    );
                    std::process::exit(1);
                }
            }
        }
        let (f, path) = opened.unwrap_or_else(|| {
            eprintln!("Error: failed to create unique temporary tee file after retries.");
            eprintln!(
                "Hint: set TMPDIR to a writable filesystem or provide an input file instead of '-'"
            );
            std::process::exit(1);
        });
        tee_writer = Some(BufWriter::with_capacity(1 << 20, f));
        tee_tmp_path = Some(path.clone());
        tee_guard.0 = Some(path.clone());
        human_log!(
            "Teeing STDIN to temporary file: {} (writing entire STDIN to disk; ensure TMPDIR has enough free space)",
            path.display()
        );
    }
    {
        let mut reader = open_reader(&input_path)?;
        let mut line = String::new();
        loop {
            line.clear();
            let n = std::io::BufRead::read_line(&mut reader, &mut line)?;
            if n == 0 {
                break;
            }
            if let Some(w) = tee_writer.as_mut() {
                // Write original line (includes newline from read_line)
                w.write_all(line.as_bytes())?;
            }
            if let Some(s) = extract_sfen(line.trim()) {
                total_positions += 1;
                if need_calib_samples && calib_samples.len() < sample_cap {
                    calib_samples.push(s);
                }
            }
        }
    }
    if let Some(mut w) = tee_writer.take() {
        w.flush()?;
    }
    human_log!("\nFound {total_positions} positions in input file");

    // Effective input path for pass-2 (use tee temp for stdin, else original path)
    let effective_input_path: PathBuf =
        tee_tmp_path.as_ref().cloned().unwrap_or_else(|| input_path.clone());

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
                    // Prefer v2 SHA match for nnue_weights when available; fallback to path equality for v1
                    let nnue_match = if let (Some(msha), Some(ref wpath)) =
                        (man.nnue_weights_sha256.as_ref(), opts.nnue_weights.as_ref())
                    {
                        if let Some((h, _)) = compute_sha_and_bytes(std::path::Path::new(wpath)) {
                            h.as_str() == msha.as_str()
                        } else {
                            false
                        }
                    } else {
                        man.nnue_weights == opts.nnue_weights
                    };
                    let compat = man.engine == engine_name
                        && nnue_match
                        && man.hash_mb == opts.hash_mb
                        && man.multipv == opts.multipv
                        && man.min_depth == effective_depth;
                    if compat {
                        if let Some(ref calib) = man.calibration {
                            if let Some(tn) = calib.target_nodes {
                                opts.nodes = Some(tn);
                                human_log!(
                                    "Reusing calibration from manifest: target_nodes={} (samples={:?}, min_depth={:?})",
                                    tn, calib.samples, calib.min_depth_used
                                );
                            }
                        }
                    } else {
                        human_log!("Existing calibration found but incompatible with current settings; recalibrating.");
                    }
                }
            }

            if opts.nodes.is_none() {
                human_log!("Starting nodes auto-calibration: target {} ms", target_ms);
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
                let sample_n = calib_samples.len().min(opts.calibrate_sample.max(10));
                for (i, sfen) in calib_samples.iter().take(sample_n).enumerate() {
                    if global_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if i % 25 == 0 {
                        human_log!("  calibrating {}/{}", i + 1, sample_n);
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
                    human_log!(
                        "Auto-calibration done: NPS≈{:.0}, nodes target={} ({} ms)",
                        nps,
                        target_nodes,
                        target_ms
                    );
                    calibration_to_write = Some(ManifestCalibration {
                        nps: Some(nps),
                        target_nodes: Some(target_nodes),
                        samples: Some(sample_n),
                        min_depth_used: Some(opts.min_depth.unwrap_or(2).max(2)),
                        timestamp: Some(chrono::Utc::now().to_rfc3339()),
                    });
                } else {
                    human_log!("Auto-calibration failed (zero ms or nodes). Using time budget.");
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
            human_log!("Progress file shows {progress} positions attempted (including skipped)");
            human_log!("Output file has {skip_count} successful results");
            human_log!("Difference of {} positions were skipped/failed", progress - skip_count);
        }
        progress
    } else {
        skip_count
    };

    // Skip based on the maximum of skip_count and actual_progress to avoid double-skip
    let positions_to_skip = skip_count.max(actual_progress);
    if positions_to_skip > 0 {
        human_log!("Skipping first {positions_to_skip} positions (already attempted)");
    }
    let remaining_positions = total_positions.saturating_sub(positions_to_skip);
    human_log!("Processing {remaining_positions} remaining positions");

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
        lines_ge2: Arc::new(AtomicUsize::new(0)),
        both_exact: Arc::new(AtomicUsize::new(0)),
        ambiguous_k3: Arc::new(AtomicUsize::new(0)),
        depth_hist: Arc::new(Mutex::new(Vec::new())),
        k3_reran: Arc::new(AtomicUsize::new(0)),
        k3_entropy: Arc::new(AtomicUsize::new(0)),
    };

    let env = ProcEnv {
        depth: effective_depth,
        time_limit_ms,
        opts: &opts,
        shared: &shared,
        global_stop: global_stop.clone(),
    };

    // Overall timer for elapsed seconds in summary
    let overall_start = std::time::Instant::now();

    // Process in batches (optionally inside a local rayon thread pool) from a streaming reader
    let mut run_batches = || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut reader = open_reader(&effective_input_path)?;
        let mut line = String::new();
        let mut seen_valid: usize = 0; // valid SFENs seen so far
        let mut batch: Vec<(usize, String)> = Vec::with_capacity(batch_size.min(1024));
        let total_batches = remaining_positions.div_ceil(batch_size);
        let mut batch_idx: usize = 0;
        loop {
            if global_stop.load(Ordering::Relaxed) {
                break;
            }
            line.clear();
            let n = std::io::BufRead::read_line(&mut reader, &mut line)?;
            if n == 0 {
                // EOF: flush remaining batch if any
                if !batch.is_empty() {
                    batch_idx += 1;
                    let batch_start = std::time::Instant::now();
                    human_log!(
                        "\nBatch {}/{}: Processing {} positions...",
                        batch_idx,
                        total_batches,
                        batch.len()
                    );
                    let batch_results: Vec<_> = batch
                        .par_iter()
                        .map_init(
                            || {
                                let mut eng = Engine::new(opts.engine);
                                eng.set_hash_size(opts.hash_mb);
                                eng.set_threads(1);
                                eng.set_multipv_persistent(opts.multipv);
                                eng.set_teacher_profile(opts.teacher_profile);
                                if matches!(
                                    opts.engine,
                                    EngineType::Nnue | EngineType::EnhancedNnue
                                ) {
                                    if let Some(ref path) = opts.nnue_weights {
                                        if let Err(e) = eng.load_nnue_weights(path) {
                                            eprintln!(
                                                "Failed to load NNUE weights ({}): {}",
                                                path, e
                                            );
                                        }
                                    }
                                }
                                eng
                            },
                            |eng, (idx, sfen)| process_position_with_engine(*idx, sfen, &env, eng),
                        )
                        .collect();
                    let successful_results: Vec<_> = batch_results.into_iter().flatten().collect();
                    if use_parted_output {
                        if let Some(pm) = part_mgr.as_mut() {
                            pm.write_lines(&successful_results)?;
                            if let Err(e) = pm.update_part_progress() {
                                eprintln!("Warning: failed to write part progress: {}", e);
                            }
                        }
                    } else if let Some(ref of) = output_file {
                        let mut file = of.lock().unwrap();
                        for result in &successful_results {
                            writeln!(file, "{result}")?;
                        }
                        file.flush()?;
                    }
                    let new_processed = processed_count
                        .fetch_add(successful_results.len(), Ordering::Relaxed)
                        + successful_results.len();
                    let new_attempted =
                        total_attempted.fetch_add(batch.len(), Ordering::Relaxed) + batch.len();
                    write_atomic_best_effort(&progress_path, &new_attempted.to_string());
                    let batch_time = batch_start.elapsed();
                    let positions_per_sec = batch.len() as f64 / batch_time.as_secs_f64();
                    human_log!(
                        "Batch complete: {} results in {:.1}s ({:.0} pos/sec)",
                        successful_results.len(),
                        batch_time.as_secs_f32(),
                        positions_per_sec
                    );
                    human_log!(
                        "Overall progress: {new_processed}/{total_positions} ({:.1}%)",
                        (new_processed as f64 / total_positions as f64) * 100.0
                    );
                    if let Some(ref lg) = structured_logger {
                        let rec = serde_json::json!({
                            "kind": "batch",
                            "version": 1,
                            "batch_index": batch_idx - 1,
                            "size": batch.len(),
                            "success": successful_results.len(),
                            "elapsed_sec": batch_time.as_secs_f64(),
                            "sps": positions_per_sec,
                            "attempted_sps": positions_per_sec,
                            "processed_total": new_processed,
                            "attempted_total": new_attempted,
                            "percent": (new_processed as f64 / total_positions as f64) * 100.0,
                        });
                        lg.write_json(&rec);
                    }
                    batch.clear();
                }
                break;
            }
            if let Some(sfen) = extract_sfen(line.trim()) {
                seen_valid += 1;
                if seen_valid <= positions_to_skip {
                    continue;
                }
                let idx0 = seen_valid - 1; // zero-based index
                batch.push((idx0, sfen));
                if batch.len() >= batch_size {
                    batch_idx += 1;
                    let batch_start = std::time::Instant::now();
                    human_log!(
                        "\nBatch {}/{}: Processing {} positions...",
                        batch_idx,
                        total_batches,
                        batch.len()
                    );
                    let batch_results: Vec<_> = batch
                        .par_iter()
                        .map_init(
                            || {
                                let mut eng = Engine::new(opts.engine);
                                eng.set_hash_size(opts.hash_mb);
                                eng.set_threads(1);
                                eng.set_multipv_persistent(opts.multipv);
                                eng.set_teacher_profile(opts.teacher_profile);
                                if matches!(
                                    opts.engine,
                                    EngineType::Nnue | EngineType::EnhancedNnue
                                ) {
                                    if let Some(ref path) = opts.nnue_weights {
                                        if let Err(e) = eng.load_nnue_weights(path) {
                                            eprintln!(
                                                "Failed to load NNUE weights ({}): {}",
                                                path, e
                                            );
                                        }
                                    }
                                }
                                eng
                            },
                            |eng, (idx, sfen)| process_position_with_engine(*idx, sfen, &env, eng),
                        )
                        .collect();
                    let successful_results: Vec<_> = batch_results.into_iter().flatten().collect();
                    if use_parted_output {
                        if let Some(pm) = part_mgr.as_mut() {
                            pm.write_lines(&successful_results)?;
                            if let Err(e) = pm.update_part_progress() {
                                eprintln!("Warning: failed to write part progress: {}", e);
                            }
                        }
                    } else if let Some(ref of) = output_file {
                        let mut file = of.lock().unwrap();
                        for result in &successful_results {
                            writeln!(file, "{result}")?;
                        }
                        file.flush()?;
                    }
                    let new_processed = processed_count
                        .fetch_add(successful_results.len(), Ordering::Relaxed)
                        + successful_results.len();
                    let new_attempted =
                        total_attempted.fetch_add(batch.len(), Ordering::Relaxed) + batch.len();
                    write_atomic_best_effort(&progress_path, &new_attempted.to_string());
                    let batch_time = batch_start.elapsed();
                    let positions_per_sec = batch.len() as f64 / batch_time.as_secs_f64();
                    human_log!(
                        "Batch complete: {} results in {:.1}s ({:.0} pos/sec)",
                        successful_results.len(),
                        batch_time.as_secs_f32(),
                        positions_per_sec
                    );
                    human_log!(
                        "Overall progress: {new_processed}/{total_positions} ({:.1}%)",
                        (new_processed as f64 / total_positions as f64) * 100.0
                    );
                    if let Some(ref lg) = structured_logger {
                        let rec = serde_json::json!({
                            "kind": "batch",
                            "version": 1,
                            "batch_index": batch_idx - 1,
                            "size": batch.len(),
                            "success": successful_results.len(),
                            "elapsed_sec": batch_time.as_secs_f64(),
                            "sps": positions_per_sec,
                            "attempted_sps": positions_per_sec,
                            "processed_total": new_processed,
                            "attempted_total": new_attempted,
                            "percent": (new_processed as f64 / total_positions as f64) * 100.0,
                        });
                        lg.write_json(&rec);
                    }
                    batch.clear();
                }
            }
        }
        Ok(())
    };

    if let Some(j) = opts.jobs {
        let pool = ThreadPoolBuilder::new().num_threads(j).build().expect("build rayon pool");
        let res: Result<(), Box<dyn std::error::Error + Send + Sync>> = pool.install(run_batches);
        if let Err(e) = res {
            let err: Box<dyn std::error::Error> = e;
            return Err(err);
        }
    } else if let Err(e) = run_batches() {
        return Err(e);
    }

    // Final statistics
    let total_processed = processed_count.load(Ordering::Relaxed);
    let total_errors = error_count.load(Ordering::Relaxed);
    let total_skipped = skipped_count.load(Ordering::Relaxed);
    let e_parse = errors_parse.load(Ordering::Relaxed);
    let e_nonexact = errors_nonexact_top1.load(Ordering::Relaxed);
    let e_empty_pv = errors_empty_pv.load(Ordering::Relaxed);
    let newly_processed = total_processed - skip_count;

    human_log!("\n{}", "=".repeat(60));
    human_log!("NNUE Training Data Generation Complete!");
    human_log!("Total positions in file: {total_positions}");
    human_log!("Previously processed: {skip_count}");
    human_log!("Newly processed: {newly_processed}");
    human_log!("Total processed: {total_processed}");
    human_log!("Errors (hard): {total_errors}");
    human_log!("  - parse: {e_parse}");
    human_log!("  - nonexact_top1: {e_nonexact}");
    human_log!("  - empty_or_missing_pv: {e_empty_pv}");
    human_log!("Skipped (timeout_overruns): {total_skipped}");

    if newly_processed > 0 {
        let success_rate = (newly_processed as f64
            / (newly_processed + total_errors + total_skipped) as f64)
            * 100.0;
        human_log!("Success rate (this run): {success_rate:.1}%");
    }

    if total_skipped > 0 {
        human_log!("\nSkipped positions saved to: {}", skipped_path.display());
        human_log!("Progress tracked in: {}", progress_path.display());
        human_log!("You can reprocess skipped positions with:");
        human_log!(
            "  cargo run --release --bin generate_nnue_training_data -- {} output_retry.txt {} {}",
            skipped_path.display(),
            search_depth + 1,
            batch_size / 2
        );
    }

    // Build and write manifests (split-aware)
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

    let comp_str = match opts.compress {
        CompressionKind::None => None,
        CompressionKind::Gz => Some("gz".into()),
        CompressionKind::Zst => Some("zst".into()),
    };

    // Provenance: compute input file hash/bytes and NNUE weights hash (if provided)
    let (input_sha256, input_bytes) = if is_stdin {
        tee_tmp_path
            .as_ref()
            .and_then(|p| compute_sha_and_bytes(p))
            .map(|(h, b)| (Some(h), Some(b)))
            .unwrap_or((None, None))
    } else {
        compute_sha_and_bytes(&input_path)
            .map(|(h, b)| (Some(h), Some(b)))
            .unwrap_or((None, None))
    };
    let nnue_weights_sha256: Option<String> = opts
        .nnue_weights
        .as_ref()
        .and_then(|p| compute_sha_and_bytes(std::path::Path::new(p)).map(|(h, _)| h));

    // Build manifest summary from aggregated counters
    fn summarize_depth_hist(hist: &[usize]) -> (u8, u8, u8, u8) {
        let mut min_idx = None;
        let mut max_idx = None;
        let mut total = 0usize;
        for (d, &c) in hist.iter().enumerate() {
            if c > 0 {
                if min_idx.is_none() {
                    min_idx = Some(d);
                }
                max_idx = Some(d);
                total += c;
            }
        }
        if total == 0 {
            return (0, 0, 0, 0);
        }
        let min = min_idx.unwrap();
        let max = max_idx.unwrap();
        let p50t = (total as f64 * 0.50).ceil() as usize;
        let p90t = (total as f64 * 0.90).ceil() as usize;
        let mut acc = 0usize;
        let mut p50 = min;
        let mut p90 = min;
        for (d, &c) in hist.iter().enumerate() {
            if c == 0 {
                continue;
            }
            acc += c;
            if acc >= p50t && p50 == min {
                p50 = d;
            }
            if acc >= p90t {
                p90 = d;
                break;
            }
        }
        (min as u8, max as u8, p50 as u8, p90 as u8)
    }

    let elapsed_sec = overall_start.elapsed().as_secs_f64();
    let success_total = total_processed; // equals manifest 'count'
    let lines_ge2 = shared.lines_ge2.load(Ordering::Relaxed);
    let both_exact_cnt = shared.both_exact.load(Ordering::Relaxed);
    let ambiguous_cnt = shared.ambiguous_k3.load(Ordering::Relaxed);
    let k3_reran_cnt = shared.k3_reran.load(Ordering::Relaxed);
    let k3_entropy_cnt = shared.k3_entropy.load(Ordering::Relaxed);
    let depth_hist_vec = shared.depth_hist.lock().unwrap().clone();
    let (dmin, dmax, dp50, dp90) = summarize_depth_hist(&depth_hist_vec);
    // Run-scoped deltas
    let attempted_run = attempted_total.saturating_sub(positions_to_skip);
    let success_run = success_total.saturating_sub(skip_count);
    let timeout_rate = if attempted_run > 0 {
        (total_skipped as f64 / attempted_run as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let top1_exact_rate = if attempted_run > 0 {
        (success_run as f64 / attempted_run as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let both_exact_rate = if lines_ge2 > 0 {
        (both_exact_cnt as f64 / lines_ge2 as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let summary_obj = ManifestSummary {
        elapsed_sec,
        throughput: ManifestThroughputSummary {
            attempted_sps: if elapsed_sec > 0.0 {
                attempted_run as f64 / elapsed_sec
            } else {
                0.0
            },
            success_sps: if elapsed_sec > 0.0 {
                success_run as f64 / elapsed_sec
            } else {
                0.0
            },
        },
        rates: ManifestRatesSummary {
            timeout: timeout_rate,
            top1_exact: top1_exact_rate,
            both_exact: both_exact_rate,
        },
        ambiguous: ManifestAmbiguousSummary {
            threshold_cp: opts.amb_gap2_threshold,
            require_exact: opts.amb_require_exact,
            count: ambiguous_cnt,
            denom: lines_ge2,
            rate: if lines_ge2 > 0 {
                (ambiguous_cnt as f64 / lines_ge2 as f64).clamp(0.0, 1.0)
            } else {
                0.0
            },
            reran: Some(k3_reran_cnt),
            with_entropy: Some(k3_entropy_cnt),
        },
        depth: ManifestDepthSummary {
            histogram: depth_hist_vec.clone(),
            min: dmin,
            max: dmax,
            p50: dp50,
            p90: dp90,
        },
        counts: ManifestCountsSummary {
            attempted: attempted_run,
            success: success_run,
            skipped_timeout: total_skipped,
            errors: ManifestErrorsSummary {
                parse: e_parse,
                nonexact_top1: e_nonexact,
                empty_or_missing_pv: e_empty_pv,
            },
        },
    };

    if let Some(ref mut pm) = part_mgr {
        // Finish the last open part and collect infos
        pm.finish_part().ok();
        let total_parts = pm.part_manifests.len();
        for (i, info) in pm.part_manifests.iter().enumerate() {
            let part_idx = i + 1;
            // Write per-part manifest next to the part file with explicit naming
            let man_path =
                pm.dir.join(format!("{}.part-{:04}.manifest.json", pm.base_stem, part_idx));
            // Compute per-part SHA-256 and size
            let (out_sha256, out_bytes) = (|| -> Option<(String, u64)> {
                use sha2::{Digest, Sha256};
                let mut f = File::open(&info.path).ok()?;
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
            // Build manifest v2 provenance fields
            let teacher_usi = TeacherUsiOpts {
                hash_mb: opts.hash_mb,
                multipv: opts.multipv,
                threads: 1,
                teacher_profile: format!("{:?}", opts.teacher_profile),
                min_depth: effective_depth,
            };
            let engine_version = std::env::var("ENGINE_SEMVER")
                .ok()
                .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
            let teacher_engine = TeacherEngineInfo {
                name: engine_name.to_string(),
                version: engine_version,
                commit: std::env::var("ENGINE_COMMIT")
                    .ok()
                    .or_else(|| std::env::var("GIT_COMMIT_HASH").ok()),
                usi_opts: teacher_usi,
            };
            let argv: Vec<String> = std::env::args().collect();
            let generation_command = argv.join(" ");
            // Deterministic seed derived from user-provided args (stable)
            let seed = stable_seed_from_args(&argv);

            let manifest = Manifest {
                generated_at: chrono::Utc::now().to_rfc3339(),
                git_commit: std::env::var("GIT_COMMIT_HASH").ok(),
                engine: engine_name.to_string(),
                manifest_scope: Some("part".to_string()),
                teacher_engine,
                generation_command,
                seed,
                manifest_version: "2".into(),
                input: ManifestInputInfo {
                    path: input_path.display().to_string(),
                    sha256: input_sha256.clone(),
                    bytes: input_bytes,
                },
                nnue_weights_sha256: nnue_weights_sha256.clone(),
                nnue_weights: opts.nnue_weights.clone(),
                preset: preset_name.clone(),
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
                count: info.count_in_part,
                cp_to_wdl_scale: opts.wdl_scale,
                wdl_semantics: "side_to_move".to_string(),
                calibration: calibration_out.clone(),
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
                part_index: Some(part_idx),
                part_count: Some(total_parts),
                count_in_part: Some(info.count_in_part),
                compression: comp_str.clone(),
                ambiguity: Some(ManifestAmbiguity {
                    gap2_threshold_cp: opts.amb_gap2_threshold,
                    require_exact: opts.amb_require_exact,
                    mate_mode: match opts.entropy_mate_mode {
                        MateEntropyMode::Exclude => "exclude".into(),
                        MateEntropyMode::Saturate => "saturate".into(),
                    },
                    entropy_scale: opts.entropy_scale,
                }),
                summary: None,
            };
            if let Ok(txt) = serde_json::to_string_pretty(&manifest) {
                if let Err(e) = std::fs::write(&man_path, txt) {
                    eprintln!(
                        "Warning: failed to write part manifest ({}): {}",
                        man_path.display(),
                        e
                    );
                }
            }
        }
        // After writing per-part manifests, write parent aggregated manifest with summary
        let teacher_usi = TeacherUsiOpts {
            hash_mb: opts.hash_mb,
            multipv: opts.multipv,
            threads: 1,
            teacher_profile: format!("{:?}", opts.teacher_profile),
            min_depth: effective_depth,
        };
        let engine_version = std::env::var("ENGINE_SEMVER")
            .ok()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let teacher_engine = TeacherEngineInfo {
            name: engine_name.to_string(),
            version: engine_version,
            commit: std::env::var("ENGINE_COMMIT")
                .ok()
                .or_else(|| std::env::var("GIT_COMMIT_HASH").ok()),
            usi_opts: teacher_usi,
        };
        let argv: Vec<String> = std::env::args().collect();
        let generation_command = argv.join(" ");
        let seed = stable_seed_from_args(&argv);
        let manifest = Manifest {
            generated_at: chrono::Utc::now().to_rfc3339(),
            git_commit: std::env::var("GIT_COMMIT_HASH").ok(),
            engine: engine_name.to_string(),
            manifest_scope: Some("aggregate".to_string()),
            teacher_engine,
            generation_command,
            seed,
            manifest_version: "2".into(),
            input: ManifestInputInfo {
                path: input_path.display().to_string(),
                sha256: input_sha256,
                bytes: input_bytes,
            },
            nnue_weights_sha256,
            nnue_weights: opts.nnue_weights.clone(),
            preset: preset_name.clone(),
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
                    "nodes".into()
                } else {
                    "time".into()
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
            wdl_semantics: "side_to_move".into(),
            calibration: calibration_out,
            attempted: attempted_total,
            skipped_timeout: total_skipped,
            errors: serde_json::json!({ "parse": e_parse, "nonexact_top1": e_nonexact, "empty_or_missing_pv": e_empty_pv }),
            reuse_tt: opts.reuse_tt,
            skip_overrun_factor: opts.skip_overrun_factor,
            search_depth_arg: search_depth,
            effective_min_depth: effective_depth,
            output_sha256: None,
            output_bytes: None,
            part_index: None,
            part_count: Some(total_parts),
            count_in_part: None,
            compression: comp_str,
            ambiguity: Some(ManifestAmbiguity {
                gap2_threshold_cp: opts.amb_gap2_threshold,
                require_exact: opts.amb_require_exact,
                mate_mode: match opts.entropy_mate_mode {
                    MateEntropyMode::Exclude => "exclude".into(),
                    MateEntropyMode::Saturate => "saturate".into(),
                },
                entropy_scale: opts.entropy_scale,
            }),
            summary: Some(summary_obj.clone()),
        };
        if let Ok(txt) = serde_json::to_string_pretty(&manifest) {
            if let Err(e) = std::fs::write(&manifest_path, txt) {
                eprintln!(
                    "Warning: failed to write manifest.json ({}): {}",
                    manifest_path.display(),
                    e
                );
            } else {
                human_log!("Manifest written: {}", manifest_path.display());
            }
        }
    } else {
        // Ensure non-parted writer is flushed before measuring
        if let Some(ref of) = output_file {
            let mut guard = of.lock().unwrap();
            let _ = guard.flush();
        }
        // Compute SHA-256/bytes of the single output file
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
        // Build manifest v2 provenance fields
        let teacher_usi = TeacherUsiOpts {
            hash_mb: opts.hash_mb,
            multipv: opts.multipv,
            threads: 1,
            teacher_profile: format!("{:?}", opts.teacher_profile),
            min_depth: effective_depth,
        };
        let engine_version = std::env::var("ENGINE_SEMVER")
            .ok()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let teacher_engine = TeacherEngineInfo {
            name: engine_name.to_string(),
            version: engine_version,
            commit: std::env::var("ENGINE_COMMIT")
                .ok()
                .or_else(|| std::env::var("GIT_COMMIT_HASH").ok()),
            usi_opts: teacher_usi,
        };
        let argv: Vec<String> = std::env::args().collect();
        let generation_command = argv.join(" ");
        let seed = stable_seed_from_args(&argv);

        let manifest = Manifest {
            generated_at: chrono::Utc::now().to_rfc3339(),
            git_commit: std::env::var("GIT_COMMIT_HASH").ok(),
            engine: engine_name.to_string(),
            manifest_scope: Some("aggregate".to_string()),
            teacher_engine,
            generation_command,
            seed,
            manifest_version: "2".into(),
            input: ManifestInputInfo {
                path: input_path.display().to_string(),
                sha256: input_sha256,
                bytes: input_bytes,
            },
            nnue_weights_sha256,
            nnue_weights: opts.nnue_weights.clone(),
            preset: preset_name.clone(),
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
                    "nodes".into()
                } else {
                    "time".into()
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
            wdl_semantics: "side_to_move".into(),
            calibration: calibration_out,
            attempted: attempted_total,
            skipped_timeout: total_skipped,
            errors: serde_json::json!({ "parse": e_parse, "nonexact_top1": e_nonexact, "empty_or_missing_pv": e_empty_pv }),
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
            part_index: None,
            part_count: None,
            count_in_part: None,
            compression: comp_str,
            ambiguity: Some(ManifestAmbiguity {
                gap2_threshold_cp: opts.amb_gap2_threshold,
                require_exact: opts.amb_require_exact,
                mate_mode: match opts.entropy_mate_mode {
                    MateEntropyMode::Exclude => "exclude".into(),
                    MateEntropyMode::Saturate => "saturate".into(),
                },
                entropy_scale: opts.entropy_scale,
            }),
            summary: Some(summary_obj.clone()),
        };
        if let Ok(txt) = serde_json::to_string_pretty(&manifest) {
            if let Err(e) = std::fs::write(&manifest_path, txt) {
                eprintln!(
                    "Warning: failed to write manifest.json ({}): {}",
                    manifest_path.display(),
                    e
                );
            } else {
                human_log!("Manifest written: {}", manifest_path.display());
            }
        }
    }

    // Structured final record (optional)
    if let Some(ref lg) = structured_logger {
        let rec = serde_json::json!({ "kind": "final", "version": 1, "summary": &summary_obj });
        lg.write_json(&rec);
    }
    Ok(())
}

fn process_position_with_engine(
    idx: usize,
    sfen: &str,
    env: &ProcEnv<'_>,
    eng: &mut Engine,
) -> Option<String> {
    let position = match engine_core::usi::parse_sfen(sfen) {
        Ok(pos) => pos,
        Err(e) => {
            if idx < 10 || (idx + 1).is_multiple_of(1000) {
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
        // Reapply teacher profile after reset since reset_for_position() resets it to default
        eng.set_teacher_profile(env.opts.teacher_profile);
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
    let k2_time_ms = elapsed_stats_ms;
    let k2_nodes = result.stats.nodes;
    let mut k3_time_ms_opt: Option<u64> = None;
    let mut k3_nodes_opt: Option<u64> = None;
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
        if idx < 10 || (idx + 1).is_multiple_of(100) {
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
                "index": idx,
            });
            let _ = writeln!(file, "{}", obj);
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
                "index": idx,
            });
            let _ = writeln!(file, "{}", obj);
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
                "index": idx,
            });
            let _ = writeln!(file, "{}", obj);
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
            if is_ambiguous_for_k3(
                l0.bound,
                l1.bound,
                l0.score_cp,
                l1.score_cp,
                env.opts.amb_gap2_threshold,
                env.opts.amb_require_exact,
            ) {
                ambiguous_k3 = true;
                env.shared.ambiguous_k3.fetch_add(1, Ordering::Relaxed);
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
                            // In nodes mode, cap K=3 re-search to a conservative fraction to avoid doubling work
                            let n_k3 = std::cmp::max(n / 4, 10_000);
                            builder2 = builder2.fixed_nodes(n_k3);
                        } else {
                            // Keep within remaining budget conservatively: at most 1/4 of time_limit
                            let rem = env.time_limit_ms.saturating_sub(elapsed_stats_ms);
                            let cap = rem.min(env.time_limit_ms / 4).max(20);
                            builder2 = builder2.fixed_time_ms(cap);
                        }
                        let limits2 = builder2.build();
                        // Count actual K=3 re-search executions
                        env.shared.k3_reran.fetch_add(1, Ordering::Relaxed);
                        let res2 = eng.search(&mut pos2, limits2);
                        if let Some(ref lines2) = res2.lines {
                            lines_k = lines2.iter().take(3).cloned().collect();
                            have_three = lines_k.len() >= 3;
                            if have_three {
                                // Adopt K=3 stats/lines (compute entropy once after this block)
                                k3_time_ms_opt = Some(res2.stats.elapsed.as_millis() as u64);
                                result_used_time_ms = res2.stats.elapsed.as_millis() as u64;
                                result_used_depth = res2.stats.depth;
                                result_used_seldepth = res2.stats.seldepth;
                                k3_nodes_opt = Some(res2.stats.nodes);
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
                }
                // Compute entropy once based on the final lines_k (either original or K=3 re-search)
                if have_three {
                    entropy_k3_opt = softmax_entropy_k3_from_lines(
                        &lines_k,
                        env.opts.entropy_scale,
                        env.opts.entropy_mate_mode,
                    );
                    if entropy_k3_opt.is_some() {
                        env.shared.k3_entropy.fetch_add(1, Ordering::Relaxed);
                    }
                    // no extra clone
                    lines_used_opt = Some(lines_k);
                }
            }
            // Update both_exact/lines_ge2 using adopted lines (K=3 if present)
            let lines_for_stats: &[engine_core::search::types::RootLine] =
                if let Some(ref v) = lines_used_opt {
                    v.as_slice()
                } else {
                    lines0.as_slice()
                };
            if lines_for_stats.len() >= 2 {
                env.shared.lines_ge2.fetch_add(1, Ordering::Relaxed);
                if matches!(lines_for_stats[0].bound, Bound::Exact)
                    && matches!(lines_for_stats[1].bound, Bound::Exact)
                {
                    env.shared.both_exact.fetch_add(1, Ordering::Relaxed);
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
    // Apply timeout mark based on total (K=2 + optional K=3) time against the budget
    let search_time_total_ms = k2_time_ms + k3_time_ms_opt.unwrap_or(0);
    let timed_out_any = is_time_mode && (search_time_total_ms > env.time_limit_ms);
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
        if let Some(ref v) = lines_used_opt {
            Some(v.as_slice())
        } else {
            fallback_small.map(|sv| sv.as_slice())
        };

    // Success path counters
    {
        let mut hist = env.shared.depth_hist.lock().unwrap();
        let d = result_used_depth as usize;
        if hist.len() <= d {
            hist.resize(d + 1, 0);
        }
        hist[d] += 1;
    }

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
                "time_ms_k2": k2_time_ms,
                "time_ms_k3": k3_time_ms_opt,
                "search_time_ms_total": k2_time_ms + k3_time_ms_opt.unwrap_or(0),
                "timeout_total": is_time_mode && (k2_time_ms + k3_time_ms_opt.unwrap_or(0) > env.time_limit_ms),
                "budget_mode": if env.opts.nodes.is_some() { "nodes" } else { "time" },
                "nodes_k2": k2_nodes,
                "nodes_k3": k3_nodes_opt,
                "nodes_total": k2_nodes + k3_nodes_opt.unwrap_or(0),
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
                    // Determine sign using mate_distance if present, otherwise score_internal/score_cp
                    let sign = if let Some(md) = l.mate_distance {
                        if md >= 0 {
                            1
                        } else {
                            -1
                        }
                    } else if l.score_internal >= 0 {
                        1
                    } else {
                        -1
                    };
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
    use std::path::Path;

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

    #[test]
    fn test_derive_skipped_path_no_ext() {
        let p = Path::new("out");
        assert_eq!(derive_skipped_path(p), Path::new("out_skipped"));
    }

    #[test]
    fn test_derive_skipped_path_jsonl() {
        let p = Path::new("out.jsonl");
        assert_eq!(derive_skipped_path(p), Path::new("out_skipped.jsonl"));
    }

    #[test]
    fn test_derive_skipped_path_dotfile() {
        let p = Path::new(".bashrc");
        assert_eq!(derive_skipped_path(p), Path::new(".bashrc_skipped"));
    }

    #[test]
    fn test_derive_skipped_path_multi_ext() {
        let p = Path::new("a.b.c");
        assert_eq!(derive_skipped_path(p), Path::new("a.b_skipped.c"));
    }
}
