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

    /// Whether ponder was converted to normal search
    ponder_converted: bool,
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
            ponder_converted: false,
        }
    }

    /// Reset context for new search
    pub fn reset(&mut self) {
        self.start_time = Instant::now();
        self.internal_stop.store(false, Ordering::Relaxed);
        self.ponder_hit_flag = None;
        self.ponder_converted = false;
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
            // Check if we've already converted
            if flag.load(Ordering::Acquire) && !self.ponder_converted {
                log::info!("Ponder hit detected in process_events");
                self.ponder_converted = true;

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
            // Use Acquire ordering for better responsiveness to stop commands
            if stop_flag.load(Ordering::Acquire) {
                return true;
            }
        }

        // Check internal stop flag
        self.internal_stop.load(Ordering::Acquire)
    }

    /// Get maximum search depth
    pub fn max_depth(&self) -> u8 {
        self.limits.depth.unwrap_or(127)
    }

    /// Signal internal stop
    pub fn stop(&self) {
        // Use Release ordering to ensure the stop signal is visible to other threads quickly
        self.internal_stop.store(true, Ordering::Release);
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

    /// Get reference to search limits
    pub fn limits(&self) -> &SearchLimits {
        &self.limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_management::TimeControl;

    #[test]
    fn test_ponder_converted_flag() {
        let mut context = SearchContext::new();

        // Initially false
        assert!(!context.ponder_converted);

        // Set up ponder mode with a ponder hit flag
        let ponder_hit_flag = Arc::new(AtomicBool::new(false));
        let mut limits = SearchLimits::builder()
            .time_control(TimeControl::Ponder(Box::new(TimeControl::Infinite)))
            .build();
        limits.ponder_hit_flag = Some(ponder_hit_flag.clone());
        context.set_limits(limits);

        // First call should not trigger (flag is false)
        context.process_events(&None);
        assert!(!context.ponder_converted);

        // Set the flag to true
        ponder_hit_flag.store(true, Ordering::Release);

        // First call with flag true should convert
        context.process_events(&None);
        assert!(context.ponder_converted);

        // Second call should not re-process (already converted)
        context.process_events(&None);
        assert!(context.ponder_converted);

        // Reset should clear the flag
        context.reset();
        assert!(!context.ponder_converted);
    }
}
