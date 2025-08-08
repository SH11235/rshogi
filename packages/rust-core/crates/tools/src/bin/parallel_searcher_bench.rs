//! Simple benchmark for ParallelSearcher
//!
//! This tool measures the performance of the new parallel search implementation
//! with a focus on reproducibility and simplicity.

use anyhow::Result;
use clap::Parser;
use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{parallel::ParallelSearcher, SearchLimitsBuilder, TranspositionTable},
    shogi::Position,
    time_management::TimeControl,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BenchResult {
    threads: usize,
    position_index: usize,
    nodes: u64,
    time_ms: u64, // Store as milliseconds for JSON serialization
    nps: u64,
    depth_reached: u8,
}

#[derive(Parser, Debug)]
#[command(author, version, about = "Benchmark ParallelSearcher", long_about = None)]
struct Args {
    /// Thread counts to test (comma-separated)
    #[arg(short, long, value_delimiter = ',', default_value = "1,2,4,8")]
    threads: Vec<usize>,

    /// Search depth
    #[arg(short, long, default_value = "6")]
    depth: u8,

    /// Fixed time per search in milliseconds
    #[arg(short, long)]
    fixed_ms: Option<u64>,

    /// Number of warmup runs
    #[arg(short, long, default_value = "1")]
    warmup_runs: usize,

    /// Number of measurement runs
    #[arg(short, long, default_value = "3")]
    measurement_runs: usize,

    /// TT size in MB
    #[arg(long, default_value = "256")]
    tt_size: usize,

    /// Skip positions (comma-separated indices)
    #[arg(long, value_delimiter = ',')]
    skip_positions: Vec<usize>,

    /// Output format (table, json, csv)
    #[arg(long, default_value = "table")]
    output: String,
}

fn get_test_positions() -> Vec<(String, Position)> {
    vec![
        ("startpos".to_string(), Position::startpos()),
        (
            "midgame1".to_string(),
            Position::from_sfen(
                "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17",
            )
            .unwrap(),
        ),
        (
            "endgame1".to_string(),
            Position::from_sfen(
                "1n5n1/2s3k2/3p1p1p1/2p3p2/9/2P3P2/3P1P1P1/2K6/1N5N1 b RBGSLPrbgs2l13p 80",
            )
            .unwrap(),
        ),
        (
            "tactical1".to_string(),
            Position::from_sfen("9/9/9/9/9/9/2k6/1N7/1K7 b G2r2b2g2s2n2l17p 1").unwrap(),
        ),
    ]
}

fn bench_once(
    evaluator: Arc<MaterialEvaluator>,
    tt: Arc<TranspositionTable>,
    position: &mut Position,
    threads: usize,
    depth: u8,
    fixed_ms: Option<u64>,
) -> BenchResult {
    let mut searcher = ParallelSearcher::new(evaluator, tt, threads);

    let limits = if let Some(ms) = fixed_ms {
        SearchLimitsBuilder::default()
            .time_control(TimeControl::FixedTime { ms_per_move: ms })
            .depth(depth)
            .build()
    } else {
        SearchLimitsBuilder::default().depth(depth).build()
    };

    let start = Instant::now();
    let result = searcher.search(position, limits);
    let elapsed = start.elapsed();

    let nps = if elapsed.as_secs_f64() > 0.001 {
        (result.stats.nodes as f64 / elapsed.as_secs_f64()) as u64
    } else {
        0
    };

    BenchResult {
        threads,
        position_index: 0, // Will be set by caller
        nodes: result.stats.nodes,
        time_ms: elapsed.as_millis() as u64,
        nps,
        depth_reached: result.stats.depth,
    }
}

fn run_benchmark(args: &Args) -> Vec<BenchResult> {
    let evaluator = Arc::new(MaterialEvaluator);
    let positions = get_test_positions();
    let mut all_results = Vec::new();

    println!("=== ParallelSearcher Benchmark ===");
    println!("Depth: {}", args.depth);
    if let Some(ms) = args.fixed_ms {
        println!("Fixed time: {ms}ms");
    }
    println!("Positions: {}", positions.len());
    println!("Warmup runs: {}", args.warmup_runs);
    println!("Measurement runs: {}", args.measurement_runs);
    println!("TT size: {}MB", args.tt_size);
    println!();

    for &threads in &args.threads {
        println!("Testing with {threads} threads...");

        for (pos_idx, (pos_name, position)) in positions.iter().enumerate() {
            if args.skip_positions.contains(&pos_idx) {
                println!("  Skipping position {pos_idx} ({pos_name})");
                continue;
            }

            // Create fresh TT for each position to avoid contamination
            let tt = Arc::new(TranspositionTable::new(args.tt_size));

            // Warmup runs
            if args.warmup_runs > 0 {
                for _ in 0..args.warmup_runs {
                    let mut pos_clone = position.clone();
                    let _ = bench_once(
                        evaluator.clone(),
                        tt.clone(),
                        &mut pos_clone,
                        threads,
                        args.depth,
                        args.fixed_ms,
                    );
                }
            }

            // Measurement runs
            let mut position_results = Vec::new();
            for run in 0..args.measurement_runs {
                let mut pos_clone = position.clone();
                let mut result = bench_once(
                    evaluator.clone(),
                    tt.clone(),
                    &mut pos_clone,
                    threads,
                    args.depth,
                    args.fixed_ms,
                );
                result.position_index = pos_idx;

                println!(
                    "  {} run {}: {} nodes in {}ms = {} NPS",
                    pos_name,
                    run + 1,
                    result.nodes,
                    result.time_ms,
                    result.nps
                );

                position_results.push(result);
            }

            // Calculate average for this position
            if !position_results.is_empty() {
                let avg_nodes: u64 = position_results.iter().map(|r| r.nodes).sum::<u64>()
                    / position_results.len() as u64;
                let avg_time_ms: u64 = position_results.iter().map(|r| r.time_ms).sum::<u64>()
                    / position_results.len() as u64;
                let avg_nps: u64 = position_results.iter().map(|r| r.nps).sum::<u64>()
                    / position_results.len() as u64;

                println!(
                    "  {pos_name} average: {avg_nodes} nodes in {avg_time_ms}ms = {avg_nps} NPS"
                );
            }

            all_results.extend(position_results);
        }
        println!();
    }

    all_results
}

fn print_summary_table(results: &[BenchResult], args: &Args) {
    println!("=== Summary ===");
    println!();

    // Group results by thread count
    let mut thread_summaries: Vec<(usize, u64, u64, u64)> = Vec::new(); // threads, nodes, time_ms, nps

    for &threads in &args.threads {
        let thread_results: Vec<&BenchResult> =
            results.iter().filter(|r| r.threads == threads).collect();

        if thread_results.is_empty() {
            continue;
        }

        let total_nodes: u64 = thread_results.iter().map(|r| r.nodes).sum();
        let total_time_ms: u64 = thread_results.iter().map(|r| r.time_ms).sum();
        let avg_nps = if total_time_ms > 0 {
            (total_nodes as f64 / (total_time_ms as f64 / 1000.0)) as u64
        } else {
            0
        };

        thread_summaries.push((threads, total_nodes, total_time_ms, avg_nps));
    }

    // Find baseline (1 thread)
    let baseline_nps = thread_summaries
        .iter()
        .find(|(t, _, _, _)| *t == 1)
        .map(|(_, _, _, nps)| *nps)
        .unwrap_or(1);

    // Print table
    println!("Threads |      NPS | Speedup | Efficiency |    Nodes |      Time");
    println!("--------|----------|---------|------------|----------|----------");

    for (threads, nodes, time_ms, nps) in thread_summaries {
        let speedup = if baseline_nps > 0 {
            nps as f64 / baseline_nps as f64
        } else {
            1.0
        };
        let efficiency = speedup / threads as f64;

        println!(
            "{:7} | {:8} | {:7.2}x | {:9.1}% | {:8} | {:9.3}s",
            threads,
            nps,
            speedup,
            efficiency * 100.0,
            nodes,
            time_ms as f64 / 1000.0
        );
    }
}

fn print_json(results: &[BenchResult]) {
    let json = serde_json::to_string_pretty(&results).unwrap();
    println!("{json}");
}

fn print_csv(results: &[BenchResult]) {
    println!("threads,position_index,nodes,time_ms,nps,depth_reached");
    for r in results {
        println!(
            "{},{},{},{},{},{}",
            r.threads, r.position_index, r.nodes, r.time_ms, r.nps, r.depth_reached
        );
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let results = run_benchmark(&args);

    match args.output.as_str() {
        "json" => print_json(&results),
        "csv" => print_csv(&results),
        _ => print_summary_table(&results, &args),
    }

    Ok(())
}
