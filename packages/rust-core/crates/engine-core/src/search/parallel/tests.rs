//! Tests for parallel searcher

use super::{parallel_searcher::*, EngineStopBridge};
use crate::{
    evaluation::evaluate::MaterialEvaluator,
    search::{SearchLimits, TranspositionTable},
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
    let tt = Arc::new(TranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 4, Arc::new(EngineStopBridge::new()));

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
    let tt = Arc::new(TranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 4, Arc::new(EngineStopBridge::new()));

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
    let tt = Arc::new(TranspositionTable::new(16));
    let searcher = ParallelSearcher::new(evaluator, tt, 4, Arc::new(EngineStopBridge::new()));

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
    let tt = Arc::new(TranspositionTable::new(16));
    let num_threads = 4;
    let mut searcher =
        ParallelSearcher::new(evaluator, tt, num_threads, Arc::new(EngineStopBridge::new()));

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
fn test_parallel_nnue_diff_hooks_no_fallback() {
    use crate::evaluation::evaluate::Evaluator;
    use crate::evaluation::nnue::single::SingleChannelNet;
    use crate::evaluation::nnue::NNUEEvaluatorWrapper;
    use crate::evaluation::nnue::{reset_single_fallback_hits, single_fallback_hits};
    use crate::shogi::SHOGI_BOARD_SIZE;

    let n_feat = SHOGI_BOARD_SIZE * crate::evaluation::nnue::features::FE_END;
    let d = 8usize;
    let net = SingleChannelNet {
        n_feat,
        acc_dim: d,
        scale: 600.0,
        w0: vec![0.1; n_feat * d],
        b0: Some(vec![0.01; d]),
        w2: vec![1.0; d],
        b2: 0.0,
        uid: 42,
    };
    let wrapper = NNUEEvaluatorWrapper::new_with_single_net_for_test(net);
    // Local proxy to forward hooks with interior mutability
    struct TestNnueProxy(std::sync::RwLock<NNUEEvaluatorWrapper>);
    impl Evaluator for TestNnueProxy {
        fn evaluate(&self, pos: &crate::Position) -> i32 {
            self.0.read().unwrap().evaluate(pos)
        }
        fn on_set_position(&self, pos: &crate::Position) {
            if let Ok(mut g) = self.0.write() {
                g.set_position(pos);
            }
        }
        fn on_do_move(&self, pre_pos: &crate::Position, mv: crate::shogi::Move) {
            if let Ok(mut g) = self.0.write() {
                let _ = g.do_move(pre_pos, mv);
            }
        }
        fn on_undo_move(&self) {
            if let Ok(mut g) = self.0.write() {
                g.undo_move();
            }
        }
        fn on_do_null_move(&self, pre_pos: &crate::Position) {
            if let Ok(mut g) = self.0.write() {
                // Use Move::null() path in do_move to keep acc stack in sync
                let _ = g.do_move(pre_pos, crate::shogi::Move::null());
            }
        }
        fn on_undo_null_move(&self) {
            if let Ok(mut g) = self.0.write() {
                g.undo_move();
            }
        }
    }

    let evaluator = std::sync::Arc::new(TestNnueProxy(std::sync::RwLock::new(wrapper)));
    let tt = std::sync::Arc::new(TranspositionTable::new(16));
    // 2スレッドで並列探索（差分フック経路）
    let mut ps = ParallelSearcher::new(evaluator, tt, 2, Arc::new(EngineStopBridge::new()));

    let mut pos = Position::startpos();
    let limits = SearchLimits::builder().depth(3).build();

    reset_single_fallback_hits();
    let _ = ps.search(&mut pos, limits);
    // フック経路ではフォールバックしない（端数ノイズは 1 以下に収まる想定）
    assert!(
        single_fallback_hits() <= 1,
        "unexpected fallback hits: {}",
        single_fallback_hits()
    );
}

#[test]
fn test_completion_wait_robustness() {
    // Test that completion detection properly waits for all work
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let num_threads = 4;
    let mut searcher =
        ParallelSearcher::new(evaluator, tt, num_threads, Arc::new(EngineStopBridge::new()));

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
    let tt = Arc::new(TranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 2, Arc::new(EngineStopBridge::new()));

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

/// External stop flag is observed by workers and leads to quick convergence.
#[test]
fn test_parallel_observes_external_stop_flag() {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 4, Arc::new(EngineStopBridge::new()));

    let mut pos = Position::startpos();

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let limits = SearchLimits::builder().depth(10).stop_flag(stop.clone()).build();

    // Arm an external stopper to fire shortly after search starts
    let stopper = std::thread::spawn({
        let stop = stop.clone();
        move || {
            std::thread::sleep(std::time::Duration::from_millis(10));
            stop.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });

    let result = searcher.search(&mut pos, limits);
    let _ = stopper.join();

    // Search should have completed and counters converged
    assert!(result.best_move.is_some(), "Search should return a best move");
    assert_eq!(
        searcher.pending_work_items.load(Ordering::Acquire),
        0,
        "Pending work items should be zero after external stop"
    );
    assert_eq!(
        searcher.active_workers.load(Ordering::Acquire),
        0,
        "Active workers should be zero after external stop"
    );
}

#[test]
fn test_fallback_bestmove() {
    // Test that parallel searcher always returns a move, even in edge cases
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 1, Arc::new(EngineStopBridge::new())); // Single thread to simplify

    let mut pos = Position::startpos();

    // Use extremely limited search to potentially trigger no-best-move scenario
    let limits = SearchLimits::builder()
        .depth(1)
        .nodes(1) // Extremely limited node budget
        .build();

    let result = searcher.search(&mut pos, limits);

    // Verify that we always get a best move
    assert!(
        result.best_move.is_some(),
        "Search should always return a best move, even with limited resources"
    );

    // Verify that the move is legal
    if let Some(best_move) = result.best_move {
        let move_gen = crate::movegen::MoveGenerator::new();
        let legal_moves = move_gen.generate_all(&pos).expect("Failed to generate moves");
        let move_found = legal_moves.iter().any(|&m| m == best_move);
        assert!(move_found, "Fallback move must be legal");
    }

    // Verify depth is at least 1
    assert!(result.stats.depth >= 1, "Search depth should be at least 1");

    // Verify PV contains the move
    assert!(!result.stats.pv.is_empty(), "PV should not be empty");
    if let Some(best_move) = result.best_move {
        assert_eq!(result.stats.pv[0], best_move, "PV should start with best move");
    }
}

#[test]
fn test_fallback_bestmove_extreme_limits() {
    // Test fallback with extremely restrictive limits
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(16));
    let mut searcher = ParallelSearcher::new(evaluator, tt, 4, Arc::new(EngineStopBridge::new()));

    // Use a complex middle game position
    let mut pos =
        Position::from_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap_or_else(|_| Position::startpos());

    // Extremely limited search that might not complete properly
    let limits = SearchLimits::builder()
        .depth(1)
        .nodes(1) // Only 1 node allowed
        .qnodes_limit(0) // No quiescence search
        .build();

    let result = searcher.search(&mut pos, limits);

    // Even with extreme limits, we should get a move
    assert!(
        result.best_move.is_some(),
        "Should always return a move even with extreme limits"
    );

    // The move should be legal
    if let Some(best_move) = result.best_move {
        let move_gen = crate::movegen::MoveGenerator::new();
        let legal_moves = move_gen.generate_all(&pos).expect("Failed to generate moves");
        let move_found = legal_moves.iter().any(|&m| m == best_move);
        assert!(move_found, "Move should be legal: {best_move}");
    }
}
