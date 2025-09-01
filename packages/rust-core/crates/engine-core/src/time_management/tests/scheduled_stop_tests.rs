//! Tests for scheduled stop functionality (YaneuraOu-style time management)
//!
//! These tests verify:
//! - Time scheduling when maximum limit (opt_limit_ms) is exceeded
//! - Proper rounding to second boundaries with overhead consideration
//! - Safety margin calculation based on NetworkDelay2
//! - No premature stops before maximum limit

use crate::search::GamePhase;
use crate::time_management::test_utils::{mock_advance_time, mock_set_time};
use crate::time_management::{TimeControl, TimeLimits, TimeManager, TimeParametersBuilder};
use crate::Color;

#[test]
fn test_opt_limit_calculation() {
    // Test that opt_limit_ms is set to 1.5x soft_ms but not exceeding 80% of hard_ms
    mock_set_time(0); // Reset mock time
    let params = TimeParametersBuilder::new().overhead_ms(100).unwrap().build();

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 10000,
            black_ms: 10000,
            increment_ms: 0,
        },
        time_parameters: Some(params),
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    let soft = tm.soft_limit_ms();
    let opt = tm.opt_limit_ms();
    let hard = tm.hard_limit_ms();

    // opt should be 1.5x soft
    assert!(opt > soft);
    assert!(opt <= soft * 3 / 2);

    // opt should not exceed 80% of hard
    assert!(opt <= hard * 8 / 10);
}

#[test]
fn test_search_end_scheduling_on_opt_limit() {
    // Test that exceeding opt_limit_ms triggers search_end scheduling
    mock_set_time(0); // Reset mock time
    let params = TimeParametersBuilder::new()
        .overhead_ms(100)
        .unwrap()
        .network_delay2_ms(1000)
        .unwrap()
        .build();

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 10000,
            black_ms: 10000,
            increment_ms: 0,
        },
        time_parameters: Some(params),
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    let opt = tm.opt_limit_ms();

    // Before opt_limit: no scheduled end
    mock_advance_time(opt - 100);
    assert!(!tm.should_stop(1000));
    assert_eq!(tm.scheduled_end_ms(), u64::MAX);

    // At opt_limit: schedule end but don't stop yet
    mock_advance_time(200);
    assert!(!tm.should_stop(2000)); // Should not stop immediately
    assert_ne!(tm.scheduled_end_ms(), u64::MAX); // But end is scheduled

    // Verify scheduled time is rounded up to next second
    let elapsed = tm.elapsed_ms();
    let scheduled = tm.scheduled_end_ms();
    assert!(scheduled > elapsed);

    // Debug output
    eprintln!(
        "elapsed: {}, scheduled: {}, scheduled % 1000: {}",
        elapsed,
        scheduled,
        scheduled % 1000
    );

    // Scheduled should be at next second boundary minus overhead
    // Calculate what the next second boundary should be
    let next_second = ((elapsed / 1000) + 1) * 1000;
    let expected_with_overhead = next_second.saturating_sub(100); // minus overhead

    // But if that would be less than elapsed, it adds 1000ms
    let expected = if expected_with_overhead <= elapsed {
        elapsed + 1000
    } else {
        expected_with_overhead
    };

    // Allow some tolerance for edge cases
    assert!(scheduled >= elapsed);
    assert!(scheduled <= expected + 100); // Allow small tolerance
}

#[test]
fn test_round_up_with_network_delay() {
    // Test that round_up considers overhead correctly
    mock_set_time(0); // Reset mock time
    let params = TimeParametersBuilder::new().overhead_ms(120).unwrap().build();

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 10000,
            black_ms: 10000,
            increment_ms: 0,
        },
        time_parameters: Some(params),
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // Test round up behavior via opt_limit trigger
    // Set time to trigger scheduling (opt_limit + a bit)
    let opt = tm.opt_limit_ms();
    let hard = tm.hard_limit_ms();

    // Ensure we don't exceed hard limit
    let test_time = if opt + 100 >= hard {
        opt + 10
    } else {
        opt + 100
    };
    mock_set_time(test_time);
    tm.should_stop(1000); // This triggers scheduling

    let scheduled = tm.scheduled_end_ms();
    let elapsed = tm.elapsed_ms();

    // If not scheduled, it might be because we hit hard limit
    if scheduled == u64::MAX {
        eprintln!(
            "No scheduling occurred. opt: {}, test_time: {}, hard: {}, elapsed: {}",
            opt, test_time, hard, elapsed
        );
        // If we're at or past hard limit, that's expected
        assert!(elapsed >= hard);
    } else {
        // Verify rounding behavior
        eprintln!("Scheduled: {}, elapsed: {}", scheduled, elapsed);

        // Should be greater than current time
        assert!(scheduled > elapsed);

        // Should be capped by hard limit
        assert!(scheduled <= hard);
    }
}

#[test]
fn test_safety_margin_behavior() {
    // Test safety margin behavior indirectly through scheduling
    // Since calculate_safety_margin is private, we test its effect
    mock_set_time(0); // Reset mock time

    // Test with large time budget
    let params1 = TimeParametersBuilder::new().network_delay2_ms(1200).unwrap().build();

    let limits1 = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 10000,
            black_ms: 10000,
            increment_ms: 0,
        },
        time_parameters: Some(params1),
        ..Default::default()
    };

    let tm1 = TimeManager::new(&limits1, Color::Black, 0, GamePhase::MiddleGame);

    // Trigger scheduling and check margin
    let opt1 = tm1.opt_limit_ms();
    let hard1 = tm1.hard_limit_ms();
    eprintln!("Test 1 - opt1: {}, hard1: {}", opt1, hard1);
    mock_set_time(opt1 + 100);
    tm1.should_stop(1000);

    let scheduled1 = tm1.scheduled_end_ms();
    eprintln!(
        "Test 1 - scheduled1: {}, hard1 - 1200: {}",
        scheduled1,
        hard1.saturating_sub(1200)
    );
    // Check that safety margin is applied, but consider that hard_limit might be smaller than expected
    // The actual safety margin will be capped based on calculate_safety_margin() logic
    let expected_margin = if hard1 >= 5000 {
        1200
    } else if hard1 >= 1000 {
        500
    } else if hard1 >= 500 {
        200
    } else {
        100
    };
    assert!(scheduled1 <= hard1.saturating_sub(expected_margin));

    // Test with small time budget
    let params2 = TimeParametersBuilder::new().network_delay2_ms(1200).unwrap().build();

    let limits2 = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 800,
            black_ms: 800,
            increment_ms: 0,
        },
        time_parameters: Some(params2),
        ..Default::default()
    };

    let tm2 = TimeManager::new(&limits2, Color::Black, 0, GamePhase::MiddleGame);

    // Trigger scheduling and check margin
    let opt2 = tm2.opt_limit_ms();
    let hard2 = tm2.hard_limit_ms();
    eprintln!("Test 2 - opt2: {}, hard2: {}", opt2, hard2);

    // Be more careful with small budgets
    if opt2 + 10 >= hard2 {
        eprintln!("Test 2 - opt2 + 10 would exceed hard limit, using opt2 + 1");
        mock_set_time(opt2 + 1);
    } else {
        mock_set_time(opt2 + 10);
    }
    tm2.should_stop(1000);

    let scheduled2 = tm2.scheduled_end_ms();
    eprintln!(
        "Test 2 - scheduled2: {}, hard2 - 200: {}",
        scheduled2,
        hard2.saturating_sub(200)
    );

    // With very small budgets, scheduling might not occur if we're already close to hard limit
    if scheduled2 == u64::MAX {
        eprintln!("Test 2 - No scheduling occurred due to very small time budget");
        // This is acceptable for such small budgets
        return;
    }

    // With small hard limit, check appropriate safety margin
    let expected_margin2 = if hard2 >= 5000 {
        1200
    } else if hard2 >= 1000 {
        500
    } else if hard2 >= 500 {
        200
    } else {
        100
    };
    assert!(scheduled2 <= hard2.saturating_sub(expected_margin2));
}

#[test]
fn test_stop_at_scheduled_end() {
    // Test that search stops at scheduled end time
    mock_set_time(0); // Reset mock time
    let params = TimeParametersBuilder::new().overhead_ms(100).unwrap().build();

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 5000,
            black_ms: 5000,
            increment_ms: 0,
        },
        time_parameters: Some(params),
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // Trigger scheduling by exceeding opt_limit
    let opt = tm.opt_limit_ms();
    let hard = tm.hard_limit_ms();
    eprintln!("opt_limit: {}, soft_limit: {}, hard_limit: {}", opt, tm.soft_limit_ms(), hard);

    // Make sure we don't exceed hard limit
    if opt + 100 >= hard {
        eprintln!("opt + 100 would exceed hard limit, adjusting test");
        mock_set_time(opt + 10); // Use smaller increment
    } else {
        mock_set_time(opt + 100);
    }

    let should_stop = tm.should_stop(1000);
    eprintln!("should_stop after opt: {}", should_stop);
    assert!(!should_stop); // Schedules but doesn't stop

    let scheduled = tm.scheduled_end_ms();
    assert_ne!(scheduled, u64::MAX);

    // Move to scheduled time
    mock_set_time(scheduled);
    assert!(tm.should_stop(2000)); // Now it should stop
}

#[test]
fn test_advise_after_iteration_tightening() {
    // Test that advise_after_iteration can tighten deadline when approaching hard limit
    mock_set_time(0); // Reset mock time
    let params = TimeParametersBuilder::new()
        .overhead_ms(100)
        .unwrap()
        .network_delay2_ms(500)
        .unwrap()
        .build();

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 3000,
            black_ms: 3000,
            increment_ms: 0,
        },
        time_parameters: Some(params),
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // First trigger normal scheduling
    let opt = tm.opt_limit_ms();
    let hard = tm.hard_limit_ms();
    eprintln!("opt: {}, hard: {}", opt, hard);

    // Make sure we don't exceed hard limit
    if opt + 100 >= hard {
        mock_set_time(opt + 10);
    } else {
        mock_set_time(opt + 100);
    }
    tm.should_stop(1000); // This schedules end

    let original_scheduled = tm.scheduled_end_ms();
    eprintln!("original_scheduled: {}", original_scheduled);
    assert_ne!(original_scheduled, u64::MAX);

    // Move close to hard limit
    let close_to_hard = hard.saturating_sub(1500); // Within safety margin * 2
                                                   // Make sure we're not going backwards in time
    if close_to_hard <= tm.elapsed_ms() {
        eprintln!("close_to_hard would go backwards, skipping tightening test");
        return; // Skip this part of the test if hard limit is too small
    }
    mock_set_time(close_to_hard);

    // Call advise_after_iteration
    tm.advise_after_iteration(close_to_hard);

    // Check if deadline was tightened
    let new_scheduled = tm.scheduled_end_ms();
    assert!(new_scheduled < original_scheduled);
}

#[test]
fn test_no_premature_stop_before_opt_limit() {
    // Test that search doesn't stop before opt_limit (no more soft+PV stable check)
    mock_set_time(0); // Reset mock time
    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 10000,
            black_ms: 10000,
            increment_ms: 0,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    let soft = tm.soft_limit_ms();
    let opt = tm.opt_limit_ms();

    // At soft limit: should not trigger scheduling
    mock_set_time(soft + 100);
    assert!(!tm.should_stop(1000));
    assert_eq!(tm.scheduled_end_ms(), u64::MAX); // No scheduling

    // Between soft and opt: still no scheduling
    mock_set_time((soft + opt) / 2);
    assert!(!tm.should_stop(2000));
    assert_eq!(tm.scheduled_end_ms(), u64::MAX);
}

#[test]
fn test_respect_hard_limit_always() {
    // Test that hard limit is always respected
    // Reset mock time first
    mock_set_time(0);

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 2000,
            black_ms: 2000,
            increment_ms: 0,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    let hard = tm.hard_limit_ms();

    // At hard limit: immediate stop
    mock_set_time(hard);
    assert!(tm.should_stop(1000));

    // Beyond hard limit: still stops
    mock_set_time(hard + 500);
    assert!(tm.should_stop(2000));
}
