use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::helpers::{
    calculate_max_search_time, generate_fallback_move, wait_for_search_completion,
};
use crate::search_session::SearchSession;
use crate::state::SearchState;
use crate::usi::{send_info_string, send_response, GoParams, UsiCommand, UsiResponse};
use crate::worker::{lock_or_recover_adapter, search_worker, WorkerMessage};
use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, Sender};
use engine_core::usi::position_to_sfen;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::bestmove_emitter::BestmoveEmitter;

/// Context for handling USI commands
pub struct CommandContext<'a> {
    pub engine: &'a Arc<Mutex<EngineAdapter>>,
    pub stop_flag: &'a Arc<AtomicBool>,
    pub worker_tx: &'a Sender<WorkerMessage>,
    pub worker_rx: &'a Receiver<WorkerMessage>,
    pub worker_handle: &'a mut Option<JoinHandle<()>>,
    pub search_state: &'a mut SearchState,
    pub bestmove_sent: &'a mut bool,
    pub current_search_timeout: &'a mut Duration,
    pub search_id_counter: &'a mut u64,
    pub current_search_id: &'a mut u64,
    pub current_search_is_ponder: &'a mut bool,
    pub current_session: &'a mut Option<SearchSession>,
    pub current_bestmove_emitter: &'a mut Option<BestmoveEmitter>,
    pub allow_null_move: bool,
}

pub fn handle_command(command: UsiCommand, ctx: &mut CommandContext) -> Result<()> {
    match command {
        UsiCommand::Usi => {
            send_response(UsiResponse::IdName("RustShogi 1.0".to_string()))?;
            send_response(UsiResponse::IdAuthor("RustShogi Team".to_string()))?;

            // Send available options
            {
                let engine = lock_or_recover_adapter(ctx.engine);
                for option in engine.get_options() {
                    send_response(UsiResponse::Option(option.to_string()))?;
                }
            }

            send_response(UsiResponse::UsiOk)?;
        }

        UsiCommand::IsReady => {
            // Initialize engine if needed
            {
                let mut engine = lock_or_recover_adapter(ctx.engine);
                engine.initialize()?;
            }
            send_response(UsiResponse::ReadyOk)?;
        }

        UsiCommand::Position {
            startpos,
            sfen,
            moves,
        } => {
            log::info!(
                "Handling position command - startpos: {startpos}, sfen: {sfen:?}, moves: {moves:?}"
            );
            // Wait for any ongoing search to complete before updating position
            wait_for_search_completion(
                ctx.search_state,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
            )?;

            let mut engine = lock_or_recover_adapter(ctx.engine);
            match engine.set_position(startpos, sfen.as_deref(), &moves) {
                Ok(()) => {
                    log::info!("Position command completed");
                }
                Err(e) => {
                    // Log error but don't crash - USI engines should be robust
                    log::error!("Failed to set position: {e}");
                    send_info_string(format!("Error: Failed to set position - {e}"))?;
                    // Don't propagate the error - continue running
                }
            }
        }

        UsiCommand::Go(params) => {
            handle_go_command(params, ctx)?;
        }

        UsiCommand::Stop => {
            handle_stop_command(ctx)?;
        }

        UsiCommand::PonderHit => {
            // Handle ponder hit only if we're actively pondering
            if *ctx.current_search_is_ponder && *ctx.search_state == SearchState::Searching {
                let mut engine = lock_or_recover_adapter(ctx.engine);
                // Mark that we're no longer in pure ponder mode
                *ctx.current_search_is_ponder = false;
                match engine.ponder_hit() {
                    Ok(()) => log::debug!("Ponder hit successfully processed"),
                    Err(e) => log::debug!("Ponder hit ignored: {e}"),
                }
            } else {
                log::debug!(
                    "Ponder hit ignored (state={:?}, is_ponder={})",
                    *ctx.search_state,
                    *ctx.current_search_is_ponder
                );
            }
        }

        UsiCommand::SetOption { name, value } => {
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.set_option(&name, value.as_deref())?;
        }

        UsiCommand::GameOver { result } => {
            // Stop any ongoing search and ensure worker is properly cleaned up
            ctx.stop_flag.store(true, Ordering::Release);

            // Wait for any ongoing search to complete before notifying game over
            wait_for_search_completion(
                ctx.search_state,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
            )?;

            // Log the previous search ID for debugging
            log::debug!("Reset state after gameover: prev_search_id={}", *ctx.current_search_id);

            // Clear all search-related state for clean baseline
            *ctx.current_session = None;
            *ctx.bestmove_sent = false;
            *ctx.current_search_is_ponder = false;
            // Reset to 0 so any late worker messages (old search_id) will be ignored
            *ctx.current_search_id = 0;
            // Explicitly set to Idle (defensive, wait_for_search_completion should have done this)
            *ctx.search_state = SearchState::Idle;

            // Notify engine of game result
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.game_over(result);
            log::debug!("Game over processed, worker cleaned up, state reset to Idle");
        }

        UsiCommand::UsiNewGame => {
            // ShogiGUI extension - new game notification
            // Stop any ongoing search
            wait_for_search_completion(
                ctx.search_state,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
            )?;

            // Reset engine state for new game
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.new_game();
            log::debug!("New game started");
        }

        UsiCommand::Quit => {
            // Quit is handled in main loop
            unreachable!("Quit should be handled in main loop");
        }
    }

    Ok(())
}

fn handle_go_command(params: GoParams, ctx: &mut CommandContext) -> Result<()> {
    log::info!("Received go command with params: {params:?}");

    // Stop any ongoing search and ensure engine is available
    wait_for_search_completion(
        ctx.search_state,
        ctx.stop_flag,
        ctx.worker_handle,
        ctx.worker_rx,
        ctx.engine,
    )?;

    // No delay needed - state transitions are atomic

    // Reset stop flag and bestmove_sent flag
    ctx.stop_flag.store(false, Ordering::Release);
    *ctx.bestmove_sent = false; // Reset for new search
    *ctx.current_session = None; // Clear any previous session to avoid reuse

    // Verify we can start a new search (defensive check)
    if !ctx.search_state.can_start_search() {
        let position_info = {
            let engine = lock_or_recover_adapter(ctx.engine);
            engine
                .get_position()
                .map(position_to_sfen)
                .unwrap_or_else(|| "<no position>".to_string())
        };
        log::error!(
            "Cannot start search in state: {:?}, position: {}",
            ctx.search_state,
            position_info
        );
        return Err(anyhow!("Invalid state for starting search"));
    }

    // Verify position is set before starting search
    {
        let engine = lock_or_recover_adapter(ctx.engine);
        if !engine.has_position() {
            log::error!("Cannot start search: position not set");
            send_response(UsiResponse::BestMove {
                best_move: "resign".to_string(),
                ponder: None,
            })?;
            return Ok(());
        }
    }

    // Increment search ID for new search
    *ctx.search_id_counter += 1;
    *ctx.current_search_id = *ctx.search_id_counter;
    let search_id = *ctx.current_search_id;
    log::info!("Starting new search with ID: {search_id}, ponder: {}", params.ponder);

    // Create new BestmoveEmitter for this search
    *ctx.current_bestmove_emitter = Some(BestmoveEmitter::new(search_id));

    // Calculate timeout for this search
    *ctx.current_search_timeout = calculate_max_search_time(&params);

    // Track if this is a ponder search
    *ctx.current_search_is_ponder = params.ponder;

    // Clone necessary data for worker thread
    let engine_clone = Arc::clone(ctx.engine);
    let stop_clone = Arc::clone(ctx.stop_flag);
    let tx_clone = ctx.worker_tx.clone();

    // Spawn worker thread for search with panic safety
    let handle = thread::spawn(move || {
        log::debug!("Worker thread spawned");
        let result = std::panic::catch_unwind(|| {
            search_worker(engine_clone, params, stop_clone, tx_clone.clone(), search_id);
        });

        if let Err(e) = result {
            log::error!("Worker thread panicked: {e:?}");
            // Send error message to main thread
            let _ = tx_clone.send(WorkerMessage::Error {
                message: "Worker thread panicked".to_string(),
                search_id,
            });
            let _ = tx_clone.send(WorkerMessage::Finished {
                from_guard: false,
                search_id,
            });
        }
    });

    *ctx.worker_handle = Some(handle);
    *ctx.search_state = SearchState::Searching;
    log::info!("Worker thread handle stored, search_state = Searching");

    // Don't block - return immediately
    Ok(())
}

fn handle_stop_command(ctx: &mut CommandContext) -> Result<()> {
    log::info!("Received stop command, search_state = {:?}", *ctx.search_state);
    log::debug!("Stop command received, entering stop handler");

    // Early return if bestmove already sent or not searching
    if *ctx.bestmove_sent || !ctx.search_state.is_searching() {
        log::debug!("Stop while idle or already sent bestmove -> ignore");
        return Ok(());
    }

    // Early return for ponder searches - no bestmove should be sent
    if *ctx.current_search_is_ponder {
        log::info!(
            "Stop during ponder (search_id: {}) - not sending bestmove",
            *ctx.current_search_id
        );

        // Signal stop to worker thread
        *ctx.search_state = SearchState::StopRequested;
        ctx.stop_flag.store(true, Ordering::Release);

        // Keep state as StopRequested and ponder flag as true
        // They will be cleaned up when the worker is properly joined
        log::debug!("Ponder stop: keeping StopRequested state for proper cleanup");

        return Ok(());
    }

    // Signal stop to worker thread for normal searches
    if ctx.search_state.is_searching() {
        *ctx.search_state = SearchState::StopRequested;
        ctx.stop_flag.store(true, Ordering::Release);
        log::info!("Stop flag set to true, search_state = StopRequested");

        // Debug: Verify stop flag was actually set
        let stop_value = ctx.stop_flag.load(Ordering::Acquire);
        log::info!("Stop flag verification: {stop_value}");

        // First try to use committed best from session immediately
        if let Some(ref session) = *ctx.current_session {
            let adapter = lock_or_recover_adapter(ctx.engine);
            if let Some(position) = adapter.get_position() {
                if let Ok((best_move, ponder)) =
                    adapter.validate_and_get_bestmove(session, position)
                {
                    // Log bestmove validation (source info now handled by BestmoveEmitter)
                    let depth = session.committed_best.as_ref().map(|b| b.depth).unwrap_or(0);
                    log::debug!("Validated bestmove from session on stop: depth={depth}");

                    log::info!("Sending committed bestmove from session on stop: {best_move}");

                    // Use BestmoveEmitter for centralized emission
                    if let Some(ref emitter) = ctx.current_bestmove_emitter {
                        use crate::bestmove_emitter::{BestmoveMeta, BestmoveStats};
                        use engine_core::search::types::{StopInfo, TerminationReason};

                        let meta = BestmoveMeta {
                            from: "session_on_stop",
                            stop_info: StopInfo {
                                reason: TerminationReason::UserStop,
                                elapsed_ms: 0, // TODO: Get actual elapsed time
                                nodes: 0,      // TODO: Get actual node count
                                depth_reached: depth,
                                hard_timeout: false,
                            },
                            stats: BestmoveStats {
                                depth: depth.into(),
                                seldepth: None,
                                score: session
                                    .committed_best
                                    .as_ref()
                                    .map(|b| match &b.score {
                                        crate::search_session::Score::Cp(cp) => format!("cp {cp}"),
                                        crate::search_session::Score::Mate(mate) => {
                                            format!("mate {mate}")
                                        }
                                    })
                                    .unwrap_or_else(|| "unknown".to_string()),
                                nodes: 0,
                                nps: 0,
                            },
                        };

                        emitter.emit(best_move, ponder, meta)?;
                        *ctx.search_state = SearchState::Idle;
                        *ctx.bestmove_sent = true;
                        *ctx.current_search_is_ponder = false;
                        return Ok(());
                    } else {
                        log::error!("BestmoveEmitter not available for current search");
                        return Err(anyhow!("BestmoveEmitter not initialized"));
                    }
                }
            }
        }

        // Check if the last search was using byoyomi time control and get safety ms
        let (is_byoyomi, safety_ms) = {
            let adapter = lock_or_recover_adapter(ctx.engine);
            (adapter.last_search_is_byoyomi(), adapter.byoyomi_safety_ms())
        };

        // Use adaptive timeouts based on byoyomi safety settings
        let stage1_timeout = if is_byoyomi {
            // Use half of safety margin for stage 1, clamped to reasonable range
            Duration::from_millis((safety_ms / 2).clamp(200, 800))
        } else {
            Duration::from_millis(100) // Normal mode: quick wait
        };
        let total_timeout = if is_byoyomi {
            // Use full safety margin for total timeout, clamped to reasonable range
            Duration::from_millis(safety_ms.clamp(600, 1500))
        } else {
            Duration::from_millis(150) // Normal mode: quick fallback
        };

        // Wait for bestmove with staged timeouts
        let start = Instant::now();
        let mut partial_result: Option<(String, u32, i32)> = None;
        let mut stage = 1;

        loop {
            let elapsed = start.elapsed();

            // Stage transition logic
            if stage == 1 && elapsed > stage1_timeout {
                stage = 2;
                log::debug!(
                    "Stop handler stage 2: trying fallback after {}ms",
                    elapsed.as_millis()
                );
            }

            if elapsed > total_timeout {
                // Timeout - use fallback strategy
                log::warn!("Timeout waiting for bestmove after stop command");
                // Log timeout error
                log::debug!("Stop command timeout: {:?}", EngineError::Timeout);

                // Use emergency fallback (session already tried at the beginning)
                match generate_fallback_move(
                    ctx.engine,
                    partial_result.clone(),
                    ctx.allow_null_move,
                ) {
                    Ok(move_str) => {
                        // Log fallback source (info now handled by BestmoveEmitter)
                        if let Some((_, depth, score)) = partial_result {
                            log::debug!("Using partial result: depth={depth}, score={score}");
                        } else {
                            log::debug!("Using emergency fallback after timeout");
                        }
                        log::debug!("Sending emergency fallback bestmove: {move_str}");

                        // Use BestmoveEmitter for centralized emission
                        if let Some(ref emitter) = ctx.current_bestmove_emitter {
                            use crate::bestmove_emitter::{BestmoveMeta, BestmoveStats};
                            use engine_core::search::types::{StopInfo, TerminationReason};

                            let meta = BestmoveMeta {
                                from: if partial_result.is_some() {
                                    "partial_result_timeout"
                                } else {
                                    "emergency_fallback_timeout"
                                },
                                stop_info: StopInfo {
                                    reason: TerminationReason::TimeLimit,
                                    elapsed_ms: elapsed.as_millis() as u64,
                                    nodes: 0, // TODO: Get actual node count
                                    depth_reached: partial_result
                                        .as_ref()
                                        .map(|(_, d, _)| *d as u8)
                                        .unwrap_or(0),
                                    hard_timeout: true,
                                },
                                stats: BestmoveStats {
                                    depth: partial_result.as_ref().map(|(_, d, _)| *d).unwrap_or(0),
                                    seldepth: None,
                                    score: partial_result
                                        .as_ref()
                                        .map(|(_, _, s)| format!("cp {s}"))
                                        .unwrap_or_else(|| "unknown".to_string()),
                                    nodes: 0,
                                    nps: 0,
                                },
                            };

                            emitter.emit(move_str, None, meta)?;
                            *ctx.search_state = SearchState::Idle;
                            *ctx.bestmove_sent = true;
                            *ctx.current_search_is_ponder = false;
                        } else {
                            log::error!("BestmoveEmitter not available for timeout fallback");
                            return Err(anyhow!("BestmoveEmitter not initialized"));
                        }
                    }
                    Err(e) => {
                        log::error!("Emergency fallback move generation failed: {e}");

                        // Use BestmoveEmitter for centralized emission
                        if let Some(ref emitter) = ctx.current_bestmove_emitter {
                            use crate::bestmove_emitter::{BestmoveMeta, BestmoveStats};
                            use engine_core::search::types::{StopInfo, TerminationReason};

                            let meta = BestmoveMeta {
                                from: "resign_timeout",
                                stop_info: StopInfo {
                                    reason: TerminationReason::Error,
                                    elapsed_ms: elapsed.as_millis() as u64,
                                    nodes: 0,
                                    depth_reached: 0,
                                    hard_timeout: true,
                                },
                                stats: BestmoveStats {
                                    depth: 0,
                                    seldepth: None,
                                    score: "unknown".to_string(),
                                    nodes: 0,
                                    nps: 0,
                                },
                            };

                            emitter.emit("resign".to_string(), None, meta)?;
                            *ctx.search_state = SearchState::Idle;
                            *ctx.bestmove_sent = true;
                            *ctx.current_search_is_ponder = false;
                        } else {
                            log::error!("BestmoveEmitter not available for resign");
                            return Err(anyhow!("BestmoveEmitter not initialized"));
                        }
                    }
                }
                break;
            }

            // Check for bestmove message
            match ctx.worker_rx.try_recv() {
                Ok(WorkerMessage::BestMove {
                    best_move,
                    ponder_move,
                    search_id,
                }) => {
                    // Only accept if it's for current search
                    if search_id == *ctx.current_search_id {
                        // Use BestmoveEmitter for centralized emission
                        if let Some(ref emitter) = ctx.current_bestmove_emitter {
                            use crate::bestmove_emitter::{BestmoveMeta, BestmoveStats};
                            use engine_core::search::types::{StopInfo, TerminationReason};

                            let meta = BestmoveMeta {
                                from: "worker_on_stop",
                                stop_info: StopInfo {
                                    reason: TerminationReason::UserStop,
                                    elapsed_ms: elapsed.as_millis() as u64,
                                    nodes: 0,         // TODO: Get actual node count
                                    depth_reached: 0, // TODO: Get actual depth from worker
                                    hard_timeout: false,
                                },
                                stats: BestmoveStats {
                                    depth: 0, // TODO: Get actual depth from worker
                                    seldepth: None,
                                    score: "unknown".to_string(),
                                    nodes: 0,
                                    nps: 0,
                                },
                            };

                            emitter.emit(best_move, ponder_move, meta)?;
                            *ctx.search_state = SearchState::Idle;
                            *ctx.bestmove_sent = true;
                            *ctx.current_search_is_ponder = false;
                        } else {
                            log::error!("BestmoveEmitter not available for worker bestmove");
                            return Err(anyhow!("BestmoveEmitter not initialized"));
                        }
                        break;
                    }
                }
                Ok(WorkerMessage::Info(info)) => {
                    // Forward info messages during active search (including StopRequested state)
                    // TODO: Add search_id to Info messages to filter out stale messages from previous searches
                    // This would prevent old search info from appearing during new searches
                    // Note: is_searching() returns true for both Searching and StopRequested states,
                    // allowing GUIs to receive final info messages during stop processing
                    if ctx.search_state.is_searching() {
                        let _ = send_response(UsiResponse::Info(info));
                    } else {
                        log::trace!("Suppressed Info message - not in searching state");
                    }
                }
                Ok(WorkerMessage::PartialResult {
                    current_best,
                    depth,
                    score,
                    search_id,
                }) => {
                    // Store partial result for fallback only if it's from current search
                    if search_id == *ctx.current_search_id {
                        partial_result = Some((current_best, depth.into(), score));
                    }
                }
                Ok(WorkerMessage::IterationComplete { session, search_id }) => {
                    // Update current session
                    if search_id == *ctx.current_search_id {
                        *ctx.current_session = Some(*session);
                    }
                }
                Ok(WorkerMessage::SearchFinished {
                    session_id: _,
                    root_hash: _,
                    search_id,
                    stop_info: _,
                }) => {
                    // Handle SearchFinished in stop command context
                    if search_id == *ctx.current_search_id {
                        log::info!("SearchFinished received in stop handler, sending bestmove");
                        // Try to use session-based bestmove
                        if let Some(ref session) = *ctx.current_session {
                            let adapter = lock_or_recover_adapter(ctx.engine);
                            if let Some(position) = adapter.get_position() {
                                match adapter.validate_and_get_bestmove(session, position) {
                                    Ok((best_move, ponder)) => {
                                        // Log bestmove validation (source info now handled by BestmoveEmitter)
                                        let depth = session
                                            .committed_best
                                            .as_ref()
                                            .map(|b| b.depth)
                                            .unwrap_or(0);
                                        log::debug!("Validated bestmove from session in stop handler: depth={depth}");

                                        log::info!(
                                            "Sending bestmove from stop handler: {best_move}"
                                        );

                                        if let Some(ref emitter) = ctx.current_bestmove_emitter {
                                            use crate::bestmove_emitter::{
                                                BestmoveMeta, BestmoveStats,
                                            };
                                            use engine_core::search::types::{
                                                StopInfo, TerminationReason,
                                            };

                                            let meta = BestmoveMeta {
                                                from: "session_in_search_finished",
                                                stop_info: StopInfo {
                                                    reason: TerminationReason::UserStop,
                                                    elapsed_ms: elapsed.as_millis() as u64,
                                                    nodes: 0, // TODO: Get actual node count
                                                    depth_reached: depth,
                                                    hard_timeout: false,
                                                },
                                                stats: BestmoveStats {
                                                    depth: depth.into(),
                                                    seldepth: None,
                                                    score: session
                                                        .committed_best
                                                        .as_ref()
                                                        .map(|b| match &b.score {
                                                            crate::search_session::Score::Cp(
                                                                cp,
                                                            ) => format!("cp {cp}"),
                                                            crate::search_session::Score::Mate(
                                                                mate,
                                                            ) => format!("mate {mate}"),
                                                        })
                                                        .unwrap_or_else(|| "unknown".to_string()),
                                                    nodes: 0,
                                                    nps: 0,
                                                },
                                            };

                                            emitter.emit(best_move, ponder, meta)?;
                                            *ctx.search_state = SearchState::Idle;
                                            *ctx.bestmove_sent = true;
                                            *ctx.current_search_is_ponder = false;
                                        } else {
                                            log::error!(
                                                "BestmoveEmitter not available for SearchFinished"
                                            );
                                            return Err(anyhow!("BestmoveEmitter not initialized"));
                                        }
                                        break;
                                    }
                                    Err(e) => {
                                        let position_info = adapter
                                            .get_position()
                                            .map(position_to_sfen)
                                            .unwrap_or_else(|| "<no position>".to_string());
                                        log::warn!(
                                            "Session validation failed in stop handler for position {}: {e}", position_info
                                        );
                                        // Continue to wait for BestMove or use fallback
                                    }
                                }
                            }
                        }
                    }
                }
                Ok(WorkerMessage::Finished {
                    from_guard,
                    search_id,
                }) => {
                    // Only process if it's for current search
                    if search_id == *ctx.current_search_id {
                        log::warn!("Worker finished without bestmove (from_guard: {from_guard})");
                        // Use fallback strategy
                        match generate_fallback_move(
                            ctx.engine,
                            partial_result.clone(),
                            ctx.allow_null_move,
                        ) {
                            Ok(move_str) => {
                                // Log fallback source (info now handled by BestmoveEmitter)
                                if let Some((_, depth, score)) = partial_result {
                                    log::debug!("Using partial result on finish: depth={depth}, score={score}");
                                } else {
                                    log::debug!("Using emergency fallback on finish");
                                }

                                if let Some(ref emitter) = ctx.current_bestmove_emitter {
                                    use crate::bestmove_emitter::{BestmoveMeta, BestmoveStats};
                                    use engine_core::search::types::{StopInfo, TerminationReason};

                                    let meta = BestmoveMeta {
                                        from: if partial_result.is_some() {
                                            "partial_result_on_finish"
                                        } else {
                                            "emergency_fallback_on_finish"
                                        },
                                        stop_info: StopInfo {
                                            reason: TerminationReason::UserStop,
                                            elapsed_ms: elapsed.as_millis() as u64,
                                            nodes: 0, // TODO: Get actual node count
                                            depth_reached: partial_result
                                                .as_ref()
                                                .map(|(_, d, _)| *d as u8)
                                                .unwrap_or(0),
                                            hard_timeout: false,
                                        },
                                        stats: BestmoveStats {
                                            depth: partial_result
                                                .as_ref()
                                                .map(|(_, d, _)| *d)
                                                .unwrap_or(0),
                                            seldepth: None,
                                            score: partial_result
                                                .as_ref()
                                                .map(|(_, _, s)| format!("cp {s}"))
                                                .unwrap_or_else(|| "unknown".to_string()),
                                            nodes: 0,
                                            nps: 0,
                                        },
                                    };

                                    emitter.emit(move_str, None, meta)?;
                                    *ctx.search_state = SearchState::Idle;
                                    *ctx.bestmove_sent = true;
                                    *ctx.current_search_is_ponder = false;
                                } else {
                                    log::error!("BestmoveEmitter not available for finish handler");
                                    return Err(anyhow!("BestmoveEmitter not initialized"));
                                }
                            }
                            Err(e) => {
                                let position_info = {
                                    let engine = lock_or_recover_adapter(ctx.engine);
                                    engine
                                        .get_position()
                                        .map(position_to_sfen)
                                        .unwrap_or_else(|| "<no position>".to_string())
                                };
                                log::error!(
                                    "Fallback move generation failed in position {}: {e}",
                                    position_info
                                );

                                if let Some(ref emitter) = ctx.current_bestmove_emitter {
                                    use crate::bestmove_emitter::{BestmoveMeta, BestmoveStats};
                                    use engine_core::search::types::{StopInfo, TerminationReason};

                                    let meta = BestmoveMeta {
                                        from: "resign_on_finish",
                                        stop_info: StopInfo {
                                            reason: TerminationReason::Error,
                                            elapsed_ms: elapsed.as_millis() as u64,
                                            nodes: 0,
                                            depth_reached: 0,
                                            hard_timeout: false,
                                        },
                                        stats: BestmoveStats {
                                            depth: 0,
                                            seldepth: None,
                                            score: "unknown".to_string(),
                                            nodes: 0,
                                            nps: 0,
                                        },
                                    };

                                    emitter.emit("resign".to_string(), None, meta)?;
                                    *ctx.search_state = SearchState::Idle;
                                    *ctx.bestmove_sent = true;
                                    *ctx.current_search_is_ponder = false;
                                } else {
                                    log::error!(
                                        "BestmoveEmitter not available for resign on finish"
                                    );
                                    return Err(anyhow!("BestmoveEmitter not initialized"));
                                }
                            }
                        }
                        break;
                    }
                }
                _ => {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    Ok(())
}
