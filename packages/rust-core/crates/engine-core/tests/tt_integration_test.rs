//! Integration test for TT in actual search

use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{unified::UnifiedSearcher, SearchLimits},
    shogi::Position,
};

type TestSearcher = UnifiedSearcher<MaterialEvaluator, true, false, 1>;

#[test]
fn test_tt_search_integration() {
    // Create searcher with TT
    let mut searcher = TestSearcher::new(MaterialEvaluator);

    // Test position
    let mut pos = Position::startpos();

    // Search with depth limit
    let limits = SearchLimits {
        depth: Some(3),
        ..Default::default()
    };

    let result = searcher.search(&mut pos, limits);

    // Verify search completed successfully
    assert!(result.stats.depth >= 3);
    assert!(result.score.abs() < 10000); // Not a mate score
    assert!(result.stats.nodes > 0);
    assert!(result.best_move.is_some());
}

#[test]
fn test_tt_performance_characteristics() {
    let mut searcher = TestSearcher::new(MaterialEvaluator);

    let mut pos = Position::startpos();

    // Do multiple searches to warm up TT
    for depth in 2..=4 {
        let limits = SearchLimits {
            depth: Some(depth),
            ..Default::default()
        };

        let result = searcher.search(&mut pos, limits);

        // Verify search improves with depth
        assert!(result.stats.depth >= depth);
        assert!(result.stats.nodes > 0);
    }

    // Final deep search should benefit from TT
    let limits = SearchLimits {
        depth: Some(5),
        ..Default::default()
    };

    let result = searcher.search(&mut pos, limits);

    assert!(result.stats.depth >= 5);
    assert!(result.best_move.is_some());
}

#[test]
fn test_tt_consistency() {
    let mut searcher = TestSearcher::new(MaterialEvaluator);

    // Test multiple positions
    let positions = vec![
        Position::startpos(),
        // Add more test positions as needed
    ];

    for pos in positions {
        let limits = SearchLimits {
            depth: Some(3),
            ..Default::default()
        };

        let result = searcher.search(&mut pos.clone(), limits);

        // Should find moves
        assert!(result.best_move.is_some());
        // Score should be reasonable
        assert!(result.score.abs() < 10000);
    }
}
