//! Benchmark for Lazy SMP parallel search

use anyhow::Result;
use clap::Parser;
use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{parallel::LazySmpSearcher, SearchLimitsBuilder},
    shogi::Position,
};
// use std::time::Instant; // Not needed anymore, using stats.elapsed

#[derive(Parser, Debug)]
#[command(author, version, about = "Lazy SMP benchmark")]
struct Args {
    /// Thread counts to test (comma-separated)
    #[arg(short, long, value_delimiter = ',', default_value = "1,2,4")]
    threads: Vec<usize>,

    /// Search depth
    #[arg(short, long, default_value = "8")]
    depth: u8,

    /// Fixed time per search in milliseconds
    #[arg(short = 'm', long)]
    fixed_total_ms: Option<u64>,

    /// TT size in MB
    #[arg(long, default_value = "256")]
    tt_size: usize,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let positions = vec![
        ("startpos", Position::startpos()),
        (
            "midgame",
            Position::from_sfen(
                "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17",
            )
            .map_err(|e| anyhow::anyhow!("Failed to parse midgame position: {}", e))?,
        ),
        (
            "endgame",
            Position::from_sfen(
                "1n5n1/2s3k2/3p1p1p1/2p3p2/9/2P3P2/3P1P1P1/2K6/1N5N1 b RBGSLPrbgs2l13p 80",
            )
            .map_err(|e| anyhow::anyhow!("Failed to parse endgame position: {}", e))?,
        ),
    ];

    println!("=== Lazy SMP Benchmark ===");
    println!();

    // Calculate single-thread baseline
    let mut single_thread_nps = 0u64;

    for &num_threads in &args.threads {
        println!("Testing with {} thread(s):", num_threads);

        let mut total_nps = 0u64;

        for (name, position) in &positions {
            println!("  Position: {}", name);

            let evaluator = MaterialEvaluator;
            let mut searcher = LazySmpSearcher::new(evaluator, num_threads, args.tt_size);

            // Strictly exclusive time/depth modes
            let limits = if let Some(ms) = args.fixed_total_ms {
                // TIME MODE: Only set time limit, NO depth
                SearchLimitsBuilder::default().fixed_time_ms(ms).build()
            } else {
                // DEPTH MODE: Only set depth limit, NO time control
                SearchLimitsBuilder::default().depth(args.depth).build()
            };

            let result = searcher.search(&position, limits);
            // Use stats.elapsed for consistent measurement
            let elapsed_ms = result.stats.elapsed.as_millis() as u64;

            let nps = if elapsed_ms > 0 {
                (result.stats.nodes as u64 * 1000) / elapsed_ms
            } else {
                0
            };

            println!("    {} nodes in {}ms = {} nps", result.stats.nodes, elapsed_ms, nps);
            if let Some(best_move) = result.best_move {
                println!("    Best move: {}, Score: {}", best_move, result.score);
            }

            total_nps += nps;
        }

        let avg_nps = total_nps / positions.len() as u64;

        if num_threads == 1 {
            single_thread_nps = avg_nps;
        }

        let speedup = if single_thread_nps > 0 {
            avg_nps as f64 / single_thread_nps as f64
        } else {
            1.0
        };

        let efficiency = speedup / num_threads as f64 * 100.0;

        println!("  Average NPS: {}", avg_nps);
        println!("  Speedup: {:.2}x", speedup);
        println!("  Efficiency: {:.1}%", efficiency);

        // Performance target check
        if num_threads == 2 {
            let target_met = speedup >= 1.25;
            println!("  Target (≥1.25x): {}", if target_met { "✓" } else { "✗" });
        } else if num_threads == 4 {
            let target_met = speedup >= 1.8;
            println!("  Target (≥1.8x): {}", if target_met { "✓" } else { "✗" });
        }
        println!();
    }

    Ok(())
}
