//! Benchmark for move ordering improvements
//!
//! Run with: cargo run --release --example move_ordering_benchmark

use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{unified::UnifiedSearcher, SearchLimitsBuilder},
    shogi::Position,
};
use std::time::Instant;

fn main() {
    println!("Move Ordering Improvement Benchmark");
    println!("====================================\n");

    // Test positions that benefit from good move ordering
    let test_positions = vec![
        ("Initial", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
        (
            "Midgame",
            "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w GR5pnsg 1",
        ),
        (
            "Tactical",
            "ln1g1g1nl/1ks2r3/1pppp1bpp/p6p1/9/P1P4P1/1P1PPPP1P/1BK1GS1R1/LNSG3NL b Pp 1",
        ),
        ("Endgame", "8l/7p1/6gk1/5Sp1p/9/5G1PP/7K1/9/7NL b RBG2S2N2L13P2rbgsnl 1"),
    ];

    for (name, sfen) in test_positions {
        println!("Position: {name}");
        println!("SFEN: {sfen}");
        println!("{}", "-".repeat(50));

        let mut pos = Position::from_sfen(sfen).expect("Valid SFEN");

        // Test at different depths
        for depth in [4, 5, 6] {
            println!("\n  Depth {depth}:");

            // Create searcher with move ordering improvements
            let mut searcher =
                UnifiedSearcher::<MaterialEvaluator, true, true, 32>::new(MaterialEvaluator);

            // Set time limit to prevent infinite search
            let limits = SearchLimitsBuilder::default().depth(depth).fixed_time_ms(5000).build();

            let start = Instant::now();
            let result = searcher.search(&mut pos, limits);
            let elapsed = start.elapsed();

            println!("    Time: {:.2}s", elapsed.as_secs_f64());
            println!("    Nodes: {}", result.stats.nodes);
            println!("    NPS: {:.0}", result.stats.nodes as f64 / elapsed.as_secs_f64());
            println!("    Score: {}", result.score);
            println!("    Best move: {:?}", result.best_move);

            // Check search efficiency
            if result.stats.nodes > 0 {
                // Calculate branching factor (lower is better)
                let branching_factor = (result.stats.nodes as f64).powf(1.0 / depth as f64);
                println!("    Branching factor: {:.2}", branching_factor);
            }
        }

        println!("\n");
    }

    // Summary
    println!("{}", "=".repeat(50));
    println!("Benchmark Complete");
    println!("\nMove Ordering Improvements:");
    println!("1. Global Killer Move Table - tracks beta cutoffs across plies");
    println!("2. Enhanced mutex fallback - uses static eval + attack/defense heuristics");
    println!("3. Dual killer tracking - both SearchStack and global table");
    println!("\nExpected improvements:");
    println!("- 30-50% reduction in search nodes");
    println!("- Higher beta cutoff rates");
    println!("- Better first-move cutoff rates (>90% is excellent)");
}
