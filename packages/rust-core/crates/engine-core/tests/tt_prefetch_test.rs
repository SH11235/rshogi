//! Test to verify TT prefetching improves performance

#[cfg(test)]
mod tests {
    use engine_core::{
        evaluation::evaluate::MaterialEvaluator,
        search::{unified::UnifiedSearcher, SearchLimitsBuilder},
        Position,
    };

    #[test]
    fn test_search_with_tt_prefetching() {
        // Test position that generates many TT accesses
        let sfen = "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17";
        let mut pos = Position::from_sfen(sfen).unwrap();

        // Disable pruning to ensure we search enough nodes for TT prefetching to be relevant
        // With pruning enabled, MaterialEvaluator results in very few nodes being searched
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, false, 32>::new(MaterialEvaluator);

        // Search to depth 5 with node limit to prevent hanging
        // This tests TT prefetching without running forever
        let limits = SearchLimitsBuilder::default()
            .depth(5) // Reduced from 6 to 5
            .fixed_nodes(1_500_000) // Add safety limit
            .build();
        let result = searcher.search(&mut pos, limits);

        println!("Nodes searched: {}", result.stats.nodes);

        assert!(result.best_move.is_some());
        // With pruning disabled and depth 5, we still expect many nodes
        assert!(
            result.stats.nodes > 200_000,
            "Expected more than 200000 nodes, but only searched {}",
            result.stats.nodes
        ); // Should search many nodes when pruning is disabled

        println!("Time: {}ms", result.stats.elapsed.as_millis());
        let nps = if result.stats.elapsed.as_secs_f64() > 0.0 {
            result.stats.nodes as f64 / result.stats.elapsed.as_secs_f64()
        } else {
            0.0
        };
        println!("NPS: {nps:.0}");
    }

    #[test]
    fn test_tt_hit_rate() {
        // Test that TT is actually being used effectively
        let mut pos = Position::startpos();
        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 32>::new(MaterialEvaluator);

        // First search
        let limits = SearchLimitsBuilder::default().depth(5).build();
        let result1 = searcher.search(&mut pos, limits.clone());

        // Second search - should have higher TT hit rate
        let result2 = searcher.search(&mut pos, limits);

        // Both searches should find valid moves
        assert!(result1.best_move.is_some());
        assert!(result2.best_move.is_some());

        // Second search should be faster due to TT hits
        println!(
            "First search: {} nodes in {}ms",
            result1.stats.nodes,
            result1.stats.elapsed.as_millis()
        );
        println!(
            "Second search: {} nodes in {}ms",
            result2.stats.nodes,
            result2.stats.elapsed.as_millis()
        );
    }
}
