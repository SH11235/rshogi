//! Concurrency tests for time management using loom

#[cfg(all(feature = "loom", not(target_arch = "wasm32")))]
mod loom_tests {
    use engine_core::search::GamePhase;
    use engine_core::time_management::{TimeControl, TimeLimits, TimeManager, TimeState};
    use engine_core::Color;
    use loom::sync::Arc;
    use loom::thread;

    fn create_test_manager() -> Arc<TimeManager> {
        let limits = TimeLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };
        Arc::new(TimeManager::new(&limits, Color::White, 0, GamePhase::Opening))
    }

    #[test]
    fn test_concurrent_should_stop() {
        loom::model(|| {
            let tm = create_test_manager();
            let tm1 = tm.clone();
            let tm2 = tm.clone();

            let t1 = thread::spawn(move || {
                // Thread 1: Continuously check should_stop
                for i in 0..100 {
                    tm1.should_stop(i);
                }
            });

            let t2 = thread::spawn(move || {
                // Thread 2: Force stop after some iterations
                thread::yield_now();
                tm2.force_stop();
            });

            t1.join().unwrap();
            t2.join().unwrap();

            // Verify stop flag is set
            assert!(tm.should_stop(0));
        });
    }

    #[test]
    fn test_concurrent_pv_changes() {
        loom::model(|| {
            let tm = create_test_manager();
            let tm1 = tm.clone();
            let tm2 = tm.clone();
            let tm3 = tm.clone();

            let t1 = thread::spawn(move || {
                // Thread 1: Report PV changes
                tm1.on_pv_change(10);
                thread::yield_now();
                tm1.on_pv_change(15);
            });

            let t2 = thread::spawn(move || {
                // Thread 2: Get time info (which internally uses PV stability)
                thread::yield_now();
                let _ = tm2.get_time_info();
                thread::yield_now();
                let _ = tm2.get_time_info();
            });

            let t3 = thread::spawn(move || {
                // Thread 3: Check should_stop (depends on PV stability)
                for _ in 0..5 {
                    tm3.should_stop(1000);
                    thread::yield_now();
                }
            });

            t1.join().unwrap();
            t2.join().unwrap();
            t3.join().unwrap();
        });
    }

    #[test]
    fn test_concurrent_time_info_access() {
        loom::model(|| {
            let tm = create_test_manager();
            let tm1 = tm.clone();
            let tm2 = tm.clone();

            let t1 = thread::spawn(move || {
                // Thread 1: Get time info repeatedly
                for _ in 0..10 {
                    let _ = tm1.get_time_info();
                    thread::yield_now();
                }
            });

            let t2 = thread::spawn(move || {
                // Thread 2: Update nodes and check stop
                for i in 0..10 {
                    tm2.should_stop(i * 1000);
                    thread::yield_now();
                }
            });

            t1.join().unwrap();
            t2.join().unwrap();
        });
    }

    #[test]
    fn test_concurrent_byoyomi_updates() {
        loom::model(|| {
            let limits = TimeLimits {
                time_control: TimeControl::Byoyomi {
                    main_time_ms: 0,
                    byoyomi_ms: 1000,
                    periods: 3,
                },
                ..Default::default()
            };

            let tm = Arc::new(TimeManager::new(&limits, Color::Black, 0, GamePhase::EndGame));
            let tm1 = tm.clone();
            let tm2 = tm.clone();

            let t1 = thread::spawn(move || {
                // Thread 1: Finish move
                tm1.update_after_move(500, TimeState::Byoyomi { main_left_ms: 0 });
            });

            let t2 = thread::spawn(move || {
                // Thread 2: Check byoyomi state
                thread::yield_now();
                let _ = tm2.get_byoyomi_state();
            });

            t1.join().unwrap();
            t2.join().unwrap();

            // Verify state consistency
            let state = tm.get_byoyomi_state();
            assert!(state.is_some());
        });
    }

    #[test]
    fn test_force_stop_ordering() {
        loom::model(|| {
            let tm = create_test_manager();
            let tm1 = tm.clone();
            let tm2 = tm.clone();
            let tm3 = tm.clone();

            let t1 = thread::spawn(move || {
                // Thread 1: Check should_stop before force_stop
                let before = tm1.should_stop(0);
                thread::yield_now();
                let after = tm1.should_stop(0);
                (before, after)
            });

            let t2 = thread::spawn(move || {
                // Thread 2: Force stop
                thread::yield_now();
                tm2.force_stop();
            });

            let (before, after) = t1.join().unwrap();
            t2.join().unwrap();

            // After force_stop, should_stop must return true
            assert!(tm3.should_stop(0));

            // If we saw false before, we might see true after (but not vice versa)
            if after {
                // OK: transitioned from false to true or was already true
            } else {
                assert!(!before); // If after is false, before must also be false
            }
        });
    }
}

// Provide a dummy test for non-loom builds
#[cfg(not(all(feature = "loom", not(target_arch = "wasm32"))))]
#[test]
fn loom_tests_require_feature() {
    eprintln!("Loom tests require --features loom and non-WASM target");
}
