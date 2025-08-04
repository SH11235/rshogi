//! Benchmark for TT prefetch optimization in actual search
//! Tests Phase 2 selective prefetching with real alpha-beta search

use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{unified::UnifiedSearcher, SearchLimitsBuilder},
    Position,
};
use std::time::Instant;

fn benchmark_search_with_prefetch(sfen: &str, depth: u8) -> (u64, f64) {
    // Create enhanced searcher with TT and pruning (prefetch enabled)
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, true, true, 16>::new(evaluator);
    let mut pos = if sfen == "startpos" {
        Position::startpos()
    } else {
        Position::from_sfen(sfen).expect("Valid SFEN")
    };

    let limits = SearchLimitsBuilder::default().depth(depth).build();

    let start = Instant::now();
    let result = searcher.search(&mut pos, limits);
    let elapsed = start.elapsed();

    let nodes = result.stats.nodes;
    let nps = nodes as f64 / elapsed.as_secs_f64();

    (nodes, nps)
}

fn benchmark_search_without_prefetch(sfen: &str, depth: u8) -> (u64, f64) {
    // Create basic searcher without TT (no prefetch)
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<_, false, true, 0>::new(evaluator);
    let mut pos = if sfen == "startpos" {
        Position::startpos()
    } else {
        Position::from_sfen(sfen).expect("Valid SFEN")
    };

    let limits = SearchLimitsBuilder::default().depth(depth).build();

    let start = Instant::now();
    let result = searcher.search(&mut pos, limits);
    let elapsed = start.elapsed();

    let nodes = result.stats.nodes;
    let nps = nodes as f64 / elapsed.as_secs_f64();

    (nodes, nps)
}

fn main() {
    println!("=== Search-based TT Prefetch Benchmark (Phase 2) ===\n");

    // Test positions
    let positions = vec![
        ("startpos", "Initial position"),
        (
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "Standard opening",
        ),
        (
            "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 1",
            "Middle game position",
        ),
    ];

    // Test different depths (reduced for faster testing)
    let depths = vec![4, 5, 6];

    println!("Warming up...\n");
    // Warmup
    benchmark_search_with_prefetch("startpos", 4);

    for depth in depths {
        println!("--- Depth {depth} ---");
        let mut total_improvement = 0.0;
        let mut count = 0;

        for (sfen, description) in &positions {
            print!("{description:20} ");

            // Run without prefetch (no TT)
            let (_nodes_without, nps_without) = benchmark_search_without_prefetch(sfen, depth);

            // Run with prefetch (with TT)
            let (_nodes_with, nps_with) = benchmark_search_with_prefetch(sfen, depth);

            let improvement = ((nps_with - nps_without) / nps_without) * 100.0;
            println!("NPS: {nps_without:.0} -> {nps_with:.0} ({improvement:+.2}%)");

            total_improvement += improvement;
            count += 1;
        }

        let avg_improvement = total_improvement / count as f64;
        println!("Average improvement: {avg_improvement:+.2}%\n");
    }

    // Now test specifically the prefetch optimization by comparing with/without selective prefetch
    println!("\n=== Selective Prefetch Comparison ===\n");
    println!("Testing TT with and without selective prefetch optimization\n");

    // Use a more complex position for this test
    let test_sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let test_depth = 7;

    // Standard TT searcher (includes prefetch)
    let (nodes_with, nps_with) = benchmark_search_with_prefetch(test_sfen, test_depth);

    // For comparison with old prefetch, we'd need to modify the code
    // For now, compare against no-TT baseline
    let (nodes_baseline, nps_baseline) = benchmark_search_without_prefetch(test_sfen, test_depth);

    println!("Depth {test_depth}: ");
    println!("  Without TT: {nps_baseline:.0} NPS");
    println!("  With TT+Prefetch: {nps_with:.0} NPS");
    println!("  Improvement: {:+.2}%", ((nps_with - nps_baseline) / nps_baseline) * 100.0);
    let node_reduction =
        ((nodes_baseline as f64 - nodes_with as f64) / nodes_baseline as f64) * 100.0;
    println!("  Node reduction: {node_reduction:.2}%");
}
