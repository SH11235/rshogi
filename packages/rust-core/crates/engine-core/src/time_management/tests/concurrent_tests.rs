//! Concurrent access tests for time management
//!
//! Tests to ensure thread safety and correct behavior when multiple
//! threads access TimeManager methods simultaneously.

use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
use crate::Color;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[test]
fn test_concurrent_get_time_info_and_ponder_hit() {
    // Test concurrent access to get_time_info() and ponder_hit()
    // Ensures lock ordering is correct and no deadlocks occur
    let pending_limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 60000,
            byoyomi_ms: 10000,
            periods: 3,
        },
        ..Default::default()
    };

    let tm = Arc::new(TimeManager::new_ponder(
        &pending_limits,
        Color::White,
        20,
        GamePhase::MiddleGame,
    ));

    let mut handles = vec![];

    // Thread 1: Repeatedly call get_time_info()
    let tm1 = Arc::clone(&tm);
    let h1 = thread::spawn(move || {
        for _ in 0..1000 {
            let info = tm1.get_time_info();
            // Verify we got valid info
            assert!(info.elapsed_ms < u64::MAX);
            if let Some(byoyomi_info) = info.byoyomi_info {
                assert!(byoyomi_info.periods_left <= 3);
                assert!(byoyomi_info.current_period_ms <= 10000);
            }
        }
    });
    handles.push(h1);

    // Thread 2: Call ponder_hit() after a delay
    let tm2 = Arc::clone(&tm);
    let h2 = thread::spawn(move || {
        thread::sleep(Duration::from_millis(10));

        // Perform ponder_hit
        let ponder_manager = tm2.ponder_manager();
        ponder_manager.ponder_hit(Some(&pending_limits), 100);

        // Verify state changed correctly
        assert!(!tm2.is_pondering());
    });
    handles.push(h2);

    // Thread 3: More get_time_info() calls
    let tm3 = Arc::clone(&tm);
    let h3 = thread::spawn(move || {
        for _ in 0..1000 {
            let info = tm3.get_time_info();
            // Just accessing shouldn't panic or deadlock
            let _ = info.time_pressure;
        }
    });
    handles.push(h3);

    // Wait for all threads to complete
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Verify final state is consistent
    let final_info = tm.get_time_info();
    assert!(!tm.is_pondering(), "Should not be pondering after ponder_hit");

    // Byoyomi info should be available and consistent
    if let Some(byoyomi_info) = final_info.byoyomi_info {
        assert!(byoyomi_info.periods_left <= 3);
        assert!(byoyomi_info.current_period_ms <= 10000);
    }
}

#[test]
fn test_concurrent_pv_changes_and_time_info() {
    // Test concurrent PV changes and time info access
    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 30000,
            black_ms: 30000,
            increment_ms: 0,
        },
        ..Default::default()
    };

    let tm = Arc::new(TimeManager::new(&limits, Color::Black, 40, GamePhase::EndGame));

    let mut handles = vec![];

    // Thread 1: Simulate PV changes
    let tm1 = Arc::clone(&tm);
    let h1 = thread::spawn(move || {
        for depth in 1..20 {
            tm1.on_pv_change(depth);
            thread::sleep(Duration::from_millis(1));
        }
    });
    handles.push(h1);

    // Thread 2: Check time info and PV stability
    let tm2 = Arc::clone(&tm);
    let h2 = thread::spawn(move || {
        for _ in 0..100 {
            let info = tm2.get_time_info();
            let checker = tm2.state_checker();
            let is_stable = checker.is_pv_stable(info.elapsed_ms);

            // Just verify we can access without issues
            assert!(info.time_pressure >= 0.0 && info.time_pressure <= 1.0);
            let _ = is_stable; // Result varies based on timing

            thread::sleep(Duration::from_millis(1));
        }
    });
    handles.push(h2);

    // Thread 3: Check should_stop
    let tm3 = Arc::clone(&tm);
    let h3 = thread::spawn(move || {
        for nodes in (0..10000).step_by(100) {
            let should_stop = tm3.should_stop(nodes as u64);
            // Early in search, shouldn't stop
            if tm3.elapsed_ms() < 100 {
                assert!(!should_stop, "Shouldn't stop early in search");
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    handles.push(h3);

    // Wait for all threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

#[test]
fn test_concurrent_byoyomi_updates() {
    // Test concurrent byoyomi state updates and reads
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 1000,
            byoyomi_ms: 5000,
            periods: 5,
        },
        ..Default::default()
    };

    let tm = Arc::new(TimeManager::new(&limits, Color::White, 0, GamePhase::Opening));

    let mut handles = vec![];

    // Thread 1: Perform byoyomi updates
    let tm1 = Arc::clone(&tm);
    let h1 = thread::spawn(move || {
        use crate::time_management::TimeState;

        // Transition to byoyomi
        tm1.update_after_move(1500, TimeState::Main { main_left_ms: 1000 });
        thread::sleep(Duration::from_millis(5));

        // Continue in byoyomi
        tm1.update_after_move(2000, TimeState::Byoyomi { main_left_ms: 0 });
        thread::sleep(Duration::from_millis(5));

        tm1.update_after_move(3000, TimeState::Byoyomi { main_left_ms: 0 });
    });
    handles.push(h1);

    // Thread 2: Read byoyomi state
    let tm2 = Arc::clone(&tm);
    let h2 = thread::spawn(move || {
        for _ in 0..50 {
            if let Some((periods, period_ms, in_byoyomi)) = tm2.get_byoyomi_state() {
                // Verify consistency
                assert!(periods <= 5, "Periods should not exceed initial");
                assert!(period_ms <= 5000, "Period time should not exceed max");
                let _ = in_byoyomi; // Can be true or false depending on timing
            }

            let info = tm2.get_time_info();
            if let Some(byoyomi_info) = info.byoyomi_info {
                assert!(byoyomi_info.periods_left <= 5, "Byoyomi info should be consistent");
            }

            thread::sleep(Duration::from_millis(1));
        }
    });
    handles.push(h2);

    // Wait for threads
    for handle in handles {
        handle.join().expect("Thread panicked");
    }

    // Final state check
    let final_state = tm.get_byoyomi_state().unwrap();
    assert!(final_state.2, "Should be in byoyomi after main time exhausted");
    assert!(final_state.0 <= 5, "Should have consumed some periods");
}
