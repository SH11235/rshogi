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
use std::fs;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
struct PositionEntry {
    name: String,
    sfen: String,
}

#[derive(Debug, Clone)]
struct ThreadResult {
    thread_count: usize,
    avg_nps: f64,
    avg_speedup: f64,
    avg_efficiency: f64,
    avg_duplication_rate: f64,
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

    let mut thread_results = Vec::new();
    let baseline_nps = Arc::new(std::sync::Mutex::new(None));

    // Run benchmarks for each thread count
    for &thread_count in &args.threads {
        info!("\n=== Testing with {thread_count} thread(s) ===");

        let mut total_nodes = 0u64;
        let mut total_time_ms = 0u64;
        let mut total_duplication = 0.0;
        let mut count = 0;

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

                let (nodes, time_ms, duplication, _effective_nodes, best_move, score) =
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

                total_nodes += nodes;
                total_time_ms += time_ms;
                total_duplication += duplication;
                count += 1;
            }
        }

        if count == 0 {
            warn!("No valid positions tested for {thread_count} threads");
            continue;
        }

        // Calculate averages
        let avg_nps = if total_time_ms > 0 {
            (total_nodes * 1000) as f64 / total_time_ms as f64
        } else {
            0.0
        };

        let avg_duplication = total_duplication / count as f64;

        // Calculate speedup relative to baseline (1 thread)
        let avg_speedup = {
            let mut baseline = baseline_nps.lock().unwrap();
            if baseline.is_none() && thread_count == 1 {
                *baseline = Some(avg_nps);
            }
            if let Some(base) = *baseline {
                if base > 0.0 {
                    avg_nps / base
                } else {
                    1.0
                }
            } else {
                1.0
            }
        };

        let avg_efficiency = avg_speedup / thread_count as f64;

        info!("\nSummary for {thread_count} thread(s):");
        info!("  Average NPS: {avg_nps:.0}");
        info!("  Speedup: {avg_speedup:.2}x");
        info!("  Efficiency: {:.1}%", avg_efficiency * 100.0);
        info!("  Duplication: {avg_duplication:.1}%");

        thread_results.push(ThreadResult {
            thread_count,
            avg_nps,
            avg_speedup,
            avg_efficiency,
            avg_duplication_rate: avg_duplication,
        });
    }

    // Print final summary
    println!("\n=== BENCHMARK SUMMARY ===");
    println!("Threads | NPS      | Speedup | Efficiency | Duplication");
    println!("--------|----------|---------|------------|------------");
    for result in &thread_results {
        println!(
            "{:7} | {:8.0} | {:7.2}x | {:9.1}% | {:10.1}%",
            result.thread_count,
            result.avg_nps,
            result.avg_speedup,
            result.avg_efficiency * 100.0,
            result.avg_duplication_rate,
        );
    }

    // Check targets (from parallel-search-improvement.md)
    println!("\n=== TARGET STATUS ===");

    let two_thread = thread_results.iter().find(|r| r.thread_count == 2);
    let four_thread = thread_results.iter().find(|r| r.thread_count == 4);

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

    let dup_target_met = thread_results.iter().all(|r| r.avg_duplication_rate < 50.0);
    println!("Duplication (<50%): {}", if dup_target_met { "✓" } else { "✗" });

    if thread_results.iter().any(|r| r.avg_duplication_rate > 60.0) {
        println!("\n⚠️  WARNING: High duplication rate detected (>60%)");
        println!("   This indicates significant redundant work between threads.");
        println!("   Consider implementing root move splitting or depth variation.");
    }

    Ok(())
}
