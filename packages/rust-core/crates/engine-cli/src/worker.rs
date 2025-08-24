use crate::engine_adapter::EngineAdapter;
use crate::search_session::SearchSession;
use crate::state::SearchState;
use crate::usi::output::{Score, SearchInfo};
use crate::usi::{self, send_info_string};
use crate::utils::lock_or_recover_generic;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use engine_core::engine::controller::Engine;
use engine_core::search::constants::MATE_SCORE;
use engine_core::search::types::{StopInfo, TerminationReason};
use engine_core::search::SearchLimits;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Messages from worker thread to main thread
pub enum WorkerMessage {
    Info {
        info: SearchInfo,
        search_id: u64,
    },

    /// Search has started
    SearchStarted {
        search_id: u64,
        start_time: Instant,
    },

    /// Iteration completed with committed results
    IterationComplete {
        session: Box<SearchSession>,
        search_id: u64,
    },

    /// Search finished (use committed results from session)
    SearchFinished {
        session_id: u64,
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

/// Guard to ensure engine is returned on drop (for panic safety)
pub struct EngineReturnGuard {
    engine: Option<Engine>,
    adapter: Arc<Mutex<EngineAdapter>>,
    tx: Sender<WorkerMessage>,
    search_id: u64,
}

impl EngineReturnGuard {
    pub fn new(
        engine: Engine,
        adapter: Arc<Mutex<EngineAdapter>>,
        tx: Sender<WorkerMessage>,
        search_id: u64,
    ) -> Self {
        Self {
            engine: Some(engine),
            adapter,
            tx,
            search_id,
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
        if let Some(engine) = self.engine.take() {
            log::debug!("EngineReturnGuard: returning engine directly to adapter");

            // Return engine directly to adapter
            let mut adapter = lock_or_recover_adapter(&self.adapter);
            adapter.return_engine(engine);
            log::debug!("Engine returned successfully to adapter");

            // Send finished notification
            if let Err(e) = self.tx.send(WorkerMessage::Finished {
                from_guard: true,
                search_id: self.search_id,
            }) {
                log::warn!("Failed to send Finished message from guard: {e}");
                // This is OK - channel might be closed during shutdown
            }
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

            // Try to notify GUI about the reset
            let _ = send_info_string(
                "Engine state reset due to error recovery. Please send 'isready' to reinitialize.",
            );

            guard
        }
    }
}

/// Worker thread function for search
pub fn search_worker(
    engine_adapter: Arc<Mutex<EngineAdapter>>,
    params: usi::GoParams,
    stop_flag: Arc<AtomicBool>,
    tx: Sender<WorkerMessage>,
    search_id: u64,
) {
    log::debug!("Search worker thread started with params: {params:?}");
    let initial_stop_value = stop_flag.load(Ordering::Acquire);
    log::info!(
        "Worker: search_id={search_id}, ponder={}, stop_flag_ptr={:p}, stop_flag_value={}",
        params.ponder,
        stop_flag.as_ref(),
        initial_stop_value
    );

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

    // Set up info callback with partial result tracking
    let tx_info = tx.clone();
    let tx_partial = tx.clone();
    let last_partial_depth = Arc::new(Mutex::new(0u8));
    let info_callback = move |info: SearchInfo| {
        // Always send the info message
        let _ = tx_info.send(WorkerMessage::Info {
            info: info.clone(),
            search_id,
        });

        // Send partial result at certain depth intervals
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
    let (engine, position, limits, ponder_hit_flag) = {
        let mut adapter = lock_or_recover_adapter(&engine_adapter);
        log::debug!("Adapter lock acquired, calling take_engine");
        let engine_available = adapter.is_engine_available();
        log::info!("Worker: engine available before take: {engine_available}");
        match adapter.take_engine() {
            Ok(engine) => {
                log::debug!("Engine taken successfully, preparing search");
                let take_duration = engine_take_start.elapsed();
                log::info!("Worker: engine taken successfully after {take_duration:?}");
                match adapter.prepare_search(&params, stop_flag.clone()) {
                    Ok((pos, lim, flag)) => {
                        log::debug!("Search prepared successfully");
                        (engine, pos, lim, flag)
                    }
                    Err(e) => {
                        // Return engine and send error
                        adapter.return_engine(engine);
                        log::error!("Search preparation error: {e}");
                        let _ = tx.send(WorkerMessage::Error {
                            message: e.to_string(),
                            search_id,
                        });

                        // Try to generate emergency move before resigning (only if not pondering)
                        if !params.ponder {
                            // Get position hash for session
                            let position_hash = adapter.get_position().map(|p| p.hash).unwrap_or(0);

                            match adapter.generate_emergency_move() {
                                Ok(emergency_move) => {
                                    log::info!(
                                        "Generated emergency move after preparation error: {emergency_move}"
                                    );

                                    // Create emergency session
                                    let emergency_session = create_emergency_session(
                                        search_id,
                                        position_hash,
                                        emergency_move,
                                        false,
                                    );

                                    // Send session update
                                    if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                                        session: Box::new(emergency_session.clone()),
                                        search_id,
                                    }) {
                                        log::error!("Failed to send emergency session: {e}");
                                    }

                                    // Send search finished
                                    if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                                        session_id: emergency_session.id,
                                        root_hash: emergency_session.root_hash,
                                        search_id,
                                        stop_info: Some(StopInfo {
                                            reason: TerminationReason::Error,
                                            elapsed_ms: 0,
                                            nodes: 0,
                                            depth_reached: 1,
                                            hard_timeout: false,
                                        }),
                                    }) {
                                        log::error!("Failed to send search finished: {e}");
                                    }
                                }
                                Err(_) => {
                                    // Only resign if no legal moves available
                                    let resign_session = create_emergency_session(
                                        search_id,
                                        position_hash,
                                        "resign".to_string(),
                                        true,
                                    );

                                    // Send session update
                                    if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                                        session: Box::new(resign_session.clone()),
                                        search_id,
                                    }) {
                                        log::error!("Failed to send resign session: {e}");
                                    }

                                    // Send search finished
                                    if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                                        session_id: resign_session.id,
                                        root_hash: resign_session.root_hash,
                                        search_id,
                                        stop_info: Some(StopInfo {
                                            reason: TerminationReason::Error,
                                            elapsed_ms: 0,
                                            nodes: 0,
                                            depth_reached: 0,
                                            hard_timeout: false,
                                        }),
                                    }) {
                                        log::error!(
                                            "Failed to send search finished after resign: {e}"
                                        );
                                    }
                                }
                            }
                        } else {
                            log::info!(
                                "Ponder preparation error, not sending bestmove (USI protocol)"
                            );
                        }

                        let _ = tx.send(WorkerMessage::Finished {
                            from_guard: false,
                            search_id,
                        });
                        return;
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to take engine: {e}");
                let _ = tx.send(WorkerMessage::Error {
                    message: e.to_string(),
                    search_id,
                });

                // Try to generate emergency move from adapter (only if not pondering)
                if !params.ponder {
                    log::info!("Attempting to generate emergency move after engine take failure");

                    // Get position hash for session
                    let position_hash = adapter.get_position().map(|p| p.hash).unwrap_or(0);

                    match adapter.generate_emergency_move() {
                        Ok(emergency_move) => {
                            log::info!(
                                "Generated emergency move after engine take error: {emergency_move}"
                            );

                            // Create emergency session
                            let emergency_session = create_emergency_session(
                                search_id,
                                position_hash,
                                emergency_move,
                                false,
                            );

                            // Send session update
                            if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                                session: Box::new(emergency_session.clone()),
                                search_id,
                            }) {
                                log::error!("Failed to send emergency session: {e}");
                            }

                            // Send search finished
                            if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                                session_id: emergency_session.id,
                                root_hash: emergency_session.root_hash,
                                search_id,
                                stop_info: Some(StopInfo {
                                    reason: TerminationReason::Error,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 1,
                                    hard_timeout: false,
                                }),
                            }) {
                                log::error!("Failed to send search finished: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves available
                            let resign_session = create_emergency_session(
                                search_id,
                                position_hash,
                                "resign".to_string(),
                                true,
                            );

                            // Send session update
                            if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                                session: Box::new(resign_session.clone()),
                                search_id,
                            }) {
                                log::error!("Failed to send resign session: {e}");
                            }

                            // Send search finished
                            if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                                session_id: resign_session.id,
                                root_hash: resign_session.root_hash,
                                search_id,
                                stop_info: Some(StopInfo {
                                    reason: TerminationReason::Error,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 0,
                                    hard_timeout: false,
                                }),
                            }) {
                                log::error!("Failed to send search finished after resign: {e}");
                            }
                        }
                    }
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

    // Create search session
    let mut session = SearchSession::new(search_id, position.hash);

    // Wrap session in Arc<Mutex<>> for shared access in callback
    let session_arc = Arc::new(Mutex::new(session.clone()));
    let session_for_callback = session_arc.clone();
    let tx_for_iteration = tx.clone();

    // Track last committed depth to avoid committing same depth multiple times
    let last_committed_depth = Arc::new(Mutex::new(0u8));
    let last_committed_depth_cb = last_committed_depth.clone();

    // Create info callback that updates session
    let enhanced_info_callback = move |info: SearchInfo| {
        // Call original callback
        info_callback(info.clone());

        // Update session on each iteration completion
        if !info.pv.is_empty() {
            // Only commit when depth increases (not for same depth updates)
            let depth = info.depth.unwrap_or(0);
            let depth_u8 = depth as u8;
            {
                let mut last = lock_or_recover_generic(&last_committed_depth_cb);
                if depth_u8 <= *last {
                    // Same or lower depth - skip commit
                    return;
                }
                *last = depth_u8;
            }
            // Note: The info.pv contains USI strings which lose piece type information.
            // This is a limitation of the current architecture where SearchInfo uses strings.
            // For now, we parse them back but accept the loss of piece type info.
            // TODO: Consider passing original Move objects through a different channel
            // to preserve full move information including piece types.
            if !info.pv.is_empty() {
                // Extract raw score from Score enum
                let raw_score = match info.score {
                    Some(Score::Cp(cp)) => Some(cp),
                    Some(Score::Mate(mate)) => {
                        if mate == 0 {
                            // mate 0: sign is ambiguous, skip score update
                            log::debug!("Skipping session update for mate 0 (sign ambiguous)");
                            None
                        } else {
                            // Convert mate score to raw score format
                            // Positive mate = we're winning
                            // Note: This preserves the engine's internal score representation
                            // (MATE_SCORE - plies), which uses 2 plies per move.
                            // This is different from the simplified pseudo-cp conversion
                            // used for partial results in mate_moves_to_pseudo_cp().
                            let mate_score = MATE_SCORE - (mate.abs() * 2);
                            if mate > 0 {
                                Some(mate_score)
                            } else {
                                Some(-mate_score)
                            }
                        }
                    }
                    None => None,
                };

                // Only update session if we have a valid score
                if let Some(raw_score) = raw_score {
                    // Convert PV from USI strings to Move objects
                    // WARNING: This loses piece type information from the original moves
                    let pv_moves: Vec<_> = info
                        .pv
                        .iter()
                        .filter_map(|m| engine_core::usi::parse_usi_move(m).ok())
                        .collect();

                    // Update session
                    if let Ok(mut session_guard) = session_for_callback.lock() {
                        // Note: During iterative deepening, we don't have NodeType information
                        // from the info callback, so we default to Exact. The actual NodeType
                        // will be set when the final search result is available.
                        session_guard.update_current_best(
                            info.depth.unwrap_or(0) as u8,
                            raw_score,
                            pv_moves,
                        );
                        session_guard.commit_iteration();

                        // Send IterationComplete message
                        let _ = tx_for_iteration.send(WorkerMessage::IterationComplete {
                            session: Box::new(session_guard.clone()),
                            search_id,
                        });
                    }
                }
            }
        }
    };

    // Wrap engine in guard for panic safety
    let mut engine_guard =
        EngineReturnGuard::new(engine, engine_adapter.clone(), tx.clone(), search_id);

    // Execute search without holding the lock
    log::info!("Calling execute_search_static");
    let search_start = Instant::now();
    let result = EngineAdapter::execute_search_static(
        &mut engine_guard,
        position.clone(),
        limits,
        Box::new(enhanced_info_callback),
    );
    let search_duration = search_start.elapsed();
    log::info!("execute_search_static returned after {search_duration:?}: {:?}", result.is_ok());

    // Handle result
    match result {
        Ok(extended_result) => {
            // Update session with final result using the original Move objects
            log::debug!(
                "Worker: Updating session with best_move: {}, ponder_move: {:?}, depth: {}",
                extended_result.best_move,
                extended_result.ponder_move,
                extended_result.depth
            );

            // Use actual search result data with original Move objects
            // This preserves the piece type information that would be lost if we
            // converted to USI strings and back
            session.update_current_best_with_seldepth(
                extended_result.depth,
                extended_result.seldepth,
                extended_result.score,
                extended_result.pv, // Original Move objects with piece types preserved
            );
            session.commit_iteration();

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

            // Send best move if:
            // - Not a ponder search OR
            // - Ponder search that was converted via ponderhit
            if !was_ponder || ponder_hit_occurred {
                log::info!(
                    "Sending search completion: was_ponder={was_ponder}, ponder_hit={ponder_hit_occurred}"
                );
                // Send completed session instead of raw bestmove
                if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                    session: Box::new(session.clone()),
                    search_id,
                }) {
                    log::error!("Failed to send iteration complete: {e}");
                }
                // Also send SearchFinished to indicate we're done
                if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                    session_id: session.id,
                    root_hash: session.root_hash,
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
            // Engine will be returned automatically by EngineReturnGuard::drop

            // Clean up ponder state if needed
            {
                let mut adapter = lock_or_recover_adapter(&engine_adapter);
                adapter.cleanup_after_search(was_ponder);
            }

            // Try to generate emergency move before sending error
            let emergency_result = {
                let adapter = lock_or_recover_adapter(&engine_adapter);
                adapter.generate_emergency_move()
            };

            if stop_flag.load(Ordering::Acquire) {
                // Check if ponderhit occurred for ponder search
                let ponder_hit_occurred = if was_ponder {
                    ponder_hit_flag_ref
                        .as_ref()
                        .map(|flag| flag.load(Ordering::Acquire))
                        .unwrap_or(false)
                } else {
                    false
                };

                // Stopped by user - send bestmove if:
                // - Not a ponder search OR
                // - Ponder search that was converted via ponderhit
                if !was_ponder || ponder_hit_occurred {
                    // Normal search or ponder-hit search that was stopped - send emergency move
                    match emergency_result {
                        Ok(emergency_move) => {
                            log::info!("Generated emergency move after stop: {emergency_move}");

                            // Create emergency session
                            let emergency_session = create_emergency_session(
                                search_id,
                                position.hash,
                                emergency_move,
                                false,
                            );

                            // Send session update
                            if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                                session: Box::new(emergency_session.clone()),
                                search_id,
                            }) {
                                log::error!("Failed to send emergency session: {e}");
                            }

                            // Send search finished
                            if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                                session_id: emergency_session.id,
                                root_hash: emergency_session.root_hash,
                                search_id,
                                stop_info: Some(StopInfo {
                                    reason: TerminationReason::UserStop,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 1,
                                    hard_timeout: false,
                                }),
                            }) {
                                log::error!("Failed to send search finished: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves
                            let resign_session = create_emergency_session(
                                search_id,
                                position.hash,
                                "resign".to_string(),
                                true,
                            );

                            // Send session update
                            if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                                session: Box::new(resign_session.clone()),
                                search_id,
                            }) {
                                log::error!("Failed to send resign session: {e}");
                            }

                            // Send search finished
                            if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                                session_id: resign_session.id,
                                root_hash: resign_session.root_hash,
                                search_id,
                                stop_info: Some(StopInfo {
                                    reason: TerminationReason::UserStop,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 0,
                                    hard_timeout: false,
                                }),
                            }) {
                                log::error!("Failed to send search finished after resign: {e}");
                            }
                        }
                    }
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

                // Send bestmove if not ponder OR ponder was converted via ponderhit
                if !was_ponder || ponder_hit_occurred {
                    match emergency_result {
                        Ok(emergency_move) => {
                            log::info!(
                                "Generated emergency move after search error: {emergency_move}"
                            );

                            // Create emergency session
                            let emergency_session = create_emergency_session(
                                search_id,
                                position.hash,
                                emergency_move,
                                false,
                            );

                            // Send session update
                            if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                                session: Box::new(emergency_session.clone()),
                                search_id,
                            }) {
                                log::error!("Failed to send emergency session: {e}");
                            }

                            // Send search finished
                            if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                                session_id: emergency_session.id,
                                root_hash: emergency_session.root_hash,
                                search_id,
                                stop_info: Some(StopInfo {
                                    reason: TerminationReason::Error,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 1,
                                    hard_timeout: false,
                                }),
                            }) {
                                log::error!("Failed to send search finished: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves
                            let resign_session = create_emergency_session(
                                search_id,
                                position.hash,
                                "resign".to_string(),
                                true,
                            );

                            // Send session update
                            if let Err(e) = tx.send(WorkerMessage::IterationComplete {
                                session: Box::new(resign_session.clone()),
                                search_id,
                            }) {
                                log::error!("Failed to send resign session: {e}");
                            }

                            // Send search finished
                            if let Err(e) = tx.send(WorkerMessage::SearchFinished {
                                session_id: resign_session.id,
                                root_hash: resign_session.root_hash,
                                search_id,
                                stop_info: Some(StopInfo {
                                    reason: TerminationReason::Error,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 0,
                                    hard_timeout: false,
                                }),
                            }) {
                                log::error!("Failed to send search finished after resign: {e}");
                            }
                        }
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
fn create_emergency_session(
    search_id: u64,
    position_hash: u64,
    best_move: String,
    is_resign: bool,
) -> SearchSession {
    let mut session = SearchSession::new(search_id, position_hash);

    // Parse the move (if not resign)
    let moves = if is_resign {
        vec![]
    } else {
        match engine_core::usi::parse_usi_move(&best_move) {
            Ok(m) => vec![m],
            Err(_) => vec![], // Invalid move format
        }
    };

    // Set a minimal depth and score for emergency moves
    let depth = 1;
    let score = if is_resign { -30000 } else { 0 }; // Large negative score for resign

    session.update_current_best_with_seldepth(depth, None, score, moves);
    session.commit_iteration();

    session
}

/// Wait for worker thread to finish with timeout
pub fn wait_for_worker_with_timeout(
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    search_state: &mut SearchState,
    timeout: Duration,
) -> Result<()> {
    use crate::helpers::MIN_JOIN_TIMEOUT;
    use crossbeam_channel::select;
    const SELECT_TIMEOUT: Duration = Duration::from_millis(50);

    let wait_start = Instant::now();
    log::info!("wait_for_worker_with_timeout: started with timeout={timeout:?}");

    let deadline = Instant::now() + timeout.max(MIN_JOIN_TIMEOUT);
    let mut finished = false;
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
                    Ok(WorkerMessage::IterationComplete { .. }) => {
                        // Iteration updates during shutdown can be ignored
                        log::trace!("IterationComplete during shutdown - ignoring");
                    }
                    Ok(WorkerMessage::SearchFinished { .. }) => {
                        // Search finished during shutdown can be ignored
                        log::trace!("SearchFinished during shutdown - ignoring");
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
                    log::error!("Worker thread timeout after {:?}", timeout.max(MIN_JOIN_TIMEOUT));
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

    let total_wait_duration = wait_start.elapsed();
    log::info!(
        "wait_for_worker_with_timeout: completed in {total_wait_duration:?}, finished={finished}"
    );

    *search_state = SearchState::Idle;

    // Drain any remaining messages in worker_rx
    while worker_rx.try_recv().is_ok() {
        // Just drain - messages during shutdown can be ignored
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
