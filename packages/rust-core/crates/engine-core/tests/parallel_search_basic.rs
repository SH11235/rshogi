//! Basic parallel search test for Thread Sanitizer validation

#![cfg(not(debug_assertions))] // Only run these tests in release builds due to timing sensitivity

use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{
        parallel::{ParallelSearcher, SearchThread, SharedSearchState},
        SearchLimitsBuilder, TranspositionTable,
    },
    shogi::Position,
};
use std::sync::{atomic::AtomicBool, Arc};
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn test_parallel_search_no_data_races() {
    // Create shared resources
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let stop_flag = Arc::new(AtomicBool::new(false));
    let shared_state = Arc::new(SharedSearchState::new(stop_flag.clone()));

    // Create multiple threads
    let num_threads = 4;
    let mut handles = vec![];

    for id in 0..num_threads {
        let evaluator = evaluator.clone();
        let tt = tt.clone();
        let shared_state = shared_state.clone();

        let handle = thread::spawn(move || {
            let mut thread = SearchThread::new(id, evaluator, tt, shared_state);
            let mut position = Position::startpos();

            // Set a short time limit
            let limits = SearchLimitsBuilder::default().fixed_time_ms(10).build();

            // Run search
            let depth = thread.get_start_depth(1);
            let _result = thread.search(&mut position, limits, depth);
        });

        handles.push(handle);
    }

    // Wait for all threads to complete
    // Note: threads will run until the time limit (10ms) expires
    for handle in handles {
        handle.join().unwrap();
    }

    // Verify some basic properties
    assert!(
        shared_state.get_nodes() > 0,
        "Expected nodes > 0, got {}",
        shared_state.get_nodes()
    );
}

#[test]
fn test_shared_history_concurrent_access() {
    use engine_core::search::parallel::SharedHistory;
    use engine_core::shogi::{Color, PieceType, Square};

    let history = Arc::new(SharedHistory::new());
    let mut handles = vec![];

    // Create multiple threads that update the same history entries
    for thread_id in 0..8 {
        let history = history.clone();

        let handle = thread::spawn(move || {
            for i in 0..100 {
                let square = Square::new((thread_id % 9) as u8, (i % 9) as u8);
                history.update(Color::Black, PieceType::Pawn, square, 10);
                history.update(Color::White, PieceType::Rook, square, 20);

                // Read values
                let _val1 = history.get(Color::Black, PieceType::Pawn, square);
                let _val2 = history.get(Color::White, PieceType::Rook, square);

                // Occasionally age the history
                if i % 20 == 0 {
                    history.age();
                }
            }
        });

        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    // Clear history from main thread while others might still be reading
    history.clear();
}

#[test]
fn test_parallel_searcher_integration() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));

    let mut searcher = ParallelSearcher::new(evaluator, tt, 4);
    let mut position = Position::startpos();

    // Test with time limit
    let limits = SearchLimitsBuilder::default().fixed_time_ms(50).depth(5).build();

    let result = searcher.search(&mut position, limits);

    // Verify results
    assert!(result.best_move.is_some(), "Should find a best move");
    assert!(result.stats.nodes > 0, "Should search some nodes");
    assert!(result.stats.depth > 0, "Should reach some depth");
}

#[test]
fn test_barrier_deadlock_prevention() {
    // Test that early stop doesn't cause deadlock
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 4);
    let mut position = Position::startpos();

    // Very short time limit to trigger early stop
    let limits = SearchLimitsBuilder::default().depth(10).fixed_time_ms(10).build();

    let start = Instant::now();
    let result = searcher.search(&mut position, limits);
    let elapsed = start.elapsed();

    // Should complete quickly without deadlock
    assert!(elapsed < Duration::from_millis(200), "Search took too long: {elapsed:?}");
    assert!(result.stats.nodes > 0);
}
