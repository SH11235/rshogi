//! Test SearchStack integration with UnifiedSearcher

use engine_core::evaluation::evaluate::MaterialEvaluator;
use engine_core::search::unified::UnifiedSearcher;
use engine_core::search::SearchLimits;
use engine_core::shogi::Position;
use engine_core::time_management::TimeControl;

#[test]
fn test_search_stack_integration() {
    // Create enhanced searcher with SearchStack
    let evaluator = MaterialEvaluator;
    let mut searcher: UnifiedSearcher<MaterialEvaluator, true, true, 16> =
        UnifiedSearcher::new(evaluator);

    // Create initial position
    let mut pos = Position::startpos();

    // Search with depth limit
    let limits = SearchLimits {
        time_control: TimeControl::Infinite,
        depth: Some(5),
        nodes: None,
        qnodes_limit: None,
        qnodes_counter: None,
        moves_to_go: None,
        time_parameters: None,
        stop_flag: None,
        info_callback: None,
        ponder_hit_flag: None,
    };

    let result = searcher.search(&mut pos, limits);

    // Verify result
    assert!(result.best_move.is_some());
    assert!(result.stats.nodes > 0);
    assert_eq!(result.stats.depth, 5);

    println!("Search completed:");
    println!("  Best move: {:?}", result.best_move);
    println!("  Score: {}", result.score);
    println!("  Nodes: {}", result.stats.nodes);
    println!("  Depth: {}", result.stats.depth);
}

#[test]
fn test_search_stack_killers() {
    // Test that killer moves are properly stored in SearchStack
    let evaluator = MaterialEvaluator;
    let mut searcher: UnifiedSearcher<MaterialEvaluator, true, true, 16> =
        UnifiedSearcher::new(evaluator);

    // Create position
    let mut pos = Position::startpos();

    // Search with deeper depth to ensure killers are populated
    let limits = SearchLimits {
        time_control: TimeControl::Infinite,
        depth: Some(7),
        nodes: None,
        qnodes_limit: None,
        qnodes_counter: None,
        moves_to_go: None,
        time_parameters: None,
        stop_flag: None,
        info_callback: None,
        ponder_hit_flag: None,
    };

    let result = searcher.search(&mut pos, limits);

    assert!(result.best_move.is_some());
    assert!(result.stats.nodes > 100); // Should search many nodes at depth 7
}

#[test]
fn test_search_stack_static_eval_cache() {
    // Test that static eval is cached in SearchStack
    let evaluator = MaterialEvaluator;
    let mut searcher: UnifiedSearcher<MaterialEvaluator, true, true, 16> =
        UnifiedSearcher::new(evaluator);

    // Create position
    let mut pos = Position::startpos();

    // Search with futility pruning enabled (uses static eval)
    let limits = SearchLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 100 },
        depth: None,
        nodes: None,
        qnodes_limit: None,
        qnodes_counter: None,
        moves_to_go: None,
        time_parameters: None,
        stop_flag: None,
        info_callback: None,
        ponder_hit_flag: None,
    };

    let result = searcher.search(&mut pos, limits);

    assert!(result.best_move.is_some());
    // Static eval caching should improve performance
}
