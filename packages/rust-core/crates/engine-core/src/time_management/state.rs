//! Time state checking and information retrieval
//!
//! This module provides methods for checking various time-related states
//! and conditions during search.

use parking_lot::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::time_management::types::ByoyomiInfo;
use crate::time_management::{ByoyomiState, TimeControl, TimeInfo, TimeParameters};
use crate::Color;

/// State checking functionality for TimeManager
pub struct StateChecker<'a> {
    pub(crate) active_time_control: &'a RwLock<TimeControl>,
    pub(crate) last_pv_change_ms: &'a AtomicU64,
    pub(crate) pv_threshold_ms: &'a AtomicU64,
    pub(crate) hard_limit_ms: &'a AtomicU64,
    pub(crate) soft_limit_ms: &'a AtomicU64,
    pub(crate) nodes_searched: &'a AtomicU64,
    pub(crate) byoyomi_state: &'a parking_lot::Mutex<ByoyomiState>,
    pub(crate) side_to_move: Color,
    pub(crate) params: &'a TimeParameters,
}

impl<'a> StateChecker<'a> {
    /// Check if PV is stable (no recent changes)
    pub fn is_pv_stable(&self, elapsed_ms: u64) -> bool {
        let last_change = self.last_pv_change_ms.load(Ordering::Relaxed);
        let threshold = self.pv_threshold_ms.load(Ordering::Relaxed);

        elapsed_ms.saturating_sub(last_change) > threshold
    }

    /// Check if we're critically low on time
    pub fn is_time_critical(&self) -> bool {
        let active_tc = self.active_time_control.read();
        match &*active_tc {
            TimeControl::Fischer {
                white_ms,
                black_ms,
                increment_ms,
            } => {
                let remain = if self.side_to_move == Color::White {
                    *white_ms
                } else {
                    *black_ms
                };
                remain < self.params.critical_fischer_ms && *increment_ms == 0
            }
            TimeControl::Byoyomi { .. } => {
                let state = self.byoyomi_state.lock();
                // Critical if in byoyomi and low on period time
                state.in_byoyomi && state.current_period_ms < self.params.critical_byoyomi_ms
            }
            _ => false,
        }
    }

    /// Get current time information (for USI/logging)
    pub fn get_time_info(&self, elapsed_ms: u64) -> TimeInfo {
        let nodes = self.nodes_searched.load(Ordering::Relaxed);

        // Calculate time pressure
        let hard_limit = self.hard_limit_ms.load(Ordering::Relaxed);
        let time_pressure = if hard_limit == u64::MAX {
            0.0 // During ponder or infinite search, no time pressure
        } else {
            (elapsed_ms as f32 / hard_limit as f32).min(1.0)
        };

        // Get byoyomi info if applicable (consistent lock order: RwLock before Mutex)
        let byoyomi_info = {
            let active_tc = self.active_time_control.read();
            match &*active_tc {
                TimeControl::Byoyomi { .. } => {
                    let state = self.byoyomi_state.lock();
                    Some(ByoyomiInfo {
                        in_byoyomi: state.in_byoyomi,
                        periods_left: state.periods_left,
                        current_period_ms: state.current_period_ms,
                    })
                }
                _ => None,
            }
        };

        TimeInfo {
            elapsed_ms,
            soft_limit_ms: self.soft_limit_ms.load(Ordering::Relaxed),
            hard_limit_ms: hard_limit,
            nodes_searched: nodes,
            time_pressure,
            byoyomi_info,
        }
    }

    /// Update PV change tracking
    pub fn on_pv_change(&self, depth: u32, elapsed_ms: u64) {
        self.last_pv_change_ms.store(elapsed_ms, Ordering::Relaxed);

        // Adjust threshold based on depth
        let threshold =
            self.params.pv_base_threshold_ms + (depth as u64 * self.params.pv_depth_slope_ms);
        self.pv_threshold_ms.store(threshold, Ordering::Relaxed);
    }
}
