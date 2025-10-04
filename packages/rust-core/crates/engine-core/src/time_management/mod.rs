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
use rand::{rngs::SmallRng, Rng, SeedableRng};
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
    #[cfg(test)]
    {
        if USE_MOCK_TIME.load(Ordering::Relaxed) {
            return test_utils::mock_current_ms();
        }
    }
    MONO_BASE.elapsed().as_millis() as u64
}

// Test-only: flag to enable mock time usage
#[cfg(test)]
pub(crate) static USE_MOCK_TIME: AtomicBool = AtomicBool::new(false);

impl TimeManager {
    /// Create a new time manager for a search
    pub fn new(limits: &TimeLimits, side: Color, ply: u32, game_phase: GamePhase) -> Self {
        let params = limits.time_parameters.unwrap_or_default();
        // Optional random time override (go rtime)
        let random_override = limits.random_time_ms.map(|base| Self::randomize_rtime(base, ply));
        let rtime_active = random_override.is_some();

        // Calculate initial time allocation (or apply override)
        let (raw_soft, raw_hard) = if let Some(override_ms) = random_override {
            (override_ms, override_ms)
        } else {
            calculate_time_allocation(
                &limits.time_control,
                side,
                ply,
                limits.moves_to_go,
                game_phase,
                &params,
            )
        };

        // Apply conservative lower bounds and ordering clamps (small, safe)
        let mut soft_ms = raw_soft;
        let mut hard_ms = raw_hard;
        let mut budget_clamped = false;
        let mut enforced_min_think = false;

        // Only clamp when budgets are finite
        if !rtime_active && soft_ms != u64::MAX && hard_ms != u64::MAX {
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
                    enforced_min_think = true;
                }

                let margin = Self::min_soft_margin(hard_ms);
                if margin > 0 && soft_ms.saturating_add(margin) > hard_ms {
                    if enforced_min_think {
                        if matches!(&limits.time_control, TimeControl::FixedTime { .. }) {
                            let clamp = margin.max(1);
                            let mut new_soft = hard_ms.saturating_sub(clamp);
                            if new_soft >= hard_ms {
                                new_soft = hard_ms.saturating_sub(1);
                            }
                            if new_soft != soft_ms {
                                soft_ms = new_soft;
                                budget_clamped = true;
                            }
                        } else {
                            let new_hard = soft_ms.saturating_add(margin);
                            if new_hard != hard_ms {
                                hard_ms = new_hard;
                                budget_clamped = true;
                            }
                        }
                    } else {
                        let mut new_soft = hard_ms.saturating_sub(margin);
                        if min_think > 0 {
                            new_soft = new_soft.max(min_think);
                        }
                        if new_soft != soft_ms {
                            soft_ms = new_soft;
                            budget_clamped = true;
                        }
                    }
                }
            }
        }

        // Initialize byoyomi state if needed
        let byoyomi_state = byoyomi::ByoyomiManager::init_state(&limits.time_control);

        // Phase 4: Set opt_limit as YaneuraOu's maximum() equivalent
        // This should be between soft and hard limits, typically 1.5x soft
        let opt_limit = if soft_ms != u64::MAX && hard_ms != u64::MAX {
            // Use 1.5x soft as maximum, but don't exceed 80% of hard limit
            let max_from_soft = soft_ms.saturating_mul(3) / 2; // 1.5x
            let max_from_hard = hard_ms.saturating_mul(8) / 10; // 80% of hard
                                                                // Ensure opt_limit is at least soft_limit to avoid premature scheduling
            max_from_soft.min(max_from_hard).max(soft_ms)
        } else {
            soft_ms // Fallback for infinite modes
        };

        // FATAL check: opt_limit should never be 0 for finite time controls
        if opt_limit == 0
            && matches!(
                &limits.time_control,
                TimeControl::Fischer { .. }
                    | TimeControl::Byoyomi { .. }
                    | TimeControl::FixedTime { .. }
            )
        {
            log::error!(
                "[TimeManager::new] FATAL: opt_limit_ms is 0! soft_ms: {}, hard_ms: {}, time_control: {:?}",
                soft_ms,
                hard_ms,
                limits.time_control
            );
        }

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

        if rtime_active {
            tm.inner.soft_limit_ms.store(raw_soft, Ordering::Relaxed);
            tm.inner.hard_limit_ms.store(raw_soft, Ordering::Relaxed);
            tm.inner.opt_limit_ms.store(raw_soft, Ordering::Relaxed);
            tm.inner.search_end_ms.store(u64::MAX, Ordering::Relaxed);
            tm.inner.final_push_active.store(false, Ordering::Relaxed);
            tm.inner.final_push_min_ms.store(raw_soft, Ordering::Relaxed);
        } else if let TimeControl::Byoyomi {
            main_time_ms,
            byoyomi_ms,
            ..
        } = &limits.time_control
        {
            let final_push_threshold = (*byoyomi_ms as f64 * 1.2) as u64;

            // Activate FinalPush when:
            // 1. Already in byoyomi (main_time == 0), OR
            // 2. Main time < 1.2 * byoyomi period
            if *main_time_ms == 0 || (*byoyomi_ms > 0 && *main_time_ms < final_push_threshold) {
                let worst = tm.inner.params.network_delay2_ms;
                let avg = tm.inner.params.overhead_ms;

                // Calculate minimum time based on whether we're in main time or byoyomi
                let available_time = if *main_time_ms > 0 {
                    // In main time FinalPush: can use main_time + byoyomi
                    main_time_ms + byoyomi_ms
                } else {
                    // Already in byoyomi: can only use byoyomi period
                    *byoyomi_ms
                };

                let guard_floor = params
                    .min_think_ms
                    .max(params.critical_byoyomi_ms)
                    .max(soft_ms.min(hard_ms))
                    .max(50);

                let safe_room = available_time.saturating_sub(guard_floor);
                let worst_clamped = worst.min(safe_room);
                let avg_clamped = avg.min(safe_room.saturating_sub(worst_clamped));

                let mut min_ms = available_time
                    .saturating_sub(worst_clamped)
                    .saturating_sub(avg_clamped)
                    .max(guard_floor);
                if min_ms > hard_ms {
                    min_ms = hard_ms;
                }

                tm.inner.final_push_active.store(true, Ordering::Relaxed);
                tm.inner.final_push_min_ms.store(min_ms, Ordering::Relaxed);

                // In FinalPush, set opt_limit with earlier scheduling for pure-byoyomi.
                let current_hard = tm.inner.hard_limit_ms.load(Ordering::Relaxed);
                let mut target_opt =
                    current_hard.saturating_sub(Self::min_soft_margin(current_hard).max(20));
                if *main_time_ms == 0 {
                    let soft = tm.inner.soft_limit_ms.load(Ordering::Relaxed);
                    target_opt = soft.min(current_hard.saturating_sub(200)).max(soft);
                }
                target_opt = target_opt.max(soft_ms);
                tm.inner.opt_limit_ms.store(target_opt, Ordering::Relaxed);
            }
        }
        tm
    }

    #[inline]
    fn min_soft_margin(hard_ms: u64) -> u64 {
        if hard_ms == u64::MAX {
            0
        } else if hard_ms >= 1_000 {
            50
        } else if hard_ms >= 500 {
            30
        } else if hard_ms >= 200 {
            20
        } else if hard_ms >= 100 {
            15
        } else {
            10
        }
    }

    #[inline]
    fn randomize_rtime(base: u64, ply: u32) -> u64 {
        if base == 0 {
            return 0;
        }
        if ply == 0 {
            return base;
        }

        let seed = monotonic_ms().wrapping_add((ply as u64) << 32).wrapping_add(base);
        let mut rng = SmallRng::seed_from_u64(seed);

        let half = base / 2;
        let dynamic = if ply > 0 {
            let numerator = (base as u128).saturating_mul(10);
            let denom = ply as u128;
            if denom == 0 {
                base
            } else {
                (numerator / denom).min(u128::from(u64::MAX)) as u64
            }
        } else {
            base
        };

        let max_bonus = half.min(dynamic);
        if max_bonus == 0 {
            base
        } else {
            base.saturating_add(rng.random_range(0..=max_bonus))
        }
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

        // Phase 4: Check if we exceeded maximum (opt_limit_ms) and should schedule stop
        // This follows YaneuraOu's design where maximum() triggers set_search_end()
        let opt_limit = self.inner.opt_limit_ms.load(Ordering::Relaxed);
        if planned == u64::MAX && elapsed >= opt_limit {
            self.set_search_end(elapsed);
            // Don't stop immediately - continue until scheduled time
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
        let diff = now_ms.saturating_sub(start_ms);
        #[cfg(test)]
        {
            if diff < 2 {
                return 0;
            }
        }

        diff
    }

    /// Get soft time limit in milliseconds
    pub fn soft_limit_ms(&self) -> u64 {
        self.inner.soft_limit_ms.load(Ordering::Relaxed)
    }

    /// Ensure a rounded stop is scheduled when watchdog側から呼び出す
    ///
    /// `TimeManager::should_stop` と同様、まだ `search_end_ms` が未設定かつ
    /// 既に最適上限を超えた場合に、丸め込みロジックを適用して計画停止時刻を入れる。
    /// ウォッチャースレッドが時間監視を担うケースで使用する。
    pub fn ensure_scheduled_stop(&self, elapsed_ms: u64) {
        let current = self.inner.search_end_ms.load(Ordering::Relaxed);
        if current == u64::MAX {
            self.set_search_end(elapsed_ms);
        }
    }

    #[cfg(test)]
    pub fn override_limits_for_test(&self, soft_ms: u64, hard_ms: u64) {
        self.inner.soft_limit_ms.store(soft_ms, Ordering::Relaxed);
        self.inner.hard_limit_ms.store(hard_ms, Ordering::Relaxed);
    }

    /// Get hard time limit in milliseconds
    pub fn hard_limit_ms(&self) -> u64 {
        self.inner.hard_limit_ms.load(Ordering::Relaxed)
    }

    /// Get configured NetworkDelay2 in milliseconds
    pub fn network_delay2_ms(&self) -> u64 {
        self.inner.params.network_delay2_ms
    }

    /// Return true if現在の残時間が危険域に入っている。
    pub fn is_time_critical(&self) -> bool {
        self.state_checker().is_time_critical()
    }

    /// Get opt time budget (Phase 1)
    pub fn opt_limit_ms(&self) -> u64 {
        self.inner.opt_limit_ms.load(Ordering::Relaxed)
    }

    /// Get scheduled rounded stop time (ms since start) or u64::MAX if unset
    pub fn scheduled_end_ms(&self) -> u64 {
        self.inner.search_end_ms.load(Ordering::Relaxed)
    }

    /// Compute a rounded stop target following YaneuraOu's design:
    /// round up to the next second boundary and subtract the average
    /// network delay (`NetworkDelay`).
    fn round_up(&self, elapsed_ms: u64) -> u64 {
        // YaneuraOu style: round to next second boundary
        let next_sec = ((elapsed_ms / 1000).saturating_add(1)).saturating_mul(1000);

        // Use average NetworkDelay for rounding (YaneuraOu design)
        // network_delay2_ms is reserved for remain upper bounds and hard limits
        let network_delay = self.inner.params.network_delay_ms;

        // Calculate target with average network delay
        let mut target = next_sec.saturating_sub(network_delay);

        // Ensure we don't go backwards - use 1 second increment (YaneuraOu style)
        if target < elapsed_ms {
            // Schedule at least 1 second ahead to allow iteration completion
            target = elapsed_ms.saturating_add(1000);
        }

        // Apply minimum thinking time (except in critical situations)
        let min_think = self.inner.params.min_think_ms;
        if min_think > 0 && target < min_think {
            target = min_think;
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

        let initial_target = self.round_up(elapsed_ms);
        let mut target = initial_target;

        // Respect final push minimum (cannot exceed hard)
        if self.inner.final_push_active.load(Ordering::Relaxed) {
            let min_ms = self.inner.final_push_min_ms.load(Ordering::Relaxed);
            if target < min_ms {
                target = min_ms.min(hard);
            }
        }

        // Apply minimum thinking time even in set_search_end
        // This ensures we always have reasonable time for at least one iteration
        let min_think = self.inner.params.min_think_ms;
        if min_think > 0 && target < min_think && elapsed_ms < min_think {
            // Only apply if we haven't already exceeded min_think
            target = min_think;
        }

        // Remain-time upper clamp (lightweight safety)
        if let Some(rem) = self.remain_upper_ms() {
            if target > rem {
                target = rem;
            }
        }

        // Phase 4: Use calculate_safety_margin() for consistent safety calculation
        let safety_ms = self.calculate_safety_margin(hard);
        if hard != u64::MAX {
            let cap = hard.saturating_sub(safety_ms);
            if target > cap {
                target = cap;
            }
        }

        let current = self.inner.search_end_ms.load(Ordering::Relaxed);

        if current == u64::MAX || target < current {
            self.inner.search_end_ms.store(target, Ordering::Relaxed);
        }
    }

    /// Conservative remain-time upper bound (ms since start),
    /// subtracting NetworkDelay2 and average overhead as safety margins.
    fn remain_upper_ms(&self) -> Option<u64> {
        let overhead = self.inner.params.overhead_ms;
        let nd2 = self.inner.params.network_delay2_ms;
        let params = &self.inner.params;
        let floor_fischer = params.min_think_ms.max(params.critical_fischer_ms).max(50);
        let floor_byoyomi = params.min_think_ms.max(params.critical_byoyomi_ms).max(50);
        let floor_fixed = params.min_think_ms.max(50);
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
                Some(remain.saturating_sub(nd2).saturating_sub(overhead).max(floor_fischer))
            }
            TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                ..
            } => {
                if self.is_in_byoyomi() {
                    Some(byoyomi_ms.saturating_sub(nd2).saturating_sub(overhead).max(floor_byoyomi))
                } else {
                    Some(
                        main_time_ms
                            .saturating_sub(nd2)
                            .saturating_sub(overhead)
                            .max(floor_fischer),
                    )
                }
            }
            TimeControl::FixedTime { ms_per_move } => {
                Some(ms_per_move.saturating_sub(overhead).max(floor_fixed))
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
                debug_assert!(false, "TimeState::NonByoyomi used with Byoyomi time control");
                warn!("TimeState::NonByoyomi used with Byoyomi time control - skipping update");
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

    /// Phase 4: Calculate safety margin based on YaneuraOu's staged
    /// `NetworkDelay2` clamp.
    fn calculate_safety_margin(&self, hard_limit: u64) -> u64 {
        // Use NetworkDelay2 as base safety margin
        let network_delay2 = self.inner.params.network_delay2_ms;

        // Apply staged margin based on hard limit
        if hard_limit >= 5000 {
            network_delay2
        } else if hard_limit >= 1000 {
            network_delay2.min(500)
        } else if hard_limit >= 500 {
            network_delay2.min(200)
        } else {
            network_delay2.min(100)
        }
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
