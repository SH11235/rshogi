use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::helpers::{
    calculate_max_search_time, generate_fallback_move, send_bestmove_and_finalize,
    wait_for_search_completion,
};
use crate::search_session::{self, SearchSession};
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
            // Handle ponder hit
            let mut engine = lock_or_recover_adapter(ctx.engine);
            // Mark that we're no longer in pure ponder mode
            *ctx.current_search_is_ponder = false;
            match engine.ponder_hit() {
                Ok(()) => log::debug!("Ponder hit successfully processed"),
                Err(e) => log::debug!("Ponder hit ignored: {e}"),
            }
        }

        UsiCommand::SetOption { name, value } => {
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.set_option(&name, value.as_deref())?;
        }

        UsiCommand::GameOver { result } => {
            // Stop any ongoing search
            ctx.stop_flag.store(true, Ordering::Release);

            // Notify engine of game result
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.game_over(result);
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

        // Clean up state
        *ctx.search_state = SearchState::Idle;
        *ctx.current_search_is_ponder = false;

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
                    // Send info string about bestmove source
                    let depth = session.committed_best.as_ref().map(|b| b.depth).unwrap_or(0);
                    let score_str = session
                        .committed_best
                        .as_ref()
                        .map(|b| match &b.score {
                            search_session::Score::Cp(cp) => format!("cp {cp}"),
                            search_session::Score::Mate(mate) => format!("mate {mate}"),
                        })
                        .unwrap_or_else(|| "unknown".to_string());
                    send_info_string(format!(
                        "bestmove_from=session_on_stop depth={depth} score={score_str}"
                    ))?;

                    log::info!("Sending committed bestmove from session on stop: {best_move}");
                    return send_bestmove_and_finalize(
                        best_move,
                        ponder,
                        ctx.search_state,
                        ctx.bestmove_sent,
                        ctx.current_search_is_ponder,
                    );
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
                        // Send info string about fallback source
                        if let Some((_, depth, score)) = partial_result {
                            send_info_string(format!(
                                "bestmove_from=partial_result depth={depth} score={score}"
                            ))?;
                        } else {
                            send_info_string("bestmove_from=emergency_fallback_timeout")?;
                        }
                        log::debug!("Sending emergency fallback bestmove: {move_str}");
                        send_bestmove_and_finalize(
                            move_str,
                            None,
                            ctx.search_state,
                            ctx.bestmove_sent,
                            ctx.current_search_is_ponder,
                        )?;
                    }
                    Err(e) => {
                        log::error!("Emergency fallback move generation failed: {e}");
                        send_bestmove_and_finalize(
                            "resign".to_string(),
                            None,
                            ctx.search_state,
                            ctx.bestmove_sent,
                            ctx.current_search_is_ponder,
                        )?;
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
                        // Send bestmove for normal searches
                        send_bestmove_and_finalize(
                            best_move,
                            ponder_move,
                            ctx.search_state,
                            ctx.bestmove_sent,
                            ctx.current_search_is_ponder,
                        )?;
                        break;
                    }
                }
                Ok(WorkerMessage::Info(info)) => {
                    // Forward info messages
                    let _ = send_response(UsiResponse::Info(info));
                }
                Ok(WorkerMessage::PartialResult {
                    current_best,
                    depth,
                    score,
                    search_id,
                }) => {
                    // Store partial result for fallback only if it's from current search
                    if search_id == *ctx.current_search_id {
                        partial_result = Some((current_best, depth, score));
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
                                        // Send info string about bestmove source
                                        let depth = session
                                            .committed_best
                                            .as_ref()
                                            .map(|b| b.depth)
                                            .unwrap_or(0);
                                        let score_str = session
                                            .committed_best
                                            .as_ref()
                                            .map(|b| match &b.score {
                                                search_session::Score::Cp(cp) => {
                                                    format!("cp {cp}")
                                                }
                                                search_session::Score::Mate(mate) => {
                                                    format!("mate {mate}")
                                                }
                                            })
                                            .unwrap_or_else(|| "unknown".to_string());
                                        send_info_string(format!("bestmove_from=session_in_stop_handler depth={depth} score={score_str}"))?;

                                        log::info!(
                                            "Sending bestmove from stop handler: {best_move}"
                                        );
                                        send_bestmove_and_finalize(
                                            best_move,
                                            ponder,
                                            ctx.search_state,
                                            ctx.bestmove_sent,
                                            ctx.current_search_is_ponder,
                                        )?;
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
                                // Send info string about fallback source
                                if let Some((_, depth, score)) = partial_result {
                                    send_info_string(format!("bestmove_from=partial_result_on_finish depth={depth} score={score}"))?;
                                } else {
                                    send_info_string("bestmove_from=emergency_fallback_on_finish")?;
                                }
                                send_bestmove_and_finalize(
                                    move_str,
                                    None,
                                    ctx.search_state,
                                    ctx.bestmove_sent,
                                    ctx.current_search_is_ponder,
                                )?;
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
                                send_bestmove_and_finalize(
                                    "resign".to_string(),
                                    None,
                                    ctx.search_state,
                                    ctx.bestmove_sent,
                                    ctx.current_search_is_ponder,
                                )?;
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
