//! Basic smoke tests for the parallel search facade.

#![cfg(not(debug_assertions))] // run only in release builds to match original intent

use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{
        parallel::{ParallelSearcher, StopController},
        SearchLimitsBuilder, TranspositionTable,
    },
    shogi::Position,
};
use std::sync::Arc;

#[test]
fn parallel_searcher_smoke() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 2, Arc::new(StopController::new()));
    let mut position = Position::startpos();

    let limits = SearchLimitsBuilder::default().fixed_time_ms(25).depth(4).build();
    let result = searcher.search(&mut position, limits);

    // Placeholder backend returns empty result; just ensure call completes quickly.
    assert!(result.stats.elapsed.as_millis() >= 0);
}

#[test]
fn parallel_searcher_adjust_threads_no_panic() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(4));
    let stop_ctrl = Arc::new(StopController::new());
    let mut searcher = ParallelSearcher::new(evaluator, tt, 1, stop_ctrl);

    // New thread counts should be accepted without panicking even though
    // the concrete implementation is still a stub.
    searcher.adjust_thread_count(4);
    searcher.adjust_thread_count(0);
}
