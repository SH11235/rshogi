//! Time management module for the Shogi engine
//!
//! This module handles all time-related decisions during search, including:
//! - Time allocation for different time control modes
//! - Dynamic time adjustment based on position complexity
//! - Search termination decisions based on time constraints

use parking_lot::Mutex;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use crate::search::GamePhase;
use crate::Color;

mod allocation;
mod parameters;
mod types;

pub use allocation::calculate_time_allocation;
pub use parameters::TimeParameters;
pub use types::{ByoyomiInfo, SearchLimits, TimeControl, TimeInfo};

/// Time manager coordinating all time-related decisions
pub struct TimeManager {
    inner: Arc<TimeManagerInner>,
}

/// Internal state shared between threads
struct TimeManagerInner {
    // === Immutable after initialization ===
    time_control: TimeControl,
    side_to_move: Color,
    #[allow(dead_code)] // May be used in future for advanced time management
    start_ply: u32,
    params: TimeParameters,

    // === Mutable state (Atomic/Mutex) ===
    // Time tracking (Mutex to avoid UB with Instant)
    start_time: Mutex<Instant>,

    // Limits (Atomic for lock-free access)
    soft_limit_ms: AtomicU64,
    hard_limit_ms: AtomicU64,
    #[allow(dead_code)] // Reserved for dynamic overhead adjustment
    overhead_ms: AtomicU64,

    // Search state
    nodes_searched: AtomicU64,
    stop_flag: AtomicBool,

    // PV stability tracking
    last_pv_change_ms: AtomicU64, // Milliseconds since start
    pv_threshold_ms: AtomicU64,   // Stability threshold

    // Byoyomi-specific state
    byoyomi_state: Mutex<ByoyomiState>,
}

/// Byoyomi (Japanese overtime) state management
#[derive(Debug, Clone, Default)]
struct ByoyomiState {
    periods_left: u32,
    current_period_ms: u64,
}

impl TimeManager {
    /// Create a new time manager for a search
    pub fn new(limits: &SearchLimits, side: Color, ply: u32, game_phase: GamePhase) -> Self {
        let params = limits.time_parameters.clone().unwrap_or_default();

        // Calculate initial time allocation
        let (soft_ms, hard_ms) = calculate_time_allocation(
            &limits.time_control,
            side,
            ply,
            limits.moves_to_go,
            game_phase,
            &params,
        );

        // Initialize byoyomi state if needed
        let byoyomi_state = match &limits.time_control {
            TimeControl::Byoyomi {
                periods,
                byoyomi_ms,
                ..
            } => ByoyomiState {
                periods_left: *periods,
                current_period_ms: *byoyomi_ms,
            },
            _ => ByoyomiState::default(),
        };

        let inner = Arc::new(TimeManagerInner {
            time_control: limits.time_control.clone(),
            side_to_move: side,
            start_ply: ply,
            params: params.clone(),
            start_time: Mutex::new(Instant::now()),
            soft_limit_ms: AtomicU64::new(soft_ms),
            hard_limit_ms: AtomicU64::new(hard_ms),
            overhead_ms: AtomicU64::new(params.overhead_ms),
            nodes_searched: AtomicU64::new(0),
            stop_flag: AtomicBool::new(false),
            last_pv_change_ms: AtomicU64::new(0),
            pv_threshold_ms: AtomicU64::new(params.pv_base_threshold_ms),
            byoyomi_state: Mutex::new(byoyomi_state),
        });

        Self { inner }
    }

    /// Check if search should stop (called frequently from search loop)
    pub fn should_stop(&self, current_nodes: u64) -> bool {
        // Check force stop flag first (cheapest check)
        if self.inner.stop_flag.load(Ordering::Acquire) {
            return true;
        }

        // Update nodes searched (using fetch_max to avoid lost updates)
        self.inner.nodes_searched.fetch_max(current_nodes, Ordering::Relaxed);

        // Check node limit
        if let TimeControl::FixedNodes { nodes } = &self.inner.time_control {
            if current_nodes >= *nodes {
                return true;
            }
        }

        // Time-based checks
        let elapsed = self.elapsed_ms();

        // Hard limit always stops
        let hard_limit = self.inner.hard_limit_ms.load(Ordering::Acquire);
        if elapsed >= hard_limit {
            return true;
        }

        // Soft limit with PV stability check
        let soft_limit = self.inner.soft_limit_ms.load(Ordering::Acquire);
        if elapsed >= soft_limit && self.is_pv_stable() {
            return true;
        }

        // Emergency stop if critically low on time
        if self.is_time_critical() {
            return true;
        }

        false
    }

    /// Notify when PV changes (for stability-based time extension)
    pub fn on_pv_change(&self, depth: u32) {
        let now_ms = self.elapsed_ms();
        self.inner.last_pv_change_ms.store(now_ms, Ordering::Relaxed);

        // Adjust threshold based on depth
        let threshold = self.inner.params.pv_base_threshold_ms
            + (depth as u64 * self.inner.params.pv_depth_slope_ms);
        self.inner.pv_threshold_ms.store(threshold, Ordering::Relaxed);
    }

    /// Force immediate stop (user interrupt)
    pub fn force_stop(&self) {
        self.inner.stop_flag.store(true, Ordering::Release);
    }

    /// Get elapsed time since search start
    pub fn elapsed_ms(&self) -> u64 {
        let start = self.inner.start_time.lock();
        start.elapsed().as_millis() as u64
    }

    /// Update remaining time after move (for Fischer/Byoyomi)
    /// 
    /// For Byoyomi time control, this method manages period consumption:
    /// - If time_spent_ms > byoyomi_ms, one period is consumed
    /// - If all periods are consumed, sets stop flag for time forfeit
    /// 
    /// Note: The original TimeControl settings remain unchanged.
    /// Current state is tracked internally and exposed via get_time_info().
    pub fn finish_move(&self, _color: Color, time_spent_ms: u64) {
        match &self.inner.time_control {
            TimeControl::Byoyomi { byoyomi_ms, .. } => {
                let mut state = self.inner.byoyomi_state.lock();

                // Check if we're in byoyomi
                if state.current_period_ms > 0 {
                    if time_spent_ms > *byoyomi_ms {
                        // Consumed one period
                        state.periods_left = state.periods_left.saturating_sub(1);
                        state.current_period_ms = *byoyomi_ms;

                        if state.periods_left == 0 {
                            // Time forfeit - set stop flag
                            self.inner.stop_flag.store(true, Ordering::Release);
                        }
                    } else {
                        // Still within period
                        state.current_period_ms = *byoyomi_ms;
                    }
                }
            }
            _ => {
                // Fischer and other modes: time update handled by GUI
            }
        }
    }

    /// Get current time information (for USI/logging)
    pub fn get_time_info(&self) -> TimeInfo {
        let elapsed = self.elapsed_ms();
        let nodes = self.inner.nodes_searched.load(Ordering::Relaxed);

        // Calculate time pressure
        let hard_limit = self.inner.hard_limit_ms.load(Ordering::Relaxed);
        let time_pressure = if hard_limit == u64::MAX {
            0.0
        } else {
            (elapsed as f32 / hard_limit as f32).min(1.0)
        };

        // Get byoyomi info if applicable
        let byoyomi_info = match &self.inner.time_control {
            TimeControl::Byoyomi { .. } => {
                let state = self.inner.byoyomi_state.lock();
                Some(ByoyomiInfo {
                    in_byoyomi: state.current_period_ms > 0,
                    periods_left: state.periods_left,
                    current_period_ms: state.current_period_ms,
                })
            }
            _ => None,
        };

        TimeInfo {
            elapsed_ms: elapsed,
            soft_limit_ms: self.inner.soft_limit_ms.load(Ordering::Relaxed),
            hard_limit_ms: hard_limit,
            nodes_searched: nodes,
            time_pressure,
            byoyomi_info,
        }
    }

    /// Handle ponder hit (convert ponder to normal search)
    pub fn ponder_hit(&self) {
        if matches!(self.inner.time_control, TimeControl::Ponder) {
            // In real implementation, would recalculate time limits
            // For now, just clear the infinite time
            self.inner.soft_limit_ms.store(1000, Ordering::Relaxed);
            self.inner.hard_limit_ms.store(2000, Ordering::Relaxed);
        }
    }

    /// Check if PV is stable (no recent changes)
    fn is_pv_stable(&self) -> bool {
        let now_ms = self.elapsed_ms();
        let last_change = self.inner.last_pv_change_ms.load(Ordering::Acquire);
        let threshold = self.inner.pv_threshold_ms.load(Ordering::Acquire);

        now_ms.saturating_sub(last_change) > threshold
    }

    /// Check if we're critically low on time
    fn is_time_critical(&self) -> bool {
        match &self.inner.time_control {
            TimeControl::Fischer {
                white_ms,
                black_ms,
                increment_ms,
            } => {
                let remain = if self.inner.side_to_move == Color::White {
                    *white_ms
                } else {
                    *black_ms
                };
                remain < self.inner.params.critical_fischer_ms && *increment_ms == 0
            }
            TimeControl::Byoyomi { .. } => {
                let state = self.inner.byoyomi_state.lock();
                state.current_period_ms < self.inner.params.critical_byoyomi_ms
            }
            TimeControl::FixedTime { .. } => {
                // Check for overrun
                let elapsed = self.elapsed_ms();
                let hard = self.inner.hard_limit_ms.load(Ordering::Acquire);
                elapsed > hard * 11 / 10 // 110% exceeded
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_limits() -> SearchLimits {
        SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            moves_to_go: None,
            depth: None,
            nodes: None,
            time_parameters: None,
        }
    }

    #[test]
    fn test_time_manager_creation() {
        let limits = create_test_limits();
        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);
        let info = tm.get_time_info();

        assert_eq!(info.elapsed_ms, 0);
        assert!(info.soft_limit_ms > 0);
        assert!(info.hard_limit_ms > info.soft_limit_ms);
    }

    #[test]
    fn test_force_stop() {
        let limits = create_test_limits();
        let tm = TimeManager::new(&limits, Color::Black, 20, GamePhase::MiddleGame);

        assert!(!tm.should_stop(0));
        tm.force_stop();
        assert!(tm.should_stop(0));
    }
}
