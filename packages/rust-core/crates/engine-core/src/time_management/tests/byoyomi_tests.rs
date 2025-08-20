//! Byoyomi-specific tests

use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager, TimeState};
use crate::Color;

#[test]
fn test_byoyomi_exact_boundary() {
    // Test exact boundary condition: time_spent == byoyomi_ms
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 1000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

    // Spend exactly one period
    tm.update_after_move(1000, TimeState::Byoyomi { main_left_ms: 0 });
    let state = tm.get_byoyomi_state().unwrap();
    assert_eq!(state.0, 2); // Should have 2 periods left
    assert_eq!(state.1, 1000); // Should reset to full period
    assert!(state.2); // Should be in byoyomi
}

#[test]
fn test_byoyomi_multiple_period_consumption() {
    // Test consuming multiple periods in one move
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 1000,
            periods: 5,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // Spend 2.5 periods worth of time
    tm.update_after_move(2500, TimeState::Byoyomi { main_left_ms: 0 });
    let state = tm.get_byoyomi_state().unwrap();
    assert_eq!(state.0, 3); // Should have consumed 2 periods, 3 left
    assert_eq!(state.1, 500); // 500ms left in current period
}

#[test]
fn test_byoyomi_main_time_transition() {
    // Test transition from main time to byoyomi
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 5000,
            byoyomi_ms: 1000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

    // Not in byoyomi initially
    let state = tm.get_byoyomi_state().unwrap();
    assert!(!state.2); // Should not be in byoyomi

    // Transition to byoyomi when main time runs out
    tm.update_after_move(3000, TimeState::Main { main_left_ms: 2000 }); // 2s left, spent 3s
    let state = tm.get_byoyomi_state().unwrap();
    assert!(state.2); // Should now be in byoyomi
    assert_eq!(state.0, 2); // Overspent by 1s, so consumed 1 period
    assert_eq!(state.1, 1000); // Full period remains after consuming the overspent period

    // Another move that consumes a period
    tm.update_after_move(1500, TimeState::Byoyomi { main_left_ms: 0 });
    let state = tm.get_byoyomi_state().unwrap();
    assert_eq!(state.0, 1); // Should have 1 period left (consumed 1 from the 2 remaining)
    assert_eq!(state.1, 500); // 500ms left in current period
}

#[test]
fn test_byoyomi_time_forfeit() {
    // Test time forfeit when all periods consumed
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 1000,
            periods: 2,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // Consume all periods
    tm.update_after_move(2000, TimeState::Byoyomi { main_left_ms: 0 }); // Consume both periods

    // Should trigger stop flag
    assert!(tm.should_stop(0));

    let state = tm.get_byoyomi_state().unwrap();
    assert_eq!(state.0, 0); // No periods left
    assert_eq!(state.1, 0); // No time left
}

#[test]
#[cfg(not(debug_assertions))]
fn test_byoyomi_transition_with_wrong_state() {
    // Test that using wrong TimeState doesn't cause transition
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 5000,
            byoyomi_ms: 1000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

    // Using NonByoyomi state with Byoyomi time control
    // This should not cause transition to byoyomi
    tm.update_after_move(2000, TimeState::NonByoyomi);
    tm.update_after_move(2000, TimeState::NonByoyomi);
    tm.update_after_move(2000, TimeState::NonByoyomi); // Total 6s spent

    let state = tm.get_byoyomi_state().unwrap();
    assert!(!state.2); // Still not in byoyomi - wrong TimeState prevents transition
}

#[test]
fn test_byoyomi_proper_transition_with_new_api() {
    // Test proper transition with new API
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 5000,
            byoyomi_ms: 1000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

    // First move: still in main time
    tm.update_after_move(2000, TimeState::Main { main_left_ms: 3000 });
    let state = tm.get_byoyomi_state().unwrap();
    assert!(!state.2); // Still in main time

    // Second move: transition to byoyomi
    tm.update_after_move(3500, TimeState::Main { main_left_ms: 3000 });

    let state = tm.get_byoyomi_state().unwrap();
    assert!(state.2); // Now in byoyomi
    assert_eq!(state.0, 3); // All periods still available
    assert_eq!(state.1, 500); // 500ms left in first period (overspent by 500ms)
}

#[test]
fn test_byoyomi_transition_with_multiple_period_overspend() {
    // Test transition from main time to byoyomi with overspend > 2 periods
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 1000,
            byoyomi_ms: 1000,
            periods: 5,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::EndGame);

    // Overspend main time by 2.5 periods worth
    tm.update_after_move(3500, TimeState::Main { main_left_ms: 1000 });

    let state = tm.get_byoyomi_state().unwrap();
    assert!(state.2); // In byoyomi
    assert_eq!(state.0, 3); // Should have consumed 2 periods (2500ms), 3 left
    assert_eq!(state.1, 500); // 500ms left in current period
}

#[test]
fn test_continuous_byoyomi_to_time_forfeit() {
    // Test main=0 start → consume all periods → time forfeit
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0, // Start in byoyomi
            byoyomi_ms: 1000,
            periods: 2,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::EndGame);

    // Consume first period
    tm.update_after_move(1200, TimeState::Byoyomi { main_left_ms: 0 });
    assert!(!tm.should_stop(0));

    let state = tm.get_byoyomi_state().unwrap();
    assert_eq!(state.0, 1); // One period left
    assert_eq!(state.1, 800); // 800ms left in current period

    // Consume second period - time forfeit
    tm.update_after_move(1500, TimeState::Byoyomi { main_left_ms: 0 });
    assert!(tm.should_stop(0)); // Time forfeit

    let state = tm.get_byoyomi_state().unwrap();
    assert_eq!(state.0, 0); // No periods left
    assert_eq!(state.1, 0); // No time left
}

#[test]
#[cfg(debug_assertions)]
#[should_panic(expected = "TimeState::NonByoyomi used with Byoyomi")]
fn test_debug_assertion_on_wrong_time_state() {
    // Test debug assertion fires on API misuse
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 5000,
            byoyomi_ms: 1000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);
    tm.update_after_move(1000, TimeState::NonByoyomi); // Wrong state!
}

#[test]
#[cfg(not(debug_assertions))]
fn test_byoyomi_gui_misuse_tolerance() {
    // Test that system handles GUI sending wrong TimeState gracefully
    // In production, debug_assert is disabled so we need to handle this case
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 30000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 40, GamePhase::MiddleGame);

    // Initial state check
    let (periods_before, period_ms_before, in_byoyomi_before) = tm.get_byoyomi_state().unwrap();
    assert_eq!(periods_before, 3);
    assert_eq!(period_ms_before, 30000);
    assert!(in_byoyomi_before);

    // GUI mistakenly sends NonByoyomi state for a Byoyomi time control
    // This should be silently ignored (not panic or corrupt state)
    tm.update_after_move(1000, TimeState::NonByoyomi);

    // Verify byoyomi state is unchanged
    let (periods_after, period_ms_after, in_byoyomi_after) = tm.get_byoyomi_state().unwrap();
    assert_eq!(periods_after, periods_before, "Periods should remain unchanged");
    assert_eq!(period_ms_after, period_ms_before, "Period time should remain unchanged");
    assert_eq!(in_byoyomi_after, in_byoyomi_before, "Byoyomi status should remain unchanged");
}

#[test]
#[cfg(not(debug_assertions))]
fn test_byoyomi_repeated_gui_errors() {
    // Test that repeated GUI errors don't accumulate problems
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 60000,
            byoyomi_ms: 10000,
            periods: 5,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);

    // Verify initial state
    let (initial_periods, initial_period_ms, initial_in_byoyomi) = tm.get_byoyomi_state().unwrap();
    assert_eq!(initial_periods, 5);
    assert_eq!(initial_period_ms, 10000);
    assert!(!initial_in_byoyomi);

    // Multiple incorrect state updates
    for _ in 0..10 {
        tm.update_after_move(100, TimeState::NonByoyomi);
    }

    // State should be unchanged after errors
    let (periods, period_ms, in_byoyomi) = tm.get_byoyomi_state().unwrap();
    assert_eq!(periods, initial_periods);
    assert_eq!(period_ms, initial_period_ms);
    assert_eq!(in_byoyomi, initial_in_byoyomi);

    // Then a correct update
    tm.update_after_move(
        5000,
        TimeState::Main {
            main_left_ms: 55000,
        },
    );

    // System should still work correctly
    let (periods_after, period_ms_after, in_byoyomi_after) = tm.get_byoyomi_state().unwrap();
    assert_eq!(periods_after, 5, "Periods should be unchanged");
    assert_eq!(period_ms_after, 10000, "Period time should be unchanged");
    assert!(!in_byoyomi_after, "Should not be in byoyomi yet");
}

#[test]
#[cfg(not(debug_assertions))]
fn test_byoyomi_mixed_correct_incorrect_updates() {
    // Test mixing correct and incorrect updates
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 5000,
            byoyomi_ms: 1000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // Correct update
    tm.update_after_move(2000, TimeState::Main { main_left_ms: 3000 });

    // Incorrect update (should be ignored)
    tm.update_after_move(1000, TimeState::NonByoyomi);

    // Another correct update that transitions to byoyomi
    tm.update_after_move(4000, TimeState::Main { main_left_ms: 3000 });

    // Verify we're in byoyomi with correct state
    let (periods, period_ms, in_byoyomi) = tm.get_byoyomi_state().unwrap();
    assert!(in_byoyomi, "Should be in byoyomi after spending all main time");
    assert_eq!(periods, 3, "All periods should be available");
    assert_eq!(
        period_ms, 1000,
        "After consuming exactly one period, current period resets to full"
    );
}
