//! Time management tests for WASM environment

use wasm_bindgen_test::*;

// Run tests in browser
wasm_bindgen_test_configure!(run_in_browser);

#[cfg(target_arch = "wasm32")]
mod wasm_tests {
    use super::*;
    use engine_core::search::GamePhase;
    use engine_core::time_management::{TimeControl, TimeLimits, TimeManager, TimeState};
    use engine_core::Color;
    use js_sys::Date;

    /// Test basic TimeManager creation in WASM
    #[wasm_bindgen_test]
    fn test_time_manager_creation_wasm() {
        let limits = TimeLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);
        let info = tm.get_time_info();

        assert_eq!(info.nodes_searched, 0);
        assert!(info.soft_limit_ms > 0);
        assert!(info.hard_limit_ms > info.soft_limit_ms);
    }

    /// Test time measurement using JS Date API
    #[wasm_bindgen_test]
    fn test_elapsed_time_wasm() {
        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 1000 },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

        // Get initial time
        let start = Date::now();

        // Busy wait for ~100ms (not ideal but works for testing)
        while Date::now() - start < 100.0 {}

        // Check elapsed time
        let elapsed = tm.elapsed_ms();

        // Should be at least 90ms (allowing some measurement error)
        assert!(elapsed >= 90, "Elapsed time {} should be >= 90ms", elapsed);
    }

    /// Test Byoyomi time control in WASM
    #[wasm_bindgen_test]
    fn test_byoyomi_wasm() {
        let limits = TimeLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 0,
                byoyomi_ms: 1000,
                periods: 3,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::EndGame);

        // Initially should have 3 periods
        let state = tm.get_byoyomi_state();
        assert!(state.is_some());
        let (periods, _, in_byoyomi) = state.unwrap();
        assert_eq!(periods, 3);
        assert!(in_byoyomi);

        // Consume one period
        tm.update_after_move(1500, TimeState::Byoyomi { main_left_ms: 0 });
        let state = tm.get_byoyomi_state().unwrap();
        assert_eq!(state.0, 2); // Should have 2 periods left
    }

    /// Test node-based stopping in WASM
    #[wasm_bindgen_test]
    fn test_node_limit_wasm() {
        let limits = TimeLimits {
            time_control: TimeControl::FixedNodes { nodes: 10000 },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::Opening);

        // Should not stop before reaching node limit
        assert!(!tm.should_stop(5000));
        assert!(!tm.should_stop(9999));

        // Should stop at or after node limit
        assert!(tm.should_stop(10000));
        assert!(tm.should_stop(15000));
    }

    /// Test force stop functionality in WASM
    #[wasm_bindgen_test]
    fn test_force_stop_wasm() {
        let limits = TimeLimits {
            time_control: TimeControl::Infinite,
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

        // Should not stop with infinite time
        assert!(!tm.should_stop(1000000));

        // Force stop
        tm.force_stop();

        // Now should stop
        assert!(tm.should_stop(0));
    }

    /// Test PV change tracking in WASM
    #[wasm_bindgen_test]
    fn test_pv_change_wasm() {
        let limits = TimeLimits {
            time_control: TimeControl::Fischer {
                white_ms: 30000,
                black_ms: 30000,
                increment_ms: 500,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 10, GamePhase::MiddleGame);

        // Report PV changes
        tm.on_pv_change(5);
        tm.on_pv_change(10);
        tm.on_pv_change(15);

        // Should still be able to get time info
        let info = tm.get_time_info();
        assert!(info.elapsed_ms < 1000); // Should be quick in test
    }

    /// Test time pressure calculation in WASM
    #[wasm_bindgen_test]
    fn test_time_pressure_wasm() {
        let limits = TimeLimits {
            time_control: TimeControl::Fischer {
                white_ms: 1000, // Very low time
                black_ms: 1000,
                increment_ms: 0,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 50, GamePhase::EndGame);
        let info = tm.get_time_info();

        // With very low time, pressure should be noticeable
        assert!(info.time_pressure > 0.0);
        assert!(info.time_pressure <= 1.0);
    }

    /// Test that Instant-based timing works in WASM
    #[wasm_bindgen_test]
    async fn test_instant_compatibility_wasm() {
        // This test verifies that our Instant usage is compatible with WASM
        // In WASM, std::time::Instant is implemented using performance.now()

        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 500 },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::Opening);

        // Initial elapsed should be very small
        let initial = tm.elapsed_ms();
        assert!(initial < 10, "Initial elapsed time should be < 10ms, got {}", initial);

        // Use wasm_bindgen_futures to properly handle async in tests
        let delay = wasm_bindgen_futures::js_sys::Promise::new(&mut |resolve, _| {
            let window = web_sys::window().unwrap();
            window
                .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 50)
                .unwrap();
        });

        wasm_bindgen_futures::JsFuture::from(delay).await.unwrap();

        // After delay, elapsed should be at least 40ms
        let after_delay = tm.elapsed_ms();
        assert!(
            after_delay >= 40,
            "After delay elapsed time should be >= 40ms, got {}",
            after_delay
        );
    }
}

// Provide informative message for non-WASM builds
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn wasm_tests_require_wasm_target() {
    eprintln!("WASM tests require --target wasm32-unknown-unknown");
}
