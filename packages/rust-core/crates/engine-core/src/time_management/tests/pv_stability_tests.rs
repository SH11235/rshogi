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
    // - last_pv_change_ms is still 3000 (from old start_time)

    // Check if PV is considered stable
    // This should fail due to the bug:
    // elapsed_ms (0) - last_pv_change_ms (3000) will saturate to 0
    // making it always appear unstable
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
