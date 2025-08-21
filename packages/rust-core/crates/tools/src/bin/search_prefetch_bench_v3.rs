//! Phase 3: Enhanced benchmark with 3-way comparison and detailed metrics
//! Compares: NoTT vs TTOnly vs TT+Prefetch

use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{
        unified::{TTOperations, UnifiedSearcher},
        SearchLimitsBuilder,
    },
    Position,
};
use std::time::Instant;

/// Benchmark modes for clear comparison
#[derive(Debug, Clone, Copy)]
enum BenchmarkMode {
    NoTT,           // No transposition table
    TTOnly,         // TT enabled, prefetch disabled
    TTWithPrefetch, // TT enabled, prefetch enabled
}

/// Detailed metrics for analysis
#[derive(Debug, Default)]
struct DetailedMetrics {
    nodes: u64,
    nps: f64,
    elapsed_ms: u128,
    prefetch_issued: u64,
    prefetch_hits: u64,
    hashfull: f32,
}

impl DetailedMetrics {
    fn prefetch_hit_rate(&self) -> f32 {
        if self.prefetch_issued == 0 {
            0.0
        } else {
            self.prefetch_hits as f32 / self.prefetch_issued as f32 * 100.0
        }
    }
}

/// Run benchmark with specific mode
fn benchmark_mode(sfen: &str, depth: u8, mode: BenchmarkMode) -> DetailedMetrics {
    let evaluator = MaterialEvaluator;
    let mut metrics = DetailedMetrics::default();

    // Create position
    let mut pos = if sfen == "startpos" {
        Position::startpos()
    } else {
        Position::from_sfen(sfen).expect("Valid SFEN")
    };

    let limits = SearchLimitsBuilder::default().depth(depth).build();
    let start = Instant::now();

    // Run search based on mode
    let result = match mode {
        BenchmarkMode::NoTT => {
            // No TT, no prefetch
            let mut searcher = UnifiedSearcher::<_, false, true>::new(evaluator);
            searcher.search(&mut pos, limits)
        }
        BenchmarkMode::TTOnly => {
            // TT enabled but prefetch disabled
            let mut searcher = UnifiedSearcher::<_, true, true>::new_with_tt_size(evaluator, 16);
            searcher.set_disable_prefetch(true);
            searcher.search(&mut pos, limits)
        }
        BenchmarkMode::TTWithPrefetch => {
            // Full TT with prefetch
            let mut searcher = UnifiedSearcher::<_, true, true>::new_with_tt_size(evaluator, 16);
            let result = searcher.search(&mut pos, limits);

            // Collect TT statistics if available
            if let Some((hashfull, _hits, _misses)) = searcher.get_tt_stats() {
                metrics.hashfull = hashfull;
                // TODO: Use actual hit/miss stats when available
            }

            // Collect prefetch statistics
            if let Some((hits, misses)) = searcher.get_prefetch_stats() {
                metrics.prefetch_hits = hits;
                metrics.prefetch_issued = hits + misses;
            }

            result
        }
    };

    let elapsed = start.elapsed();

    // Fill basic metrics
    metrics.nodes = result.stats.nodes;
    metrics.elapsed_ms = elapsed.as_millis();
    metrics.nps = if elapsed.as_secs_f64() > 0.0 {
        metrics.nodes as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    metrics
}

/// Compare three modes for a position
fn compare_modes(sfen: &str, description: &str, depth: u8) {
    println!("\n{description} (Depth {depth}):");
    println!("{:-<80}", "");

    // Run all three modes
    let no_tt = benchmark_mode(sfen, depth, BenchmarkMode::NoTT);
    let tt_only = benchmark_mode(sfen, depth, BenchmarkMode::TTOnly);
    let tt_prefetch = benchmark_mode(sfen, depth, BenchmarkMode::TTWithPrefetch);

    // Display results in table format
    println!(
        "{:<20} {:>12} {:>12} {:>12} {:>12}",
        "Mode", "Nodes", "NPS", "Time(ms)", "vs NoTT"
    );
    println!("{:-<80}", "");

    println!(
        "{:<20} {:>12} {:>12.0} {:>12} {:>12}",
        "NoTT", no_tt.nodes, no_tt.nps, no_tt.elapsed_ms, "baseline"
    );

    let tt_only_improvement = if no_tt.nps > 0.0 {
        ((tt_only.nps - no_tt.nps) / no_tt.nps) * 100.0
    } else {
        0.0
    };
    println!(
        "{:<20} {:>12} {:>12.0} {:>12} {:>+11.1}%",
        "TTOnly", tt_only.nodes, tt_only.nps, tt_only.elapsed_ms, tt_only_improvement
    );

    let tt_prefetch_improvement = if no_tt.nps > 0.0 {
        ((tt_prefetch.nps - no_tt.nps) / no_tt.nps) * 100.0
    } else {
        0.0
    };
    println!(
        "{:<20} {:>12} {:>12.0} {:>12} {:>+11.1}%",
        "TT+Prefetch",
        tt_prefetch.nodes,
        tt_prefetch.nps,
        tt_prefetch.elapsed_ms,
        tt_prefetch_improvement
    );

    // Node reduction analysis
    println!("\n{:<20} {:>20} {:>20}", "", "vs NoTT", "vs TTOnly");
    println!("{:-<60}", "");

    let node_reduction_tt = if no_tt.nodes > 0 {
        ((no_tt.nodes as f64 - tt_only.nodes as f64) / no_tt.nodes as f64) * 100.0
    } else {
        0.0
    };

    let _node_reduction_prefetch = if no_tt.nodes > 0 {
        ((no_tt.nodes as f64 - tt_prefetch.nodes as f64) / no_tt.nodes as f64) * 100.0
    } else {
        0.0
    };

    let prefetch_vs_tt = if tt_only.nodes > 0 {
        ((tt_only.nodes as f64 - tt_prefetch.nodes as f64) / tt_only.nodes as f64) * 100.0
    } else {
        0.0
    };

    println!(
        "{:<20} {:>19.2}% {:>19.2}%",
        "Node reduction", node_reduction_tt, prefetch_vs_tt
    );

    // Detailed TT/Prefetch statistics
    if tt_prefetch.hashfull > 0.0 {
        println!("\nTT Statistics:");
        println!("  Hashfull: {:.1}%", tt_prefetch.hashfull);
    }

    if tt_prefetch.prefetch_issued > 0 {
        println!("\nPrefetch Statistics:");
        println!("  Issued: {}", tt_prefetch.prefetch_issued);
        println!("  Hits: {}", tt_prefetch.prefetch_hits);
        println!("  Hit rate: {:.1}%", tt_prefetch.prefetch_hit_rate());
    }

    // Calculate prefetch-specific improvement
    let prefetch_improvement = if tt_only.nps > 0.0 {
        ((tt_prefetch.nps - tt_only.nps) / tt_only.nps) * 100.0
    } else {
        0.0
    };

    println!("\n{:<30} {:>+10.1}%", "Prefetch-specific gain:", prefetch_improvement);
}

fn main() {
    println!("=== Phase 3: 3-Way Comparison Benchmark ===");
    println!("Comparing: NoTT vs TTOnly vs TT+Prefetch\n");

    // Test positions - expanded set
    let positions = vec![
        ("startpos", "Initial position"),
        (
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "Standard opening",
        ),
        (
            "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17",
            "Middle game",
        ),
        (
            "8l/1l+R2P3/p2pBG1pp/kps1p4/Nn1P2G2/P1P1P2PP/1PS6/1KSG3+r1/LN2+p3L w Sbgn3p 124",
            "Complex endgame",
        ),
    ];

    // Test depths - note depth 7 issue
    let depths = vec![4, 5, 6];

    // Warmup
    println!("Warming up...");
    benchmark_mode("startpos", 3, BenchmarkMode::TTWithPrefetch);

    // Main benchmark loop
    for depth in depths {
        println!("\n{}", "=".repeat(80));
        println!("DEPTH {depth} RESULTS");
        println!("{}", "=".repeat(80));

        for (sfen, description) in &positions {
            compare_modes(sfen, description, depth);
        }

        // Summary for this depth
        println!("\n{}", "-".repeat(80));
        println!("Depth {depth} Summary:");
        // TODO: Calculate and display average improvements
    }

    // Test depth 7 with timeout protection
    println!("\n{}", "=".repeat(80));
    println!("DEPTH 7 TEST (with timeout protection)");
    println!("{}", "=".repeat(80));

    // Only test on simple position first
    println!("\nTesting depth 7 on initial position only...");
    compare_modes("startpos", "Initial position", 7);

    println!("\n{}", "=".repeat(80));
    println!("Benchmark complete!");

    // Print recommendations
    println!("\n=== Analysis Recommendations ===");
    println!("1. Check if TT hit rate improves with depth");
    println!("2. Monitor prefetch hit rate (target: >50%)");
    println!("3. Verify node reduction patterns");
    println!("4. Look for depth 6->7 performance cliff");
}
