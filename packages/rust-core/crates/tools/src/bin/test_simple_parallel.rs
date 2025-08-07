//! Test program for SimpleParallelSearcher
//!
//! This tests the new simplified parallel search implementation

use anyhow::Result;
use clap::Parser;
use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{parallel::SimpleParallelSearcher, SearchLimitsBuilder, TranspositionTable},
    shogi::Position,
    time_management::TimeControl,
};
use std::sync::Arc;
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Number of threads
    #[arg(short, long, default_value = "2")]
    threads: usize,

    /// Search depth
    #[arg(short, long, default_value = "5")]
    depth: u8,

    /// Fixed time per position in milliseconds
    #[arg(short, long)]
    fixed_ms: Option<u64>,

    /// SFEN position to test (default: startpos)
    #[arg(short, long)]
    sfen: Option<String>,

    /// Number of test runs
    #[arg(short, long, default_value = "3")]
    runs: usize,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    println!("Testing SimpleParallelSearcher");
    println!("Threads: {}", args.threads);
    println!("Depth: {}", args.depth);
    println!("Runs: {}", args.runs);

    // Create position
    let mut position = if let Some(sfen) = &args.sfen {
        Position::from_sfen(sfen).map_err(|e| anyhow::anyhow!("Failed to parse SFEN: {}", e))?
    } else {
        Position::startpos()
    };
    println!(
        "Position: {} (ply: {})",
        if args.sfen.is_some() {
            "custom"
        } else {
            "startpos"
        },
        position.ply
    );

    // Create evaluator and TT
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(256)); // 256MB TT

    // Create search limits
    let limits = if let Some(fixed_ms) = args.fixed_ms {
        SearchLimitsBuilder::default()
            .time_control(TimeControl::FixedTime {
                ms_per_move: fixed_ms,
            })
            .depth(args.depth)
            .build()
    } else {
        SearchLimitsBuilder::default().depth(args.depth).build()
    };

    println!("\nRunning tests...\n");

    for run in 1..=args.runs {
        println!("=== Run {run} ===");

        // Create searcher for each run to ensure clean state
        let mut searcher = SimpleParallelSearcher::new(evaluator.clone(), tt.clone(), args.threads);

        let start = Instant::now();
        let result = searcher.search(&mut position, limits.clone());
        let elapsed = start.elapsed();

        println!("Best move: {:?}", result.best_move);
        println!("Score: {}", result.score);
        println!("Depth: {}", result.stats.depth);
        println!("Nodes: {}", result.stats.nodes);
        println!("Time: {elapsed:?}");

        if result.stats.nodes > 0 {
            let nps = (result.stats.nodes as f64) / elapsed.as_secs_f64();
            println!("NPS: {nps:.0}");
        }

        println!();

        // Check if search completed without issues
        if result.best_move.is_none() {
            eprintln!("WARNING: No best move found!");
        }
    }

    println!("All tests completed successfully!");

    Ok(())
}
