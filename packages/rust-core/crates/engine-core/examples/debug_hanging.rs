use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{unified::UnifiedSearcher, SearchLimitsBuilder},
    Position,
};
use std::time::Instant;

fn main() {
    // The position from the hanging test
    let sfen = "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17";
    let mut pos = Position::from_sfen(sfen).unwrap();

    println!("Testing position: {sfen}");
    println!("With pruning disabled (like the test)");

    // Create searcher with same config as the hanging test
    let mut searcher =
        UnifiedSearcher::<MaterialEvaluator, true, false, 32>::new(MaterialEvaluator);

    // Try different depths to see where it hangs
    for depth in 1..=6 {
        println!("\nSearching depth {depth}...");
        let start = Instant::now();

        // Same as test - no special limits
        let limits = SearchLimitsBuilder::default().depth(depth).build();

        let result = searcher.search(&mut pos, limits);
        let elapsed = start.elapsed();

        println!("Depth {}: {} nodes in {:?}", depth, result.stats.nodes, elapsed);
        println!("  Total nodes: {}", result.stats.nodes);
        println!("  QNodes: {}", result.stats.qnodes);

        if result.stats.nodes > 1_000_000 {
            println!("  -> Already exceeds 1M nodes at depth {depth}");
        }

        // Stop if it's taking too long
        if elapsed.as_secs() > 30 {
            println!("  -> Taking too long (>30s), stopping test");
            break;
        }
    }
}
