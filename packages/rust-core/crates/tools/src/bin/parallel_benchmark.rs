//! Parallel search benchmark tool
//!
//! Usage: parallel_benchmark [OPTIONS]
//!
//! Options:
//!   -t, --threads <THREADS>     Thread counts to test (comma-separated)
//!   -d, --depth <DEPTH>         Search depth [default: 10]
//!   -p, --positions <FILE>      Position file (JSON format)
//!   -o, --output <FILE>         Output file for results
//!   --baseline <FILE>           Baseline file for comparison
//!   --tolerance <PERCENT>       Regression tolerance percentage [default: 2.0]

use anyhow::{Context, Result};
use clap::Parser;
use engine_core::{
    benchmark::{
        metrics::{calculate_summary, compare_benchmarks, format_regression_report},
        parallel::{print_benchmark_results, run_parallel_benchmark, ParallelBenchmarkConfig},
    },
    evaluation::evaluate::MaterialEvaluator,
    shogi::Position,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, sync::Arc};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Thread counts to test (comma-separated)
    #[arg(short, long, default_value = "1,2,4,8")]
    threads: String,

    /// Search depth
    #[arg(short, long, default_value_t = 10)]
    depth: u8,

    /// Position file (JSON format)
    #[arg(short, long)]
    positions: Option<PathBuf>,

    /// Output file for results
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Baseline file for comparison
    #[arg(long)]
    baseline: Option<PathBuf>,

    /// Regression tolerance percentage
    #[arg(long, default_value_t = 2.0)]
    tolerance: f64,

    /// Skip stop latency measurement
    #[arg(long)]
    skip_stop_latency: bool,

    /// Minimum duration per position in milliseconds
    #[arg(long, default_value_t = 500)]
    min_duration_ms: u64,

    /// Fixed total time per position in milliseconds (overrides min_duration_ms)
    #[arg(long)]
    fixed_total_ms: Option<u64>,

    /// Dump raw measurement data to file
    #[arg(long)]
    dump_raw: Option<PathBuf>,

    /// Log level (error, warn, info, debug, trace)
    #[arg(long, default_value = "info")]
    log_level: String,

    /// Number of warmup runs before measurement
    #[arg(long, default_value_t = 1)]
    warmup_runs: u32,
}

/// Position set for benchmarking
#[derive(Debug, Serialize, Deserialize)]
struct BenchmarkPositions {
    positions: Vec<PositionEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PositionEntry {
    name: String,
    sfen: String,
    category: String,
}

/// Raw measurement data for debugging
#[derive(Debug, Serialize, Deserialize)]
struct RawMeasurementData {
    timestamp: String, // Use string for simplicity
    thread_count: usize,
    position_index: usize,
    position_sfen: String,
    nodes: u64,
    elapsed_ms: f64,
    depth_reached: u8,
    nps: u64,
}

/// Collection of all raw measurements
#[derive(Debug, Serialize, Deserialize)]
struct RawDataCollection {
    measurements: Vec<RawMeasurementData>,
    summary: String,
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logger with specified level
    let log_level = args.log_level.parse::<log::LevelFilter>().unwrap_or(log::LevelFilter::Info);
    env_logger::Builder::from_default_env().filter_level(log_level).init();

    // Parse thread counts
    let thread_counts: Vec<usize> = args
        .threads
        .split(',')
        .map(|s| s.trim().parse().with_context(|| format!("Invalid thread count: '{s}'")))
        .collect::<Result<Vec<_>, _>>()?;

    // Load positions and remember if custom file was used
    let (positions, using_custom) = if let Some(pos_file) = args.positions {
        (load_positions(&pos_file)?, true)
    } else {
        // Try to load default benchmark positions
        let default_path = PathBuf::from("crates/engine-core/resources/benchmark_positions.json");
        if default_path.exists() {
            (load_positions(&default_path)?, false)
        } else {
            // Fallback to hardcoded positions
            (create_default_positions(), false)
        }
    };

    println!("Running parallel benchmark with {} positions", positions.len());
    println!("Thread configurations: {thread_counts:?}");
    println!("Search depth: {}", args.depth);
    if let Some(fixed_ms) = args.fixed_total_ms {
        println!("Fixed total time per position: {fixed_ms}ms");
    } else {
        println!("Minimum duration per position: {}ms", args.min_duration_ms);
    }
    println!();

    // Configure benchmark
    let config = ParallelBenchmarkConfig {
        thread_counts,
        search_depth: args.depth,
        time_limit_ms: None,
        positions,
        measure_stop_latency: !args.skip_stop_latency,
        min_duration_ms: args.min_duration_ms,
        fixed_total_ms: args.fixed_total_ms,
        warmup_runs: args.warmup_runs,
        collect_raw_data: args.dump_raw.is_some(),
    };

    // Run benchmark
    let evaluator = Arc::new(MaterialEvaluator);
    let results = run_parallel_benchmark(evaluator, config);

    // Print results
    print_benchmark_results(&results);

    // Save raw data if requested
    if let Some(raw_file) = args.dump_raw {
        let mut all_measurements = Vec::new();
        for result in &results {
            for measurement in &result.raw_measurements {
                all_measurements.push(RawMeasurementData {
                    timestamp: format!(
                        "{}",
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap()
                            .as_secs()
                    ),
                    thread_count: result.thread_count,
                    position_index: measurement.position_index,
                    position_sfen: format!("Position {}", measurement.position_index),
                    nodes: measurement.nodes,
                    elapsed_ms: measurement.elapsed_ms,
                    depth_reached: measurement.depth_reached,
                    nps: if measurement.elapsed_ms > 0.0 {
                        (measurement.nodes as f64 / (measurement.elapsed_ms / 1000.0)) as u64
                    } else {
                        0
                    },
                });
            }
        }

        let raw_collection = RawDataCollection {
            measurements: all_measurements,
            summary: format!(
                "Benchmark with {} positions, {} thread configs",
                if using_custom { "custom" } else { "default" },
                results.len()
            ),
        };

        let json = serde_json::to_string_pretty(&raw_collection)?;
        fs::write(&raw_file, json)?;
        println!("\nRaw data saved to: {}", raw_file.display());
    }

    // Calculate summary
    let summary = calculate_summary(&results);

    // Save results if requested
    if let Some(output_file) = args.output {
        let json = serde_json::to_string_pretty(&summary)?;
        fs::write(&output_file, json)?;
        println!("\nResults saved to: {}", output_file.display());
    }

    // Compare with baseline if provided
    if let Some(baseline_file) = args.baseline {
        println!("\nComparing with baseline...");
        let baseline_json = fs::read_to_string(&baseline_file)?;
        let baseline_summary = serde_json::from_str(&baseline_json)?;

        let report = compare_benchmarks(&baseline_summary, &summary, args.tolerance);
        let report_text = format_regression_report(&report);
        println!("{report_text}");

        if report.has_regression {
            std::process::exit(1);
        }
    }

    // Check performance targets
    let targets = &summary.overall_metrics.targets_met;
    println!("\n=== Performance Targets ===");
    println!("NPS(4T) ≥ 2.4×: {}", if targets.nps_4t_target { "✅" } else { "❌" });
    println!(
        "Duplication ≤ 35%: {}",
        if targets.duplication_target {
            "✅"
        } else {
            "❌"
        }
    );
    println!(
        "PV match ≥ 97%: {}",
        if targets.pv_match_target {
            "✅"
        } else {
            "❌"
        }
    );
    println!(
        "Stop latency ≤ 5ms: {}",
        if targets.stop_latency_target {
            "✅"
        } else {
            "❌"
        }
    );

    Ok(())
}

/// Load positions from JSON file
fn load_positions(path: &PathBuf) -> Result<Vec<Position>> {
    let json = fs::read_to_string(path)?;
    let position_set: BenchmarkPositions = serde_json::from_str(&json)?;

    position_set
        .positions
        .into_iter()
        .map(|entry| {
            Position::from_sfen(&entry.sfen)
                .map_err(|e| anyhow::anyhow!("Invalid SFEN '{}': {e}", entry.sfen))
        })
        .collect()
}

/// Create default test positions
fn create_default_positions() -> Vec<Position> {
    // Reduced set for quicker testing
    vec![
        // Starting position
        Position::startpos(),
        // Middle game position
        Position::from_sfen(
            "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17",
        )
        .unwrap(),
    ]
}

/// Create a comprehensive benchmark position set (100 positions)
#[allow(dead_code)]
fn create_full_benchmark_set() -> BenchmarkPositions {
    // This would be expanded to include 100 carefully selected positions
    // For now, return a smaller set
    BenchmarkPositions {
        positions: vec![
            PositionEntry {
                name: "startpos".to_string(),
                sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1".to_string(),
                category: "opening".to_string(),
            },
            PositionEntry {
                name: "midgame_1".to_string(),
                sfen:
                    "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17"
                        .to_string(),
                category: "midgame".to_string(),
            },
            PositionEntry {
                name: "endgame_1".to_string(),
                sfen: "1n5n1/2s3k2/3p1p1p1/2p3p2/9/2P3P2/3P1P1P1/2K6/1N5N1 b RBGSLPrbgs2l13p 80"
                    .to_string(),
                category: "endgame".to_string(),
            },
            PositionEntry {
                name: "tactical_1".to_string(),
                sfen: "3g1ks2/5g3/2n1pp1p1/p3P1p2/1pP5P/P8/2N2PP2/6K2/L4G1NL b RSBPrslp 45"
                    .to_string(),
                category: "tactical".to_string(),
            },
            PositionEntry {
                name: "opening_2".to_string(),
                sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 2"
                    .to_string(),
                category: "opening".to_string(),
            },
        ],
    }
}
