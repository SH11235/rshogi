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

    assert!(result.nodes > 0, "parallel search should accumulate nodes");
}

#[test]
fn parallel_searcher_adjust_threads_no_panic() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(4));
    let stop_ctrl = Arc::new(StopController::new());
    let mut searcher = ParallelSearcher::new(evaluator, tt, 1, stop_ctrl);

    searcher.adjust_thread_count(4);
    searcher.adjust_thread_count(0);

    let mut position = Position::startpos();
    let limits = SearchLimitsBuilder::default().fixed_time_ms(20).depth(3).build();
    let result = searcher.search(&mut position, limits);
    assert!(result.nodes > 0);
}

#[test]
fn parallel_matches_single_thread_bestmove() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt_single = Arc::new(TranspositionTable::new(8));
    let tt_multi = Arc::new(TranspositionTable::new(8));
    let stop_single = Arc::new(StopController::new());
    let stop_multi = Arc::new(StopController::new());

    let mut single = ParallelSearcher::new(
        Arc::clone(&evaluator),
        Arc::clone(&tt_single),
        1,
        Arc::clone(&stop_single),
    );
    let mut multi =
        ParallelSearcher::new(evaluator, Arc::clone(&tt_multi), 3, Arc::clone(&stop_multi));

    let mut pos_single = Position::startpos();
    let mut pos_multi = pos_single.clone();

    let limits_single = SearchLimitsBuilder::default().fixed_time_ms(25).depth(3).build();
    let limits_multi = SearchLimitsBuilder::default().fixed_time_ms(25).depth(3).build();

    let single_result = single.search(&mut pos_single, limits_single);
    let multi_result = multi.search(&mut pos_multi, limits_multi);

    assert_eq!(single_result.best_move, multi_result.best_move);
    assert!(multi_result.nodes >= single_result.nodes);
}
