//! Integration test for TT v2 in actual search

use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{tt_config::TTVersion, unified::UnifiedSearcher, SearchLimits},
    shogi::Position,
};

type TestSearcher = UnifiedSearcher<MaterialEvaluator, true, false, 1>;

#[test]
fn test_tt_v2_search_integration() {
    // Create searcher with default TT (v2)
    let mut searcher = TestSearcher::new(MaterialEvaluator);
    assert_eq!(searcher.get_tt_version(), Some(TTVersion::V2));

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
fn test_tt_version_switching() {
    let mut searcher = TestSearcher::new(MaterialEvaluator);
    let mut pos = Position::startpos();

    // Test with V1
    searcher.set_tt_version(TTVersion::V1);
    assert_eq!(searcher.get_tt_version(), Some(TTVersion::V1));

    let limits = SearchLimits {
        depth: Some(2),
        ..Default::default()
    };

    let result_v1 = searcher.search(&mut pos.clone(), limits.clone());

    // Test with V2
    searcher.set_tt_version(TTVersion::V2);
    assert_eq!(searcher.get_tt_version(), Some(TTVersion::V2));

    let result_v2 = searcher.search(&mut pos, limits);

    // Both should find valid moves
    assert!(result_v1.best_move.is_some());
    assert!(result_v2.best_move.is_some());

    // Both should have reasonable scores
    assert!(result_v1.score.abs() < 10000);
    assert!(result_v2.score.abs() < 10000);
}

#[test]
fn test_tt_v2_performance_characteristics() {
    let mut searcher = TestSearcher::new(MaterialEvaluator);
    searcher.set_tt_version(TTVersion::V2);

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
    let mut searcher_v1 = TestSearcher::new(MaterialEvaluator);
    searcher_v1.set_tt_version(TTVersion::V1);

    let mut searcher_v2 = TestSearcher::new(MaterialEvaluator);
    searcher_v2.set_tt_version(TTVersion::V2);

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

        let result_v1 = searcher_v1.search(&mut pos.clone(), limits.clone());
        let result_v2 = searcher_v2.search(&mut pos.clone(), limits);

        // Both should find moves
        assert!(result_v1.best_move.is_some());
        assert!(result_v2.best_move.is_some());

        // Scores should be in similar range (allowing for search variations)
        let score_diff = (result_v1.score - result_v2.score).abs();
        assert!(
            score_diff < 200,
            "Score difference too large: v1={}, v2={}",
            result_v1.score,
            result_v2.score
        );
    }
}
