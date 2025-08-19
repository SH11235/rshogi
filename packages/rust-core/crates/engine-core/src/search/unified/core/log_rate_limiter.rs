//! Log rate limiting utilities
//!
//! Provides utilities to limit the frequency of warning logs to avoid spam

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Static start time for consistent elapsed time calculation
static START_TIME: OnceLock<Instant> = OnceLock::new();

/// Get elapsed milliseconds since the first call
fn get_elapsed_millis() -> u64 {
    let start = START_TIME.get_or_init(Instant::now);
    start.elapsed().as_millis() as u64
}

/// A rate limiter for log messages
pub struct LogRateLimiter {
    last_log_time: AtomicU64,
    min_interval: Duration,
}

impl LogRateLimiter {
    /// Create a new rate limiter with the specified minimum interval between logs
    pub const fn new(min_interval: Duration) -> Self {
        Self {
            last_log_time: AtomicU64::new(0),
            min_interval,
        }
    }

    /// Check if we should log based on rate limiting
    /// Returns true if enough time has passed since the last log
    pub fn should_log(&self) -> bool {
        let now = get_elapsed_millis();
        let last = self.last_log_time.load(Ordering::Relaxed);
        let interval_ms = self.min_interval.as_millis() as u64;

        // Special case: first call (last == 0 and now == 0)
        if last == 0 && now == 0 {
            // First call immediately after process start
            self.last_log_time.store(1, Ordering::SeqCst);
            return true;
        }

        if now.saturating_sub(last) >= interval_ms {
            // Try to update the last log time
            // Use compare_exchange to avoid race conditions
            self.last_log_time
                .compare_exchange(last, now, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
        } else {
            false
        }
    }
}

// Global rate limiter for quiescence depth warnings (1 second interval)
pub static QUIESCE_DEPTH_LIMITER: LogRateLimiter = LogRateLimiter::new(Duration::from_secs(1));

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_rate_limiter_basic() {
        let limiter = LogRateLimiter::new(Duration::from_millis(100));

        // First call should always allow logging
        assert!(limiter.should_log());

        // Immediate second call should be rate limited
        assert!(!limiter.should_log());

        // After waiting, should allow logging again
        thread::sleep(Duration::from_millis(110));
        assert!(limiter.should_log());
    }
}
