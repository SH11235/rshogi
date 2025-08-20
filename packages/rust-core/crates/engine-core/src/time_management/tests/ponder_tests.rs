//! Ponder-specific tests

use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
use crate::Color;

use super::{mock_advance_time, mock_set_time};

#[test]
fn test_ponder_to_fischer() {
    mock_set_time(0);

    // Create pending limits with Fischer time control
    let pending_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    // Create TimeManager in ponder mode
    let tm = TimeManager::new_ponder(&pending_limits, Color::White, 0, GamePhase::Opening);

    // Verify it's pondering
    assert!(tm.is_pondering());
    assert!(!tm.should_stop(1000)); // Should not stop during ponder

    // Simulate 5 seconds of pondering
    mock_advance_time(5000);

    // Ponder hit
    tm.ponder_hit(None, 5000);

    // Verify ponder mode is off
    assert!(!tm.is_pondering());

    // Check time allocation is adjusted for spent time
    let info = tm.get_time_info();
    assert!(info.soft_limit_ms < 60000 - 5000); // Less than remaining time
    assert!(info.hard_limit_ms < 60000 - 5000);
}

#[test]
fn test_ponder_to_byoyomi() {
    mock_set_time(0);

    // Create pending limits with Byoyomi
    let pending_limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 10000, // 10 seconds main time
            byoyomi_ms: 30000,   // 30 seconds per period
            periods: 3,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_ponder(&pending_limits, Color::Black, 40, GamePhase::MiddleGame);
    assert!(tm.is_pondering());

    // Ponder for 3 seconds
    mock_advance_time(3000);
    tm.ponder_hit(None, 3000);

    assert!(!tm.is_pondering());

    // Should have conservative allocation from remaining main time
    let info = tm.get_time_info();
    assert!(info.soft_limit_ms > 0);
    assert!(info.soft_limit_ms < 10000 - 3000); // Less than remaining main time
}

#[test]
fn test_ponder_edge_cases() {
    mock_set_time(0);

    // Test 1: Ponder hit with spent > remain
    let pending_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 2000, // Only 2 seconds
            black_ms: 2000,
            increment_ms: 0,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_ponder(&pending_limits, Color::White, 80, GamePhase::EndGame);

    // Ponder for 5 seconds (more than available time)
    mock_advance_time(5000);
    tm.ponder_hit(None, 5000);

    // Should have minimal time allocation
    let info = tm.get_time_info();
    assert_eq!(info.soft_limit_ms, 100); // Minimum safety limit
    assert_eq!(info.hard_limit_ms, 200);

    // Test 2: Force stop during ponder
    let tm2 = TimeManager::new_ponder(&pending_limits, Color::Black, 0, GamePhase::Opening);
    assert!(!tm2.should_stop(1000)); // Normal check returns false

    tm2.force_stop();
    assert!(tm2.should_stop(1000)); // Force stop works even during ponder
}

#[test]
fn test_ponder_with_new_limits() {
    mock_set_time(0);

    // Initial pending limits
    let pending_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 50000,
            black_ms: 50000,
            increment_ms: 500,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_ponder(&pending_limits, Color::White, 20, GamePhase::MiddleGame);

    mock_advance_time(2000);

    // New limits provided at ponder hit (e.g., opponent's time updated)
    let new_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 48000, // Updated time
            black_ms: 45000,
            increment_ms: 500,
        },
        moves_to_go: Some(30), // Additional info
        ..Default::default()
    };

    tm.ponder_hit(Some(&new_limits), 2000);

    // Should use new limits for calculation
    let info = tm.get_time_info();
    assert!(info.soft_limit_ms > 0);
    assert!(info.soft_limit_ms < 48000 - 2000);
}

#[test]
fn test_active_time_control_switch() {
    mock_set_time(0);

    // Start with Fischer in pending
    let pending_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_ponder(&pending_limits, Color::White, 0, GamePhase::Opening);

    // Initially, active time control should be Ponder
    assert!(tm.is_pondering());
    // No byoyomi state since we're not in byoyomi mode
    assert!(tm.get_byoyomi_state().is_none());

    // After ponder hit, active time control should be Fischer
    tm.ponder_hit(None, 1000);
    assert!(!tm.is_pondering());

    // Still no byoyomi state (Fischer mode)
    assert!(tm.get_byoyomi_state().is_none());

    // Test with Byoyomi
    let byoyomi_limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 5000,
            byoyomi_ms: 10000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm2 = TimeManager::new_ponder(&byoyomi_limits, Color::Black, 0, GamePhase::MiddleGame);
    tm2.ponder_hit(None, 1000);

    // Now should have byoyomi state
    let state = tm2.get_byoyomi_state();
    assert!(state.is_some());
    let (periods, _, in_byoyomi) = state.unwrap();
    assert_eq!(periods, 3);
    assert!(!in_byoyomi); // Still in main time
}

#[test]
fn test_ponder_hit_concurrent_access() {
    use std::sync::Arc;
    use std::thread;

    mock_set_time(0);

    let pending_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm =
        Arc::new(TimeManager::new_ponder(&pending_limits, Color::White, 0, GamePhase::Opening));

    // Clone for threads
    let tm1 = Arc::clone(&tm);
    let tm2 = Arc::clone(&tm);

    // Thread 1: Continuously check should_stop
    let handle1 = thread::spawn(move || {
        let mut nodes = 0u64;
        for _ in 0..1000 {
            nodes += 1;
            let should_stop = tm1.should_stop(nodes);
            // During ponder, should not stop
            if tm1.is_pondering() {
                assert!(!should_stop);
            }
            // Yield to other threads to increase chance of race
            thread::yield_now();
        }
    });

    // Thread 2: Call ponder_hit after ensuring thread 1 has started
    let handle2 = thread::spawn(move || {
        // Yield multiple times to let thread 1 get going
        for _ in 0..10 {
            thread::yield_now();
        }
        tm2.ponder_hit(None, 1000);
    });

    // Wait for both threads
    handle1.join().unwrap();
    handle2.join().unwrap();

    // Verify final state
    assert!(!tm.is_pondering());
}

#[test]
fn test_fischer_to_byoyomi_switch() {
    mock_set_time(0);

    // Start with Fischer in pending
    let fischer_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_ponder(&fischer_limits, Color::White, 0, GamePhase::Opening);

    // Provide new Byoyomi limits at ponder hit
    let byoyomi_limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 10000,
            byoyomi_ms: 5000,
            periods: 5,
        },
        ..Default::default()
    };

    tm.ponder_hit(Some(&byoyomi_limits), 2000);

    // Should now have byoyomi state
    let state = tm.get_byoyomi_state();
    assert!(state.is_some());
    let (periods, period_ms, in_byoyomi) = state.unwrap();
    assert_eq!(periods, 5);
    assert_eq!(period_ms, 5000);
    assert!(!in_byoyomi);
}

#[test]
fn test_elapsed_time_after_ponder_hit() {
    mock_set_time(0);

    let pending_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_ponder(&pending_limits, Color::White, 0, GamePhase::Opening);

    // Ponder for 5 seconds
    mock_advance_time(5000);
    let elapsed_before = tm.elapsed_ms();
    assert!((4999..=5001).contains(&elapsed_before)); // Allow small variance

    // After ponder hit, elapsed should reset
    tm.ponder_hit(None, 5000);
    let elapsed_after = tm.elapsed_ms();
    assert!(elapsed_after <= 1); // Should be reset (allow 1ms for execution time)

    // Advance more time
    mock_advance_time(2000);
    let elapsed_final = tm.elapsed_ms();
    assert!((1999..=2001).contains(&elapsed_final)); // Should count from ponder_hit
}

#[test]
fn test_ponder_hit_clears_ponder_flag_and_sets_control() {
    mock_set_time(0);

    // Fischer inner control, 10 s each side
    let inner_tc = TimeControl::Fischer {
        black_ms: 10_000,
        white_ms: 10_000,
        increment_ms: 0,
    };
    let limits = TimeLimits {
        time_control: inner_tc.clone(), // Pass the regular time control, not wrapped in Ponder
        ..Default::default()
    };
    let tm = TimeManager::new_ponder(&limits, Color::Black, 0, GamePhase::Opening);

    assert!(tm.is_pondering());

    // Check initial active time control
    let initial_active = tm.time_control();
    eprintln!("Initial active time control: {initial_active:?}");
    assert!(matches!(initial_active, TimeControl::Ponder(_)));

    tm.ponder_hit(None, /* elapsed */ 500);

    assert!(!tm.is_pondering());
    let active = tm.time_control();
    eprintln!("Active time control after ponder_hit: {active:?}");
    assert!(matches!(active, TimeControl::Fischer { .. }));
}
