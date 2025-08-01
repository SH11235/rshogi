//! Search context management
//!
//! Manages search limits, timing, and stopping conditions

use crate::search::SearchLimits;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Search context for managing limits and state
pub struct SearchContext {
    /// Search limits
    limits: SearchLimits,

    /// Start time of search
    start_time: Instant,

    /// Internal stop flag
    internal_stop: AtomicBool,
}

impl Default for SearchContext {
    fn default() -> Self {
        Self::new()
    }
}

impl SearchContext {
    /// Create new search context
    pub fn new() -> Self {
        Self {
            limits: SearchLimits::default(),
            start_time: Instant::now(),
            internal_stop: AtomicBool::new(false),
        }
    }

    /// Reset context for new search
    pub fn reset(&mut self) {
        self.start_time = Instant::now();
        self.internal_stop.store(false, Ordering::Relaxed);
    }

    /// Set search limits
    pub fn set_limits(&mut self, limits: SearchLimits) {
        self.limits = limits;
    }

    /// Check if search should stop
    pub fn should_stop(&self) -> bool {
        // Check external stop flag
        if let Some(ref stop_flag) = self.limits.stop_flag {
            if stop_flag.load(Ordering::Relaxed) {
                return true;
            }
        }

        // Check internal stop flag
        if self.internal_stop.load(Ordering::Relaxed) {
            return true;
        }

        // Check time limit based on time control
        use crate::time_management::TimeControl;
        match &self.limits.time_control {
            TimeControl::FixedTime { ms_per_move } => {
                let elapsed_ms = self.start_time.elapsed().as_millis() as u64;
                if elapsed_ms >= *ms_per_move {
                    self.internal_stop.store(true, Ordering::Relaxed);
                    return true;
                }
            }
            TimeControl::Fischer { .. } | TimeControl::Byoyomi { .. } => {
                // TODO: Implement time management for these modes
            }
            TimeControl::FixedNodes { .. } => {
                // TODO: Implement node limit checking
            }
            TimeControl::Infinite | TimeControl::Ponder(_) => {
                // No time limit
            }
        }

        false
    }

    /// Get maximum search depth
    pub fn max_depth(&self) -> u8 {
        self.limits.depth.unwrap_or(127)
    }

    /// Signal internal stop
    pub fn stop(&self) {
        self.internal_stop.store(true, Ordering::Relaxed);
    }

    /// Get elapsed time
    pub fn elapsed(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    /// Get reference to info callback if available
    pub fn info_callback(&self) -> Option<&crate::search::types::InfoCallback> {
        self.limits.info_callback.as_ref()
    }
}
