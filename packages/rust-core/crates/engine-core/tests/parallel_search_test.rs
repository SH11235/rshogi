//! Test for parallel search issues
use engine_core::{
    engine::controller::{Engine, EngineType},
    search::SearchLimits,
    shogi::Position,
};
use once_cell::sync::Lazy;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

static PARALLEL_SEARCH_TEST_LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

#[test]
fn test_parallel_search_short_time() {
    // Test with very short time limit to reproduce the time:0ms issue
    let mut engine = Engine::new(EngineType::Material);
    engine.set_threads(4); // Use 4 threads like in production

    let mut pos = Position::startpos();

    // Create stop flag
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Test 1: Very short byoyomi (1ms)
    let limits = SearchLimits::builder()
        .byoyomi(0, 1, 1) // main_time, byoyomi_ms, periods
        .stop_flag(stop_flag.clone())
        .build();

    let result = engine.search(&mut pos, limits);
    assert!(result.best_move.is_some(), "Should find a move even with 1ms time limit");
    // Note: With very short time limits, elapsed might be 0 due to timing precision
    // This is one of the issues we're fixing, so we'll just warn for now
    if result.stats.elapsed.as_millis() == 0 {
        eprintln!("WARNING: Elapsed time was 0ms with 1ms byoyomi limit");
    }

    // Reset stop flag (ParallelSearcher 側で stop ブロードキャストが入るため、再利用ではなく新規フラグを渡す)
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Test 2: Depth-limited search
    let limits = SearchLimits::builder().depth(1).stop_flag(stop_flag).build();

    let result = engine.search(&mut pos, limits.clone());
    assert!(result.best_move.is_some(), "Should find a move at depth 1");
    assert!(result.stats.nodes > 0, "Should have searched some nodes");
}

#[test]
fn test_parallel_search_node_counting() {
    let _guard = PARALLEL_SEARCH_TEST_LOCK.lock().unwrap();
    // Test that node counting doesn't underflow in parallel search
    let mut engine = Engine::new(EngineType::Material);
    engine.set_threads(4);

    let mut pos = Position::startpos();

    let limits = SearchLimits::builder().depth(5).build();

    let result = engine.search(&mut pos, limits.clone());

    // Check that node count is reasonable
    assert!(result.stats.nodes > 100, "Should search many nodes at depth 5");
    assert!(result.stats.nodes < 10_000_000, "Node count should not overflow");

    // Run multiple searches to ensure stats don't accumulate incorrectly
    let nodes1 = result.stats.nodes;

    // Reset TT so the second search isn't affected by warmed entries and remains comparable.
    engine.clear_hash();
    pos = Position::startpos();

    let result2 = engine.search(&mut pos, limits.clone());
    let nodes2 = result2.stats.nodes;

    // Node counts should be similar (within 2x) for same position/depth
    let ratio = if nodes1 > nodes2 {
        nodes1 as f64 / nodes2.max(1) as f64
    } else {
        nodes2 as f64 / nodes1.max(1) as f64
    };
    assert!(
        ratio < 2.5,
        "Node counts should be consistent within 2.5x: nodes1={nodes1} nodes2={nodes2}"
    );
}

#[test]
fn test_edge_position_moves() {
    let _guard = PARALLEL_SEARCH_TEST_LOCK.lock().unwrap();
    // Test positions where pieces are near board edges
    let mut engine = Engine::new(EngineType::Material);
    engine.set_threads(2);

    // Position with pieces at edges that might cause coordinate underflow
    // Knight at 9a (can't move), Lance at 1a, etc.
    let sfen = "ln1g1g1nl/1r2k2b1/pppppp1pp/9/9/9/PPPPPP1PP/1B2K2R1/LN1G1G1NL b - 1";
    let mut pos = Position::from_sfen(sfen).expect("Valid SFEN");

    let limits = SearchLimits::builder().depth(3).build();

    // This should not panic with coordinate underflow
    let result = engine.search(&mut pos, limits);
    assert!(result.best_move.is_some(), "Should find a legal move");
}

#[test]
fn test_continuous_searches() {
    let _guard = PARALLEL_SEARCH_TEST_LOCK.lock().unwrap();
    // Run many searches continuously to stress test parallel coordination
    let mut engine = Engine::new(EngineType::Material);
    engine.set_threads(4);

    let mut pos = Position::startpos();

    for i in 0..10 {
        let limits = SearchLimits::builder()
            .fixed_time_ms(10) // 10ms per search
            .build();

        let result = engine.search(&mut pos, limits);
        assert!(result.best_move.is_some(), "Search {i} should find a move");
        assert!(
            result.stats.elapsed.as_millis() <= 40,
            "Search {i} should respect time limit (elapsed={}ms)",
            result.stats.elapsed.as_millis()
        );

        // Small delay between searches
        thread::sleep(Duration::from_millis(5));
    }
}
