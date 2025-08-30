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
    // Phase 1 budget (optimum). Minimum/maximum will be introduced when needed.
    opt_limit_ms: AtomicU64,
    // Planned rounded stop time (u64::MAX = unset)
    search_end_ms: AtomicU64,

    // Final push (byoyomi) controls
    final_push_active: AtomicBool,
    final_push_min_ms: AtomicU64, // Minimum think time to use (cannot stop before this)

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

    // Budget status
    budget_clamped: AtomicBool,
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
        let (raw_soft, raw_hard) = calculate_time_allocation(
            &limits.time_control,
            side,
            ply,
            limits.moves_to_go,
            game_phase,
            &params,
        );

        // Apply conservative lower bounds and ordering clamps (small, safe)
        let mut soft_ms = raw_soft;
        let mut hard_ms = raw_hard;
        let mut budget_clamped = false;

        // Only clamp when budgets are finite
        if soft_ms != u64::MAX && hard_ms != u64::MAX {
            let lower = match &limits.time_control {
                TimeControl::Byoyomi { .. } => params.critical_byoyomi_ms.max(50),
                TimeControl::Fischer { .. } => params.critical_fischer_ms.max(50),
                TimeControl::FixedTime { .. } => 50,
                _ => 0,
            };

            if lower > 0 {
                if hard_ms < lower {
                    hard_ms = lower;
                    budget_clamped = true;
                }
                if soft_ms < lower {
                    soft_ms = lower;
                    budget_clamped = true;
                }
                if soft_ms >= hard_ms {
                    let new_soft = hard_ms.saturating_sub(1);
                    if new_soft != soft_ms {
                        soft_ms = new_soft;
                        budget_clamped = true;
                    }
                }
            }

            // Apply MinThinkMs to soft limit for finite time controls (not Infinite/FixedNodes/Ponder)
            // Ensures we allow at least one committed iteration before soft stop, unless budgets are extremely tight.
            let eligible = matches!(
                &limits.time_control,
                TimeControl::Fischer { .. }
                    | TimeControl::Byoyomi { .. }
                    | TimeControl::FixedTime { .. }
            );
            if eligible {
                let min_think = params.min_think_ms;
                if min_think > 0 && soft_ms < min_think {
                    soft_ms = min_think;
                    budget_clamped = true;
                }

                // Enforce margin: soft <= hard - 50ms (δ=50ms)
                if soft_ms.saturating_add(50) > hard_ms {
                    let new_soft = hard_ms.saturating_sub(50);
                    if new_soft != soft_ms {
                        soft_ms = new_soft;
                        budget_clamped = true;
                    }
                }
            }
        }

        // Initialize byoyomi state if needed
        let byoyomi_state = byoyomi::ByoyomiManager::init_state(&limits.time_control);

        // Derive simple optimum budget from soft (Phase 1)
        let opt_limit = soft_ms;

        let inner = Arc::new(TimeManagerInner {
            side_to_move: side,
            start_ply: ply,
            params,
            game_phase,
            active_time_control: RwLock::new(limits.time_control.clone()),
            start_mono_ms: AtomicU64::new(monotonic_ms()),
            soft_limit_ms: AtomicU64::new(soft_ms),
            hard_limit_ms: AtomicU64::new(hard_ms),
            opt_limit_ms: AtomicU64::new(opt_limit),
            search_end_ms: AtomicU64::new(u64::MAX),
            final_push_active: AtomicBool::new(false),
            final_push_min_ms: AtomicU64::new(0),
            nodes_searched: AtomicU64::new(0),
            stop_flag: AtomicBool::new(false),
            last_pv_change_ms: AtomicU64::new(0),
            pv_threshold_ms: AtomicU64::new(params.pv_base_threshold_ms),
            byoyomi_state: Mutex::new(byoyomi_state),
            is_ponder: AtomicBool::new(matches!(&limits.time_control, TimeControl::Ponder(_))),
            budget_clamped: AtomicBool::new(budget_clamped),
        });

        let tm = Self { inner };

        // Final push activation (strict): enable when already in byoyomi (main_time == 0)
        if let TimeControl::Byoyomi {
            main_time_ms,
            byoyomi_ms,
            ..
        } = &limits.time_control
        {
            if *main_time_ms == 0 {
                let worst = tm.inner.params.network_delay2_ms;
                let avg = tm.inner.params.overhead_ms;
                let min_ms = byoyomi_ms.saturating_sub(worst).saturating_sub(avg);
                tm.inner.final_push_active.store(true, Ordering::Relaxed);
                tm.inner.final_push_min_ms.store(min_ms, Ordering::Relaxed);
                // Ensure opt covers minimum but never above hard
                let hard = tm.inner.hard_limit_ms.load(Ordering::Relaxed);
                let target_opt = min_ms.min(hard);
                let _ = tm.inner.opt_limit_ms.fetch_max(target_opt, Ordering::Relaxed);
                log::debug!(
                    "[FinalPush] active (in byoyomi): period={}ms, min_ms={}ms (worst={}, avg={})",
                    byoyomi_ms,
                    min_ms,
                    worst,
                    avg
                );
            }
        }

        tm
    }

    /// Whether initial budgets were clamped to maintain sane bounds/order
    #[inline]
    pub fn budgets_were_clamped(&self) -> bool {
        self.inner.budget_clamped.load(Ordering::Relaxed)
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

        // Planned rounded stop (Phase 1)
        let planned = self.inner.search_end_ms.load(Ordering::Relaxed);
        if planned != u64::MAX && elapsed >= planned {
            return true;
        }

        // Soft limit with PV stability → schedule rounded stop instead of immediate stop
        let soft_limit = self.inner.soft_limit_ms.load(Ordering::Relaxed);
        if elapsed >= soft_limit && self.state_checker().is_pv_stable(elapsed) {
            // Schedule a rounded stop near the next second boundary
            self.set_search_end(elapsed);

            // After scheduling, stop only when we reach the scheduled end
            let scheduled = self.inner.search_end_ms.load(Ordering::Relaxed);
            if scheduled != u64::MAX && elapsed >= scheduled {
                return true;
            }
            return false;
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

    /// Get opt time budget (Phase 1)
    pub fn opt_limit_ms(&self) -> u64 {
        self.inner.opt_limit_ms.load(Ordering::Relaxed)
    }

    /// Get scheduled rounded stop time (ms since start) or u64::MAX if unset
    pub fn scheduled_end_ms(&self) -> u64 {
        self.inner.search_end_ms.load(Ordering::Relaxed)
    }

    /// Compute a rounded stop target: next second boundary minus average overhead
    fn round_up(&self, elapsed_ms: u64) -> u64 {
        let next_sec = ((elapsed_ms / 1000).saturating_add(1)).saturating_mul(1000);
        let overhead = self.inner.params.overhead_ms;
        // Ensure rounding does not go backwards
        let mut target = next_sec.saturating_sub(overhead);
        if target <= elapsed_ms {
            target = elapsed_ms.saturating_add(1);
        }
        // Never exceed hard limit
        let hard = self.inner.hard_limit_ms.load(Ordering::Relaxed);
        if hard != u64::MAX && target > hard {
            target = hard;
        }
        target
    }

    /// Schedule a rounded stop time, respecting final-push minimum and hard limit
    fn set_search_end(&self, elapsed_ms: u64) {
        let hard = self.inner.hard_limit_ms.load(Ordering::Relaxed);
        if hard == u64::MAX {
            // No scheduling for infinite/ponder modes
            return;
        }

        let mut target = self.round_up(elapsed_ms);

        // Respect final push minimum (cannot exceed hard)
        if self.inner.final_push_active.load(Ordering::Relaxed) {
            let min_ms = self.inner.final_push_min_ms.load(Ordering::Relaxed);
            if target < min_ms {
                target = min_ms.min(hard);
            }
        }

        // Remain-time upper clamp (lightweight safety)
        if let Some(rem) = self.remain_upper_ms() {
            if target > rem {
                target = rem;
            }
        }

        let current = self.inner.search_end_ms.load(Ordering::Relaxed);
        if current == u64::MAX || target < current {
            self.inner.search_end_ms.store(target, Ordering::Relaxed);
            log::debug!(
                "[TimeBudget] schedule stop at {}ms (elapsed={}, hard={})",
                target,
                elapsed_ms,
                hard
            );
        }
    }

    /// Conservative remain-time upper bound (ms since start),
    /// subtracting NetworkDelay2 and average overhead as safety margins.
    fn remain_upper_ms(&self) -> Option<u64> {
        let overhead = self.inner.params.overhead_ms;
        let nd2 = self.inner.params.network_delay2_ms;
        let tc = self.get_active_time_control();
        match &*tc {
            TimeControl::Fischer {
                white_ms,
                black_ms,
                increment_ms: _,
            } => {
                let remain = if self.inner.side_to_move == crate::Color::White {
                    *white_ms
                } else {
                    *black_ms
                };
                Some(remain.saturating_sub(nd2).saturating_sub(overhead).max(50))
            }
            TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                ..
            } => {
                if self.is_in_byoyomi() {
                    Some(byoyomi_ms.saturating_sub(nd2).saturating_sub(overhead).max(50))
                } else {
                    Some(main_time_ms.saturating_sub(nd2).saturating_sub(overhead).max(50))
                }
            }
            TimeControl::FixedTime { ms_per_move } => {
                Some(ms_per_move.saturating_sub(overhead).max(50))
            }
            _ => None,
        }
    }

    /// Are we currently in byoyomi period (Phase 1 helper)?
    pub fn is_in_byoyomi(&self) -> bool {
        let active_tc = self.get_active_time_control();
        if let TimeControl::Byoyomi { .. } = &*active_tc {
            let st = self.inner.byoyomi_state.lock();
            st.in_byoyomi
        } else {
            false
        }
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

    /// Phase 1: Advise a rounded stop after finishing an iteration
    pub fn advise_after_iteration(&self, elapsed_ms: u64) {
        let opt = self.inner.opt_limit_ms.load(Ordering::Relaxed);
        let hard = self.inner.hard_limit_ms.load(Ordering::Relaxed);
        if hard == u64::MAX {
            return;
        }
        if elapsed_ms >= opt {
            // First, schedule a rounded stop based on next second boundary
            self.set_search_end(elapsed_ms);

            // Also ensure we don't plan past a conservative near-hard deadline
            let safety_ms = if hard >= 500 {
                let three_percent = hard.saturating_mul(3) / 100;
                three_percent.clamp(120, 400)
            } else if hard >= 200 {
                40
            } else {
                0
            };
            let mut deadline = hard.saturating_sub(safety_ms);
            // Do not tighten below final-push minimum when active
            if self.inner.final_push_active.load(Ordering::Relaxed) {
                let min_ms = self.inner.final_push_min_ms.load(Ordering::Relaxed);
                if deadline < min_ms {
                    deadline = min_ms;
                }
            }
            let current = self.inner.search_end_ms.load(Ordering::Relaxed);
            if current == u64::MAX || deadline < current {
                self.inner.search_end_ms.store(deadline, Ordering::Relaxed);
                log::debug!(
                    "[TimeBudget] tighten schedule to {}ms (elapsed={}, opt={}, hard={}, safety={})",
                    deadline,
                    elapsed_ms,
                    opt,
                    hard,
                    safety_ms
                );
            }
        }
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
            final_push_active: &self.inner.final_push_active,
            final_push_min_ms: &self.inner.final_push_min_ms,
            opt_limit_ms: &self.inner.opt_limit_ms,
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
            final_push_active: &self.inner.final_push_active,
            final_push_min_ms: &self.inner.final_push_min_ms,
            opt_limit_ms: &self.inner.opt_limit_ms,
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
