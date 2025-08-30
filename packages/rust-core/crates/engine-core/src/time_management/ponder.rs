//! Ponder (think on opponent's time) functionality
//!
//! This module handles ponder mode, which allows the engine to think
//! during the opponent's turn and continue calculation if the expected move is played.

use parking_lot::{Mutex, RwLock};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::time_management::{
    allocation::calculate_time_allocation, ByoyomiState, GamePhase, TimeControl, TimeLimits,
    TimeParameters,
};
use crate::Color;

/// Ponder-specific functionality for TimeManager
pub struct PonderManager<'a> {
    pub(crate) is_ponder: &'a AtomicBool,
    pub(crate) active_time_control: &'a RwLock<TimeControl>,
    pub(crate) soft_limit_ms: &'a AtomicU64,
    pub(crate) hard_limit_ms: &'a AtomicU64,
    pub(crate) start_mono_ms: &'a AtomicU64,
    pub(crate) byoyomi_state: &'a Mutex<ByoyomiState>,
    pub(crate) side_to_move: Color,
    pub(crate) start_ply: u32,
    pub(crate) game_phase: GamePhase,
    pub(crate) params: TimeParameters,
    pub(crate) last_pv_change_ms: &'a AtomicU64,
    pub(crate) pv_threshold_ms: &'a AtomicU64,
}

impl<'a> PonderManager<'a> {
    /// Check if currently pondering
    #[inline]
    pub fn is_pondering(&self) -> bool {
        self.is_ponder.load(Ordering::Acquire)
    }

    /// Handle ponder hit (convert ponder to normal search)
    ///
    /// This method should be called when a ponder search becomes a real search
    /// because the opponent played the expected move.
    ///
    /// # Arguments
    /// - `new_limits`: Updated search limits with actual time control
    /// - `time_already_spent_ms`: Time already spent during pondering
    pub fn ponder_hit(&self, new_limits: Option<&TimeLimits>, time_already_spent_ms: u64) {
        // Check if currently pondering
        if !self.is_pondering() {
            log::warn!("PonderManager::ponder_hit called but not pondering");
            return;
        }

        log::info!(
            "PonderManager::ponder_hit called with time_already_spent_ms: {time_already_spent_ms}"
        );

        // Get the actual time control from new_limits or extract from Ponder(inner)
        let (actual_time_control, moves_to_go, params) = if let Some(limits) = new_limits {
            (
                limits.time_control.clone(),
                limits.moves_to_go,
                limits.time_parameters.unwrap_or_default(),
            )
        } else {
            // Extract inner time control from Ponder
            let active_tc = self.active_time_control.read();
            match &*active_tc {
                TimeControl::Ponder(inner) => ((**inner).clone(), None, self.params),
                _ => {
                    log::warn!("ponder_hit: Not in Ponder mode, active_tc = {:?}", *active_tc);
                    log::warn!("ponder_hit: No pending time control, using conservative defaults");
                    // Fallback: Set conservative limits
                    self.soft_limit_ms.store(1000, Ordering::Release);
                    self.hard_limit_ms.store(2000, Ordering::Release);
                    self.is_ponder.store(false, Ordering::Release);
                    return;
                }
            }
        };

        // Calculate new time allocation using saved game phase
        let (soft_ms, hard_ms) = calculate_time_allocation(
            &actual_time_control,
            self.side_to_move,
            self.start_ply,
            moves_to_go,
            self.game_phase, // Use saved game phase
            &params,
        );

        // Adjust for time already spent
        let mut adjusted_soft = soft_ms.saturating_sub(time_already_spent_ms).max(100);
        let adjusted_hard = hard_ms.saturating_sub(time_already_spent_ms).max(200);

        // Apply MinThinkMs lower bound for soft
        if params.min_think_ms > 0 && adjusted_soft < params.min_think_ms {
            adjusted_soft = params.min_think_ms;
        }

        // Ensure soft <= hard - 50ms (reasonable margin)
        if adjusted_soft.saturating_add(50) > adjusted_hard {
            // If extremely constrained, keep at least 50ms difference when possible
            adjusted_soft = adjusted_hard.saturating_sub(50);
        }

        // Update limits atomically
        self.soft_limit_ms.store(adjusted_soft, Ordering::Release);
        self.hard_limit_ms.store(adjusted_hard, Ordering::Release);

        log::info!(
            "PonderManager::ponder_hit - Set time limits: soft={adjusted_soft}ms, hard={adjusted_hard}ms"
        );

        // Update active time control first (consistent lock order: RwLock before Mutex)
        {
            let mut active_tc = self.active_time_control.write();
            *active_tc = actual_time_control.clone();
        }

        // Then initialize byoyomi state if needed
        if let TimeControl::Byoyomi {
            periods,
            byoyomi_ms,
            main_time_ms,
        } = &actual_time_control
        {
            let mut byoyomi = self.byoyomi_state.lock();
            *byoyomi = ByoyomiState {
                periods_left: *periods,
                current_period_ms: *byoyomi_ms,
                in_byoyomi: *main_time_ms == 0,
            };
        }

        // Reset start time to avoid double counting
        use crate::time_management::monotonic_ms;
        self.start_mono_ms.store(monotonic_ms(), Ordering::Release);

        // Reset PV stability tracking to match new time origin
        self.last_pv_change_ms.store(0, Ordering::Release);
        self.pv_threshold_ms.store(self.params.pv_base_threshold_ms, Ordering::Release);

        // Clear ponder flag
        self.is_ponder.store(false, Ordering::Release);
    }

    /// Create ponder limits from pending limits
    pub fn create_ponder_limits(pending_limits: &TimeLimits) -> TimeLimits {
        // Check if already Ponder to avoid double-wrapping
        let time_control = match &pending_limits.time_control {
            TimeControl::Ponder(_) => {
                // Already Ponder, use as-is
                log::debug!("create_ponder_limits: time_control already Ponder, using as-is");
                pending_limits.time_control.clone()
            }
            other => {
                // Wrap in Ponder
                log::debug!("create_ponder_limits: wrapping time_control in Ponder: {other:?}");
                TimeControl::Ponder(Box::new(other.clone()))
            }
        };

        TimeLimits {
            time_control,
            moves_to_go: pending_limits.moves_to_go,
            depth: pending_limits.depth,
            nodes: pending_limits.nodes,
            time_parameters: pending_limits.time_parameters,
        }
    }
}
