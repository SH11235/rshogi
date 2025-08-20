//! Byoyomi (Japanese overtime) time control implementation
//!
//! This module handles byoyomi-specific time management, including:
//! - State tracking for byoyomi periods
//! - Time consumption and period management
//! - Transition from main time to byoyomi

use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::time_management::TimeControl;

/// Byoyomi (Japanese overtime) state management
///
/// This struct tracks the runtime state of byoyomi time control,
/// separate from the immutable configuration in TimeControl::Byoyomi.
#[derive(Debug, Clone, Default)]
pub struct ByoyomiState {
    pub periods_left: u32,
    pub current_period_ms: u64,
    pub in_byoyomi: bool, // Whether main time is exhausted
}

/// Byoyomi-specific functionality for TimeManager
pub struct ByoyomiManager<'a> {
    pub(crate) byoyomi_state: &'a Mutex<ByoyomiState>,
    pub(crate) stop_flag: &'a AtomicBool,
}

impl<'a> ByoyomiManager<'a> {
    /// Update time after move completion for Byoyomi
    pub fn handle_update(
        &self,
        time_spent_ms: u64,
        main_time_left_ms: Option<u64>,
        byoyomi_ms: u64,
    ) {
        let mut state = self.byoyomi_state.lock();

        if !state.in_byoyomi {
            // Still in main time
            // Check if we should transition to byoyomi based on GUI's report
            if let Some(main_left) = main_time_left_ms {
                if main_left == 0 || time_spent_ms >= main_left {
                    // Transition to byoyomi
                    state.in_byoyomi = true;
                    // If we overspent, handle it with the byoyomi period consumption logic
                    if time_spent_ms > main_left {
                        let overspent = time_spent_ms - main_left;
                        // Drop the lock and recursively call to handle overspent time
                        // Note: This recursion is bounded to max one level (main->byoyomi transition)
                        drop(state);
                        self.handle_update(overspent, None, byoyomi_ms);
                    }
                }
            }
        } else {
            // In byoyomi - handle multiple period consumption
            let mut remaining_time = time_spent_ms;
            let mut current_ms = state.current_period_ms;

            // Consume periods while time exceeds current period
            while remaining_time >= current_ms && state.periods_left > 0 {
                remaining_time -= current_ms;
                state.periods_left = state.periods_left.saturating_sub(1);
                current_ms = byoyomi_ms; // Reset to full period
            }

            if state.periods_left == 0 {
                // Time forfeit - set stop flag
                self.stop_flag.store(true, Ordering::Release);
                state.current_period_ms = 0;
            } else {
                // Set remaining time in current period
                state.current_period_ms = current_ms.saturating_sub(remaining_time);
            }
        }
    }

    /// Get byoyomi-specific information
    ///
    /// Returns (periods_left, current_period_ms, in_byoyomi)
    pub fn get_state(&self) -> (u32, u64, bool) {
        let state = self.byoyomi_state.lock();
        (state.periods_left, state.current_period_ms, state.in_byoyomi)
    }

    /// Initialize byoyomi state from time control
    pub fn init_state(time_control: &TimeControl) -> ByoyomiState {
        match time_control {
            TimeControl::Byoyomi {
                periods,
                byoyomi_ms,
                main_time_ms,
            } => ByoyomiState {
                periods_left: *periods,
                current_period_ms: *byoyomi_ms,
                in_byoyomi: *main_time_ms == 0, // Start in byoyomi if no main time
            },
            _ => ByoyomiState::default(),
        }
    }
}
