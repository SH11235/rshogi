//! Tests for parallel searcher

use super::parallel_searcher::*;
use crate::{
    evaluation::evaluate::MaterialEvaluator,
    search::{SearchLimits, ShardedTranspositionTable},
    shogi::Position,
};
use std::sync::{atomic::Ordering, Arc};

/// Create a test position with many captures available
fn create_capture_heavy_position() -> Position {
    // Create a position with many mutual captures
    // This is designed to stress the quiescence search
    // This position has many pieces that can capture each other
    Position::from_sfen("k8/1r1b3g1/p1p1ppp1p/1p1ps2p1/2P6/PP1P1S2P/2SGPPP2/1B5R1/LN1K2GNL b - 1")
        .unwrap_or_else(|_| Position::startpos())
}

#[test]
fn test_parallel_qnodes_budget() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(ShardedTranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 4);

    let mut pos = create_capture_heavy_position();

    // Set a small qnodes budget
    let limits = SearchLimits::builder()
        .depth(5)
        .qnodes_limit(10000) // Small limit to ensure it's hit
        .build();

    let result = searcher.search(&mut pos, limits);

    // Verify that the total qnodes doesn't exceed the limit by much
    // With prev-value checking, overshoot should be minimal but can be more than num_threads
    // due to in-check positions and timing
    let max_overshoot = 1000; // Allow reasonable overshoot for in-check positions
    assert!(
        result.stats.qnodes <= 10000 + max_overshoot,
        "QNodes exceeded limit by too much: {} > {}",
        result.stats.qnodes,
        10000 + max_overshoot
    );

    // Verify we found a move
    assert!(result.best_move.is_some());
}

#[test]
fn test_parallel_qnodes_aggregation() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(ShardedTranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 4);

    // Use a position with captures available to ensure quiescence search
    let mut pos = create_capture_heavy_position();

    // Run search without qnodes limit
    let limits = SearchLimits::builder().depth(4).build();

    let result = searcher.search(&mut pos, limits);

    // Verify that qnodes are properly aggregated
    let shared_qnodes = searcher.shared_state.get_qnodes();
    assert_eq!(
        result.stats.qnodes, shared_qnodes,
        "QNodes not properly aggregated: stats={} shared={}",
        result.stats.qnodes, shared_qnodes
    );

    // With shared counter always incrementing, we should see qnodes > 0
    // for any search that enters quiescence (which should happen with captures)
    assert!(
        result.stats.qnodes > 0,
        "Expected qnodes > 0 for capture-heavy position, got {}",
        result.stats.qnodes
    );
    println!("QNodes recorded: {}", result.stats.qnodes);
}

#[test]
fn test_qnodes_counter_sharing() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(ShardedTranspositionTable::new(16));
    let searcher = ParallelSearcher::new(evaluator, tt, 4);

    // Get the qnodes counter
    let counter1 = searcher.shared_state.get_qnodes_counter();
    let counter2 = searcher.shared_state.get_qnodes_counter();

    // Both should point to the same atomic counter
    counter1.store(42, Ordering::Relaxed);
    assert_eq!(counter2.load(Ordering::Relaxed), 42);

    // Reset should clear it
    searcher.shared_state.reset();
    assert_eq!(counter1.load(Ordering::Relaxed), 0);
}

#[test]
fn test_parallel_qnodes_overshoot_minimal() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(ShardedTranspositionTable::new(16));
    let num_threads = 4;
    let mut searcher = ParallelSearcher::new(evaluator, tt, num_threads);

    let mut pos = create_capture_heavy_position();

    // Set a moderate qnodes budget
    let qnodes_limit = 5000;
    let limits = SearchLimits::builder().depth(5).qnodes_limit(qnodes_limit).build();

    let result = searcher.search(&mut pos, limits);

    // With previous-value checking, overshoot should be minimal
    // However, due to in-check positions and timing, it can be more than num_threads
    let overshoot = result.stats.qnodes.saturating_sub(qnodes_limit);
    // Allow up to 25% overshoot or 1000 nodes, whichever is smaller
    let max_overshoot = (qnodes_limit / 4).min(1000);
    assert!(
        overshoot <= max_overshoot,
        "QNodes overshoot too large: {} (limit={}, actual={}, threads={}, max_allowed={})",
        overshoot,
        qnodes_limit,
        result.stats.qnodes,
        num_threads,
        max_overshoot
    );

    println!(
        "QNodes overshoot test: limit={}, actual={}, overshoot={}",
        qnodes_limit, result.stats.qnodes, overshoot
    );
}

#[test]
fn test_completion_wait_robustness() {
    // Test that completion detection properly waits for all work
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(ShardedTranspositionTable::new(16));
    let num_threads = 4;
    let mut searcher = ParallelSearcher::new(evaluator, tt, num_threads);

    let mut pos = Position::startpos();

    // Set up search with moderate depth
    let limits = SearchLimits::builder().depth(6).build();

    let result = searcher.search(&mut pos, limits);

    // Verify that search completed properly
    assert!(result.best_move.is_some(), "Should find a best move");
    assert!(result.stats.nodes > 0, "Should search some nodes");

    // Check that pending work counter is back to zero
    assert_eq!(
        searcher.pending_work_items.load(Ordering::Acquire),
        0,
        "Pending work items should be zero after search completes"
    );

    // Check that active workers is zero
    assert_eq!(
        searcher.active_workers.load(Ordering::Acquire),
        0,
        "Active workers should be zero after search completes"
    );
}

#[test]
fn test_pending_work_counter_accuracy() {
    // Test that pending_work_items accurately tracks work
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(ShardedTranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 2);

    // Verify initial state
    assert_eq!(searcher.pending_work_items.load(Ordering::Acquire), 0);

    let mut pos = Position::startpos();

    // Run a short search
    let limits = SearchLimits::builder().depth(3).build();
    let _result = searcher.search(&mut pos, limits);

    // After search, pending work should be zero
    assert_eq!(
        searcher.pending_work_items.load(Ordering::Acquire),
        0,
        "Pending work counter should return to zero after search"
    );
}
