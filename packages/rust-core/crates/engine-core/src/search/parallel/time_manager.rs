//! Time management thread implementation

use super::SharedSearchState;
use crate::{
    search::types::{StopInfo, TerminationReason},
    time_management::TimeManager,
};
use log::debug;
#[cfg(feature = "diagnostics")]
use log::info;
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

/// Start time management thread
pub fn start_time_manager(
    time_manager: Arc<TimeManager>,
    shared_state: Arc<SharedSearchState>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if log::log_enabled!(log::Level::Debug) {
            debug!("Time manager started");
        }

        loop {
            // Poll interval based on time control
            let poll_interval = match time_manager.soft_limit_ms() {
                0..=50 => Duration::from_millis(2),
                51..=100 => Duration::from_millis(5),
                101..=500 => Duration::from_millis(10),
                _ => Duration::from_millis(20),
            };

            thread::sleep(poll_interval);

            if shared_state.should_stop() {
                break;
            }

            let nodes = shared_state.get_nodes();
            let elapsed_ms = time_manager.elapsed_ms();
            let soft = time_manager.soft_limit_ms();
            let hard = time_manager.hard_limit_ms();
            let planned = time_manager.scheduled_end_ms();

            let near_hard = hard != u64::MAX
                && hard > 0
                && elapsed_ms.saturating_add(compute_finalize_window_ms(hard)) >= hard;
            let near_planned = planned != u64::MAX
                && planned > 0
                && elapsed_ms.saturating_add(compute_finalize_window_ms(planned)) >= planned;

            // Evaluate time-based stop unconditionally (no node-count guard)
            if time_manager.should_stop(nodes) || near_hard || near_planned {
                let hard_timeout = hard != u64::MAX && elapsed_ms >= hard;
                let depth = shared_state.get_best_depth();

                debug!(
                    "Time limit reached/near: elapsed={}ms soft={}ms hard={}ms planned={}ms nodes={} depth={} hard_timeout={} near_hard={} near_planned={}",
                    elapsed_ms,
                    soft,
                    hard,
                    planned,
                    nodes,
                    depth,
                    hard_timeout,
                    near_hard,
                    near_planned
                );

                #[cfg(feature = "diagnostics")]
                {
                    if near_hard || near_planned {
                        info!(
                            "diag tm_near_finalize=1 elapsed={} soft={} hard={} planned={}",
                            elapsed_ms, soft, hard, planned
                        );
                    }
                }

                // Record structured stop info and signal stop
                shared_state.set_stop_with_reason(StopInfo {
                    reason: TerminationReason::TimeLimit,
                    elapsed_ms,
                    nodes,
                    depth_reached: depth,
                    hard_timeout,
                    soft_limit_ms: soft,
                    hard_limit_ms: hard,
                });
                break;
            }
        }

        if log::log_enabled!(log::Level::Debug) {
            debug!("Time manager stopped");
        }
    })
}

fn compute_finalize_window_ms(total_limit_ms: u64) -> u64 {
    use crate::search::constants::NEAR_HARD_FINALIZE_MS;

    if total_limit_ms == 0 || total_limit_ms == u64::MAX {
        0
    } else if total_limit_ms >= 1_000 {
        NEAR_HARD_FINALIZE_MS
    } else if total_limit_ms >= 500 {
        NEAR_HARD_FINALIZE_MS / 2
    } else if total_limit_ms >= 200 {
        120
    } else {
        0
    }
}

/// Start fail-safe guard thread
/// This thread will abort the process if search exceeds hard timeout
pub fn start_fail_safe_guard(
    search_start: Instant,
    limits: crate::search::SearchLimits,
    shared_state: Arc<SharedSearchState>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        // Calculate hard timeout
        use crate::time_management::TimeControl;

        // Initial hard timeout calculation
        let mut hard_timeout_ms = match &limits.time_control {
            TimeControl::FixedTime { ms_per_move } => ms_per_move * 3, // 3x safety margin
            TimeControl::Fischer {
                white_ms,
                black_ms,
                increment_ms: _,
            } => {
                // Use 90% of remaining time as absolute maximum
                let time_ms = white_ms.max(black_ms);
                (time_ms * 9) / 10
            }
            TimeControl::Byoyomi {
                main_time_ms,
                byoyomi_ms,
                periods: _,
            } => {
                // Safety margin for I/O and network latency
                const SAFETY_MARGIN_MS: u64 = 300;

                if *main_time_ms > 0 {
                    // In main time: use main time + one byoyomi period
                    main_time_ms + byoyomi_ms
                } else {
                    // In byoyomi: use byoyomi time minus safety margin
                    // This prevents timeout losses due to I/O delays
                    byoyomi_ms.saturating_sub(SAFETY_MARGIN_MS).max(100)
                }
            }
            TimeControl::FixedNodes { .. } => {
                // For node-limited search, use 1 hour as safety limit
                3_600_000
            }
            TimeControl::Infinite => {
                // For infinite search, use 1 hour as safety limit
                3_600_000
            }
            TimeControl::Ponder(ref _inner) => {
                // For pondering, initially use 1 hour as safety limit
                // Will switch to inner time control after ponderhit
                3_600_000
            }
        };

        // Store inner time control for ponderhit switching
        let ponder_inner = match &limits.time_control {
            TimeControl::Ponder(ref inner) => Some((**inner).clone()),
            _ => None,
        };
        let mut switched_after_hit = false;

        // Add extra safety margin for depth-limited searches
        // But keep it reasonable when time control is also specified
        hard_timeout_ms =
            if limits.depth.is_some() && matches!(limits.time_control, TimeControl::Infinite) {
                hard_timeout_ms.max(10_000) // 10 seconds for depth-only searches (reduced from 60s)
            } else {
                hard_timeout_ms.max(1000) // At least 1 second for time-controlled searches
            };

        if log::log_enabled!(log::Level::Debug) {
            debug!("Fail-safe guard started with hard timeout: {hard_timeout_ms}ms");
        }

        // Check periodically (fail-safe is optional and should be conservative)
        loop {
            thread::sleep(Duration::from_millis(100));

            // Check if search stopped normally
            if shared_state.should_stop() {
                if log::log_enabled!(log::Level::Debug) {
                    debug!("Fail-safe guard: Search stopped normally");
                }
                break;
            }

            // Check for ponderhit and switch to inner time control if needed
            if !switched_after_hit {
                if let (Some(ref flag), Some(ref inner)) = (&limits.ponder_hit_flag, &ponder_inner)
                {
                    if flag.load(std::sync::atomic::Ordering::Acquire) {
                        // Recalculate hard timeout based on inner time control
                        hard_timeout_ms = match inner {
                            TimeControl::FixedTime { ms_per_move } => ms_per_move * 3,
                            TimeControl::Fischer {
                                white_ms,
                                black_ms,
                                increment_ms: _,
                            } => {
                                let time_ms = white_ms.max(black_ms);
                                (time_ms * 9) / 10
                            }
                            TimeControl::Byoyomi {
                                main_time_ms,
                                byoyomi_ms,
                                periods: _,
                            } => {
                                const SAFETY_MARGIN_MS: u64 = 300;
                                if *main_time_ms > 0 {
                                    main_time_ms + byoyomi_ms
                                } else {
                                    byoyomi_ms.saturating_sub(SAFETY_MARGIN_MS).max(100)
                                }
                            }
                            TimeControl::FixedNodes { .. } => 3_600_000,
                            TimeControl::Infinite => 3_600_000,
                            TimeControl::Ponder(_) => 3_600_000, // Shouldn't happen
                        }
                        .max(1000); // At least 1 second

                        switched_after_hit = true;
                        if log::log_enabled!(log::Level::Debug) {
                            debug!("Fail-safe switched to inner time control after ponderhit: {hard_timeout_ms}ms");
                        }
                    }
                }
            }

            // Check if hard timeout exceeded
            let elapsed = search_start.elapsed();
            if elapsed.as_millis() > hard_timeout_ms as u128 {
                log::warn!(
                    "FAIL-SAFE: Hard timeout {}ms exceeded (elapsed: {}ms). Attempting graceful stop...",
                    hard_timeout_ms,
                    elapsed.as_millis()
                );

                // Step 1: graceful stop signal
                shared_state.set_stop();
                thread::sleep(Duration::from_millis(1000)); // grace 1000ms

                if shared_state.should_stop() {
                    break;
                }

                // Step 2: repeat stop signal and escalate log
                log::warn!("FAIL-SAFE: Stop not observed after 1000ms. Retrying stop signal...");
                shared_state.set_stop();
                thread::sleep(Duration::from_millis(1000));

                if shared_state.should_stop() {
                    break;
                }

                // Step 3: final warning. Abort only when explicitly enabled at build time.
                log::error!("FAIL-SAFE: Search still running after additional 1000ms");
                #[cfg(feature = "fail-safe-abort")]
                {
                    log::error!("FAIL-SAFE: Forced abort due to unresponsive search (feature fail-safe-abort)");
                    std::process::abort();
                }
                // Without abort feature, exit the guard loop to avoid busy spinning
                break;
            }
        }

        if log::log_enabled!(log::Level::Debug) {
            debug!("Fail-safe guard stopped");
        }
    })
}
