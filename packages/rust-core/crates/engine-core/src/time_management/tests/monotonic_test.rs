//! Tests for monotonic time behavior

use crate::time_management::{monotonic_ms, GamePhase, TimeControl, TimeLimits, TimeManager};
use crate::Color;
use std::thread;
use std::time::Duration;

#[cfg(test)]
use super::{mock_advance_time, mock_set_time};

#[test]
fn test_monotonic_ms_is_monotonic() {
    // Get multiple samples and ensure they're non-decreasing
    let mut last_ms = monotonic_ms();

    for _ in 0..10 {
        thread::sleep(Duration::from_millis(1));
        let current_ms = monotonic_ms();
        assert!(current_ms >= last_ms, "monotonic_ms must be non-decreasing");
        last_ms = current_ms;
    }
}

#[test]
fn test_elapsed_ms_is_monotonic() {
    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);

    let mut last_elapsed = tm.elapsed_ms();

    for _ in 0..10 {
        thread::sleep(Duration::from_millis(1));
        let current_elapsed = tm.elapsed_ms();
        assert!(current_elapsed >= last_elapsed, "elapsed_ms must be non-decreasing");
        last_elapsed = current_elapsed;
    }
}

#[test]
fn test_elapsed_ms_starts_near_zero() {
    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);

    let elapsed = tm.elapsed_ms();
    // Should be very close to 0, allowing for some initialization time
    assert!(elapsed < 10, "elapsed_ms should start near 0, but was {elapsed}");
}

#[test]
fn test_monotonic_ms_advances_with_mock() {
    mock_set_time(0);
    assert_eq!(monotonic_ms(), 0);

    mock_advance_time(123);
    assert_eq!(monotonic_ms(), 123);

    mock_advance_time(456);
    assert_eq!(monotonic_ms(), 579); // 123 + 456
}

#[test]
fn test_elapsed_ms_with_mock_advance() {
    mock_set_time(1000);

    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);

    // Should start at 0 (relative to creation time)
    assert_eq!(tm.elapsed_ms(), 0);

    mock_advance_time(500);
    assert_eq!(tm.elapsed_ms(), 500);

    mock_advance_time(1500);
    assert_eq!(tm.elapsed_ms(), 2000);
}
