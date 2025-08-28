//! Time management module for the Shogi engine
//!
//! This module handles all time-related decisions during search, including:
//! - Time allocation for different time control modes
//! - Dynamic time adjustment based on position complexity
//! - Search termination decisions based on time constraints
//!
//! # Byoyomi State Management
//!
//! For Byoyomi time control, the initial settings are immutable in `TimeControl::Byoyomi`,
//! while the runtime state (remaining periods) is tracked internally in `ByoyomiState`.
//! This separation ensures:
//! - TimeControl remains a pure configuration type
//! - State changes don't affect the original settings
//! - Current state is accessible via `TimeInfo::byoyomi_info`

use lazy_static::lazy_static;
use log::warn;
use parking_lot::{Mutex, RwLock};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;

use crate::Color;

// Use the new game_phase module integration
mod game_phase_integration;
pub use game_phase_integration::{
    detect_game_phase_for_time, estimate_moves_remaining_by_phase, GamePhase,
};

mod allocation;
mod byoyomi;
mod parameters;
mod ponder;
mod state;
mod types;

#[cfg(test)]
mod test_utils;
#[cfg(test)]
mod tests;

pub use allocation::calculate_time_allocation;
#[cfg(test)]
pub use allocation::estimate_moves_remaining;
pub use byoyomi::ByoyomiState;
pub use parameters::{constants, TimeParameterError, TimeParameters, TimeParametersBuilder};
pub use types::{ByoyomiInfo, TimeControl, TimeInfo, TimeLimits, TimeState};

/// Time manager coordinating all time-related decisions
pub struct TimeManager {
    inner: Arc<TimeManagerInner>,
}

#[cfg(test)]
pub use test_utils::{mock_advance_time, mock_now, mock_set_time};

/// Internal state shared between threads
struct TimeManagerInner {
    side_to_move: Color,
    start_ply: u32,
    params: TimeParameters,
    game_phase: GamePhase, // Game phase at creation time

    // === Mutable state (Atomic/Mutex) ===
    // Active time control (can change after ponder_hit)
    // Using RwLock for better read performance in hot path
    active_time_control: RwLock<TimeControl>,

    // Time tracking (AtomicU64 for lock-free access in hot path)
    start_mono_ms: AtomicU64, // Milliseconds since monotonic base

    // Limits (Atomic for lock-free access)
    soft_limit_ms: AtomicU64,
    hard_limit_ms: AtomicU64,

    // Search state
    nodes_searched: AtomicU64,
    stop_flag: AtomicBool,

    // PV stability tracking
    last_pv_change_ms: AtomicU64, // Milliseconds since start
    pv_threshold_ms: AtomicU64,   // Stability threshold

    // Byoyomi-specific state
    byoyomi_state: Mutex<ByoyomiState>,

    // Ponder-specific state
    is_ponder: AtomicBool, // Whether currently pondering
}

lazy_static! {
    static ref MONO_BASE: Instant = {
        #[cfg(debug_assertions)]
        {
            use std::sync::atomic::{AtomicBool, Ordering};
            static MONO_BASE_INIT_STARTED: AtomicBool = AtomicBool::new(false);
            if MONO_BASE_INIT_STARTED.swap(true, Ordering::SeqCst) {
                panic!("MONO_BASE initialization re-entered! Circular dependency detected.");
            }
            // Debug output removed to prevent I/O deadlock in subprocess context
        }

        // Debug output removed to prevent I/O deadlock in subprocess context
        Instant::now()
    };
}

/// Get current monotonic time in milliseconds since process start
#[inline]
pub(crate) fn monotonic_ms() -> u64 {
    MONO_BASE.elapsed().as_millis() as u64
}

// Test-only: flag to enable mock time usage
#[cfg(test)]
pub(crate) static USE_MOCK_TIME: AtomicBool = AtomicBool::new(false);

impl TimeManager {
    /// Create a new time manager for a search
    pub fn new(limits: &TimeLimits, side: Color, ply: u32, game_phase: GamePhase) -> Self {
        let params = limits.time_parameters.unwrap_or_default();

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
        let byoyomi_state = byoyomi::ByoyomiManager::init_state(&limits.time_control);

        let inner = Arc::new(TimeManagerInner {
            side_to_move: side,
            start_ply: ply,
            params,
            game_phase,
            active_time_control: RwLock::new(limits.time_control.clone()),
            start_mono_ms: AtomicU64::new(monotonic_ms()),
            soft_limit_ms: AtomicU64::new(soft_ms),
            hard_limit_ms: AtomicU64::new(hard_ms),
            nodes_searched: AtomicU64::new(0),
            stop_flag: AtomicBool::new(false),
            last_pv_change_ms: AtomicU64::new(0),
            pv_threshold_ms: AtomicU64::new(params.pv_base_threshold_ms),
            byoyomi_state: Mutex::new(byoyomi_state),
            is_ponder: AtomicBool::new(matches!(&limits.time_control, TimeControl::Ponder(_))),
        });

        Self { inner }
    }

    /// Create a new time manager for pondering
    #[inline]
    pub fn new_ponder(
        pending_limits: &TimeLimits,
        side: Color,
        ply: u32,
        game_phase: GamePhase,
    ) -> Self {
        // Create ponder limits
        let ponder_limits = ponder::PonderManager::create_ponder_limits(pending_limits);

        // Create TimeManager with ponder mode
        let tm = Self::new(&ponder_limits, side, ply, game_phase);

        // Initialize byoyomi state if pending time control is Byoyomi
        if let TimeControl::Byoyomi {
            periods,
            byoyomi_ms,
            main_time_ms,
        } = &pending_limits.time_control
        {
            let mut byoyomi_state = tm.inner.byoyomi_state.lock();
            *byoyomi_state = ByoyomiState {
                periods_left: *periods,
                current_period_ms: *byoyomi_ms,
                in_byoyomi: *main_time_ms == 0,
            };
        }

        tm
    }

    /// Check if currently pondering
    #[inline]
    pub fn is_pondering(&self) -> bool {
        self.inner.is_ponder.load(Ordering::Acquire)
    }

    /// Get the active time control (helper for internal use)
    ///
    /// Returns a read guard that should be dropped as soon as possible
    /// to avoid blocking other threads. Current usage is minimal (match/if let),
    /// but be mindful of lock duration if code complexity increases.
    #[inline]
    fn get_active_time_control(&self) -> parking_lot::RwLockReadGuard<'_, TimeControl> {
        self.inner.active_time_control.read()
    }

    /// Create a new time manager with mock time for testing
    #[cfg(test)]
    pub fn new_with_mock_time(
        limits: &TimeLimits,
        side: Color,
        ply: u32,
        game_phase: GamePhase,
    ) -> Self {
        // In test mode, get_epoch_ms() already uses mock time
        Self::new(limits, side, ply, game_phase)
    }

    /// Check if search should stop (called frequently from search loop)
    pub fn should_stop(&self, current_nodes: u64) -> bool {
        // Check force stop flag first (cheapest check)
        if self.inner.stop_flag.load(Ordering::Acquire) {
            return true;
        }

        // If pondering, only stop on force_stop
        if self.is_pondering() {
            return false;
        }

        // Update nodes searched (using fetch_max to avoid lost updates)
        // Note: This ignores node count decreases, which is acceptable as it's
        // extremely rare in practice. If history reset is needed, a separate
        // reset_nodes() API with swap(0, Ordering::Relaxed) would be cleaner.
        self.inner.nodes_searched.fetch_max(current_nodes, Ordering::Relaxed);

        // Check node limit
        // TODO: For future optimization, consider caching TimeControl variant in AtomicU8
        // to avoid RwLock acquisition in hot path (especially for non-FixedNodes cases)
        let active_tc = self.get_active_time_control();
        if let TimeControl::FixedNodes { nodes } = &*active_tc {
            if current_nodes >= *nodes {
                return true;
            }
        }

        // Time-based checks
        let elapsed = self.elapsed_ms();

        // Hard limit always stops
        let hard_limit = self.inner.hard_limit_ms.load(Ordering::Relaxed);
        if elapsed >= hard_limit {
            return true;
        }

        // Soft limit with PV stability check
        let soft_limit = self.inner.soft_limit_ms.load(Ordering::Relaxed);
        if elapsed >= soft_limit && self.state_checker().is_pv_stable(elapsed) {
            return true;
        }

        // Emergency stop if critically low on time
        if self.state_checker().is_time_critical() {
            return true;
        }

        false
    }

    /// Notify when PV changes (for stability-based time extension)
    pub fn on_pv_change(&self, depth: u32) {
        let checker = self.state_checker();
        checker.on_pv_change(depth, self.elapsed_ms());
    }

    /// Force immediate stop (user interrupt)
    pub fn force_stop(&self) {
        self.inner.stop_flag.store(true, Ordering::Release);
    }

    /// Get elapsed time since search start
    pub fn elapsed_ms(&self) -> u64 {
        let now_ms = monotonic_ms();
        let start_ms = self.inner.start_mono_ms.load(Ordering::Relaxed);
        now_ms.saturating_sub(start_ms)
    }

    /// Get soft time limit in milliseconds
    pub fn soft_limit_ms(&self) -> u64 {
        self.inner.soft_limit_ms.load(Ordering::Relaxed)
    }

    /// Get hard time limit in milliseconds
    pub fn hard_limit_ms(&self) -> u64 {
        self.inner.hard_limit_ms.load(Ordering::Relaxed)
    }

    /// Build StopInfo for TimeLimit termination using current state
    pub fn build_stop_info(&self, depth_reached: u8, nodes: u64) -> crate::search::types::StopInfo {
        use crate::search::types::{StopInfo, TerminationReason};
        let elapsed_ms = self.elapsed_ms();
        let hard = elapsed_ms >= self.hard_limit_ms();
        StopInfo {
            reason: TerminationReason::TimeLimit,
            elapsed_ms,
            nodes,
            depth_reached,
            hard_timeout: hard,
            soft_limit_ms: self.soft_limit_ms(),
            hard_limit_ms: self.hard_limit_ms(),
        }
    }

    /// Get current time control
    pub fn time_control(&self) -> TimeControl {
        self.inner.active_time_control.read().clone()
    }

    /// Update time after move completion (recommended API)
    ///
    /// # Arguments
    /// - `time_spent_ms`: Time spent on this move
    /// - `time_state`: Current time state (required for proper Byoyomi transition)
    ///
    /// # Example
    /// ```
    /// use engine_core::time_management::{TimeManager, TimeState, TimeLimits, TimeControl, GamePhase};
    /// use engine_core::Color;
    ///
    /// let limits = TimeLimits {
    ///     time_control: TimeControl::Byoyomi {
    ///         main_time_ms: 150000,
    ///         byoyomi_ms: 10000,
    ///         periods: 3,
    ///     },
    ///     ..Default::default()
    /// };
    ///
    /// let time_manager = TimeManager::new(&limits, Color::Black, 20, GamePhase::MiddleGame);
    ///
    /// // USI: go btime 150000 wtime 140000 byoyomi 10000
    /// let time_state = TimeState::Main { main_left_ms: 150000 };
    /// time_manager.update_after_move(2000, time_state);
    /// ```
    pub fn update_after_move(&self, time_spent_ms: u64, time_state: TimeState) {
        let active_tc = self.get_active_time_control();
        match (&*active_tc, time_state) {
            (
                TimeControl::Byoyomi { byoyomi_ms, .. },
                TimeState::Main { main_left_ms } | TimeState::Byoyomi { main_left_ms },
            ) => {
                let manager = self.byoyomi_manager();
                manager.handle_update(time_spent_ms, Some(main_left_ms), *byoyomi_ms);
            }
            (TimeControl::Byoyomi { .. }, TimeState::NonByoyomi) => {
                warn!("TimeState::NonByoyomi used with Byoyomi time control - ignoring update");
                debug_assert!(false, "TimeState::NonByoyomi used with Byoyomi time control");
            }
            _ => {
                // Fischer and other modes: time update handled by GUI
            }
        }
    }

    /// Get current time information (for USI/logging)
    pub fn get_time_info(&self) -> TimeInfo {
        self.state_checker().get_time_info(self.elapsed_ms())
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
        let manager = self.ponder_manager();
        manager.ponder_hit(new_limits, time_already_spent_ms);
    }

    /// Get byoyomi-specific information
    ///
    /// Returns None if not using byoyomi time control.
    /// Returns Some((periods_left, current_period_ms, in_byoyomi)) for byoyomi.
    pub fn get_byoyomi_state(&self) -> Option<(u32, u64, bool)> {
        let active_tc = self.get_active_time_control();
        match &*active_tc {
            TimeControl::Byoyomi { .. } => {
                let manager = self.byoyomi_manager();
                Some(manager.get_state())
            }
            _ => None,
        }
    }

    // Helper methods to create managers
    #[cfg(not(test))]
    fn byoyomi_manager(&self) -> byoyomi::ByoyomiManager<'_> {
        byoyomi::ByoyomiManager {
            byoyomi_state: &self.inner.byoyomi_state,
            stop_flag: &self.inner.stop_flag,
        }
    }

    #[cfg(test)]
    pub fn byoyomi_manager(&self) -> byoyomi::ByoyomiManager<'_> {
        byoyomi::ByoyomiManager {
            byoyomi_state: &self.inner.byoyomi_state,
            stop_flag: &self.inner.stop_flag,
        }
    }

    #[cfg(not(test))]
    fn ponder_manager(&self) -> ponder::PonderManager<'_> {
        ponder::PonderManager {
            is_ponder: &self.inner.is_ponder,
            active_time_control: &self.inner.active_time_control,
            soft_limit_ms: &self.inner.soft_limit_ms,
            hard_limit_ms: &self.inner.hard_limit_ms,
            start_mono_ms: &self.inner.start_mono_ms,
            byoyomi_state: &self.inner.byoyomi_state,
            side_to_move: self.inner.side_to_move,
            start_ply: self.inner.start_ply,
            game_phase: self.inner.game_phase,
            params: self.inner.params,
            last_pv_change_ms: &self.inner.last_pv_change_ms,
            pv_threshold_ms: &self.inner.pv_threshold_ms,
        }
    }

    #[cfg(test)]
    pub fn ponder_manager(&self) -> ponder::PonderManager<'_> {
        ponder::PonderManager {
            is_ponder: &self.inner.is_ponder,
            active_time_control: &self.inner.active_time_control,
            soft_limit_ms: &self.inner.soft_limit_ms,
            hard_limit_ms: &self.inner.hard_limit_ms,
            start_mono_ms: &self.inner.start_mono_ms,
            byoyomi_state: &self.inner.byoyomi_state,
            side_to_move: self.inner.side_to_move,
            start_ply: self.inner.start_ply,
            game_phase: self.inner.game_phase,
            params: self.inner.params,
            last_pv_change_ms: &self.inner.last_pv_change_ms,
            pv_threshold_ms: &self.inner.pv_threshold_ms,
        }
    }

    #[cfg(not(test))]
    fn state_checker(&self) -> state::StateChecker<'_> {
        state::StateChecker {
            active_time_control: &self.inner.active_time_control,
            last_pv_change_ms: &self.inner.last_pv_change_ms,
            pv_threshold_ms: &self.inner.pv_threshold_ms,
            hard_limit_ms: &self.inner.hard_limit_ms,
            soft_limit_ms: &self.inner.soft_limit_ms,
            nodes_searched: &self.inner.nodes_searched,
            byoyomi_state: &self.inner.byoyomi_state,
            side_to_move: self.inner.side_to_move,
            params: &self.inner.params,
        }
    }

    #[cfg(test)]
    pub fn state_checker(&self) -> state::StateChecker<'_> {
        state::StateChecker {
            active_time_control: &self.inner.active_time_control,
            last_pv_change_ms: &self.inner.last_pv_change_ms,
            pv_threshold_ms: &self.inner.pv_threshold_ms,
            hard_limit_ms: &self.inner.hard_limit_ms,
            soft_limit_ms: &self.inner.soft_limit_ms,
            nodes_searched: &self.inner.nodes_searched,
            byoyomi_state: &self.inner.byoyomi_state,
            side_to_move: self.inner.side_to_move,
            params: &self.inner.params,
        }
    }
}
