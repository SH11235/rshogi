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

use parking_lot::{Mutex, RwLock};
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

#[cfg(test)]
mod test_utils;

pub use allocation::calculate_time_allocation;
pub use parameters::TimeParameters;
pub use types::{ByoyomiInfo, SearchLimits, TimeControl, TimeInfo, TimeState};

/// Time manager coordinating all time-related decisions
pub struct TimeManager {
    inner: Arc<TimeManagerInner>,
}

#[cfg(test)]
pub use test_utils::{mock_advance_time, mock_current_ms, mock_now, mock_set_time};

/// Internal state shared between threads
struct TimeManagerInner {
    // === Immutable after initialization ===
    #[allow(dead_code)]
    time_control: TimeControl, // Initial time control (kept for reference)
    side_to_move: Color,
    #[allow(dead_code)] // May be used in future for advanced time management
    start_ply: u32,
    params: TimeParameters,
    game_phase: GamePhase, // Game phase at creation time

    // === Mutable state (Atomic/Mutex) ===
    // Active time control (can change after ponder_hit)
    // Using RwLock for better read performance in hot path
    active_time_control: RwLock<TimeControl>,

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

    // Ponder-specific state
    pending_time_control: Mutex<Option<TimeControl>>, // Time control to switch to after ponder_hit
    is_ponder: AtomicBool,                            // Whether currently pondering
}

/// Byoyomi (Japanese overtime) state management
///
/// This struct tracks the runtime state of byoyomi time control,
/// separate from the immutable configuration in TimeControl::Byoyomi.
#[derive(Debug, Clone, Default)]
struct ByoyomiState {
    periods_left: u32,
    current_period_ms: u64,
    in_byoyomi: bool, // Whether main time is exhausted
}

impl TimeManager {
    /// Create a new time manager for a search
    pub fn new(limits: &SearchLimits, side: Color, ply: u32, game_phase: GamePhase) -> Self {
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
        let byoyomi_state = match &limits.time_control {
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
        };

        let inner = Arc::new(TimeManagerInner {
            time_control: limits.time_control.clone(),
            side_to_move: side,
            start_ply: ply,
            params,
            game_phase,
            active_time_control: RwLock::new(limits.time_control.clone()),
            start_time: Mutex::new(Instant::now()),
            soft_limit_ms: AtomicU64::new(soft_ms),
            hard_limit_ms: AtomicU64::new(hard_ms),
            overhead_ms: AtomicU64::new(params.overhead_ms),
            nodes_searched: AtomicU64::new(0),
            stop_flag: AtomicBool::new(false),
            last_pv_change_ms: AtomicU64::new(0),
            pv_threshold_ms: AtomicU64::new(params.pv_base_threshold_ms),
            byoyomi_state: Mutex::new(byoyomi_state),
            pending_time_control: Mutex::new(None),
            is_ponder: AtomicBool::new(matches!(&limits.time_control, TimeControl::Ponder)),
        });

        Self { inner }
    }

    /// Create a new time manager for pondering
    #[inline]
    pub fn new_ponder(
        pending_limits: &SearchLimits,
        side: Color,
        ply: u32,
        game_phase: GamePhase,
    ) -> Self {
        // Create ponder limits with infinite time
        let ponder_limits = SearchLimits {
            time_control: TimeControl::Ponder,
            moves_to_go: pending_limits.moves_to_go,
            depth: pending_limits.depth,
            nodes: pending_limits.nodes,
            time_parameters: pending_limits.time_parameters,
        };

        // Create TimeManager with ponder mode
        let tm = Self::new(&ponder_limits, side, ply, game_phase);

        // Store the pending time control
        {
            let mut pending = tm.inner.pending_time_control.lock();
            *pending = Some(pending_limits.time_control.clone());
        }

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
    fn new_with_mock_time(
        limits: &SearchLimits,
        side: Color,
        ply: u32,
        game_phase: GamePhase,
    ) -> Self {
        let tm = Self::new(limits, side, ply, game_phase);
        // Replace start_time with mock time
        {
            let mut start_time = tm.inner.start_time.lock();
            *start_time = mock_now();
        } // MutexGuard is dropped here
        tm
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
        let active_tc = self.get_active_time_control();
        if let TimeControl::FixedNodes { nodes } = &*active_tc {
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
        #[cfg(test)]
        {
            // In test mode, use mock time
            let now = mock_now();
            now.duration_since(*start).as_millis() as u64
        }
        #[cfg(not(test))]
        {
            start.elapsed().as_millis() as u64
        }
    }

    /// Update time after move completion (recommended API)
    ///
    /// # Arguments
    /// - `time_spent_ms`: Time spent on this move
    /// - `time_state`: Current time state (required for proper Byoyomi transition)
    ///
    /// # Example
    /// ```
    /// use engine_core::time_management::{TimeManager, TimeState, SearchLimits, TimeControl};
    /// use engine_core::search::GamePhase;
    /// use engine_core::Color;
    ///
    /// let limits = SearchLimits {
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
                self.handle_byoyomi_update(time_spent_ms, Some(main_left_ms), *byoyomi_ms);
            }
            (TimeControl::Byoyomi { .. }, TimeState::NonByoyomi) => {
                debug_assert!(false, "TimeState::NonByoyomi used with Byoyomi time control");
            }
            _ => {
                // Fischer and other modes: time update handled by GUI
            }
        }
    }

    /// Internal helper for Byoyomi time updates
    fn handle_byoyomi_update(
        &self,
        time_spent_ms: u64,
        main_time_left_ms: Option<u64>,
        byoyomi_ms: u64,
    ) {
        let mut state = self.inner.byoyomi_state.lock();

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
                        self.handle_byoyomi_update(overspent, None, byoyomi_ms);
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
                self.inner.stop_flag.store(true, Ordering::Release);
                state.current_period_ms = 0;
            } else {
                // Set remaining time in current period
                state.current_period_ms = current_ms.saturating_sub(remaining_time);
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
            0.0 // During ponder or infinite search, no time pressure
        } else {
            (elapsed as f32 / hard_limit as f32).min(1.0)
        };

        // Get byoyomi info if applicable (consistent lock order: RwLock before Mutex)
        let byoyomi_info = {
            let active_tc = self.inner.active_time_control.read();
            match &*active_tc {
                TimeControl::Byoyomi { .. } => {
                    let state = self.inner.byoyomi_state.lock();
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
            elapsed_ms: elapsed,
            soft_limit_ms: self.inner.soft_limit_ms.load(Ordering::Relaxed),
            hard_limit_ms: hard_limit,
            nodes_searched: nodes,
            time_pressure,
            byoyomi_info,
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
    pub fn ponder_hit(&self, new_limits: Option<&SearchLimits>, time_already_spent_ms: u64) {
        // Check if currently pondering
        if !self.is_pondering() {
            return;
        }

        // Get the actual time control from new_limits or pending_time_control
        let (actual_time_control, moves_to_go, params) = if let Some(limits) = new_limits {
            (
                limits.time_control.clone(),
                limits.moves_to_go,
                limits.time_parameters.unwrap_or_default(),
            )
        } else {
            // Use pending time control if no new limits provided
            let pending = self.inner.pending_time_control.lock();
            if let Some(ref tc) = *pending {
                (tc.clone(), None, self.inner.params)
            } else {
                // Fallback: Set conservative limits
                self.inner.soft_limit_ms.store(1000, Ordering::Release);
                self.inner.hard_limit_ms.store(2000, Ordering::Release);
                self.inner.is_ponder.store(false, Ordering::Release);
                return;
            }
        };

        // Calculate new time allocation using saved game phase
        let (soft_ms, hard_ms) = calculate_time_allocation(
            &actual_time_control,
            self.inner.side_to_move,
            self.inner.start_ply,
            moves_to_go,
            self.inner.game_phase, // Use saved game phase
            &params,
        );

        // Adjust for time already spent
        let adjusted_soft = soft_ms.saturating_sub(time_already_spent_ms).max(100);
        let adjusted_hard = hard_ms.saturating_sub(time_already_spent_ms).max(200);

        // Ensure soft <= hard invariant with reasonable margin
        // Use 50% of hard limit for soft limit when extremely time-constrained
        let adjusted_soft = if adjusted_soft >= adjusted_hard {
            adjusted_hard / 2
        } else {
            adjusted_soft
        };

        // Update limits atomically
        self.inner.soft_limit_ms.store(adjusted_soft, Ordering::Release);
        self.inner.hard_limit_ms.store(adjusted_hard, Ordering::Release);

        // Update active time control first (consistent lock order: RwLock before Mutex)
        {
            let mut active_tc = self.inner.active_time_control.write();
            *active_tc = actual_time_control.clone();
        }

        // Then initialize byoyomi state if needed
        if let TimeControl::Byoyomi {
            periods,
            byoyomi_ms,
            main_time_ms,
        } = &actual_time_control
        {
            let mut byoyomi = self.inner.byoyomi_state.lock();
            *byoyomi = ByoyomiState {
                periods_left: *periods,
                current_period_ms: *byoyomi_ms,
                in_byoyomi: *main_time_ms == 0,
            };
        }

        // Reset start time to avoid double counting
        {
            let mut start = self.inner.start_time.lock();
            #[cfg(test)]
            {
                *start = mock_now();
            }
            #[cfg(not(test))]
            {
                *start = Instant::now();
            }
        }

        // Clear ponder flag
        self.inner.is_ponder.store(false, Ordering::Release);
    }

    /// Get byoyomi-specific information
    ///
    /// Returns None if not using byoyomi time control.
    /// Returns Some((periods_left, current_period_ms, in_byoyomi)) for byoyomi.
    pub fn get_byoyomi_state(&self) -> Option<(u32, u64, bool)> {
        let active_tc = self.get_active_time_control();
        match &*active_tc {
            TimeControl::Byoyomi { .. } => {
                let state = self.inner.byoyomi_state.lock();
                Some((state.periods_left, state.current_period_ms, state.in_byoyomi))
            }
            _ => None,
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
        let active_tc = self.get_active_time_control();
        match &*active_tc {
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
                // Critical if in byoyomi and low on period time
                state.in_byoyomi && state.current_period_ms < self.inner.params.critical_byoyomi_ms
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

    #[test]
    fn test_byoyomi_exact_boundary() {
        // Test exact boundary condition: time_spent == byoyomi_ms
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 0,
                byoyomi_ms: 1000,
                periods: 3,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

        // Spend exactly one period
        tm.update_after_move(1000, TimeState::Byoyomi { main_left_ms: 0 });
        let state = tm.get_byoyomi_state().unwrap();
        assert_eq!(state.0, 2); // Should have 2 periods left
        assert_eq!(state.1, 1000); // Should reset to full period
        assert!(state.2); // Should be in byoyomi
    }

    #[test]
    fn test_byoyomi_multiple_period_consumption() {
        // Test consuming multiple periods in one move
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 0,
                byoyomi_ms: 1000,
                periods: 5,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

        // Spend 2.5 periods worth of time
        tm.update_after_move(2500, TimeState::Byoyomi { main_left_ms: 0 });
        let state = tm.get_byoyomi_state().unwrap();
        assert_eq!(state.0, 3); // Should have consumed 2 periods, 3 left
        assert_eq!(state.1, 500); // 500ms left in current period
    }

    #[test]
    fn test_byoyomi_main_time_transition() {
        // Test transition from main time to byoyomi
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 5000,
                byoyomi_ms: 1000,
                periods: 3,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

        // Not in byoyomi initially
        let state = tm.get_byoyomi_state().unwrap();
        assert!(!state.2); // Should not be in byoyomi

        // Transition to byoyomi when main time runs out
        tm.update_after_move(3000, TimeState::Main { main_left_ms: 2000 }); // 2s left, spent 3s
        let state = tm.get_byoyomi_state().unwrap();
        assert!(state.2); // Should now be in byoyomi
        assert_eq!(state.0, 2); // Overspent by 1s, so consumed 1 period
        assert_eq!(state.1, 1000); // Full period remains after consuming the overspent period

        // Another move that consumes a period
        tm.update_after_move(1500, TimeState::Byoyomi { main_left_ms: 0 });
        let state = tm.get_byoyomi_state().unwrap();
        assert_eq!(state.0, 1); // Should have 1 period left (consumed 1 from the 2 remaining)
        assert_eq!(state.1, 500); // 500ms left in current period
    }

    #[test]
    fn test_pv_stability_with_mock_time() {
        // Test PV stability tracking with MockClock
        mock_set_time(0);

        let limits = create_test_limits();
        let tm = TimeManager::new_with_mock_time(&limits, Color::White, 0, GamePhase::Opening);

        // Initial PV change
        tm.on_pv_change(10);
        assert!(!tm.is_pv_stable()); // Just changed

        // Advance time but not enough for stability
        mock_advance_time(50);
        assert!(!tm.is_pv_stable()); // 50ms < 80ms base + 10*5ms = 130ms

        // Advance past threshold
        mock_advance_time(100); // Total 150ms
        assert!(tm.is_pv_stable()); // 150ms > 130ms

        // Another PV change at deeper depth
        tm.on_pv_change(20);
        assert!(!tm.is_pv_stable()); // Just changed again

        // Deeper searches require more stability
        mock_advance_time(150); // Need 80 + 20*5 = 180ms
        assert!(!tm.is_pv_stable()); // 150ms < 180ms

        mock_advance_time(50); // Total 200ms
        assert!(tm.is_pv_stable()); // 200ms > 180ms
    }

    #[test]
    fn test_time_pressure_calculation() {
        mock_set_time(0);

        let limits = SearchLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 1000 },
            ..Default::default()
        };

        let tm = TimeManager::new_with_mock_time(&limits, Color::Black, 0, GamePhase::MiddleGame);

        // Initially no pressure
        let info = tm.get_time_info();
        assert!(info.time_pressure < 0.1);

        // Half way through
        mock_advance_time(500);
        let info = tm.get_time_info();
        assert!((info.time_pressure - 0.5).abs() < 0.1);

        // Near hard limit
        mock_advance_time(450); // Total 950ms
        let info = tm.get_time_info();
        assert!(info.time_pressure > 0.9);
    }

    #[test]
    fn test_emergency_stop_with_mock_time() {
        mock_set_time(0);

        // Test Fischer emergency stop
        let limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 200, // Critical time
                black_ms: 200,
                increment_ms: 0,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_with_mock_time(&limits, Color::White, 100, GamePhase::EndGame);
        assert!(tm.is_time_critical()); // Should be critical immediately

        // Test Byoyomi emergency stop
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 0,
                byoyomi_ms: 1000,
                periods: 1,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_with_mock_time(&limits, Color::Black, 80, GamePhase::EndGame);

        // Use most of the period
        tm.update_after_move(950, TimeState::Byoyomi { main_left_ms: 0 });
        assert!(tm.is_time_critical()); // Only 50ms left < 80ms critical threshold
    }

    #[test]
    fn test_soft_limit_extension() {
        mock_set_time(0);

        let limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_with_mock_time(&limits, Color::White, 20, GamePhase::MiddleGame);
        let info = tm.get_time_info();
        let soft_limit = info.soft_limit_ms;

        // Advance to soft limit
        mock_advance_time(soft_limit);

        // With unstable PV, should not stop
        tm.on_pv_change(15);
        assert!(!tm.should_stop(1000));

        // Advance past soft limit but PV still unstable
        mock_advance_time(50);
        assert!(!tm.should_stop(2000)); // Should continue searching

        // Make PV stable
        mock_advance_time(200); // Well past stability threshold
        assert!(tm.should_stop(3000)); // Now should stop
    }

    #[test]
    fn test_byoyomi_time_forfeit() {
        // Test time forfeit when all periods consumed
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 0,
                byoyomi_ms: 1000,
                periods: 2,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

        // Consume all periods
        tm.update_after_move(2000, TimeState::Byoyomi { main_left_ms: 0 }); // Consume both periods

        // Should trigger stop flag
        assert!(tm.should_stop(0));

        let state = tm.get_byoyomi_state().unwrap();
        assert_eq!(state.0, 0); // No periods left
        assert_eq!(state.1, 0); // No time left
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn test_byoyomi_transition_with_wrong_state() {
        // Test that using wrong TimeState doesn't cause transition
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 5000,
                byoyomi_ms: 1000,
                periods: 3,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

        // Using NonByoyomi state with Byoyomi time control
        // This should not cause transition to byoyomi
        tm.update_after_move(2000, TimeState::NonByoyomi);
        tm.update_after_move(2000, TimeState::NonByoyomi);
        tm.update_after_move(2000, TimeState::NonByoyomi); // Total 6s spent

        let state = tm.get_byoyomi_state().unwrap();
        assert!(!state.2); // Still not in byoyomi - wrong TimeState prevents transition
    }

    #[test]
    fn test_byoyomi_proper_transition_with_new_api() {
        // Test proper transition with new API
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 5000,
                byoyomi_ms: 1000,
                periods: 3,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);

        // First move: still in main time
        tm.update_after_move(2000, TimeState::Main { main_left_ms: 3000 });
        let state = tm.get_byoyomi_state().unwrap();
        assert!(!state.2); // Still in main time

        // Second move: transition to byoyomi
        tm.update_after_move(3500, TimeState::Main { main_left_ms: 3000 });

        let state = tm.get_byoyomi_state().unwrap();
        assert!(state.2); // Now in byoyomi
        assert_eq!(state.0, 3); // All periods still available
        assert_eq!(state.1, 500); // 500ms left in first period (overspent by 500ms)
    }

    #[test]
    fn test_byoyomi_transition_with_multiple_period_overspend() {
        // Test transition from main time to byoyomi with overspend > 2 periods
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 1000,
                byoyomi_ms: 1000,
                periods: 5,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::EndGame);

        // Overspend main time by 2.5 periods worth
        tm.update_after_move(3500, TimeState::Main { main_left_ms: 1000 });

        let state = tm.get_byoyomi_state().unwrap();
        assert!(state.2); // In byoyomi
        assert_eq!(state.0, 3); // Should have consumed 2 periods (2500ms), 3 left
        assert_eq!(state.1, 500); // 500ms left in current period
    }

    #[test]
    fn test_continuous_byoyomi_to_time_forfeit() {
        // Test main=0 start → consume all periods → time forfeit
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 0, // Start in byoyomi
                byoyomi_ms: 1000,
                periods: 2,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::EndGame);

        // Consume first period
        tm.update_after_move(1200, TimeState::Byoyomi { main_left_ms: 0 });
        assert!(!tm.should_stop(0));

        let state = tm.get_byoyomi_state().unwrap();
        assert_eq!(state.0, 1); // One period left
        assert_eq!(state.1, 800); // 800ms left in current period

        // Consume second period - time forfeit
        tm.update_after_move(1500, TimeState::Byoyomi { main_left_ms: 0 });
        assert!(tm.should_stop(0)); // Time forfeit

        let state = tm.get_byoyomi_state().unwrap();
        assert_eq!(state.0, 0); // No periods left
        assert_eq!(state.1, 0); // No time left
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "TimeState::NonByoyomi used with Byoyomi")]
    fn test_debug_assertion_on_wrong_time_state() {
        // Test debug assertion fires on API misuse
        let limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 5000,
                byoyomi_ms: 1000,
                periods: 3,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);
        tm.update_after_move(1000, TimeState::NonByoyomi); // Wrong state!
    }

    #[test]
    fn test_new_api_with_various_time_controls() {
        // Test that new API works correctly with non-byoyomi time controls
        let fischer_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&fischer_limits, Color::White, 0, GamePhase::Opening);
        tm.update_after_move(2000, TimeState::NonByoyomi); // Should work fine

        let fixed_limits = SearchLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 1000 },
            ..Default::default()
        };

        let tm2 = TimeManager::new(&fixed_limits, Color::Black, 0, GamePhase::MiddleGame);
        tm2.update_after_move(500, TimeState::NonByoyomi); // Should work fine
    }

    #[test]
    fn test_ponder_to_fischer() {
        mock_set_time(0);

        // Create pending limits with Fischer time control
        let pending_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        // Create TimeManager in ponder mode
        let tm = TimeManager::new_ponder(&pending_limits, Color::White, 0, GamePhase::Opening);

        // Verify it's pondering
        assert!(tm.is_pondering());
        assert!(!tm.should_stop(1000)); // Should not stop during ponder

        // Simulate 5 seconds of pondering
        mock_advance_time(5000);

        // Ponder hit
        tm.ponder_hit(None, 5000);

        // Verify ponder mode is off
        assert!(!tm.is_pondering());

        // Check time allocation is adjusted for spent time
        let info = tm.get_time_info();
        assert!(info.soft_limit_ms < 60000 - 5000); // Less than remaining time
        assert!(info.hard_limit_ms < 60000 - 5000);
    }

    #[test]
    fn test_ponder_to_byoyomi() {
        mock_set_time(0);

        // Create pending limits with Byoyomi
        let pending_limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 10000, // 10 seconds main time
                byoyomi_ms: 30000,   // 30 seconds per period
                periods: 3,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_ponder(&pending_limits, Color::Black, 40, GamePhase::MiddleGame);
        assert!(tm.is_pondering());

        // Ponder for 3 seconds
        mock_advance_time(3000);
        tm.ponder_hit(None, 3000);

        assert!(!tm.is_pondering());

        // Should have conservative allocation from remaining main time
        let info = tm.get_time_info();
        assert!(info.soft_limit_ms > 0);
        assert!(info.soft_limit_ms < 10000 - 3000); // Less than remaining main time
    }

    #[test]
    fn test_ponder_edge_cases() {
        mock_set_time(0);

        // Test 1: Ponder hit with spent > remain
        let pending_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 2000, // Only 2 seconds
                black_ms: 2000,
                increment_ms: 0,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_ponder(&pending_limits, Color::White, 80, GamePhase::EndGame);

        // Ponder for 5 seconds (more than available time)
        mock_advance_time(5000);
        tm.ponder_hit(None, 5000);

        // Should have minimal time allocation
        let info = tm.get_time_info();
        assert_eq!(info.soft_limit_ms, 100); // Minimum safety limit
        assert_eq!(info.hard_limit_ms, 200);

        // Test 2: Force stop during ponder
        let tm2 = TimeManager::new_ponder(&pending_limits, Color::Black, 0, GamePhase::Opening);
        assert!(!tm2.should_stop(1000)); // Normal check returns false

        tm2.force_stop();
        assert!(tm2.should_stop(1000)); // Force stop works even during ponder
    }

    #[test]
    fn test_ponder_with_new_limits() {
        mock_set_time(0);

        // Initial pending limits
        let pending_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 50000,
                black_ms: 50000,
                increment_ms: 500,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_ponder(&pending_limits, Color::White, 20, GamePhase::MiddleGame);

        mock_advance_time(2000);

        // New limits provided at ponder hit (e.g., opponent's time updated)
        let new_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 48000, // Updated time
                black_ms: 45000,
                increment_ms: 500,
            },
            moves_to_go: Some(30), // Additional info
            ..Default::default()
        };

        tm.ponder_hit(Some(&new_limits), 2000);

        // Should use new limits for calculation
        let info = tm.get_time_info();
        assert!(info.soft_limit_ms > 0);
        assert!(info.soft_limit_ms < 48000 - 2000);
    }

    #[test]
    fn test_active_time_control_switch() {
        mock_set_time(0);

        // Start with Fischer in pending
        let pending_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_ponder(&pending_limits, Color::White, 0, GamePhase::Opening);

        // Initially, active time control should be Ponder
        assert!(tm.is_pondering());
        // No byoyomi state since we're not in byoyomi mode
        assert!(tm.get_byoyomi_state().is_none());

        // After ponder hit, active time control should be Fischer
        tm.ponder_hit(None, 1000);
        assert!(!tm.is_pondering());

        // Still no byoyomi state (Fischer mode)
        assert!(tm.get_byoyomi_state().is_none());

        // Test with Byoyomi
        let byoyomi_limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 5000,
                byoyomi_ms: 10000,
                periods: 3,
            },
            ..Default::default()
        };

        let tm2 = TimeManager::new_ponder(&byoyomi_limits, Color::Black, 0, GamePhase::MiddleGame);
        tm2.ponder_hit(None, 1000);

        // Now should have byoyomi state
        let state = tm2.get_byoyomi_state();
        assert!(state.is_some());
        let (periods, _, in_byoyomi) = state.unwrap();
        assert_eq!(periods, 3);
        assert!(!in_byoyomi); // Still in main time
    }

    #[test]
    fn test_ponder_hit_concurrent_access() {
        use std::sync::Arc;
        use std::thread;

        mock_set_time(0);

        let pending_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        let tm =
            Arc::new(TimeManager::new_ponder(&pending_limits, Color::White, 0, GamePhase::Opening));

        // Clone for threads
        let tm1 = Arc::clone(&tm);
        let tm2 = Arc::clone(&tm);

        // Thread 1: Continuously check should_stop
        let handle1 = thread::spawn(move || {
            let mut nodes = 0u64;
            for _ in 0..1000 {
                nodes += 1;
                let should_stop = tm1.should_stop(nodes);
                // During ponder, should not stop
                if tm1.is_pondering() {
                    assert!(!should_stop);
                }
                // Yield to other threads to increase chance of race
                thread::yield_now();
            }
        });

        // Thread 2: Call ponder_hit after ensuring thread 1 has started
        let handle2 = thread::spawn(move || {
            // Yield multiple times to let thread 1 get going
            for _ in 0..10 {
                thread::yield_now();
            }
            tm2.ponder_hit(None, 1000);
        });

        // Wait for both threads
        handle1.join().unwrap();
        handle2.join().unwrap();

        // Verify final state
        assert!(!tm.is_pondering());
    }

    #[test]
    fn test_fischer_to_byoyomi_switch() {
        mock_set_time(0);

        // Start with Fischer in pending
        let fischer_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_ponder(&fischer_limits, Color::White, 0, GamePhase::Opening);

        // Provide new Byoyomi limits at ponder hit
        let byoyomi_limits = SearchLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 10000,
                byoyomi_ms: 5000,
                periods: 5,
            },
            ..Default::default()
        };

        tm.ponder_hit(Some(&byoyomi_limits), 2000);

        // Should now have byoyomi state
        let state = tm.get_byoyomi_state();
        assert!(state.is_some());
        let (periods, period_ms, in_byoyomi) = state.unwrap();
        assert_eq!(periods, 5);
        assert_eq!(period_ms, 5000);
        assert!(!in_byoyomi);
    }

    #[test]
    fn test_elapsed_time_after_ponder_hit() {
        mock_set_time(0);

        let pending_limits = SearchLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        let tm = TimeManager::new_ponder(&pending_limits, Color::White, 0, GamePhase::Opening);

        // Ponder for 5 seconds
        mock_advance_time(5000);
        let elapsed_before = tm.elapsed_ms();
        assert!(elapsed_before >= 4999 && elapsed_before <= 5001); // Allow small variance

        // After ponder hit, elapsed should reset
        tm.ponder_hit(None, 5000);
        let elapsed_after = tm.elapsed_ms();
        assert!(elapsed_after <= 1); // Should be reset (allow 1ms for execution time)

        // Advance more time
        mock_advance_time(2000);
        let elapsed_final = tm.elapsed_ms();
        assert!(elapsed_final >= 1999 && elapsed_final <= 2001); // Should count from ponder_hit
    }
}
