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

    /// Current search depth for logging
    current_depth: u8,

    /// Flag to log time stop only once
    time_stop_logged: bool,
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
            current_depth: 0,
            time_stop_logged: false,
        }
    }

    /// Reset context for new search
    pub fn reset(&mut self) {
        self.start_time = Instant::now();
        self.internal_stop.store(false, Ordering::Relaxed);
        self.ponder_hit_flag = None;
        self.ponder_converted = false;
        self.current_depth = 0;
        self.time_stop_logged = false;
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
                // Capture ponder elapsed time BEFORE converting (which resets start_time)
                let ponder_elapsed_ms = self.elapsed().as_millis() as u64;

                log::info!("Ponder hit detected in process_events");
                self.ponder_converted = true;

                // Convert search context from ponder to normal FIRST
                // This must be done before notifying TimeManager
                self.convert_from_ponder();

                // Now notify TimeManager about ponder hit with updated limits
                if let Some(tm) = time_manager {
                    // Create TimeLimits from SearchLimits using Into trait
                    let time_limits: crate::time_management::TimeLimits =
                        self.limits.clone().into();
                    tm.ponder_hit(Some(&time_limits), ponder_elapsed_ms);
                    log::info!("TimeManager notified of ponder hit after {ponder_elapsed_ms}ms");
                }
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

    /// Check time limit via TimeManager
    pub fn check_time_limit(
        &mut self,
        nodes: u64,
        time_manager: &Option<Arc<crate::time_management::TimeManager>>,
    ) -> bool {
        if let Some(ref tm) = time_manager {
            // Proactive early stop near hard limit to allow unwind/commit time
            let elapsed = self.elapsed();
            let hard_limit_ms = tm.hard_limit_ms();
            if hard_limit_ms > 0 {
                let elapsed_ms = elapsed.as_millis() as u64;
                // Safety window before hard limit to exit gracefully (adaptive)
                // Do not preempt ultra-short budgets
                let safety_ms = if hard_limit_ms >= 500 {
                    let three_percent = (hard_limit_ms.saturating_mul(3)) / 100; // 3%
                    three_percent.clamp(120, 400)
                } else if hard_limit_ms >= 200 {
                    40
                } else {
                    0
                };
                if elapsed_ms + safety_ms >= hard_limit_ms {
                    if !self.time_stop_logged {
                        log::info!(
                            "Near hard limit: depth={} nodes={} elapsed={}ms hard={}ms safety={}ms",
                            self.current_depth,
                            nodes,
                            elapsed_ms,
                            hard_limit_ms,
                            safety_ms
                        );
                        self.time_stop_logged = true;
                    }
                    self.stop();
                    return true;
                }
            }

            if tm.should_stop(nodes) {
                // Log once per search (engine-core internal logging)
                if !self.time_stop_logged {
                    let elapsed = self.elapsed();
                    log::info!(
                        "Time limit exceeded: depth={} nodes={} elapsed={}ms",
                        self.current_depth,
                        nodes,
                        elapsed.as_millis()
                    );
                    self.time_stop_logged = true;
                }
                self.stop();
                return true;
            }
        }
        false
    }

    /// Whether this search stopped due to time limit (as decided by TimeManager)
    #[inline]
    pub fn was_time_stopped(&self) -> bool {
        self.time_stop_logged
    }

    /// Get appropriate time check mask based on time control
    pub fn get_time_check_mask(&self) -> u64 {
        use crate::search::constants::{TIME_CHECK_MASK_BYOYOMI, TIME_CHECK_MASK_NORMAL};
        use crate::time_management::TimeControl;

        match &self.limits.time_control {
            TimeControl::FixedNodes { .. } => 0x3FF, // 1024 nodes - more frequent checks for node-based limits
            TimeControl::Byoyomi { .. } => TIME_CHECK_MASK_BYOYOMI,
            TimeControl::Ponder(_) => TIME_CHECK_MASK_NORMAL, // Ponder always uses NORMAL (opponent's time)
            _ => TIME_CHECK_MASK_NORMAL,
        }
    }

    /// Set current depth for logging
    pub fn set_current_depth(&mut self, depth: u8) {
        self.current_depth = depth;
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

    #[test]
    fn test_ponder_hit_timing() {
        use crate::time_management::{TimeManager, TimeParameters};
        use std::{thread, time::Duration};

        let mut context = SearchContext::new();

        // Set up ponder mode with a ponder hit flag
        let ponder_hit_flag = Arc::new(AtomicBool::new(false));
        let mut limits = SearchLimits::builder()
            .time_control(TimeControl::Ponder(Box::new(TimeControl::Infinite)))
            .build();
        limits.ponder_hit_flag = Some(ponder_hit_flag.clone());

        // Track the start time
        let start = context.start_time;
        context.set_limits(limits);

        // Create a mock TimeManager to capture ponder_hit call
        let time_params = TimeParameters::default();
        let time_limits = crate::time_management::TimeLimits {
            time_control: TimeControl::Infinite,
            moves_to_go: None,
            depth: None,
            nodes: None,
            time_parameters: Some(time_params),
        };
        let time_manager = Arc::new(TimeManager::new(
            &time_limits,
            crate::shogi::Color::Black,
            0,
            crate::time_management::GamePhase::Opening,
        ));

        // Sleep for a measurable amount of time
        thread::sleep(Duration::from_millis(10));

        // Trigger ponder hit
        ponder_hit_flag.store(true, Ordering::Release);

        // Capture elapsed time before process_events
        let elapsed_before = start.elapsed().as_millis() as u64;

        // Process events should capture elapsed time BEFORE converting
        context.process_events(&Some(time_manager.clone()));

        // Verify that elapsed time was captured correctly
        assert!(
            elapsed_before >= 5,
            "Ponder elapsed time should be at least 5ms, got {elapsed_before}ms"
        );

        // Verify that start_time was reset after conversion
        let elapsed_after_convert = context.start_time.elapsed().as_millis() as u64;
        assert!(
            elapsed_after_convert < elapsed_before,
            "Start time should be reset after conversion. Before: {elapsed_before}ms, After: {elapsed_after_convert}ms"
        );

        // Verify that ponder was converted
        assert!(context.ponder_converted);
    }

    #[test]
    fn test_time_check_mask_efficiency() {
        use crate::search::constants::{TIME_CHECK_MASK_BYOYOMI, TIME_CHECK_MASK_NORMAL};

        // Test that mask check is efficient
        let mask_byoyomi = TIME_CHECK_MASK_BYOYOMI;
        let mask_normal = TIME_CHECK_MASK_NORMAL;

        let mut hits_byoyomi = 0;
        let mut hits_normal = 0;

        for i in 0..100_000 {
            if (i & mask_byoyomi) == 0 {
                hits_byoyomi += 1;
            }
            if (i & mask_normal) == 0 {
                hits_normal += 1;
            }
        }

        // TIME_CHECK_MASK_BYOYOMI = 0x7FF = 2047, so check every 2048 nodes
        let expected_byoyomi: i32 = 100_000 / 2048;
        assert!((hits_byoyomi - expected_byoyomi).abs() <= 2,
            "Byoyomi mask should check approximately every 2048 nodes, got {hits_byoyomi} hits, expected around {expected_byoyomi}");

        // TIME_CHECK_MASK_NORMAL = 0x1FFF = 8191, so check every 8192 nodes
        let expected_normal: i32 = 100_000 / 8192;
        assert!((hits_normal - expected_normal).abs() <= 2,
            "Normal mask should check approximately every 8192 nodes, got {hits_normal} hits, expected around {expected_normal}");
    }

    #[test]
    fn test_search_context_time_check_mask() {
        use crate::search::constants::{TIME_CHECK_MASK_BYOYOMI, TIME_CHECK_MASK_NORMAL};

        let mut ctx = SearchContext::new();

        // Test normal time control
        ctx.set_limits(SearchLimits::default());
        assert_eq!(ctx.get_time_check_mask(), TIME_CHECK_MASK_NORMAL);

        // Test byoyomi
        let byoyomi_limits = SearchLimits::builder()
            .time_control(TimeControl::Byoyomi {
                main_time_ms: 0,
                byoyomi_ms: 6000,
                periods: 1,
            })
            .build();
        ctx.set_limits(byoyomi_limits);
        assert_eq!(ctx.get_time_check_mask(), TIME_CHECK_MASK_BYOYOMI);

        // Test fixed nodes (should use more frequent checks)
        let node_limits = SearchLimits::builder()
            .time_control(TimeControl::FixedNodes { nodes: 100000 })
            .build();
        ctx.set_limits(node_limits);
        assert_eq!(ctx.get_time_check_mask(), 0x3FF); // 1024 nodes

        // Test ponder mode (should use normal mask)
        let ponder_limits = SearchLimits::builder()
            .time_control(TimeControl::Ponder(Box::new(TimeControl::Infinite)))
            .build();
        ctx.set_limits(ponder_limits);
        assert_eq!(ctx.get_time_check_mask(), TIME_CHECK_MASK_NORMAL);
    }
}
