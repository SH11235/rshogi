//! Time management thread implementation

use super::SharedSearchState;
use crate::time_management::TimeManager;
use log::{debug, info};
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
            // Don't stop if we haven't done any real work yet
            if nodes > 100 && time_manager.should_stop(nodes) {
                info!("Time limit reached after {nodes} nodes, stopping search");
                shared_state.set_stop();
                break;
            }
        }

        if log::log_enabled!(log::Level::Debug) {
            debug!("Time manager stopped");
        }
    })
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

        // Check periodically
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
                log::error!(
                    "FAIL-SAFE: Search exceeded hard timeout of {}ms (elapsed: {}ms)",
                    hard_timeout_ms,
                    elapsed.as_millis()
                );

                // Try to stop gracefully first
                shared_state.set_stop();

                // Give 500ms for graceful shutdown
                thread::sleep(Duration::from_millis(500));

                // If still not stopped, abort
                if !shared_state.should_stop() {
                    log::error!("FAIL-SAFE: Forced abort due to unresponsive search!");
                    std::process::abort();
                }
            }
        }

        if log::log_enabled!(log::Level::Debug) {
            debug!("Fail-safe guard stopped");
        }
    })
}
