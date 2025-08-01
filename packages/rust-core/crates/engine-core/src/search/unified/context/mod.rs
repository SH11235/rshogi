//! Search context management
//!
//! Manages search limits, timing, and stopping conditions

use crate::search::SearchLimits;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// Search context for managing limits and state
pub struct SearchContext {
    /// Search limits
    limits: SearchLimits,

    /// Start time of search
    start_time: Instant,

    /// Internal stop flag
    internal_stop: AtomicBool,

    /// Ponder hit flag reference for mode conversion
    ponder_hit_flag: Option<Arc<AtomicBool>>,
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
            ponder_hit_flag: None,
        }
    }

    /// Reset context for new search
    pub fn reset(&mut self) {
        self.start_time = Instant::now();
        self.internal_stop.store(false, Ordering::Relaxed);
        self.ponder_hit_flag = None;
    }

    /// Set search limits
    pub fn set_limits(&mut self, limits: SearchLimits) {
        self.ponder_hit_flag = limits.ponder_hit_flag.clone();
        self.limits = limits;
    }

    /// Convert from ponder mode to normal search
    pub fn convert_from_ponder(&mut self) {
        use crate::time_management::TimeControl;

        if let TimeControl::Ponder(inner) = &self.limits.time_control {
            // Extract the inner time control for normal search
            self.limits.time_control = (**inner).clone();
            log::info!(
                "Converted from Ponder to normal search with time_control: {:?}",
                self.limits.time_control
            );

            // Reset start time so new time limits start from now
            self.start_time = Instant::now();
        }
    }

    /// Process events like ponder hit during search
    /// This should be called frequently from search loops
    pub fn process_events(
        &mut self,
        time_manager: &Option<Arc<crate::time_management::TimeManager>>,
    ) {
        // Check for ponder hit (only once)
        if let Some(flag) = &self.ponder_hit_flag {
            if flag.swap(false, Ordering::Acquire) {
                log::info!("Ponder hit detected in process_events");

                // Notify TimeManager about ponder hit
                if let Some(tm) = time_manager {
                    let elapsed_ms = self.elapsed().as_millis() as u64;
                    tm.ponder_hit(None, elapsed_ms);
                    log::info!("TimeManager notified of ponder hit after {elapsed_ms}ms");
                }

                // Convert search context from ponder to normal
                self.convert_from_ponder();
            }
        }
    }

    /// Check if search should stop
    ///
    /// This method only checks stop flags. Time management is handled by TimeManager.
    pub fn should_stop(&self) -> bool {
        // Check external stop flag
        if let Some(ref stop_flag) = self.limits.stop_flag {
            if stop_flag.load(Ordering::Relaxed) {
                return true;
            }
        }

        // Check internal stop flag
        self.internal_stop.load(Ordering::Relaxed)
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

    /// Get reference to ponder hit flag
    pub fn ponder_hit_flag(&self) -> Option<&Arc<AtomicBool>> {
        self.ponder_hit_flag.as_ref()
    }
}
