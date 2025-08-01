//! Search engine smoke tests
//!
//! Tests basic search engine functionality and responsiveness

use engine_core::{
    engine::controller::{Engine, EngineType},
    search::SearchLimitsBuilder,
    Position,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Test helper to run search operations
struct SearchTester {
    engine: Engine,
}

impl SearchTester {
    fn new(engine_type: EngineType) -> Self {
        let engine = Engine::new(engine_type);
        Self { engine }
    }

    fn search_with_depth(&self, pos: &mut Position, depth: u8) -> Duration {
        let limits = SearchLimitsBuilder::default().depth(depth).build();
        let start = Instant::now();
        let result = self.engine.search(pos, limits);
        assert!(result.best_move.is_some());
        start.elapsed()
    }

    fn search_with_time(&self, pos: &mut Position, time_ms: u64) -> u64 {
        let limits = SearchLimitsBuilder::default().fixed_time_ms(time_ms).build();
        let result = self.engine.search(pos, limits);
        assert!(result.best_move.is_some());
        result.stats.nodes
    }
}

#[test]
fn test_search_response_time() {
    let tester = SearchTester::new(EngineType::Material);
    let mut pos = Position::startpos();

    // Search to depth 3 should complete quickly
    let elapsed = tester.search_with_depth(&mut pos, 3);

    assert!(elapsed < Duration::from_secs(1), "Depth 3 search took too long: {elapsed:?}");
}

#[test]
fn test_search_produces_output() {
    let tester = SearchTester::new(EngineType::Material);
    let mut pos = Position::startpos();

    // Fixed time search should complete within time limit
    let start = Instant::now();
    let nodes = tester.search_with_time(&mut pos, 100);
    let elapsed = start.elapsed();

    assert!(nodes > 0, "Search should visit nodes");
    assert!(
        elapsed < Duration::from_millis(150),
        "100ms search exceeded time limit: {elapsed:?}"
    );
}

#[test]
fn test_search_with_stop_flag() {
    let engine = Arc::new(Engine::new(EngineType::Material));
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Start search in thread
    let engine_clone = engine.clone();
    let stop_flag_clone = stop_flag.clone();
    let handle = thread::spawn(move || {
        let mut pos = Position::startpos();
        let limits = SearchLimitsBuilder::default()
            .depth(100) // Deep search
            .stop_flag(stop_flag_clone)
            .build();
        let start = Instant::now();
        let result = engine_clone.search(&mut pos, limits);
        (result.best_move.is_some(), start.elapsed())
    });

    // Stop after 50ms
    thread::sleep(Duration::from_millis(50));
    stop_flag.store(true, Ordering::Release);

    // Should stop quickly
    let (found_move, elapsed) = handle.join().unwrap();
    assert!(found_move);
    assert!(elapsed < Duration::from_millis(100), "Search didn't stop quickly: {elapsed:?}");
}

#[test]
fn test_depth_search_terminates() {
    let tester = SearchTester::new(EngineType::Material);
    let mut pos = Position::startpos();

    // Test depth 5 search (performance issue should be fixed)
    let elapsed = tester.search_with_depth(&mut pos, 5);

    // Debug builds are significantly slower
    let time_limit = if cfg!(debug_assertions) {
        Duration::from_secs(30)
    } else {
        Duration::from_secs(5)
    };
    assert!(elapsed < time_limit, "Depth 5 search took too long: {elapsed:?}");
    println!("Depth 5 search completed in {elapsed:?}");
}

#[test]
#[ignore] // This test requires large stack size
fn test_enhanced_search_performance() {
    let tester = SearchTester::new(EngineType::Enhanced);
    let mut pos = Position::startpos();

    // Enhanced should search deeper in same time
    let basic_tester = SearchTester::new(EngineType::Material);
    let mut pos2 = Position::startpos();

    let basic_nodes = basic_tester.search_with_time(&mut pos2, 100);
    let enhanced_nodes = tester.search_with_time(&mut pos, 100);

    println!("Basic nodes: {basic_nodes}, Enhanced nodes: {enhanced_nodes}");
    // Enhanced might visit fewer nodes due to pruning, but should be more efficient
}

#[test]
fn test_engine_type_switching() {
    let mut engine = Engine::new(EngineType::Material);
    let mut pos = Position::startpos();

    // Test Material engine
    assert_eq!(engine.get_engine_type(), EngineType::Material);
    let limits = SearchLimitsBuilder::default().depth(2).build();
    let result = engine.search(&mut pos, limits);
    assert!(result.best_move.is_some());

    // Switch to Enhanced
    engine.set_engine_type(EngineType::Enhanced);
    assert_eq!(engine.get_engine_type(), EngineType::Enhanced);
    let limits2 = SearchLimitsBuilder::default().depth(2).build();
    let result2 = engine.search(&mut pos, limits2);
    assert!(result2.best_move.is_some());
}

#[test]
fn test_various_positions() {
    let tester = SearchTester::new(EngineType::Material);

    // Test various positions
    let positions = [
        Position::startpos(),
        Position::from_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap(),
        Position::from_sfen(
            "8l/1l+R2P3/p2pBG1pp/kps1p4/Nn1P2G2/P1P1P2PP/1PS6/1KSG3+r1/LN2+p3L w Sbgn3p 124",
        )
        .unwrap(),
    ];

    for mut pos in positions {
        let elapsed = tester.search_with_depth(&mut pos, 3);
        // Debug builds are significantly slower
        let time_limit = if cfg!(debug_assertions) {
            Duration::from_secs(10)
        } else {
            Duration::from_secs(2)
        };
        assert!(elapsed < time_limit, "Search took too long for position");
    }
}

#[test]
fn test_concurrent_searches() {
    let engine = Arc::new(Engine::new(EngineType::Material));
    let mut handles = vec![];

    // Spawn multiple threads doing searches
    for i in 0..4 {
        let engine_clone = engine.clone();
        let handle = thread::spawn(move || {
            let mut pos = Position::startpos();
            let limits = SearchLimitsBuilder::default().depth(3).build();
            let start = Instant::now();
            let result = engine_clone.search(&mut pos, limits);
            let elapsed = start.elapsed();
            let nodes = result.stats.nodes;
            println!("Thread {i} completed in {elapsed:?} with {nodes} nodes");
            result.stats.nodes
        });
        handles.push(handle);
    }

    // All searches should complete
    let mut total_nodes = 0;
    for handle in handles {
        let nodes = handle.join().unwrap();
        total_nodes += nodes;
    }

    assert!(total_nodes > 0);
}
