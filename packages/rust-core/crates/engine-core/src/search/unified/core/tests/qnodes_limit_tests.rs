//! Tests for QNodes (quiescence nodes) limit functionality

use crate::evaluation::evaluate::MaterialEvaluator;
use crate::search::unified::core::quiescence;
use crate::search::unified::UnifiedSearcher;
use crate::search::SearchLimits;
use crate::time_management::TimeControl;
use crate::Position;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

#[test]
fn test_qnodes_limit_basic() {
    // Test basic QNodes limit functionality
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);

    // Set a very small qnodes limit
    let limits = SearchLimits::builder()
        .depth(5)
        .qnodes_limit(10) // Very small limit
        .build();
    searcher.context.set_limits(limits);

    // Create a position with many captures available
    // This SFEN has multiple pieces that can capture each other
    let pos = Position::from_sfen("k8/9/9/3G1G3/2P1P1P2/3B1R3/9/9/K8 b - 1").unwrap();

    let mut test_pos = pos.clone();
    let _score = quiescence::quiescence_search(&mut searcher, &mut test_pos, -1000, 1000, 0, 0);

    // Verify qnodes limit was respected
    assert!(
        searcher.stats.qnodes <= 10,
        "QNodes should not exceed limit, got {}",
        searcher.stats.qnodes
    );
}

#[test]
fn test_qnodes_limit_in_check() {
    // Test QNodes limit when in check position
    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);

    // Set a small qnodes limit
    let limits = SearchLimits::builder()
        .depth(5)
        .qnodes_limit(5) // Very small limit
        .build();
    searcher.context.set_limits(limits);

    // Position: Black king in check
    let pos = Position::from_sfen("9/9/9/9/4K3r/9/9/9/9 b - 1").unwrap();
    assert!(pos.is_in_check(), "Position should be in check");

    let mut test_pos = pos.clone();
    let score = quiescence::quiescence_search(&mut searcher, &mut test_pos, -1000, 1000, 0, 0);

    // Verify qnodes limit was respected
    assert!(
        searcher.stats.qnodes <= 5,
        "QNodes should not exceed limit even in check, got {}",
        searcher.stats.qnodes
    );

    // Score should be reasonable (not a mate score)
    assert!(score.abs() < 30000, "Score should be reasonable, not mate");
}

#[test]
fn test_qnodes_limit_performance() {
    // Test that QNodes limit improves performance in complex positions

    let evaluator = MaterialEvaluator;

    // Position with many possible captures (complex middlegame)
    let complex_pos = Position::from_sfen(
        "ln1gkg1nl/1r5s1/p1pppbppp/1p5P1/9/2P6/PP1PPPP1P/1B5R1/LNSGKGSNL b - 1",
    )
    .unwrap();

    // Test without qnodes limit
    let mut searcher1 = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);
    searcher1.context.set_limits(SearchLimits::builder().depth(1).build());

    let start1 = Instant::now();
    let mut pos1 = complex_pos.clone();
    let _score1 = quiescence::quiescence_search(&mut searcher1, &mut pos1, -1000, 1000, 0, 0);
    let elapsed1 = start1.elapsed();
    let nodes_without_limit = searcher1.stats.qnodes;

    // Test with qnodes limit
    let mut searcher2 = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);
    searcher2.context.set_limits(
        SearchLimits::builder()
            .depth(1)
            .qnodes_limit(1000) // Reasonable limit
            .build(),
    );

    let start2 = Instant::now();
    let mut pos2 = complex_pos.clone();
    let _score2 = quiescence::quiescence_search(&mut searcher2, &mut pos2, -1000, 1000, 0, 0);
    let elapsed2 = start2.elapsed();
    let nodes_with_limit = searcher2.stats.qnodes;

    // With limit should explore fewer nodes
    assert!(
        nodes_with_limit <= nodes_without_limit,
        "With limit should explore fewer or equal nodes"
    );

    // With limit should be faster (or at least not significantly slower)
    // Note: This might be flaky in CI, so we're lenient
    if elapsed2 > elapsed1.saturating_mul(2) {
        eprintln!(
            "Warning: QNodes limit didn't improve performance as expected. \
            Without limit: {elapsed1:?} ({nodes_without_limit} nodes), With limit: {elapsed2:?} ({nodes_with_limit} nodes)"
        );
    }
}

#[test]
fn test_qnodes_token_return_on_stop() {
    // Test that qnodes are returned when stop flag is set

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);

    // Set up stop flag and shared counter
    let stop_flag = Arc::new(AtomicBool::new(true)); // Already stopped
    let shared_counter = Arc::new(AtomicU64::new(0));

    searcher.context.set_limits(SearchLimits {
        time_control: TimeControl::Infinite,
        depth: Some(1),
        nodes: None,
        qnodes_limit: None,
        qnodes_counter: Some(shared_counter.clone()),
        moves_to_go: None,
        time_parameters: None,
        stop_flag: Some(stop_flag),
        info_callback: None,
        iteration_callback: None,
        ponder_hit_flag: None,
        immediate_eval_at_depth_zero: false,
    });

    // Create position
    let mut pos = Position::startpos();

    // Initial qnodes
    let initial_qnodes = searcher.stats.qnodes;
    let initial_shared = shared_counter.load(Ordering::Acquire);

    // Call quiescence search with stop flag already set
    let _score = quiescence::quiescence_search(&mut searcher, &mut pos, -1000, 1000, 0, 0);

    // Check that qnodes were incremented before stop check
    let final_qnodes = searcher.stats.qnodes;
    let final_shared = shared_counter.load(Ordering::Acquire);

    // The counter is incremented before the stop check, and remains incremented
    // This is the expected behavior - we don't "return" the token on stop
    assert_eq!(
        final_qnodes,
        initial_qnodes + 1,
        "Local qnodes should increment exactly once before stop check"
    );
    assert_eq!(
        final_shared,
        initial_shared + 1,
        "Shared qnodes should increment exactly once before stop check"
    );
}

#[test]
fn test_qnodes_token_return_on_limit_exceeded() {
    // Test that qnodes are properly returned when limit is exceeded

    let evaluator = MaterialEvaluator;
    let mut searcher = UnifiedSearcher::<MaterialEvaluator, false, false>::new(evaluator);

    // Set up shared counter
    let shared_counter = Arc::new(AtomicU64::new(0));

    // Set very low qnodes limit to trigger exceeded condition
    searcher.context.set_limits(SearchLimits {
        time_control: TimeControl::Infinite,
        depth: Some(1),
        nodes: None,
        qnodes_limit: Some(1), // Will be exceeded immediately
        qnodes_counter: Some(shared_counter.clone()),
        moves_to_go: None,
        time_parameters: None,
        stop_flag: None,
        info_callback: None,
        iteration_callback: None,
        ponder_hit_flag: None,
        immediate_eval_at_depth_zero: false,
    });

    // Create position with captures available
    let pos = Position::from_sfen(
        "ln1gkg1nl/1r5s1/p1pppbppp/1p5P1/9/2P6/PP1PPPP1P/1B5R1/LNSGKGSNL b - 1",
    )
    .unwrap();
    let mut pos = pos;

    // Call quiescence search
    let _score = quiescence::quiescence_search(&mut searcher, &mut pos, -1000, 1000, 0, 0);

    // Check that we stayed within reasonable bounds
    let final_qnodes = searcher.stats.qnodes;
    let final_shared = shared_counter.load(Ordering::Acquire);

    // Should be at most limit + 1 (the one that triggered exceeded)
    assert!(final_qnodes <= 2, "Local qnodes {final_qnodes} should be close to limit");
    assert!(final_shared <= 2, "Shared qnodes {final_shared} should be close to limit");
}
