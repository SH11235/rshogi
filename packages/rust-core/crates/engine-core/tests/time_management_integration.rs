//! Integration tests for time management with search

use engine_core::{
    engine::controller::{Engine, EngineType},
    search::{search_basic::SearchLimits as CoreSearchLimits, GamePhase, SEARCH_INF},
    shogi::Position,
    time_management::{SearchLimits, TimeControl, TimeState},
    Color,
};
use std::time::{Duration, Instant};

// Initial position SFEN
const INITIAL_SFEN: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

/// Test that search respects fixed time limits
#[test]
fn test_search_respects_fixed_time() {
    let engine = Engine::new(EngineType::Material);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();

    // Search with 100ms time limit
    let limits = CoreSearchLimits {
        time: Some(Duration::from_millis(100)),
        depth: 10, // Max depth
        nodes: None,
        stop_flag: None,
        info_callback: None,
    };

    let start = Instant::now();
    let result = engine.search(&mut position, limits);
    let elapsed = start.elapsed();

    // Should complete within reasonable time (allow 50ms buffer for overhead)
    assert!(
        elapsed.as_millis() <= 150,
        "Search took {}ms, expected <= 150ms",
        elapsed.as_millis()
    );

    // Should return a valid move
    assert!(result.best_move.is_some());
    assert!(result.score != 0); // Should have evaluated
}

/// Test that search respects node limits
#[test]
fn test_search_respects_node_limit() {
    let engine = Engine::new(EngineType::Material);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();

    // Search with node limit
    let limits = CoreSearchLimits {
        time: None,
        depth: 10, // Max depth
        nodes: Some(10000),
        stop_flag: None,
        info_callback: None,
    };

    let result = engine.search(&mut position, limits);

    // Should return a valid move
    assert!(result.best_move.is_some());
    // Score should be meaningful (not 0) since we use stand-pat evaluation
    assert!(result.score != 0, "Score: {}, Nodes: {}", result.score, result.stats.nodes);

    // Nodes searched should be close to limit (allow some overhead)
    assert!(
        result.stats.nodes <= 11000,
        "Searched {} nodes, expected <= 11000",
        result.stats.nodes
    );
}

/// Test search with byoyomi time control
#[test]
fn test_search_with_byoyomi() {
    // This test verifies that search can handle byoyomi time control
    let limits = SearchLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 500,
            periods: 1,
        },
        moves_to_go: None,
        depth: None,
        nodes: None,
        time_parameters: None,
    };

    // Create time manager for testing
    use engine_core::time_management::TimeManager;
    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);

    // Should allocate reasonable time from byoyomi
    let info = tm.get_time_info();
    assert!(info.soft_limit_ms > 0);
    assert!(info.soft_limit_ms < 500); // Should not use full byoyomi time

    // Simulate using most of the period
    tm.update_after_move(450, TimeState::Byoyomi { main_left_ms: 0 });

    // Should have less time available now
    let _info = tm.get_time_info();
    let state = tm.get_byoyomi_state().unwrap();
    assert_eq!(state.0, 1); // Should still have 1 period (450ms < 500ms)
    assert_eq!(state.1, 50); // 50ms left in current period
}

/// Test search with depth limit
#[test]
fn test_search_with_depth_limit() {
    let engine = Engine::new(EngineType::Material);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();

    // Search with depth limit
    let limits = CoreSearchLimits {
        time: None,
        depth: 5,
        nodes: None,
        stop_flag: None,
        info_callback: None,
    };

    let result = engine.search(&mut position, limits);

    // Should have search result
    assert!(result.best_move.is_some());
    assert!(result.score != 0); // Should have evaluated
                                // PV should not exceed depth limit
    assert!(result.stats.pv.len() <= 5);
}

/// Test time allocation for different game phases
#[test]
fn test_time_allocation_by_phase() {
    use engine_core::time_management::{calculate_time_allocation, TimeParameters};

    let time_control = TimeControl::Fischer {
        white_ms: 60000,
        black_ms: 60000,
        increment_ms: 1000,
    };

    let params = TimeParameters::default();

    // Opening phase gets more time
    let (soft_opening, _) = calculate_time_allocation(
        &time_control,
        Color::White,
        10,
        None,
        GamePhase::Opening,
        &params,
    );

    // Middle game standard time
    let (soft_middle, _) = calculate_time_allocation(
        &time_control,
        Color::White,
        40,
        None,
        GamePhase::MiddleGame,
        &params,
    );

    // Endgame gets less time
    let (soft_endgame, _) = calculate_time_allocation(
        &time_control,
        Color::White,
        80,
        None,
        GamePhase::EndGame,
        &params,
    );

    // Verify phase-based allocation
    // Opening gets 1.2x, middle gets 1.0x, endgame gets 0.8x
    // But move estimation also affects allocation
    // - Opening (ply 10): 60 moves expected
    // - Middle (ply 40): 40 moves expected
    // - Endgame (ply 80): 20 moves expected
    // So the base time allocation differs significantly

    // Opening should generally get more absolute time
    assert!(soft_opening > soft_endgame, "Opening should get more time than endgame");

    // The phase factors are applied, but move estimation dominates
    // Just verify they're all reasonable
    assert!(soft_opening > 500, "Opening allocation too low");
    assert!(soft_middle > 500, "Middle game allocation too low");
    assert!(soft_endgame > 300, "Endgame allocation too low");
}

/// Test emergency time management
#[test]
fn test_emergency_time_management() {
    // Test with critically low Fischer time
    let limits = SearchLimits {
        time_control: TimeControl::Fischer {
            white_ms: 200, // Critical threshold
            black_ms: 200,
            increment_ms: 0,
        },
        moves_to_go: None,
        depth: None,
        nodes: None,
        time_parameters: None,
    };

    use engine_core::time_management::TimeManager;
    let tm = TimeManager::new(&limits, Color::White, 100, GamePhase::EndGame);

    // Should allocate minimal time
    let info = tm.get_time_info();
    assert!(info.soft_limit_ms <= 50, "Emergency allocation should be minimal");
    assert!(info.hard_limit_ms <= 100, "Emergency hard limit should be small");
}

/// Test that enhanced search respects fixed time limits
#[test]
fn test_enhanced_search_respects_fixed_time() {
    let engine = Engine::new(EngineType::Enhanced);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();

    // Search with 100ms time limit
    let limits = CoreSearchLimits {
        time: Some(Duration::from_millis(100)),
        depth: 10, // Max depth
        nodes: None,
        stop_flag: None,
        info_callback: None,
    };

    let start = Instant::now();
    let result = engine.search(&mut position, limits);
    let elapsed = start.elapsed();

    // Should complete within reasonable time (allow 50ms buffer for overhead)
    assert!(
        elapsed.as_millis() <= 150,
        "Enhanced search took {}ms, expected <= 150ms",
        elapsed.as_millis()
    );

    // Should return a valid move
    assert!(result.best_move.is_some());
    assert!(result.score != 0); // Should have evaluated
}

/// Test that enhanced search respects node limits
#[test]
fn test_enhanced_search_respects_node_limit() {
    let engine = Engine::new(EngineType::Enhanced);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();

    // Search with node limit
    let limits = CoreSearchLimits {
        time: None,
        depth: 10, // Max depth
        nodes: Some(10000),
        stop_flag: None,
        info_callback: None,
    };

    let result = engine.search(&mut position, limits);

    // Should return a valid move
    assert!(result.best_move.is_some());
    // Score should be meaningful (not 0)
    assert!(
        result.score != 0,
        "Enhanced score: {}, Nodes: {}",
        result.score,
        result.stats.nodes
    );

    // Nodes searched should be close to limit (allow some overhead)
    assert!(
        result.stats.nodes <= 11000,
        "Enhanced searched {} nodes, expected <= 11000",
        result.stats.nodes
    );
}

/// Test enhanced search with depth limit
#[test]
fn test_enhanced_search_with_depth_limit() {
    let engine = Engine::new(EngineType::Enhanced);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();

    // Search with depth limit
    let limits = CoreSearchLimits {
        time: None,
        depth: 5,
        nodes: None,
        stop_flag: None,
        info_callback: None,
    };

    let result = engine.search(&mut position, limits);

    // Should have search result
    assert!(result.best_move.is_some());
    assert!(result.score != 0); // Should have evaluated
                                // PV should not exceed depth limit
    assert!(result.stats.pv.len() <= 5);
}

/// Test enhanced search with stop flag
#[test]
fn test_enhanced_search_with_stop_flag() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    use std::thread;

    let engine = Engine::new(EngineType::Enhanced);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();
    let stop_flag = Arc::new(AtomicBool::new(false));

    let limits = CoreSearchLimits {
        time: None,
        depth: 10, // Deep search
        nodes: None,
        stop_flag: Some(stop_flag.clone()),
        info_callback: None,
    };

    // Set stop flag after short delay
    let stop_flag_clone = stop_flag.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));
        stop_flag_clone.store(true, std::sync::atomic::Ordering::Release);
    });

    let start = Instant::now();
    let result = engine.search(&mut position, limits);
    let elapsed = start.elapsed();

    // Should find a move (even if search was stopped)
    assert!(result.best_move.is_some());

    // Should have stopped quickly
    assert!(elapsed < Duration::from_secs(1));

    // Should have searched relatively few nodes
    assert!(result.stats.nodes < 1_000_000);
}

/// Test enhanced search fallback behavior (immediate stop)
#[test]
fn test_enhanced_search_fallback_move_quality() {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let engine = Engine::new(EngineType::Enhanced);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();
    let stop_flag = Arc::new(AtomicBool::new(false));

    let limits = CoreSearchLimits {
        time: None,
        depth: 5,
        nodes: None,
        stop_flag: Some(stop_flag.clone()),
        info_callback: None,
    };

    // Set stop flag immediately to force fallback
    stop_flag.store(true, std::sync::atomic::Ordering::Release);

    let result = engine.search(&mut position, limits);

    // Should find a move even when stopped immediately
    assert!(result.best_move.is_some());

    // Score should be reasonable (not just 0 or -INFINITY)
    assert!(result.score > -SEARCH_INF);
    assert!(result.score < SEARCH_INF);

    // Should have evaluated at least some positions
    assert!(result.stats.nodes >= 1, "Enhanced should have evaluated at least one position");
}

/// Test enhanced search time allocation by phase
#[test]
fn test_enhanced_search_time_allocation() {
    let engine = Engine::new(EngineType::Enhanced);
    let mut position = Position::from_sfen(INITIAL_SFEN).unwrap();

    // Test that search allocates reasonable time
    let limits = CoreSearchLimits {
        time: Some(Duration::from_millis(500)),
        depth: 10,
        nodes: None,
        stop_flag: None,
        info_callback: None,
    };

    let start = Instant::now();
    let result = engine.search(&mut position, limits);
    let elapsed = start.elapsed();

    // Should use reasonable portion of available time
    assert!(elapsed.as_millis() >= 50, "Should use at least some time");
    assert!(elapsed.as_millis() <= 550, "Should not exceed time limit by much");

    // Should return meaningful result
    assert!(result.best_move.is_some());
    assert!(result.score != 0);
}

/// Test time management with moves_to_go
#[test]
fn test_moves_to_go_allocation() {
    let limits = SearchLimits {
        time_control: TimeControl::Fischer {
            white_ms: 30000,
            black_ms: 30000,
            increment_ms: 0,
        },
        moves_to_go: Some(10), // 10 moves until time control
        depth: None,
        nodes: None,
        time_parameters: None,
    };

    use engine_core::time_management::TimeManager;
    let tm = TimeManager::new(&limits, Color::Black, 50, GamePhase::MiddleGame);

    let info = tm.get_time_info();

    // Should allocate roughly 1/10 of remaining time
    assert!(info.soft_limit_ms > 2000);
    assert!(info.soft_limit_ms < 4000);
}
