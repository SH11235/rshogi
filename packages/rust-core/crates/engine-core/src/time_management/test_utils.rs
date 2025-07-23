//! Test utilities for time management module
//!
//! Provides MockClock for deterministic time testing

#[cfg(test)]
use parking_lot::Mutex;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::{Duration, Instant};

/// Mock clock for testing time-dependent behavior
#[cfg(test)]
pub struct MockClock {
    /// Shared state for the mock clock
    state: Arc<Mutex<MockClockState>>,
}

#[cfg(test)]
struct MockClockState {
    /// Current mock time in milliseconds
    current_time_ms: u64,
    /// Base instant for time calculations
    base_instant: Instant,
}

#[cfg(test)]
impl MockClock {
    /// Create a new mock clock starting at the given time
    pub fn new(start_time_ms: u64) -> Self {
        Self {
            state: Arc::new(Mutex::new(MockClockState {
                current_time_ms: start_time_ms,
                base_instant: Instant::now(),
            })),
        }
    }

    /// Get the current mock time as an Instant
    pub fn now(&self) -> Instant {
        let state = self.state.lock();
        state.base_instant + Duration::from_millis(state.current_time_ms)
    }

    /// Advance the mock time by the given number of milliseconds
    pub fn advance(&self, ms: u64) {
        let mut state = self.state.lock();
        state.current_time_ms += ms;
    }

    /// Set the mock time to a specific value
    pub fn set_time(&self, ms: u64) {
        let mut state = self.state.lock();
        state.current_time_ms = ms;
    }

    /// Get the current time in milliseconds
    pub fn current_ms(&self) -> u64 {
        let state = self.state.lock();
        state.current_time_ms
    }
}

#[cfg(test)]
thread_local! {
    /// Thread-local mock clock for tests
    static MOCK_CLOCK: MockClock = MockClock::new(0);
}

/// Set the global mock time
#[cfg(test)]
pub fn mock_set_time(ms: u64) {
    MOCK_CLOCK.with(|clock| clock.set_time(ms));
}

/// Advance the global mock time
#[cfg(test)]
pub fn mock_advance_time(ms: u64) {
    MOCK_CLOCK.with(|clock| clock.advance(ms));
}

/// Get the current global mock time as Instant
#[cfg(test)]
pub fn mock_now() -> Instant {
    MOCK_CLOCK.with(|clock| clock.now())
}

/// Get the current global mock time in milliseconds
#[cfg(test)]
pub fn mock_current_ms() -> u64 {
    MOCK_CLOCK.with(|clock| clock.current_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mock_clock_basic() {
        let clock = MockClock::new(1000);
        assert_eq!(clock.current_ms(), 1000);

        clock.advance(500);
        assert_eq!(clock.current_ms(), 1500);

        clock.set_time(2000);
        assert_eq!(clock.current_ms(), 2000);
    }

    #[test]
    fn test_mock_clock_instant() {
        let clock = MockClock::new(0);
        let start = clock.now();

        clock.advance(1000);
        let end = clock.now();

        let elapsed = end.duration_since(start);
        assert_eq!(elapsed.as_millis(), 1000);
    }

    #[test]
    fn test_global_mock_clock() {
        mock_set_time(5000);
        assert_eq!(mock_current_ms(), 5000);

        mock_advance_time(1500);
        assert_eq!(mock_current_ms(), 6500);
    }
}
