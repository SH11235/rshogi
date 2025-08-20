//! Integration tests for TimeManager

use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager, TimeState};
use crate::Color;

use super::{mock_advance_time, mock_set_time};

fn create_test_limits() -> TimeLimits {
    TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        moves_to_go: None,
        depth: None,
        nodes: None,
        time_parameters: None,
    }
}

#[test]
fn test_time_manager_creation() {
    let limits = create_test_limits();
    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);
    let info = tm.get_time_info();

    assert_eq!(info.elapsed_ms, 0);
    assert!(info.soft_limit_ms > 0);
    assert!(info.hard_limit_ms > info.soft_limit_ms);
}

#[test]
fn test_force_stop() {
    let limits = create_test_limits();
    let tm = TimeManager::new(&limits, Color::Black, 20, GamePhase::MiddleGame);

    assert!(!tm.should_stop(0));
    tm.force_stop();
    assert!(tm.should_stop(0));
}

#[test]
fn test_pv_stability_with_mock_time() {
    // Test PV stability tracking with MockClock
    mock_set_time(0);

    let limits = create_test_limits();
    let tm = TimeManager::new_with_mock_time(&limits, Color::White, 0, GamePhase::Opening);

    // Initial PV change
    tm.on_pv_change(10);

    // Check if PV is stable using state_checker
    let checker = tm.state_checker();
    assert!(!checker.is_pv_stable(tm.elapsed_ms())); // Just changed

    // Advance time but not enough for stability
    mock_advance_time(50);
    assert!(!checker.is_pv_stable(tm.elapsed_ms())); // 50ms < 80ms base + 10*5ms = 130ms

    // Advance past threshold
    mock_advance_time(100); // Total 150ms
    assert!(checker.is_pv_stable(tm.elapsed_ms())); // 150ms > 130ms

    // Another PV change at deeper depth
    tm.on_pv_change(20);
    assert!(!checker.is_pv_stable(tm.elapsed_ms())); // Just changed again

    // Deeper searches require more stability
    mock_advance_time(150); // Need 80 + 20*5 = 180ms
    assert!(!checker.is_pv_stable(tm.elapsed_ms())); // 150ms < 180ms

    mock_advance_time(50); // Total 200ms
    assert!(checker.is_pv_stable(tm.elapsed_ms())); // 200ms > 180ms
}

#[test]
fn test_time_pressure_calculation() {
    mock_set_time(0);

    let limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 1000 },
        ..Default::default()
    };

    let tm = TimeManager::new_with_mock_time(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // Initially no pressure
    let info = tm.get_time_info();
    assert!(info.time_pressure < 0.1);

    // Half way through
    mock_advance_time(500);
    let info = tm.get_time_info();
    assert!((info.time_pressure - 0.5).abs() < 0.1);

    // Near hard limit
    mock_advance_time(450); // Total 950ms
    let info = tm.get_time_info();
    assert!(info.time_pressure > 0.9);
}

#[test]
fn test_emergency_stop_with_mock_time() {
    mock_set_time(0);

    // Test Fischer emergency stop
    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 200, // Critical time
            black_ms: 200,
            increment_ms: 0,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_with_mock_time(&limits, Color::White, 100, GamePhase::EndGame);
    let checker = tm.state_checker();
    assert!(checker.is_time_critical(tm.elapsed_ms())); // Should be critical immediately

    // Test Byoyomi emergency stop
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 1000,
            periods: 1,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_with_mock_time(&limits, Color::Black, 80, GamePhase::EndGame);

    // Use most of the period
    tm.update_after_move(950, TimeState::Byoyomi { main_left_ms: 0 });
    let checker = tm.state_checker();
    assert!(checker.is_time_critical(tm.elapsed_ms())); // Only 50ms left < 80ms critical threshold
}

#[test]
fn test_soft_limit_extension() {
    mock_set_time(0);

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_with_mock_time(&limits, Color::White, 20, GamePhase::MiddleGame);
    let info = tm.get_time_info();
    let soft_limit = info.soft_limit_ms;

    // Advance to soft limit
    mock_advance_time(soft_limit);

    // With unstable PV, should not stop
    tm.on_pv_change(15);
    assert!(!tm.should_stop(1000));

    // Advance past soft limit but PV still unstable
    mock_advance_time(50);
    assert!(!tm.should_stop(2000)); // Should continue searching

    // Make PV stable
    mock_advance_time(200); // Well past stability threshold
    assert!(tm.should_stop(3000)); // Now should stop
}

#[test]
fn test_new_api_with_various_time_controls() {
    // Test that new API works correctly with non-byoyomi time controls
    let fischer_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&fischer_limits, Color::White, 0, GamePhase::Opening);
    tm.update_after_move(2000, TimeState::NonByoyomi); // Should work fine

    let fixed_limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 1000 },
        ..Default::default()
    };

    let tm2 = TimeManager::new(&fixed_limits, Color::Black, 0, GamePhase::MiddleGame);
    tm2.update_after_move(500, TimeState::NonByoyomi); // Should work fine
}
