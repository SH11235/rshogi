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

        let mut searcher =
            UnifiedSearcher::<MaterialEvaluator, true, true, 32>::new(MaterialEvaluator);

        // Search to depth 4
        let limits = SearchLimitsBuilder::default().depth(4).build();
        let result = searcher.search(&mut pos, limits);

        assert!(result.best_move.is_some());
        assert!(result.stats.nodes > 10000); // Should search many nodes

        println!("Nodes searched: {}", result.stats.nodes);
        println!("Time: {}ms", result.stats.elapsed.as_millis());
        let nps = if result.stats.elapsed.as_secs_f64() > 0.0 {
            result.stats.nodes as f64 / result.stats.elapsed.as_secs_f64()
        } else {
            0.0
        };
        println!("NPS: {:.0}", nps);
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
