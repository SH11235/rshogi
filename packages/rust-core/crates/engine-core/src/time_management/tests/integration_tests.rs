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
        random_time_ms: None,
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
    assert!(checker.is_time_critical()); // Should be critical immediately

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
    assert!(checker.is_time_critical()); // Only 50ms left < 80ms critical threshold
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

#[test]
fn test_pv_stability_threshold_updates() {
    // Build a simple TimeManager
    let limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 1000 },
        ..Default::default()
    };
    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

    // Access state checker (test-only API)
    let checker = tm.state_checker();

    // Initially, with elapsed=0, PV is not stable (0 > threshold=false)
    assert!(!checker.is_pv_stable(0));

    // Simulate PV change at depth 10
    tm.on_pv_change(10);
    let params = crate::time_management::TimeParameters::default();
    let thr = params.pv_base_threshold_ms + (10u64 * params.pv_depth_slope_ms);

    // Before threshold elapsed, not stable
    assert!(!checker.is_pv_stable(thr));
    // After threshold elapsed, stable
    assert!(checker.is_pv_stable(thr + 1));
}

#[test]
fn test_should_stop_schedules_and_stops_at_search_end() {
    mock_set_time(0);

    // Use a time that allows proper rounding behavior
    let limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 2000 },
        ..Default::default()
    };
    let tm = TimeManager::new_with_mock_time(&limits, Color::Black, 0, GamePhase::Opening);

    // Make PV stable by waiting over base threshold
    tm.on_pv_change(0); // mark change at elapsed=0
    mock_advance_time(90); // base 80ms + margin

    // Get the opt limit (which triggers scheduling in Phase 4)
    let opt = tm.opt_limit_ms();

    // Advance to just past opt limit
    mock_advance_time(opt - 90 + 10); // Total elapsed = opt + 10

    // First should_stop schedules rounded stop (search_end), not immediate stop
    let stop_now = tm.should_stop(0);
    assert!(!stop_now, "should not stop immediately at opt_limit");

    let scheduled = tm.scheduled_end_ms();
    assert!(scheduled != u64::MAX, "scheduled_end must be set after opt_limit");

    // Before scheduled end, should_stop must remain false
    if tm.elapsed_ms() < scheduled {
        assert!(!tm.should_stop(0));
    }

    // Advance to scheduled end
    let current_elapsed = tm.elapsed_ms();
    if scheduled > current_elapsed {
        mock_advance_time(scheduled - current_elapsed);
    }

    assert!(tm.should_stop(0), "should stop at or after scheduled_end");
}
