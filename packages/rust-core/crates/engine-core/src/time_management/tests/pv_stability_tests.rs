//! Tests for PV stability functionality and ponder_hit timestamp handling

use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
use crate::Color;

use super::{mock_advance_time, mock_set_time};

#[test]
fn test_pv_stability_after_ponder_hit() {
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

    // Simulate 3 seconds of pondering
    mock_advance_time(3000);

    // Notify a PV change at depth 10 during pondering
    tm.on_pv_change(10);

    // Advance another 2 seconds
    mock_advance_time(2000);

    // Now we're at 5 seconds total pondering time
    // last_pv_change_ms should be at 3000ms

    // Ponder hit
    tm.ponder_hit(None, 5000);

    // After ponder_hit:
    // - start_time is reset to current time
    // - elapsed_ms() will return ~0
    // - last_pv_change_ms is reset to 0 (properly handled in ponder_hit)

    // Check if PV is considered stable
    // With the fix, PV stability tracking is properly reset:
    // elapsed_ms (0) - last_pv_change_ms (0) = 0
    // which correctly makes it unstable until threshold time passes
    let elapsed = tm.elapsed_ms();
    eprintln!("Elapsed after ponder_hit: {elapsed}ms");

    // Get state checker to test is_pv_stable
    let checker = tm.state_checker();
    let is_stable = checker.is_pv_stable(elapsed);
    eprintln!("PV stable check immediately after ponder_hit: {is_stable}");

    // The PV should be considered unstable immediately after ponder_hit
    // because elapsed_ms (0) - last_pv_change_ms (0) = 0, which is not > threshold (80ms)
    assert!(
        !is_stable,
        "PV should be unstable immediately after ponder_hit (0ms < 80ms threshold)"
    );

    // Advance time past the stability threshold (default 80ms)
    mock_advance_time(100);
    let elapsed = tm.elapsed_ms();
    let is_stable = checker.is_pv_stable(elapsed);
    eprintln!("PV stable after 100ms: {is_stable} (elapsed: {elapsed}ms)");
    assert!(is_stable, "PV should be stable after exceeding threshold");

    // Trigger a new PV change and verify it becomes unstable
    tm.on_pv_change(12); // This sets threshold to 80 + (12 * 5) = 140ms
    let elapsed = tm.elapsed_ms();
    let is_stable = checker.is_pv_stable(elapsed);
    eprintln!("PV stable immediately after change: {is_stable}");
    assert!(!is_stable, "PV should be unstable immediately after change");

    // Verify it becomes stable again after threshold (140ms for depth 12)
    mock_advance_time(150); // Need more than 140ms
    let elapsed = tm.elapsed_ms();
    let is_stable = checker.is_pv_stable(elapsed);
    eprintln!("PV stable after 150ms more: {is_stable} (elapsed: {elapsed}ms)");
    assert!(is_stable, "PV should be stable again after exceeding depth-adjusted threshold");
}

#[test]
fn test_pv_stability_allows_soft_limit_stop() {
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

    // Ponder for 3 seconds
    mock_advance_time(3000);

    // PV change during pondering
    tm.on_pv_change(10);

    // Ponder hit - this should reset PV stability tracking
    tm.ponder_hit(None, 3000);

    // Now the soft limit is adjusted for the 3 seconds already spent
    let info = tm.get_time_info();
    eprintln!("Soft limit after ponder_hit: {}ms", info.soft_limit_ms);

    // Advance time past the soft limit + PV stability threshold
    // PV should be stable since it was reset at ponder_hit
    mock_advance_time(info.soft_limit_ms + 1000);

    // With the fix, PV is stable and we should stop at soft limit
    let should_stop = tm.should_stop(10000);
    eprintln!("Should stop after exceeding soft limit: {should_stop}");

    // Verify the fix works: we stop at soft limit when PV is stable
    assert!(should_stop, "Should stop at soft limit with stable PV after fix");
}

#[test]
fn test_pv_stability_after_ponder_hit_without_pv_change() {
    // Test that soft limit extension works correctly after ponder_hit
    // when no PV change occurs after the transition
    mock_set_time(0);

    let pending_limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 5000,
            black_ms: 5000,
            increment_ms: 0,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_ponder(&pending_limits, Color::White, 20, GamePhase::MiddleGame);

    // Simulate pondering for 500ms without PV changes
    mock_advance_time(500);
    assert!(tm.is_pondering());

    // Get soft limit before ponder_hit
    let ponder_manager = tm.ponder_manager();
    ponder_manager.ponder_hit(Some(&pending_limits), 500);

    // After ponder_hit, PV stability should be reset
    let checker = tm.state_checker();
    let initial_elapsed = tm.elapsed_ms();
    let initial_stable = checker.is_pv_stable(initial_elapsed);
    eprintln!("After ponder_hit - elapsed: {initial_elapsed}ms, PV stable: {initial_stable}");
    assert!(!initial_stable, "PV should be unstable immediately after ponder_hit");

    // Get the actual soft limit after ponder_hit
    let soft_limit = tm.soft_limit_ms();
    eprintln!("Soft limit after ponder_hit: {soft_limit}ms");

    // If soft limit is already reached or very close, advance less
    if initial_elapsed < soft_limit {
        let advance_to_soft = soft_limit.saturating_sub(initial_elapsed);
        if advance_to_soft > 10 {
            mock_advance_time(advance_to_soft - 10);
            assert!(!tm.should_stop(1000), "Should not stop before soft limit");
            mock_advance_time(10);
        } else {
            mock_advance_time(advance_to_soft);
        }
    }

    // At soft limit, PV should still be unstable (not enough time passed since ponder_hit)
    let elapsed_at_soft = tm.elapsed_ms();
    let is_stable_at_soft = checker.is_pv_stable(elapsed_at_soft);
    eprintln!("At soft limit - elapsed: {elapsed_at_soft}ms, soft: {soft_limit}ms, PV stable: {is_stable_at_soft}");

    if elapsed_at_soft >= soft_limit && !is_stable_at_soft {
        assert!(!tm.should_stop(1000), "Should not stop at soft limit with unstable PV");
    }

    // Advance time past stability threshold (default is 80ms base + depth*5ms = 80 + 20*5 = 180ms for depth 20)
    mock_advance_time(200);

    // Now PV should be stable and search should stop
    let elapsed = tm.elapsed_ms();
    eprintln!("After stability wait - elapsed: {elapsed}ms");
    assert!(checker.is_pv_stable(elapsed), "PV should be stable after threshold");

    if elapsed >= soft_limit {
        assert!(tm.should_stop(1000), "Should stop with stable PV past soft limit");
    }
}

#[test]
fn test_pv_instability_extends_soft_limit() {
    // Test that frequent PV changes prevent soft limit stop
    mock_set_time(0);

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 5000,
            black_ms: 5000,
            increment_ms: 0,
        },
        ..Default::default()
    };

    let tm = TimeManager::new_with_mock_time(&limits, Color::White, 20, GamePhase::MiddleGame);
    let soft_limit = tm.soft_limit_ms();

    // Simulate frequent PV changes
    for i in 0..10 {
        mock_advance_time(50);
        tm.on_pv_change(5 + i);
    }

    // Advance to soft limit
    mock_set_time(soft_limit);

    // Should NOT stop because PV is unstable
    assert!(!tm.should_stop(1000), "Should not stop at soft limit with unstable PV");

    // But should stop at hard limit
    let time_info = tm.get_time_info();
    let hard_limit = time_info.hard_limit_ms;
    mock_set_time(hard_limit);
    assert!(tm.should_stop(1000), "Should always stop at hard limit");
}
