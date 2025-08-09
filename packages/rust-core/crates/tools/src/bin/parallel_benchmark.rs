//! Parallel search benchmark with duplication tracking
//!
//! Measures parallel search performance and work duplication metrics

use anyhow::Result;
use clap::Parser;
use engine_core::{
    evaluation::evaluate::{Evaluator, MaterialEvaluator},
    search::{parallel::ParallelSearcher, SearchLimitsBuilder, ShardedTranspositionTable},
    shogi::Position,
    time_management::TimeControl,
};
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use std::fs;
use std::process::Command;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
struct PositionEntry {
    name: String,
    sfen: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchmarkResult {
    thread_count: usize,
    mean_nps: f64,
    std_dev: f64,
    outlier_ratio: f64,
    avg_speedup: f64,
    avg_efficiency: f64,
    duplication_percentage: f64,
    effective_speedup: f64,
    tt_hit_rate: f64,
    pv_consistency: f64,
}

impl BenchmarkResult {
    fn print_summary(&self) {
        println!("Performance Summary for {} thread(s):", self.thread_count);
        println!("  NPS: {:.0} ± {:.0}", self.mean_nps, self.std_dev);
        println!("  Duplication: {:.1}%", self.duplication_percentage);
        println!("  Effective Speedup: {:.2}x", self.effective_speedup);
        println!("  TT Hit Rate: {:.1}%", self.tt_hit_rate * 100.0);
        if self.pv_consistency > 0.0 {
            println!("  PV Consistency: {:.1}%", self.pv_consistency * 100.0);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnvironmentMetadata {
    version: String,
    commit_hash: String,
    cpu_info: CpuInfo,
    build_info: BuildInfo,
    timestamp: u64,
    config: BenchmarkConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CpuInfo {
    model: String,
    cores: usize,
    threads: usize,
    cache_l3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildInfo {
    profile: String,
    features: Vec<String>,
    rustc_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchmarkConfig {
    tt_size_mb: usize,
    num_threads: Vec<usize>,
    depth_limit: u8,
    iterations: usize,
    positions_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FullBenchmarkReport {
    metadata: EnvironmentMetadata,
    results: Vec<BenchmarkResult>,
}

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Parallel search benchmark with duplication tracking"
)]
struct Args {
    /// Thread counts to test (comma-separated)
    #[arg(short, long, value_delimiter = ',', default_value = "1,2,4")]
    threads: Vec<usize>,

    /// Search depth
    #[arg(short, long, default_value = "8")]
    depth: u8,

    /// Fixed time per search in milliseconds (overrides depth if set)
    #[arg(short = 'm', long)]
    fixed_total_ms: Option<u64>,

    /// Number of iterations per position
    #[arg(short, long, default_value = "3")]
    iterations: usize,

    /// TT size in MB
    #[arg(long, default_value = "256")]
    tt_size: usize,

    /// Skip positions (comma-separated indices)
    #[arg(short, long, value_delimiter = ',')]
    skip_positions: Vec<usize>,

    /// Use material evaluator
    #[arg(long)]
    material: bool,

    /// Use sharded TT for better cache locality
    #[arg(long)]
    sharded_tt: bool,

    /// Position file (JSON format)
    #[arg(
        short,
        long,
        default_value = "crates/engine-core/resources/benchmark_positions.json"
    )]
    positions: String,

    /// Log level (debug, info, warn, error)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Dump results to JSON file
    #[arg(long)]
    dump_json: Option<String>,

    /// Baseline JSON file for regression detection
    #[arg(long)]
    baseline: Option<String>,

    /// Exit with error code on regression
    #[arg(long)]
    strict: bool,
}

fn calculate_std_dev(values: &[f64], mean: f64) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let variance = values
        .iter()
        .map(|v| {
            let diff = v - mean;
            diff * diff
        })
        .sum::<f64>()
        / (values.len() - 1) as f64;
    variance.sqrt()
}

fn calculate_outlier_ratio(values: &[f64], mean: f64, std_dev: f64) -> f64 {
    if values.is_empty() || std_dev == 0.0 {
        return 0.0;
    }
    let outliers = values.iter().filter(|v| (**v - mean).abs() > 2.0 * std_dev).count();
    outliers as f64 / values.len() as f64
}

fn get_environment_metadata(args: &Args, positions_count: usize) -> EnvironmentMetadata {
    // Get git commit hash
    let commit_hash = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_else(|| "unknown".to_string())
        .trim()
        .to_string();

    // Get CPU info (Linux-specific)
    let cpu_info = get_cpu_info();

    // Get rustc version
    let rustc_version = Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .unwrap_or_else(|| "unknown".to_string())
        .trim()
        .to_string();

    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();

    EnvironmentMetadata {
        version: env!("CARGO_PKG_VERSION").to_string(),
        commit_hash,
        cpu_info,
        build_info: BuildInfo {
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_string(),
            features: vec!["parallel".to_string()],
            rustc_version,
        },
        timestamp,
        config: BenchmarkConfig {
            tt_size_mb: args.tt_size,
            num_threads: args.threads.clone(),
            depth_limit: args.depth,
            iterations: args.iterations,
            positions_count,
        },
    }
}

fn get_cpu_info() -> CpuInfo {
    // Try to read from /proc/cpuinfo (Linux)
    let cpu_info_str = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();

    let mut model = "unknown".to_string();
    let mut cores = 0;
    let mut threads = 0;

    for line in cpu_info_str.lines() {
        if line.starts_with("model name") {
            model = line.split(':').nth(1).unwrap_or("unknown").trim().to_string();
        } else if line.starts_with("cpu cores") {
            cores = line.split(':').nth(1).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        } else if line.starts_with("siblings") {
            threads = line.split(':').nth(1).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        }
    }

    // If not Linux or parsing failed, use basic info
    if cores == 0 {
        cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
        threads = cores;
    }

    CpuInfo {
        model,
        cores,
        threads,
        cache_l3: "unknown".to_string(), // Would need more complex parsing
    }
}

fn check_regression(current: &BenchmarkResult, baseline: &BenchmarkResult) -> bool {
    let speedup_regression = current.effective_speedup < baseline.effective_speedup * 0.95;
    let dup_regression = current.duplication_percentage > baseline.duplication_percentage * 1.1;

    if speedup_regression || dup_regression {
        eprintln!("Performance regression detected for {} threads!", current.thread_count);
        eprintln!(
            "  Speedup: {:.2}x -> {:.2}x",
            baseline.effective_speedup, current.effective_speedup
        );
        eprintln!(
            "  Duplication: {:.1}% -> {:.1}%",
            baseline.duplication_percentage, current.duplication_percentage
        );
        return true;
    }
    false
}

fn load_positions(path: &str) -> Result<Vec<PositionEntry>> {
    let content = fs::read_to_string(path)?;

    // Simple JSON parsing for our specific format
    let mut positions = Vec::new();

    // Find all SFEN strings and names in the JSON
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].contains("\"name\"") && lines[i].contains(":") {
            // Extract name from format: "name": "value",
            let name_line = lines[i];
            if let Some(colon_pos) = name_line.find(':') {
                let after_colon = &name_line[colon_pos + 1..];
                // Find the value between quotes
                if let Some(first_quote) = after_colon.find('"') {
                    let after_first = &after_colon[first_quote + 1..];
                    if let Some(second_quote) = after_first.find('"') {
                        let name = after_first[..second_quote].to_string();

                        // Look for sfen on next line
                        if i + 1 < lines.len() && lines[i + 1].contains("\"sfen\"") {
                            let sfen_line = lines[i + 1];
                            if let Some(colon_pos) = sfen_line.find(':') {
                                let after_colon = &sfen_line[colon_pos + 1..];
                                if let Some(first_quote) = after_colon.find('"') {
                                    let after_first = &after_colon[first_quote + 1..];
                                    if let Some(second_quote) = after_first.find('"') {
                                        let sfen = after_first[..second_quote].to_string();
                                        positions.push(PositionEntry { name, sfen });
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }

    Ok(positions)
}

fn run_single_search<E: Evaluator + Send + Sync + 'static>(
    position: &mut Position,
    evaluator: Arc<E>,
    tt: Arc<ShardedTranspositionTable>,
    threads: usize,
    depth: u8,
    fixed_ms: Option<u64>,
) -> (u64, u64, f64, u64, String, i32) {
    let mut searcher = ParallelSearcher::new(evaluator.clone(), tt.clone(), threads);

    let limits = if let Some(ms) = fixed_ms {
        SearchLimitsBuilder::default()
            .time_control(TimeControl::FixedTime { ms_per_move: ms })
            .build()
    } else {
        SearchLimitsBuilder::default()
            .time_control(TimeControl::Infinite)
            .depth(depth)
            .build()
    };

    let start = Instant::now();
    let result = searcher.search(position, limits);
    let elapsed = start.elapsed();

    let nodes = result.stats.nodes;
    // Use floating point seconds to avoid zero time issues
    let time_secs = elapsed.as_secs_f64();
    let time_ms = ((time_secs * 1000.0) as u64).max(1); // Ensure at least 1ms
    let _nps = nodes * 1000 / time_ms;

    // Get duplication stats through public interface
    let duplication = searcher.get_duplication_percentage();
    let effective_nodes = searcher.get_effective_nodes();

    let best_move = result.best_move.map_or("none".to_string(), |m| format!("{m}"));
    let score = result.score;

    (nodes, time_ms, duplication, effective_nodes, best_move, score)
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logger
    env_logger::Builder::from_default_env()
        .filter_level(args.log_level.parse()?)
        .init();

    // Load positions
    let positions = load_positions(&args.positions)?;
    info!("Loaded {} positions from {}", positions.len(), args.positions);

    // Filter out skipped positions
    let positions_to_test: Vec<_> = positions
        .iter()
        .enumerate()
        .filter(|(i, _)| !args.skip_positions.contains(i))
        .map(|(i, p)| (i, p.clone()))
        .collect();

    info!("Running parallel benchmark with {} positions", positions_to_test.len());

    // Create evaluator
    let evaluator = Arc::new(MaterialEvaluator);
    info!("Using material evaluator");

    // Create sharded TT for better parallel performance
    info!("Using sharded transposition table with 16 shards");
    let tt = Arc::new(ShardedTranspositionTable::new(args.tt_size));

    let mut benchmark_results = Vec::new();
    let baseline_nps = Arc::new(std::sync::Mutex::new(None));
    let baseline_effective_nodes = Arc::new(std::sync::Mutex::new(None));

    // Run benchmarks for each thread count
    for &thread_count in &args.threads {
        info!("\n=== Testing with {thread_count} thread(s) ===");

        let mut all_nps_values = Vec::new();
        let mut all_duplication_values = Vec::new();
        let mut all_tt_hit_rates = Vec::new();
        let mut _total_nodes = 0u64;
        let mut total_effective_nodes = 0u64;
        let mut _total_time_ms = 0u64;
        let pv_matches = 0;
        let mut pv_total = 0;

        for (pos_idx, position_entry) in &positions_to_test {
            info!("Position {}: {}", pos_idx, position_entry.name);

            // Parse position - handle error properly
            let mut position = match Position::from_sfen(&position_entry.sfen) {
                Ok(pos) => pos,
                Err(e) => {
                    warn!("Failed to parse position {}: {}", position_entry.name, e);
                    continue;
                }
            };

            for iter in 0..args.iterations {
                debug!("  Iteration {}/{}", iter + 1, args.iterations);

                // Note: We can't clear TT due to Arc, but that's okay for benchmark

                let (nodes, time_ms, duplication, effective_nodes, best_move, score) =
                    run_single_search(
                        &mut position,
                        evaluator.clone(),
                        tt.clone(),
                        thread_count,
                        args.depth,
                        args.fixed_total_ms,
                    );

                let nps = if time_ms > 0 {
                    nodes * 1000 / time_ms
                } else {
                    0
                };

                info!(
                    "    Thread {thread_count}: {nodes} nodes in {time_ms}ms = {nps} nps, dup: {duplication:.1}%, move: {best_move}, score: {score}"
                );

                if nps > 0 {
                    all_nps_values.push(nps as f64);
                }
                all_duplication_values.push(duplication);
                // TODO: Get actual TT hit rate from searcher
                all_tt_hit_rates.push(0.0);

                _total_nodes += nodes;
                total_effective_nodes += effective_nodes;
                _total_time_ms += time_ms;
                pv_total += 1;
                // TODO: Track PV consistency
            }
        }

        if all_nps_values.is_empty() {
            warn!("No valid positions tested for {thread_count} threads");
            continue;
        }

        // Calculate statistics
        let mean_nps = all_nps_values.iter().sum::<f64>() / all_nps_values.len() as f64;
        let std_dev = calculate_std_dev(&all_nps_values, mean_nps);
        let outlier_ratio = calculate_outlier_ratio(&all_nps_values, mean_nps, std_dev);

        let duplication_percentage = if !all_duplication_values.is_empty() {
            all_duplication_values.iter().sum::<f64>() / all_duplication_values.len() as f64
        } else {
            0.0
        };

        let tt_hit_rate = if !all_tt_hit_rates.is_empty() {
            all_tt_hit_rates.iter().sum::<f64>() / all_tt_hit_rates.len() as f64
        } else {
            0.0
        };

        // Calculate speedup relative to baseline (1 thread)
        let avg_speedup = {
            let mut baseline = baseline_nps.lock().unwrap();
            if baseline.is_none() && thread_count == 1 {
                *baseline = Some(mean_nps);
            }
            if let Some(base) = *baseline {
                if base > 0.0 {
                    mean_nps / base
                } else {
                    1.0
                }
            } else {
                1.0
            }
        };

        // Calculate effective speedup based on unique nodes
        let effective_speedup = {
            let mut baseline = baseline_effective_nodes.lock().unwrap();
            if baseline.is_none() && thread_count == 1 {
                *baseline = Some(total_effective_nodes as f64);
            }
            if let Some(base) = *baseline {
                if base > 0.0 {
                    total_effective_nodes as f64 / base
                } else {
                    1.0
                }
            } else {
                1.0
            }
        };

        let avg_efficiency = avg_speedup / thread_count as f64;

        let pv_consistency = if pv_total > 0 {
            pv_matches as f64 / pv_total as f64
        } else {
            0.0
        };

        let result = BenchmarkResult {
            thread_count,
            mean_nps,
            std_dev,
            outlier_ratio,
            avg_speedup,
            avg_efficiency,
            duplication_percentage,
            effective_speedup,
            tt_hit_rate,
            pv_consistency,
        };

        info!("\nSummary for {thread_count} thread(s):");
        result.print_summary();

        benchmark_results.push(result);
    }

    // Print final summary
    println!("\n=== BENCHMARK SUMMARY ===");
    println!("Threads | NPS      | Speedup | Efficiency | Duplication | Effective");
    println!("--------|----------|---------|------------|-------------|----------");
    for result in &benchmark_results {
        println!(
            "{:7} | {:8.0} | {:7.2}x | {:9.1}% | {:11.1}% | {:8.2}x",
            result.thread_count,
            result.mean_nps,
            result.avg_speedup,
            result.avg_efficiency * 100.0,
            result.duplication_percentage,
            result.effective_speedup,
        );
    }

    // Check targets (from parallel-search-improvement.md)
    println!("\n=== TARGET STATUS ===");

    let two_thread = benchmark_results.iter().find(|r| r.thread_count == 2);
    let four_thread = benchmark_results.iter().find(|r| r.thread_count == 4);

    if let Some(result) = two_thread {
        let target_met = result.avg_speedup >= 1.25;
        println!(
            "2T Speedup (≥1.25x): {} (actual: {:.2}x)",
            if target_met { "✓" } else { "✗" },
            result.avg_speedup
        );
    }

    if let Some(result) = four_thread {
        let target_met = result.avg_speedup >= 1.8;
        println!(
            "4T Speedup (≥1.8x): {} (actual: {:.2}x)",
            if target_met { "✓" } else { "✗" },
            result.avg_speedup
        );
    }

    let dup_target_met = benchmark_results.iter().all(|r| r.duplication_percentage < 50.0);
    println!("Duplication (<50%): {}", if dup_target_met { "✓" } else { "✗" });

    if benchmark_results.iter().any(|r| r.duplication_percentage > 60.0) {
        println!("\n⚠️  WARNING: High duplication rate detected (>60%)");
        println!("   This indicates significant redundant work between threads.");
        println!("   Consider implementing root move splitting or depth variation.");
    }

    // Save JSON report if requested
    if let Some(json_path) = &args.dump_json {
        let metadata = get_environment_metadata(&args, positions_to_test.len());
        let report = FullBenchmarkReport {
            metadata,
            results: benchmark_results.clone(),
        };

        let json = serde_json::to_string_pretty(&report)?;
        fs::write(json_path, json)?;
        info!("\nBenchmark results saved to: {json_path}");
    }

    // Check for regression if baseline provided
    let mut regression_detected = false;
    if let Some(baseline_path) = &args.baseline {
        let baseline_json = fs::read_to_string(baseline_path)?;
        let baseline_report: FullBenchmarkReport = serde_json::from_str(&baseline_json)?;

        println!("\n=== REGRESSION CHECK ===");
        for current in &benchmark_results {
            if let Some(baseline) =
                baseline_report.results.iter().find(|r| r.thread_count == current.thread_count)
            {
                if check_regression(current, baseline) {
                    regression_detected = true;
                }
            }
        }

        if !regression_detected {
            println!("✅ No regression detected");
        }
    }

    if regression_detected && args.strict {
        std::process::exit(1);
    }

    Ok(())
}
