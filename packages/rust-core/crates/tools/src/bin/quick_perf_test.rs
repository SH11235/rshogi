//! Quick performance test to verify TT optimizations

use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{unified::UnifiedSearcher, SearchLimitsBuilder},
    shogi::Position,
};
use std::time::Instant;

fn main() {
    println!("Quick Performance Test");
    println!("=====================\n");

    // Test positions
    let positions = [
        "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        "ln1g1g1nl/1r2k1sb1/p1pppp1pp/1p4p2/9/2P4P1/PP1PPPP1P/1BS1K2R1/LN1G1G1NL w - 1",
        "l2gk2nl/1r3gsb1/p1pppp1pp/1p4p2/9/2P4P1/PP1PPPP1P/1BS1G2R1/LN1GK2NL w Pn 1",
    ];

    let depth = 10;
    let mut total_nodes = 0u64;
    let mut total_time_ms = 0u64;

    for (i, sfen) in positions.iter().enumerate() {
        println!("Position {}/{}:", i + 1, positions.len());

        let mut pos = Position::from_sfen(sfen).expect("Valid SFEN");
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(MaterialEvaluator);

        let limits = SearchLimitsBuilder::default().depth(depth).build();

        let start = Instant::now();
        let result = searcher.search(&mut pos, limits);
        let elapsed = start.elapsed();

        let nodes = result.stats.nodes;
        let ms = elapsed.as_millis() as u64;
        let nps = if ms > 0 { (nodes * 1000) / ms } else { 0 };

        println!("  Nodes: {nodes}");
        println!("  Time: {ms}ms");
        println!("  NPS: {nps}\n");

        total_nodes += nodes;
        total_time_ms += ms;
    }

    let avg_nps = if total_time_ms > 0 {
        (total_nodes * 1000) / total_time_ms
    } else {
        0
    };

    println!("Summary:");
    println!("--------");
    println!("Total nodes: {total_nodes}");
    println!("Total time: {total_time_ms}ms");
    println!("Average NPS: {avg_nps}");

    // Compare with expected baseline (from previous runs)
    println!("\nExpected baseline NPS: ~450,000-500,000");
    println!("Your NPS: {avg_nps}");

    if avg_nps > 1_000_000 {
        println!("\n✅ Great! TT optimizations are working well!");
    } else if avg_nps > 700_000 {
        println!("\n✓ Good performance improvement");
    } else {
        println!("\n⚠ Performance might need more optimization");
    }
}
