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
