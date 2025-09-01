use crate::emit_utils::log_tsv;
use crate::engine_adapter::EngineAdapter;
use crate::state::SearchState;
use crate::usi::output::{Score, SearchInfo};
use crate::usi::GoParams;
use crate::utils::lock_or_recover_generic;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use engine_core::engine::controller::Engine;
use engine_core::search::constants::MATE_SCORE;
use engine_core::search::types::{StopInfo, TerminationReason};
use engine_core::search::SearchLimits;
use engine_core::time_management::{self as core_tm, TimeManager};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Derive approximate budget (soft/hard limits in ms) from SearchLimits
///
/// This is used to enrich StopInfo in fallback/error paths where the core
/// engine doesn't provide detailed time limits. Values are best-effort and
/// may be conservative. For Byoyomi, we use TimeParameters to estimate
/// soft/hard limits; for FixedTime, both are the fixed time; other modes
/// default to 0 when not reliably derivable here.
fn derive_budgets_via_core(
    position: &engine_core::shogi::Position,
    limits: &SearchLimits,
) -> Option<(u64, u64, bool)> {
    // Convert SearchLimits to core TimeLimits and instantiate a TimeManager to get soft/hard
    let time_limits: core_tm::TimeLimits = limits.clone().into();
    // Determine game phase consistently with core
    let phase = core_tm::detect_game_phase_for_time(position, position.ply as u32);
    // Create a temporary TimeManager (no thread) to extract budgets
    let tm = TimeManager::new(&time_limits, position.side_to_move, position.ply as u32, phase);
    let soft = tm.soft_limit_ms();
    let hard = tm.hard_limit_ms();
    let clamped = tm.budgets_were_clamped();

    // Discard non-finite or zero budgets
    if soft == u64::MAX || hard == u64::MAX || soft == 0 || hard == 0 {
        return None;
    }
    // Clamp to ensure hard >= soft
    let hard = hard.max(soft);
    Some((soft, hard, clamped))
}

/// Messages from worker thread to main thread
pub enum WorkerMessage {
    /// Engine instance to be returned by USI thread (engine return unification)
    ReturnEngine {
        engine: Engine,
        search_id: u64,
    },
    Info {
        info: SearchInfo,
        search_id: u64,
    },
    /// Hard deadline fire (insurance) – based on go_begin + hard_ms
    HardDeadlineFire {
        search_id: u64,
        hard_ms: u64,
    },

    /// Search has started
    SearchStarted {
        search_id: u64,
        start_time: Instant,
    },

    /// Iteration committed result from core (preferred)
    IterationCommitted {
        committed: engine_core::search::CommittedIteration,
        search_id: u64,
    },

    /// Search finished (finalization notification)
    SearchFinished {
        root_hash: u64,
        search_id: u64,
        stop_info: Option<StopInfo>,
    },

    /// Partial result available during search
    PartialResult {
        current_best: String,
        depth: u8,
        score: i32,
        search_id: u64,
    },
    /// Thread finished - from_guard indicates if sent by EngineReturnGuard
    Finished {
        from_guard: bool,
        search_id: u64,
    },
    Error {
        message: String,
        search_id: u64,
    },
}

/// Convert mate moves to pseudo centipawn value for ordering
///
/// This helper function provides a consistent scale for converting mate scores
/// to centipawn equivalents. This is used for ordering purposes when comparing
/// different search results.
///
/// Note: This function uses a simplified 100cp per move scale for UI display purposes.
/// This is different from the engine's internal representation which uses 2 plies
/// per move. The purpose here is to provide a smooth gradient for partial results,
/// not to preserve the exact engine score.
///
/// # Arguments
///
/// * `mate` - Number of moves to mate (positive = we're winning, negative = we're losing)
///
/// # Returns
///
/// * `Some(i32)` - Pseudo centipawn value
/// * `None` - If mate is 0 (immediate mate with ambiguous sign)
fn mate_moves_to_pseudo_cp(mate: i32) -> Option<i32> {
    if mate == 0 {
        // mate 0: sign is ambiguous, don't convert
        return None;
    }
    // Consistent scale: use 100 cp per move to mate
    // This gives a smooth gradient for ordering purposes
    const CP_PER_MOVE: i32 = 100;
    if mate > 0 {
        Some(MATE_SCORE - mate * CP_PER_MOVE)
    } else {
        Some(-MATE_SCORE - mate * CP_PER_MOVE)
    }
}

/// Guard to hold engine during search (no automatic return on drop)
pub struct EngineReturnGuard {
    engine: Option<Engine>,
    tx: Sender<WorkerMessage>,
    search_id: u64,
}

impl EngineReturnGuard {
    pub fn new(engine: Engine, tx: Sender<WorkerMessage>, search_id: u64) -> Self {
        Self {
            engine: Some(engine),
            tx,
            search_id,
        }
    }

    /// Explicitly return the engine via ReturnEngine message
    pub fn return_engine(mut self) {
        if let Some(engine) = self.engine.take() {
            log::debug!("EngineReturnGuard: explicitly returning engine to USI thread");
            if let Err(e) = self.tx.send(WorkerMessage::ReturnEngine {
                engine,
                search_id: self.search_id,
            }) {
                log::error!("Failed to send ReturnEngine: {e}");
            }
        }
    }
}

impl std::ops::Deref for EngineReturnGuard {
    type Target = Engine;

    fn deref(&self) -> &Self::Target {
        self.engine.as_ref().expect("Engine already taken")
    }
}

impl std::ops::DerefMut for EngineReturnGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.engine.as_mut().expect("Engine already taken")
    }
}

impl Drop for EngineReturnGuard {
    fn drop(&mut self) {
        if self.engine.is_some() {
            log::error!("EngineReturnGuard dropped without explicit return! This is a bug.");
            // Do NOT automatically return - this violates the USI-side return principle
        }
    }
}

/// Specialized lock_or_recover for EngineAdapter with state reset
pub fn lock_or_recover_adapter(mutex: &Mutex<EngineAdapter>) -> MutexGuard<'_, EngineAdapter> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::error!("EngineAdapter mutex was poisoned, attempting recovery with state reset");
            let mut guard = poisoned.into_inner();

            // Force reset engine state to safe defaults
            guard.force_reset_state();

            // Notify main to emit info about the reset
            // Note: This function doesn't have tx in scope; skip USI output here.

            guard
        }
    }
}

/// Worker thread function for search
#[allow(clippy::too_many_arguments)]
pub fn search_worker(
    engine_adapter: Arc<Mutex<EngineAdapter>>,
    params: GoParams,
    stop_flag: Arc<AtomicBool>,
    global_stop_flag: Arc<AtomicBool>,
    tx: Sender<WorkerMessage>,
    search_id: u64,
    finalized_flag: Option<Arc<AtomicBool>>,
    go_begin_at: Instant,
) {
    log::debug!("Search worker thread started with params: {params:?}");

    // Immediately check stop flag value
    let immediate_stop_value = stop_flag.load(Ordering::SeqCst);
    let immediate_global_value = global_stop_flag.load(Ordering::SeqCst);
    log::debug!(
        "Worker immediate stop flag check: per_search={} (ptr: {:p}), global={} (ptr: {:p})",
        immediate_stop_value,
        stop_flag.as_ref(),
        immediate_global_value,
        global_stop_flag.as_ref()
    );

    // Add delay to check if this is a race condition (only in debug builds)
    #[cfg(debug_assertions)]
    std::thread::sleep(std::time::Duration::from_micros(1));

    let initial_stop_value = stop_flag.load(Ordering::SeqCst);
    let initial_global_value = global_stop_flag.load(Ordering::SeqCst);
    log::info!(
        "Worker: search_id={search_id}, ponder={}, stop_flag_ptr={:p}, stop_flag_value={}, global_stop={}",
        params.ponder,
        stop_flag.as_ref(),
        initial_stop_value,
        initial_global_value
    );

    // Early return if stop was already requested (check both flags)
    if (initial_stop_value || initial_global_value) && !params.ponder {
        log::warn!(
            "Worker: stop flag already set at start (per_search={}, global={}), aborting search",
            initial_stop_value,
            initial_global_value
        );
        let _ = tx.send(WorkerMessage::Error {
            message: "initial_stop_flag_true_at_worker_start".to_string(),
            search_id,
        });
        let _ = tx.send(WorkerMessage::Finished {
            from_guard: false,
            search_id,
        });
        return;
    }

    let _worker_start_time = Instant::now();

    // Send SearchStarted message with current time
    // Note: This is sent before take_engine() to ensure GUI sees activity ASAP.
    // If take_engine fails, subsequent Error and SearchFinished messages will clean up.
    let start_time = Instant::now();
    let _ = tx.send(WorkerMessage::SearchStarted {
        search_id,
        start_time,
    });
    log::info!("Worker: SearchStarted message sent");

    // Helper closure to check combined stop condition
    let should_stop = {
        let per_search = stop_flag.clone();
        let global = global_stop_flag.clone();
        move || per_search.load(Ordering::Acquire) || global.load(Ordering::Acquire)
    };

    // Set up info callback with partial result tracking
    let tx_info = tx.clone();
    let tx_partial = tx.clone();
    let last_partial_depth = Arc::new(Mutex::new(0u8));
    // Throttling state for info messages
    let last_info_sent = Arc::new(Mutex::new(Instant::now()));
    let last_pv_head = Arc::new(Mutex::new(String::new()));
    let min_info_interval_ms = std::env::var("USI_INFO_MIN_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(100);
    let should_stop_for_info = should_stop.clone();
    let finalized_for_info = finalized_flag.clone();
    let info_callback = move |info: SearchInfo| {
        // Check stop flag before sending messages (combined check)
        if should_stop_for_info() {
            log::trace!("Info callback: stop flag set, skipping message");
            return;
        }
        // Suppress after finalize to avoid backlog
        if let Some(flag) = &finalized_for_info {
            if flag.load(Ordering::Acquire) {
                log::trace!("Info callback: finalized flag set, skipping message");
                return;
            }
        }

        // Throttle: forward only if head changed or min interval elapsed
        let head = info.pv.first().cloned().unwrap_or_default();
        let mut last_head = lock_or_recover_generic(&last_pv_head);
        let mut last_sent = lock_or_recover_generic(&last_info_sent);
        let allow = if head != *last_head {
            *last_head = head;
            true
        } else {
            last_sent.elapsed() >= std::time::Duration::from_millis(min_info_interval_ms)
        };
        if allow {
            *last_sent = Instant::now();
            let _ = tx_info.send(WorkerMessage::Info {
                info: info.clone(),
                search_id,
            });
        }
        // Send partial result at certain depth intervals (unchanged)
        if let (Some(depth), Some(score), Some(pv)) =
            (info.depth, info.score.as_ref(), info.pv.first())
        {
            // Check if we should send a partial result
            let should_send = {
                let mut last_depth = lock_or_recover_generic(&last_partial_depth);
                let depth_u8 = depth as u8;
                if depth_u8 >= *last_depth + 5 || (depth_u8 >= 10 && depth_u8 > *last_depth) {
                    *last_depth = depth_u8;
                    true
                } else {
                    false
                }
            };

            if should_send {
                let score_value = match score {
                    Score::Cp(cp) => Some(*cp),
                    Score::Mate(mate) => mate_moves_to_pseudo_cp(*mate),
                };

                if let Some(score_value) = score_value {
                    log::debug!(
                        "Sending partial result: move={pv}, depth={depth}, score={score_value}"
                    );
                    let _ = tx_partial.send(WorkerMessage::PartialResult {
                        current_best: pv.clone(),
                        depth: depth as u8,
                        score: score_value,
                        search_id,
                    });
                } else {
                    log::debug!("Skipping partial result for mate 0 (sign ambiguous)");
                }
            }
        }
    };

    // Take engine out and prepare search
    let was_ponder = params.ponder;
    log::debug!("Attempting to take engine from adapter");
    let engine_take_start = Instant::now();
    let (mut guard, pos_snapshot, limits, ponder_hit_flag, threads_for_log) = {
        let mut adapter = lock_or_recover_adapter(&engine_adapter);
        log::debug!("Adapter lock acquired, calling take_engine");
        let _ = tx.send(WorkerMessage::Info {
            info: SearchInfo {
                string: Some(log_tsv(&[
                    ("kind", "worker_take_engine_begin"),
                    ("search_id", &search_id.to_string()),
                ])),
                ..Default::default()
            },
            search_id,
        });
        let engine_available = adapter.is_engine_available();
        log::info!("Worker: engine available before take: {engine_available}");
        match adapter.take_engine() {
            Ok(engine) => {
                log::debug!("Engine taken successfully, preparing search");
                let take_duration = engine_take_start.elapsed();
                log::info!("Worker: engine taken successfully after {take_duration:?}");
                let _ = tx.send(WorkerMessage::Info {
                    info: SearchInfo {
                        string: Some(log_tsv(&[
                            ("kind", "worker_take_engine_end"),
                            ("search_id", &search_id.to_string()),
                            ("elapsed_ms", &take_duration.as_millis().to_string()),
                        ])),
                        ..Default::default()
                    },
                    search_id,
                });
                // Create guard to hold engine during search
                let guard = EngineReturnGuard::new(engine, tx.clone(), search_id);

                // Snapshot minimal adapter state needed for limit computation, then drop lock
                let pos_snapshot = match adapter.get_position() {
                    Some(p) => p.clone(),
                    None => {
                        log::error!("Position not set at prepare time");
                        // Explicitly return engine before early return
                        guard.return_engine();
                        let _ = tx.send(WorkerMessage::Error {
                            message: "Position not set".to_string(),
                            search_id,
                        });
                        if !params.ponder {
                            let _ = tx.send(WorkerMessage::SearchFinished {
                                root_hash: 0,
                                search_id,
                                stop_info: Some(StopInfo {
                                    reason: TerminationReason::Error,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 0,
                                    hard_timeout: false,
                                    soft_limit_ms: 0,
                                    hard_limit_ms: 0,
                                }),
                            });
                        }
                        let _ = tx.send(WorkerMessage::Finished {
                            from_guard: false,
                            search_id,
                        });
                        return;
                    }
                };
                // Copy overhead and tuning params
                let (
                    overhead_ms,
                    byoyomi_overhead_ms,
                    byoyomi_safety_ms,
                    byoyomi_early_finish_ratio,
                    pv_base,
                    pv_slope,
                ) = adapter.get_overheads_and_tuning();
                // Log prep begin and drop lock before heavy work
                let prep_begin = Instant::now();
                let _ = tx.send(WorkerMessage::Info {
                    info: SearchInfo {
                        string: Some(log_tsv(&[
                            ("kind", "prepare_search_begin"),
                            ("search_id", &search_id.to_string()),
                        ])),
                        ..Default::default()
                    },
                    search_id,
                });
                // Capture threads for logging before dropping adapter
                let threads_for_log = adapter.threads();
                // Capture MinThinkMs before dropping adapter
                let min_think_ms_val = adapter.min_think_ms() as u32;
                let time_policy_extras = adapter.get_time_policy_extras();
                drop(adapter); // release adapter lock early

                // Compute effective byoyomi status without holding adapter lock
                let is_byoyomi_active = match params.byoyomi {
                    Some(byo) if byo > 0 && !params.ponder => {
                        !crate::engine_adapter::time_control::is_fischer_disguised_as_byoyomi(
                            byo,
                            params.binc,
                            params.winc,
                        )
                    }
                    _ => false,
                };
                let network_delay2_ms = if is_byoyomi_active {
                    byoyomi_overhead_ms
                } else {
                    0
                };

                // Early cancel check (combined)
                if should_stop() {
                    log::info!("Stop requested during prepare; skipping limit computation");
                }

                // Create a synthetic stop flag that combines both per-search and global
                // This is necessary because engine core only accepts a single stop flag
                let synthetic_stop_flag = Arc::new(AtomicBool::new(false));

                // Set initial value to avoid initial window where synthetic is false but sources are true
                let initial_should_stop =
                    stop_flag.load(Ordering::Acquire) || global_stop_flag.load(Ordering::Acquire);
                synthetic_stop_flag.store(initial_should_stop, Ordering::Release);

                let synthetic_for_thread = synthetic_stop_flag.clone();
                let stop_for_thread = stop_flag.clone();
                let global_for_thread = global_stop_flag.clone();
                let finalized_for_monitor = finalized_flag.clone();

                // Spawn a thread to monitor both flags and update synthetic
                // This thread will terminate when either stop is requested or search is finalized
                let _monitor_handle = std::thread::spawn(move || {
                    loop {
                        let should_stop = stop_for_thread.load(Ordering::Acquire)
                            || global_for_thread.load(Ordering::Acquire);
                        synthetic_for_thread.store(should_stop, Ordering::Release);

                        if should_stop {
                            log::debug!("Monitor thread: stopping due to stop flag");
                            break;
                        }

                        // Check if search has been finalized (normal termination)
                        if let Some(ref finalized) = finalized_for_monitor {
                            if finalized.load(Ordering::Acquire) {
                                log::debug!("Monitor thread: stopping due to finalized flag");
                                break;
                            }
                        }

                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                });

                // Apply go params to build limits (lock-free)
                let limits_res = crate::engine_adapter::time_control::apply_go_params(
                    &params,
                    &pos_snapshot,
                    overhead_ms,
                    Some(synthetic_stop_flag),
                    byoyomi_safety_ms,
                    network_delay2_ms,
                    byoyomi_early_finish_ratio,
                    pv_base,
                    pv_slope,
                    min_think_ms_val,
                    time_policy_extras.0,
                    time_policy_extras.1,
                    time_policy_extras.2,
                    time_policy_extras.3,
                );
                let limits = match limits_res {
                    Ok(l) => l,
                    Err(e) => {
                        // Engine will be returned by guard's Drop here
                        log::error!("Search preparation (limits) error: {e}");
                        guard.return_engine();
                        let _ = tx.send(WorkerMessage::Error {
                            message: e.to_string(),
                            search_id,
                        });
                        if !params.ponder {
                            let _ = tx.send(WorkerMessage::SearchFinished {
                                root_hash: pos_snapshot.zobrist_hash(),
                                search_id,
                                stop_info: Some(StopInfo {
                                    reason: TerminationReason::Error,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 0,
                                    hard_timeout: false,
                                    soft_limit_ms: 0,
                                    hard_limit_ms: 0,
                                }),
                            });
                        }
                        let _ = tx.send(WorkerMessage::Finished {
                            from_guard: false,
                            search_id,
                        });
                        return;
                    }
                };

                // Re-acquire adapter lock shortly to store flags/state and ponder flag
                let mut adapter = lock_or_recover_adapter(&engine_adapter);
                adapter.set_search_start_snapshot(
                    pos_snapshot.zobrist_hash(),
                    pos_snapshot.side_to_move,
                );
                // Detect if final limits indicate byoyomi
                match &limits.time_control {
                    engine_core::time_management::TimeControl::Byoyomi { .. } => {
                        adapter.set_last_search_is_byoyomi(true);
                    }
                    engine_core::time_management::TimeControl::Ponder(inner) => {
                        let val = matches!(
                            **inner,
                            engine_core::time_management::TimeControl::Byoyomi { .. }
                        );
                        adapter.set_last_search_is_byoyomi(val);
                    }
                    _ => adapter.set_last_search_is_byoyomi(false),
                }
                adapter.set_current_stop_flag(stop_flag.clone());

                // Set up ponder hit flag if pondering
                let ponder_flag_opt = if params.ponder {
                    Some(adapter.begin_ponder())
                } else {
                    None
                };

                // Done with adapter mutations
                let prep_el = prep_begin.elapsed();
                let _ = tx.send(WorkerMessage::Info {
                    info: SearchInfo {
                        string: Some(log_tsv(&[
                            ("kind", "prepare_search_end"),
                            ("search_id", &search_id.to_string()),
                            ("elapsed_ms", &prep_el.as_millis().to_string()),
                        ])),
                        ..Default::default()
                    },
                    search_id,
                });
                if prep_el.as_millis() as u64 > 100 {
                    let _ = tx.send(WorkerMessage::Info {
                        info: SearchInfo {
                            string: Some(log_tsv(&[
                                ("kind", "prepare_timeout"),
                                ("search_id", &search_id.to_string()),
                                ("elapsed_ms", &prep_el.as_millis().to_string()),
                            ])),
                            ..Default::default()
                        },
                        search_id,
                    });
                }
                (guard, pos_snapshot, limits, ponder_flag_opt, threads_for_log)
            }
            Err(e) => {
                log::error!("Failed to take engine: {e}");
                let _ = tx.send(WorkerMessage::Error {
                    message: e.to_string(),
                    search_id,
                });

                // Try to generate emergency move from adapter (only if not pondering)
                if !params.ponder {
                    log::info!("Attempting to notify completion after engine take failure");
                    let root = adapter.get_position().map(|p| p.zobrist_hash()).unwrap_or(0);
                    let _ = tx.send(WorkerMessage::SearchFinished {
                        root_hash: root,
                        search_id,
                        stop_info: Some(StopInfo {
                            reason: TerminationReason::Error,
                            elapsed_ms: 0,
                            nodes: 0,
                            depth_reached: 0,
                            hard_timeout: false,
                            soft_limit_ms: 0,
                            hard_limit_ms: 0,
                        }),
                    });
                } else {
                    log::info!("Ponder engine take error, not sending bestmove (USI protocol)");
                }

                let _ = tx.send(WorkerMessage::Finished {
                    from_guard: false,
                    search_id,
                });
                return;
            }
        }
    }; // Lock released here

    // Keep ponder_hit_flag for checking later
    let ponder_hit_flag_ref = ponder_hit_flag.clone();

    // Update limits with ponder_hit_flag if present
    let limits = if let Some(ref flag) = ponder_hit_flag {
        SearchLimits {
            ponder_hit_flag: Some(flag.clone()),
            ..limits
        }
    } else {
        limits
    };

    // Explicitly drop ponder_hit_flag (it's used internally by the engine)
    drop(ponder_hit_flag);

    // Pre-compute budgets via core before moving into the engine
    let budgets = derive_budgets_via_core(&pos_snapshot, &limits);
    // Emit go_received_detail with budgets and time control summary
    {
        let (soft_log, hard_log, note) = match budgets {
            Some((s, h, clamped)) => {
                let note = if clamped { "clamped" } else { "ok" };
                (s, h, note.to_string())
            }
            None => (0, 0, "no_budget".to_string()),
        };
        let tc_label = match &limits.time_control {
            engine_core::time_management::TimeControl::Byoyomi { .. } => "byoyomi",
            engine_core::time_management::TimeControl::Fischer { .. } => "fischer",
            engine_core::time_management::TimeControl::FixedTime { .. } => "fixed_time",
            engine_core::time_management::TimeControl::FixedNodes { .. } => "fixed_nodes",
            engine_core::time_management::TimeControl::Infinite => "infinite",
            engine_core::time_management::TimeControl::Ponder(_) => "ponder",
        };
        let mtg = limits.moves_to_go.unwrap_or(0);
        // Extract overhead/safety/nd2 from limits' time parameters for observability
        let (ov_ms, saf_ms, nd2_ms, min_think_param) = match &limits.time_parameters {
            Some(tp) => (
                tp.overhead_ms,
                tp.byoyomi_hard_limit_reduction_ms,
                tp.network_delay2_ms,
                tp.min_think_ms,
            ),
            None => (0, 0, 0, 0),
        };
        let _ = tx.send(WorkerMessage::Info {
            info: SearchInfo {
                string: Some(log_tsv(&[
                    ("kind", "go_received_detail"),
                    ("search_id", &search_id.to_string()),
                    ("tc", tc_label),
                    ("soft_ms", &soft_log.to_string()),
                    ("hard_ms", &hard_log.to_string()),
                    ("mtg", &mtg.to_string()),
                    ("threads", &threads_for_log.to_string()),
                    ("budget_status", &note),
                    ("overhead_ms", &ov_ms.to_string()),
                    ("safety_ms", &saf_ms.to_string()),
                    ("nd2_ms", &nd2_ms.to_string()),
                    ("min_think_ms", &min_think_param.to_string()),
                ])),
                ..Default::default()
            },
            search_id,
        });
    }
    // Re-extract overhead/safety/nd2/min_think for watchdog logging (out-of-scope above)
    // Worker watchdog removed: rely on core polling and optional hard deadline only.

    // Phase: Add hard-deadline insurance timer (single-shot)
    if !params.ponder {
        if let Some((_, hard_ms, _)) = budgets {
            let tx_deadline2 = tx.clone();
            let finalized_for_hard = finalized_flag.clone();
            let search_id_for_hard = search_id;
            std::thread::spawn(move || {
                // Sleep relative to go_begin baseline
                let since_go = go_begin_at.elapsed().as_millis() as u64;
                let wait_ms = hard_ms.saturating_sub(since_go);
                if wait_ms > 0 {
                    std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                }
                if let Some(flag) = &finalized_for_hard {
                    if flag.load(Ordering::Acquire) {
                        return;
                    }
                }
                let _ = tx_deadline2.send(WorkerMessage::HardDeadlineFire {
                    search_id: search_id_for_hard,
                    hard_ms,
                });
            });
        }
    }

    // Create search session
    // legacy session removed

    // Create info callback (forward info + partials only)
    let enhanced_info_callback = move |info: SearchInfo| {
        // Call original callback
        info_callback(info.clone());
    };

    // Set up iteration callback to forward committed iteration (core type)
    let tx_for_iteration = tx.clone();
    let stop_flag_for_iter = stop_flag.clone();
    let iteration_callback: engine_core::search::IterationCallback = Arc::new(move |iter| {
        if stop_flag_for_iter.load(Ordering::SeqCst) {
            log::trace!("Iteration callback: stop flag set, skipping commit");
            return;
        }
        // Log IterationCommitted for visibility
        let _ = tx_for_iteration.send(WorkerMessage::Info {
            info: SearchInfo {
                string: Some(log_tsv(&[
                    ("kind", "iteration_committed"),
                    ("depth", &iter.depth.to_string()),
                    ("score", &format!("{:?}", iter.score)),
                    ("nodes", &iter.nodes.to_string()),
                    ("elapsed_ms", &iter.elapsed.as_millis().to_string()),
                    ("search_id", &search_id.to_string()),
                ])),
                ..Default::default()
            },
            search_id,
        });
        let _ = tx_for_iteration.send(WorkerMessage::IterationCommitted {
            committed: iter.clone(),
            search_id,
        });
    });

    // Guard was already created immediately after take_engine()

    // Execute search without holding the lock
    log::info!("Calling execute_search_static");
    let search_start = Instant::now();
    let _ = tx.send(WorkerMessage::Info {
        info: SearchInfo {
            string: Some(log_tsv(&[
                ("kind", "execute_search_begin"),
                ("search_id", &search_id.to_string()),
            ])),
            ..Default::default()
        },
        search_id,
    });
    let result = EngineAdapter::execute_search_static(
        &mut guard,
        pos_snapshot.clone(),
        limits,
        Box::new(enhanced_info_callback),
        Some(iteration_callback),
    );
    let search_duration = search_start.elapsed();
    log::info!("execute_search_static returned after {search_duration:?}: {:?}", result.is_ok());
    let _ = tx.send(WorkerMessage::Info {
        info: SearchInfo {
            string: Some(log_tsv(&[
                ("kind", "execute_search_end"),
                ("search_id", &search_id.to_string()),
                ("elapsed_ms", &search_duration.as_millis().to_string()),
            ])),
            ..Default::default()
        },
        search_id,
    });

    // Handle result
    match result {
        Ok(extended_result) => {
            log::debug!(
                "Worker: Search completed, best_move: {}, ponder_move: {:?}, depth: {}",
                extended_result.best_move,
                extended_result.ponder_move,
                extended_result.depth
            );

            // Send PV owner statistics if available
            if let (Some(mismatches), Some(checks)) =
                (extended_result.pv_owner_mismatches, extended_result.pv_owner_checks)
            {
                if checks > 0 {
                    let mismatch_rate = (mismatches as f64 / checks as f64) * 100.0;
                    let pv_owner_info = SearchInfo {
                        string: Some(format!(
                            "PV owner mismatches: {mismatches}/{checks} ({mismatch_rate:.1}%)"
                        )),
                        ..Default::default()
                    };
                    let _ = tx.send(WorkerMessage::Info {
                        info: pv_owner_info,
                        search_id,
                    });
                }
            }

            // Send PV trimming statistics if available
            if let (Some(cuts), Some(checks)) =
                (extended_result.pv_trim_cuts, extended_result.pv_trim_checks)
            {
                if checks > 0 {
                    let trim_rate = (cuts as f64 / checks as f64) * 100.0;
                    let pv_trim_info = SearchInfo {
                        string: Some(format!(
                            "PV trimming: {cuts}/{checks} trimmed ({trim_rate:.1}%)"
                        )),
                        ..Default::default()
                    };
                    let _ = tx.send(WorkerMessage::Info {
                        info: pv_trim_info,
                        search_id,
                    });
                }
            }

            // Clean up ponder state if needed
            {
                let mut adapter = lock_or_recover_adapter(&engine_adapter);
                adapter.cleanup_after_search(was_ponder);
            }

            // Check if ponderhit occurred during ponder search
            let ponder_hit_occurred = if was_ponder {
                // Check if ponder_hit_flag was set during search
                ponder_hit_flag_ref
                    .as_ref()
                    .map(|flag| flag.load(Ordering::Acquire))
                    .unwrap_or(false)
            } else {
                false
            };

            // Finalize if:
            // - Not a ponder search OR
            // - Ponder search that was converted via ponderhit
            if !was_ponder || ponder_hit_occurred {
                log::info!(
                    "Sending search completion: was_ponder={was_ponder}, ponder_hit={ponder_hit_occurred}"
                );
                if let Some(flag) = &finalized_flag {
                    if flag.load(Ordering::Acquire) {
                        // Already finalized; skip SearchFinished
                        return;
                    }
                }
                // Send SearchFinished to indicate we're done
                if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                    root_hash: pos_snapshot.zobrist_hash(),
                    search_id,
                    stop_info: extended_result.stop_info,
                }) {
                    log::error!("Failed to send search finished: {e}");
                }
            } else {
                log::info!("Ponder search without ponderhit, not sending bestmove (USI protocol)");
            }
        }
        Err(e) => {
            log::error!("Search error: {e}");
            let _ = tx.send(WorkerMessage::Info {
                info: SearchInfo {
                    string: Some(log_tsv(&[
                        ("kind", "execute_search_error"),
                        ("search_id", &search_id.to_string()),
                        ("error", &e.to_string()),
                    ])),
                    ..Default::default()
                },
                search_id,
            });
            // Engine will be returned automatically by EngineReturnGuard::drop

            // Clean up ponder state if needed
            {
                let mut adapter = lock_or_recover_adapter(&engine_adapter);
                adapter.cleanup_after_search(was_ponder);
            }

            // Emergency generation removed; main thread handles fallback bestmove

            if stop_flag.load(Ordering::SeqCst) {
                // Check if ponderhit occurred for ponder search
                let ponder_hit_occurred = if was_ponder {
                    ponder_hit_flag_ref
                        .as_ref()
                        .map(|flag| flag.load(Ordering::Acquire))
                        .unwrap_or(false)
                } else {
                    false
                };

                // Stopped by user - finalize if not ponder or after ponderhit
                if !was_ponder || ponder_hit_occurred {
                    if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                        root_hash: pos_snapshot.zobrist_hash(),
                        search_id,
                        stop_info: Some(StopInfo {
                            reason: TerminationReason::UserStop,
                            elapsed_ms: 0,
                            nodes: 0,
                            depth_reached: 0,
                            hard_timeout: false,
                            soft_limit_ms: budgets.map(|b| b.0).unwrap_or(0),
                            hard_limit_ms: budgets.map(|b| b.1).unwrap_or(0),
                        }),
                    }) {
                        log::error!("Failed to send search finished: {e}");
                    }
                    guard.return_engine();
                    return;
                } else {
                    // Ponder search that was stopped (not ponderhit) - don't send bestmove
                    log::info!("Ponder search stopped, not sending bestmove (USI protocol)");
                }
            } else {
                // Other error - send error and try emergency move
                // Check if ponderhit occurred for ponder search
                let ponder_hit_occurred = if was_ponder {
                    ponder_hit_flag_ref
                        .as_ref()
                        .map(|flag| flag.load(Ordering::Acquire))
                        .unwrap_or(false)
                } else {
                    false
                };

                let _ = tx.send(WorkerMessage::Error {
                    message: e.to_string(),
                    search_id,
                });

                // If not ponder OR ponder was converted via ponderhit, finalize; main emits fallback
                if !was_ponder || ponder_hit_occurred {
                    if let Some(flag) = &finalized_flag {
                        if flag.load(Ordering::Acquire) {
                            // Already finalized; skip SearchFinished
                            guard.return_engine();
                            return;
                        }
                    }
                    if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                        root_hash: pos_snapshot.zobrist_hash(),
                        search_id,
                        stop_info: Some(StopInfo {
                            reason: TerminationReason::Error,
                            elapsed_ms: 0,
                            nodes: 0,
                            depth_reached: 0,
                            hard_timeout: false,
                            soft_limit_ms: budgets.map(|b| b.0).unwrap_or(0),
                            hard_limit_ms: budgets.map(|b| b.1).unwrap_or(0),
                        }),
                    }) {
                        log::error!("Failed to send search finished after error: {e}");
                    }
                } else {
                    log::info!(
                        "Ponder search error without ponderhit, not sending bestmove (USI protocol)"
                    );
                }
            }
        }
    }

    // Always send Finished at the end - use blocking send to ensure delivery
    match tx.send(WorkerMessage::Finished {
        from_guard: false,
        search_id,
    }) {
        Ok(()) => log::debug!("Finished message sent successfully"),
        Err(e) => {
            log::error!("Failed to send Finished message: {e}. Channel might be closed.");
            // This is a critical error but we can't do much about it
        }
    }

    log::debug!("Search worker finished");
}

/// Create an emergency search session for fallback moves
/// Wait for worker thread to finish with timeout
pub fn wait_for_worker_with_timeout(
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    search_state: &mut SearchState,
    timeout: Duration,
    engine_adapter: Option<&Arc<Mutex<EngineAdapter>>>,
) -> Result<()> {
    use crossbeam_channel::select;
    const SELECT_TIMEOUT: Duration = Duration::from_millis(50);

    let wait_start = Instant::now();
    log::info!("wait_for_worker_with_timeout: started with timeout={timeout:?}");

    // Respect the caller-provided timeout; do not clamp to MIN_JOIN_TIMEOUT here.
    // Shutdown paths should pass MIN_JOIN_TIMEOUT explicitly.
    let deadline = Instant::now() + timeout;
    let mut finished = false;
    let mut pending_engine: Option<Engine> = None;
    let mut finished_count = 0u32;

    // Wait for Finished message or timeout
    loop {
        select! {
            recv(worker_rx) -> msg => {
                match msg {
                    Ok(WorkerMessage::Finished { from_guard, search_id: _ }) => {
                        finished_count += 1;
                        if !finished {
                            log::debug!("Worker thread finished cleanly (from_guard: {from_guard})");
                            finished = true;
                            break;
                        } else {
                            log::trace!("Ignoring duplicate Finished message #{finished_count} (from_guard: {from_guard})");
                        }
                    }
                    Ok(WorkerMessage::SearchFinished { .. }) => {
                        // Treat SearchFinished as completion signal in shutdown wait
                        // The EngineReturnGuard::drop will still run; join attempt follows below.
                        log::debug!("Worker reported SearchFinished during shutdown wait");
                        finished = true;
                        break;
                    }
                    Ok(WorkerMessage::ReturnEngine { engine, .. }) => {
                        // Hold engine until join, then return from USI thread
                        log::debug!("wait_with_timeout: captured ReturnEngine for later return");
                        pending_engine = Some(engine);
                    }
                    Ok(WorkerMessage::HardDeadlineFire { .. }) => {
                        // ignore in shutdown wait loop
                        log::trace!("HardDeadlineFire during shutdown - ignoring");
                    }
                    Ok(WorkerMessage::Info { info, search_id }) => {
                        // Info messages during shutdown can be ignored
                        log::trace!("Received info during shutdown (search_id={}): {info:?}", search_id);
                    }
                    // WorkerMessage::BestMove has been completely removed.
                    // All bestmove emissions now go through the session-based approach
                    Ok(WorkerMessage::PartialResult { .. }) => {
                        // Partial results during shutdown can be ignored
                        log::trace!("PartialResult during shutdown - ignoring");
                    }
                    Ok(WorkerMessage::Error { message, search_id }) => {
                        log::error!("Worker error during shutdown (search_id: {search_id}): {message}");
                    }
                    Ok(WorkerMessage::IterationCommitted { .. }) => {
                        // Committed iteration updates during shutdown can be ignored
                        log::trace!("IterationCommitted during shutdown - ignoring");
                    }
                    Ok(WorkerMessage::SearchStarted { .. }) => {
                        // Search started during shutdown can be ignored
                        log::trace!("SearchStarted during shutdown - ignoring");
                    }

                    Err(_) => {
                        log::error!("Worker channel closed unexpectedly");
                        break;
                    }
                }
            }
            default(SELECT_TIMEOUT) => {
                if Instant::now() > deadline {
                    log::error!("Worker thread timeout (timeout={:?})", timeout);
                    // Return error instead of exit for graceful handling
                    return Err(anyhow::anyhow!("Worker thread timeout"));
                }
            }
        }
    }

    // If we received Finished, join() should complete immediately
    if finished {
        if let Some(handle) = worker_handle.take() {
            let join_start = Instant::now();

            // Try to join with a short timeout
            const MAX_JOIN_WAIT: Duration = Duration::from_millis(100);

            // Use a channel to signal join completion
            let (tx, rx) = crossbeam_channel::bounded(1);

            // Spawn a thread to perform the join
            std::thread::spawn(move || match handle.join() {
                Ok(()) => {
                    let _ = tx.send(Ok(()));
                }
                Err(_) => {
                    let _ = tx.send(Err(()));
                }
            });

            // Wait for join to complete with timeout
            match rx.recv_timeout(MAX_JOIN_WAIT) {
                Ok(Ok(())) => {
                    let join_duration = join_start.elapsed();
                    log::debug!("Worker thread joined successfully in {join_duration:?}");
                }
                Ok(Err(())) => {
                    log::error!("Worker thread panicked");
                }
                Err(_) => {
                    log::warn!("Worker thread join timeout after {MAX_JOIN_WAIT:?}, continuing without join");
                    // The join thread will clean up eventually
                }
            }
        }
    }

    // If engine has been transferred, return it now from USI thread
    if let (Some(engine), Some(adapter_mutex)) = (pending_engine.take(), engine_adapter) {
        log::debug!("wait_with_timeout: returning engine from USI thread after join");
        let mut adapter = lock_or_recover_adapter(adapter_mutex);
        adapter.return_engine(engine);
    }

    let total_wait_duration = wait_start.elapsed();
    log::info!(
        "wait_for_worker_with_timeout: completed in {total_wait_duration:?}, finished={finished}"
    );

    // Set state idle after worker join
    *search_state = SearchState::Idle;

    // Drain any remaining messages in worker_rx
    while worker_rx.try_recv().is_ok() {
        // Just drain - messages during shutdown can be ignored
    }

    Ok(())
}

/// Wait synchronously for the worker thread to finish (no timeout)
///
/// - Blocks on `worker_rx.recv()` until a `Finished` message arrives.
/// - Then joins the worker thread handle without any timeout.
/// - Sets `search_state` to `Idle` and drains any remaining messages.
pub fn wait_for_worker_sync(
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    search_state: &mut SearchState,
    engine_adapter: &Arc<Mutex<EngineAdapter>>,
) -> Result<()> {
    log::info!("wait_for_worker_sync: started (no timeout)");

    // Block until we get Finished (collect ReturnEngine if arrives)
    let mut pending_engine: Option<Engine> = None;
    loop {
        match worker_rx.recv() {
            Ok(WorkerMessage::Finished { from_guard, .. }) => {
                log::debug!("wait_for_worker_sync: received Finished (from_guard: {from_guard})");
                break;
            }
            Ok(WorkerMessage::SearchFinished { .. }) => {
                // Keep waiting for Finished to ensure guard drop/engine return completed
                log::debug!("wait_for_worker_sync: observed SearchFinished; waiting for Finished");
            }
            Ok(WorkerMessage::ReturnEngine { engine, .. }) => {
                log::debug!(
                    "wait_for_worker_sync: captured ReturnEngine for later USI-side return"
                );
                pending_engine = Some(engine);
            }
            Ok(WorkerMessage::HardDeadlineFire { .. }) => {
                // Ignore during shutdown/sync wait
                log::trace!("wait_for_worker_sync: ignoring HardDeadlineFire");
            }
            Ok(WorkerMessage::Info { .. })
            | Ok(WorkerMessage::IterationCommitted { .. })
            | Ok(WorkerMessage::PartialResult { .. })
            | Ok(WorkerMessage::SearchStarted { .. }) => {
                // Ignore during sync wait
            }
            Ok(WorkerMessage::Error { message, search_id }) => {
                log::error!(
                    "wait_for_worker_sync: worker error during sync wait (search_id={search_id}): {message}"
                );
                // Continue to wait for Finished
            }
            Err(_) => {
                log::error!("wait_for_worker_sync: worker channel closed unexpectedly");
                break;
            }
        }
    }

    // Join the worker thread handle without timeout
    if let Some(handle) = worker_handle.take() {
        match handle.join() {
            Ok(()) => log::debug!("wait_for_worker_sync: worker thread joined successfully"),
            Err(_) => log::error!("wait_for_worker_sync: worker thread panicked"),
        }
    } else {
        log::trace!("wait_for_worker_sync: no worker handle to join");
    }

    // Return engine now from USI thread if transferred
    if let Some(engine) = pending_engine.take() {
        log::debug!("wait_for_worker_sync: returning engine to adapter after join");
        let mut adapter = lock_or_recover_adapter(engine_adapter);
        adapter.return_engine(engine);
        log::debug!("wait_for_worker_sync: engine returned");
    }

    // Set state idle after worker join
    *search_state = SearchState::Idle;

    // Drain remaining non-structural messages from worker_rx (Info-like only)
    let mut drained_info = 0usize;
    loop {
        match worker_rx.try_recv() {
            Ok(WorkerMessage::Info { .. })
            | Ok(WorkerMessage::IterationCommitted { .. })
            | Ok(WorkerMessage::PartialResult { .. })
            | Ok(WorkerMessage::HardDeadlineFire { .. }) => {
                drained_info += 1;
            }
            Ok(WorkerMessage::ReturnEngine { .. }) => {
                // Engine should have been handled earlier in wait_for_worker_sync path.
                log::trace!("wait_for_worker_sync: late ReturnEngine dropped (already joined)");
            }
            Ok(WorkerMessage::SearchStarted { .. }) => {
                // Late SearchStarted should not happen; log and drop
                log::trace!("wait_for_worker_sync: late SearchStarted dropped");
            }
            Ok(WorkerMessage::SearchFinished { .. }) => {
                // Structural message after Finished/join; drop with trace
                log::trace!("wait_for_worker_sync: late SearchFinished dropped");
            }
            Ok(WorkerMessage::Finished { .. }) => {
                // Structural duplicate; drop
                log::trace!("wait_for_worker_sync: late Finished dropped");
            }
            Ok(WorkerMessage::Error { .. }) => {
                // Error after join — log and drop
                log::trace!("wait_for_worker_sync: late Error dropped");
            }
            Err(_) => break,
        }
    }
    if drained_info > 0 {
        log::trace!("wait_for_worker_sync: drained {drained_info} leftover info messages");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_adapter::EngineAdapter;
    use engine_core::time_management::{TimeControl, TimeParameters};
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_mate_moves_to_pseudo_cp() {
        // Test mate 0 returns None (sign ambiguous)
        assert_eq!(mate_moves_to_pseudo_cp(0), None);

        // Test positive mate (we're winning)
        assert_eq!(mate_moves_to_pseudo_cp(1), Some(MATE_SCORE - 100));
        assert_eq!(mate_moves_to_pseudo_cp(3), Some(MATE_SCORE - 300));
        assert_eq!(mate_moves_to_pseudo_cp(10), Some(MATE_SCORE - 1000));

        // Test negative mate (we're losing)
        assert_eq!(mate_moves_to_pseudo_cp(-1), Some(-MATE_SCORE + 100));
        assert_eq!(mate_moves_to_pseudo_cp(-2), Some(-MATE_SCORE + 200));
        assert_eq!(mate_moves_to_pseudo_cp(-5), Some(-MATE_SCORE + 500));

        // Test edge cases
        assert_eq!(mate_moves_to_pseudo_cp(300), Some(MATE_SCORE - 30000));
        assert_eq!(mate_moves_to_pseudo_cp(-300), Some(-MATE_SCORE + 30000));
    }

    #[test]
    fn test_derive_budgets_fixed_time() {
        let limits = engine_core::search::SearchLimits::builder()
            .time_control(TimeControl::FixedTime { ms_per_move: 1000 })
            .build();
        let pos = engine_core::shogi::Position::startpos();
        let budgets = derive_budgets_via_core(&pos, &limits).expect("budgets for fixed_time");
        assert!(budgets.0 > 0 && budgets.1 >= budgets.0);
    }

    #[test]
    fn test_derive_budgets_byoyomi_with_params() {
        let params = TimeParameters::default(); // overhead=50, soft_ratio=0.8, byoyomi_safety=500
        let limits = engine_core::search::SearchLimits::builder()
            .byoyomi(0, 10_000, 1)
            .time_parameters(params)
            .build();
        let pos = engine_core::shogi::Position::startpos();
        let (soft, hard, _) = derive_budgets_via_core(&pos, &limits).expect("budgets for byoyomi");
        // Soft includes half of network_delay2_ms; hard includes full network_delay2_ms
        assert_eq!(soft, 8000 - params.overhead_ms - params.network_delay2_ms / 2);
        assert_eq!(
            hard,
            10_000
                - (params.overhead_ms
                    + params.byoyomi_hard_limit_reduction_ms
                    + params.network_delay2_ms)
        );
    }

    #[test]
    fn test_derive_budgets_ponder_byoyomi_none() {
        let params = TimeParameters::default();
        let limits = engine_core::search::SearchLimits::builder()
            .byoyomi(0, 6_000, 1)
            .time_parameters(params)
            .ponder_with_inner()
            .build();
        let pos = engine_core::shogi::Position::startpos();
        assert!(derive_budgets_via_core(&pos, &limits).is_none());
    }

    #[test]
    fn test_derive_budgets_fixed_nodes_none() {
        let limits = engine_core::search::SearchLimits::builder().fixed_nodes(100_000).build();
        let pos = engine_core::shogi::Position::startpos();
        assert!(derive_budgets_via_core(&pos, &limits).is_none());
    }

    #[test]
    fn test_return_engine_unified_on_sync_wait() {
        // Prepare adapter with no engine (simulate taken engine)
        let adapter = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut ad = adapter.lock().unwrap();
            // Take the engine out to simulate worker owning it
            let _ = ad.take_engine().expect("engine available");
            assert!(!ad.is_engine_available());
        }

        // Channel to simulate worker messages
        let (tx, rx) = crossbeam_channel::unbounded::<WorkerMessage>();

        // Send ReturnEngine followed by Finished (from worker, not guard)
        let engine = engine_core::engine::controller::Engine::new(
            engine_core::engine::controller::EngineType::Material,
        );
        tx.send(WorkerMessage::ReturnEngine {
            engine,
            search_id: 1,
        })
        .unwrap();
        tx.send(WorkerMessage::Finished {
            from_guard: false,
            search_id: 1,
        })
        .unwrap();

        // Call sync wait with no worker handle
        let mut handle: Option<std::thread::JoinHandle<()>> = None;
        let mut state = crate::state::SearchState::StopRequested;
        let r = wait_for_worker_sync(&mut handle, &rx, &mut state, &adapter);
        assert!(r.is_ok());
        assert_eq!(state, crate::state::SearchState::Idle);

        // Engine should have been returned to adapter by USI thread path
        let ad = adapter.lock().unwrap();
        assert!(ad.is_engine_available());
    }
}
