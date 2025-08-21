use crate::engine_adapter::EngineAdapter;
use crate::search_session::SearchSession;
use crate::state::SearchState;
use crate::usi::output::{Score, SearchInfo};
use crate::usi::{self, send_info_string, send_response_or_exit, UsiResponse};
use crate::utils::lock_or_recover_generic;
use anyhow::Result;
use crossbeam_channel::{Receiver, Sender};
use engine_core::engine::controller::Engine;
use engine_core::search::constants::MATE_SCORE;
use engine_core::search::SearchLimits;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Messages from worker thread to main thread
pub enum WorkerMessage {
    Info(SearchInfo),

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
    },

    // Legacy messages (to be phased out)
    BestMove {
        best_move: String,
        ponder_move: Option<String>,
        search_id: u64,
    },
    /// Partial result available during search
    PartialResult {
        current_best: String,
        depth: u32,
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
    EngineReturn(Engine), // Return the engine after search
}

/// Convert mate moves to pseudo centipawn value for ordering
///
/// This helper function provides a consistent scale for converting mate scores
/// to centipawn equivalents. This is used for ordering purposes when comparing
/// different search results.
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
            log::debug!("EngineReturnGuard: returning engine");

            // Try to return engine through channel
            match self.tx.try_send(WorkerMessage::EngineReturn(engine)) {
                Ok(()) => {
                    log::debug!("Engine returned successfully through channel");
                }
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    // Channel is full - this shouldn't happen with unbounded channel
                    log::error!("Channel full, cannot return engine");
                    // Engine will be dropped here, which is safe
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    // Channel is disconnected - main thread has exited
                    log::warn!("Channel disconnected, cannot return engine");
                    // Engine will be dropped here, which is safe
                }
            }

            // Always try to send Finished message to signal completion (from guard)
            let _ = self.tx.try_send(WorkerMessage::Finished {
                from_guard: true,
                search_id: self.search_id,
            });
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

    // Set up info callback with partial result tracking
    let tx_info = tx.clone();
    let tx_partial = tx.clone();
    let last_partial_depth = Arc::new(Mutex::new(0u32));
    let info_callback = move |info: SearchInfo| {
        // Always send the info message
        let _ = tx_info.send(WorkerMessage::Info(info.clone()));

        // Send partial result at certain depth intervals
        if let (Some(depth), Some(score), Some(pv)) =
            (info.depth, info.score.as_ref(), info.pv.first())
        {
            // Check if we should send a partial result
            let should_send = {
                let mut last_depth = lock_or_recover_generic(&last_partial_depth);
                if depth >= *last_depth + 5 || (depth >= 10 && depth > *last_depth) {
                    *last_depth = depth;
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
                        depth,
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
    let (engine, position, limits, ponder_hit_flag) = {
        let mut adapter = lock_or_recover_adapter(&engine_adapter);
        log::debug!("Adapter lock acquired, calling take_engine");
        match adapter.take_engine() {
            Ok(engine) => {
                log::debug!("Engine taken successfully, preparing search");
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
                            match adapter.generate_emergency_move() {
                                Ok(emergency_move) => {
                                    log::info!(
                                        "Generated emergency move after preparation error: {emergency_move}"
                                    );
                                    if let Err(e) = tx.send(WorkerMessage::BestMove {
                                        best_move: emergency_move,
                                        ponder_move: None,
                                        search_id,
                                    }) {
                                        log::error!("Failed to send emergency move: {e}");
                                    }
                                }
                                Err(_) => {
                                    // Only resign if no legal moves available
                                    if let Err(e) = tx.send(WorkerMessage::BestMove {
                                        best_move: "resign".to_string(),
                                        ponder_move: None,
                                        search_id,
                                    }) {
                                        log::error!(
                                            "Failed to send resign after preparation error: {e}"
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
                    match adapter.generate_emergency_move() {
                        Ok(emergency_move) => {
                            log::info!(
                                "Generated emergency move after engine take error: {emergency_move}"
                            );
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: emergency_move,
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send emergency move: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves available
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: "resign".to_string(),
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send resign after engine take error: {e}");
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
    let last_committed_depth = Arc::new(Mutex::new(0u32));
    let last_committed_depth_cb = last_committed_depth.clone();

    // Create info callback that updates session
    let enhanced_info_callback = move |info: SearchInfo| {
        // Call original callback
        info_callback(info.clone());

        // Update session on each iteration completion
        if !info.pv.is_empty() {
            // Only commit when depth increases (not for same depth updates)
            let depth = info.depth.unwrap_or(0);
            {
                let mut last = lock_or_recover_generic(&last_committed_depth_cb);
                if depth <= *last {
                    // Same or lower depth - skip commit
                    return;
                }
                *last = depth;
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
                            let mate_score = MATE_SCORE - (mate.abs() * 2);
                            if mate > 0 {
                                Some(mate_score)
                            } else {
                                Some(-mate_score)
                            }
                        }
                    }
                    None => Some(0),
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
                            info.depth.unwrap_or(0),
                            raw_score,
                            pv_moves,
                            engine_core::search::NodeType::Exact,
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
    let mut engine_guard = EngineReturnGuard::new(engine, tx.clone(), search_id);

    // Execute search without holding the lock
    log::info!("Calling execute_search_static");
    let result = EngineAdapter::execute_search_static(
        &mut engine_guard,
        position.clone(),
        limits,
        Box::new(enhanced_info_callback),
    );
    log::info!("execute_search_static returned: {:?}", result.is_ok());

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
            session.update_current_best(
                extended_result.depth,
                extended_result.score,
                extended_result.pv, // Original Move objects with piece types preserved
                extended_result.node_type,
            );
            session.commit_iteration();
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
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: emergency_move,
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send emergency move: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: "resign".to_string(),
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send resign after stop: {e}");
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
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: emergency_move,
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send emergency move: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: "resign".to_string(),
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send resign after error: {e}");
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

/// Wait for worker thread to finish with timeout
pub fn wait_for_worker_with_timeout(
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    engine: &Arc<Mutex<EngineAdapter>>,
    search_state: &mut SearchState,
    timeout: Duration,
) -> Result<()> {
    use crossbeam_channel::select;
    const SELECT_TIMEOUT: Duration = Duration::from_millis(50);
    const MIN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

    let deadline = Instant::now() + timeout.max(MIN_JOIN_TIMEOUT);
    let mut finished = false;
    let mut engine_returned = false;
    let mut finished_count = 0u32;

    // Wait for Finished message AND EngineReturn message or timeout
    loop {
        select! {
            recv(worker_rx) -> msg => {
                match msg {
                    Ok(WorkerMessage::Finished { from_guard, search_id: _ }) => {
                        finished_count += 1;
                        if !finished {
                            log::debug!("Worker thread finished cleanly (from_guard: {from_guard})");
                            finished = true;
                            if engine_returned {
                                break;
                            }
                        } else {
                            log::trace!("Ignoring duplicate Finished message #{finished_count} (from_guard: {from_guard})");
                        }
                    }
                    Ok(WorkerMessage::EngineReturn(returned_engine)) => {
                        log::debug!("Engine returned from worker");
                        let mut adapter = lock_or_recover_adapter(engine);
                        adapter.return_engine(returned_engine);
                        engine_returned = true;
                        if finished {
                            break;
                        }
                    }
                    Ok(WorkerMessage::Info(info)) => {
                        // Info messages during shutdown can be ignored
                        log::trace!("Received info during shutdown: {info:?}");
                    }
                    Ok(WorkerMessage::BestMove { best_move, ponder_move, search_id }) => {
                        // During shutdown, we may accept late bestmoves
                        // For safety, we could check search_id here but during shutdown
                        // we're more lenient since we're trying to clean up
                        if search_state.can_accept_bestmove() {
                            log::debug!("Accepting bestmove during shutdown (search_id: {search_id})");
                            send_response_or_exit(UsiResponse::BestMove {
                                best_move,
                                ponder: ponder_move,
                            });
                            // Mark search as finished when bestmove is received
                            *search_state = SearchState::Idle;
                        } else {
                            log::warn!("Ignoring late bestmove during shutdown: {best_move} (search_id: {search_id})");
                        }
                    }
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
            match handle.join() {
                Ok(()) => log::debug!("Worker thread joined successfully"),
                Err(_) => log::error!("Worker thread panicked"),
            }
        }
    }

    *search_state = SearchState::Idle;

    // Drain any remaining messages in worker_rx
    while let Ok(msg) = worker_rx.try_recv() {
        match msg {
            WorkerMessage::EngineReturn(returned_engine) => {
                log::debug!("Engine returned during drain");
                let mut adapter = lock_or_recover_adapter(engine);
                adapter.return_engine(returned_engine);
            }
            _ => {
                log::trace!("Drained message: {:?}", std::any::type_name_of_val(&msg));
            }
        }
    }

    Ok(())
}
