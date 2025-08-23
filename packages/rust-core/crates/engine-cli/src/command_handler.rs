use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::helpers::{generate_fallback_move, wait_for_search_completion};
use crate::search_session::SearchSession;
use crate::state::SearchState;
use crate::types::BestmoveSource;
use crate::usi::{send_info_string, send_response, GoParams, UsiCommand, UsiResponse};
use crate::worker::{lock_or_recover_adapter, search_worker, WorkerMessage};
use anyhow::{anyhow, Result};
use crossbeam_channel::{Receiver, Sender};
use engine_core::usi::position_to_sfen;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::bestmove_emitter::{BestmoveEmitter, BestmoveMeta, BestmoveStats};
use engine_core::search::types::{StopInfo, TerminationReason};

/// Helper to construct BestmoveMeta with common defaults
fn create_bestmove_meta(
    from: BestmoveSource,
    reason: TerminationReason,
    elapsed_ms: u64,
    depth: u8,
    score: Option<String>,
    nodes: u64,
    hard_timeout: bool,
) -> BestmoveMeta {
    create_bestmove_meta_with_seldepth(
        from,
        reason,
        elapsed_ms,
        depth,
        None,
        score,
        nodes,
        hard_timeout,
    )
}

/// Helper to construct BestmoveMeta with seldepth
#[allow(clippy::too_many_arguments)]
fn create_bestmove_meta_with_seldepth(
    from: BestmoveSource,
    reason: TerminationReason,
    elapsed_ms: u64,
    depth: u8,
    seldepth: Option<u8>,
    score: Option<String>,
    nodes: u64,
    hard_timeout: bool,
) -> BestmoveMeta {
    BestmoveMeta {
        from,
        stop_info: StopInfo {
            reason,
            elapsed_ms,
            nodes,
            depth_reached: depth,
            hard_timeout,
        },
        stats: BestmoveStats {
            depth,
            seldepth,
            score: score.unwrap_or_else(|| "unknown".to_string()),
            nodes,
            nps: if elapsed_ms > 0 {
                nodes.saturating_mul(1000) / elapsed_ms
            } else {
                0
            },
        },
    }
}

/// Context for handling USI commands
pub struct CommandContext<'a> {
    pub engine: &'a Arc<Mutex<EngineAdapter>>,
    pub stop_flag: &'a Arc<AtomicBool>, // Global stop flag (for shutdown)
    pub worker_tx: &'a Sender<WorkerMessage>,
    pub worker_rx: &'a Receiver<WorkerMessage>,
    pub worker_handle: &'a mut Option<JoinHandle<()>>,
    pub search_state: &'a mut SearchState,
    pub search_id_counter: &'a mut u64,
    pub current_search_id: &'a mut u64,
    pub current_search_is_ponder: &'a mut bool,
    pub current_session: &'a mut Option<SearchSession>,
    pub current_bestmove_emitter: &'a mut Option<BestmoveEmitter>,
    pub current_stop_flag: &'a mut Option<Arc<AtomicBool>>, // Per-search stop flag
    pub allow_null_move: bool,
}

impl<'a> CommandContext<'a> {
    #[inline]
    pub fn finalize_search(&mut self, where_: &str) {
        log::debug!("Finalize search {} ({})", *self.current_search_id, where_);
        *self.search_state = SearchState::Idle;
        *self.current_search_is_ponder = false;
        *self.current_bestmove_emitter = None;
        *self.current_session = None;
        *self.current_stop_flag = None; // Clear per-search stop flag
    }
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
                ctx.current_stop_flag.as_ref(),
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
                ctx.current_stop_flag.as_ref(),
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
            )?;

            // Log the previous search ID for debugging
            log::debug!("Reset state after gameover: prev_search_id={}", *ctx.current_search_id);

            // Clear all search-related state for clean baseline
            ctx.finalize_search("GameOver");
            // Reset to 0 so any late worker messages (old search_id) will be ignored
            *ctx.current_search_id = 0;

            // Notify engine of game result
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.game_over(result);

            // Reset stop flag for next game
            ctx.stop_flag.store(false, Ordering::Release);
            log::debug!("Game over processed, worker cleaned up, state reset to Idle, stop_flag reset to false");
        }

        UsiCommand::UsiNewGame => {
            // ShogiGUI extension - new game notification
            // Stop any ongoing search
            wait_for_search_completion(
                ctx.search_state,
                ctx.stop_flag,
                ctx.current_stop_flag.as_ref(),
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
    let go_received_time = Instant::now();
    send_info_string(format!("NewSearchStart: go received at {go_received_time:?}"))?;

    // Stop any ongoing search and ensure engine is available
    let wait_start = Instant::now();
    wait_for_search_completion(
        ctx.search_state,
        ctx.stop_flag,
        ctx.current_stop_flag.as_ref(),
        ctx.worker_handle,
        ctx.worker_rx,
        ctx.engine,
    )?;
    let wait_duration = wait_start.elapsed();
    log::info!("Wait for search completion took: {wait_duration:?}");

    // Check engine availability before proceeding
    {
        let adapter = lock_or_recover_adapter(ctx.engine);
        let engine_available = adapter.is_engine_available();
        log::info!("Engine availability after wait: {engine_available}");
        if !engine_available {
            log::error!("Engine is not available after wait_for_search_completion");
        }
    }

    // No delay needed - state transitions are atomic

    // Create new per-search stop flag
    let search_stop_flag = Arc::new(AtomicBool::new(false));
    *ctx.current_stop_flag = Some(search_stop_flag.clone());
    log::info!("Created new per-search stop flag for search_id={}", *ctx.current_search_id);

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

    // Track if this is a ponder search
    *ctx.current_search_is_ponder = params.ponder;

    // Clone necessary data for worker thread
    let engine_clone = Arc::clone(ctx.engine);
    let stop_clone = search_stop_flag.clone();
    let tx_clone = ctx.worker_tx.clone();
    log::info!("Using per-search stop flag for search_id={search_id}");

    // Log before spawning worker
    log::info!("About to spawn worker thread for search_id={search_id}");
    send_info_string(format!("NewSearchStart: spawning worker, search_id={search_id}"))?;

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

    // Send immediate info depth 1 to confirm search started (ensures GUI sees activity)
    send_response(UsiResponse::Info(crate::usi::output::SearchInfo {
        depth: Some(1),
        time: Some(1),
        nodes: Some(0),
        string: Some("search starting".to_string()),
        ..Default::default()
    }))?;
    log::debug!("Sent initial info depth 1 heartbeat to GUI");

    // Don't block - return immediately
    Ok(())
}

fn handle_stop_command(ctx: &mut CommandContext) -> Result<()> {
    log::info!("Received stop command, search_state = {:?}", *ctx.search_state);
    log::debug!("Stop command received, entering stop handler");
    send_info_string(format!(
        "StopRequested: search_id={}, state={:?}",
        *ctx.current_search_id, *ctx.search_state
    ))?;

    // Early return if not searching
    if !ctx.search_state.is_searching() {
        log::debug!("Stop while idle -> ignore");
        send_info_string("StopAck: ignored (not searching)")?;
        return Ok(());
    }

    // Early return for ponder searches - no bestmove should be sent
    if *ctx.current_search_is_ponder {
        log::info!(
            "Stop during ponder (search_id: {}) - not sending bestmove",
            *ctx.current_search_id
        );

        // Signal stop to worker thread using per-search flag
        *ctx.search_state = SearchState::StopRequested;
        if let Some(ref stop_flag) = *ctx.current_stop_flag {
            stop_flag.store(true, Ordering::SeqCst);
        }

        // Keep state as StopRequested and ponder flag as true
        // They will be cleaned up when the worker is properly joined
        log::debug!("Ponder stop: keeping StopRequested state for proper cleanup");

        return Ok(());
    }

    // Signal stop to worker thread for normal searches
    if ctx.search_state.is_searching() {
        *ctx.search_state = SearchState::StopRequested;
        if let Some(ref stop_flag) = *ctx.current_stop_flag {
            stop_flag.store(true, Ordering::SeqCst);
            log::info!(
                "Per-search stop flag set to true for search_id={}, search_state = StopRequested",
                *ctx.current_search_id
            );

            // Debug: Verify stop flag was actually set
            let stop_value = stop_flag.load(Ordering::SeqCst);
            log::debug!("Stop flag verification: {stop_value}");
        } else {
            log::warn!("No current stop flag available for search_id={}", *ctx.current_search_id);
        }
        send_info_string(format!("StopAck: stop_flag set, search_id={}", *ctx.current_search_id))?;

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
                        let score_str = session.committed_best.as_ref().map(|b| match &b.score {
                            crate::search_session::Score::Cp(cp) => format!("cp {cp}"),
                            crate::search_session::Score::Mate(mate) => format!("mate {mate}"),
                        });

                        let seldepth = session.committed_best.as_ref().and_then(|b| b.seldepth);

                        let meta = create_bestmove_meta_with_seldepth(
                            BestmoveSource::SessionOnStop,
                            TerminationReason::UserStop,
                            0, // TODO: Get actual elapsed time
                            depth,
                            seldepth,
                            score_str,
                            0, // TODO: Get actual node count
                            false,
                        );

                        emitter.emit(best_move, ponder, meta)?;
                        ctx.finalize_search("SessionOnStop");
                        return Ok(());
                    } else {
                        log::error!("BestmoveEmitter not available for current search; sending bestmove directly");
                        send_response(UsiResponse::BestMove { best_move, ponder })?;
                        ctx.finalize_search("DirectSend");
                        return Ok(());
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
            Duration::from_millis((safety_ms / 2).clamp(400, 1000))
        } else {
            Duration::from_millis(100) // Normal mode: quick wait
        };
        let total_timeout = if is_byoyomi {
            // Use full safety margin for total timeout, clamped to reasonable range
            Duration::from_millis(safety_ms.clamp(800, 2000))
        } else {
            Duration::from_millis(150) // Normal mode: quick fallback
        };

        // Wait for bestmove with staged timeouts
        let start = Instant::now();
        let mut partial_result: Option<(String, u8, i32)> = None;
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
                            let (from, depth, score_str) = if let Some((_, d, s)) = partial_result {
                                (BestmoveSource::PartialResultTimeout, d, Some(format!("cp {s}")))
                            } else {
                                (BestmoveSource::EmergencyFallbackTimeout, 0, None)
                            };

                            let meta = create_bestmove_meta(
                                from,
                                TerminationReason::TimeLimit,
                                0, // BestmoveEmitter に補完させる
                                depth,
                                score_str,
                                0, // nodes は未知なので 0 のままでOK
                                true,
                            );

                            emitter.emit(move_str, None, meta)?;
                            ctx.finalize_search("TimeoutFallback");
                        } else {
                            log::error!("BestmoveEmitter not available for timeout fallback; sending bestmove directly");
                            send_response(UsiResponse::BestMove {
                                best_move: move_str,
                                ponder: None,
                            })?;
                            ctx.finalize_search("DirectSend");
                        }
                    }
                    Err(e) => {
                        log::error!("Emergency fallback move generation failed: {e}");

                        // Use BestmoveEmitter for centralized emission
                        if let Some(ref emitter) = ctx.current_bestmove_emitter {
                            let meta = create_bestmove_meta(
                                BestmoveSource::ResignTimeout,
                                TerminationReason::Error,
                                0, // BestmoveEmitter に補完させる
                                0,
                                None,
                                0,
                                true,
                            );

                            emitter.emit("resign".to_string(), None, meta)?;
                            ctx.finalize_search("ResignTimeout");
                        } else {
                            log::error!("BestmoveEmitter not available for resign; sending bestmove directly");
                            send_response(UsiResponse::BestMove {
                                best_move: "resign".to_string(),
                                ponder: None,
                            })?;
                            ctx.finalize_search("DirectSend");
                        }
                    }
                }
                break;
            }

            // Check for bestmove message
            match ctx.worker_rx.try_recv() {
                // WorkerMessage::BestMove has been completely removed.
                // All bestmove emissions now go through the session-based approach
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
                    stop_info,
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
                                            let score_str =
                                                session.committed_best.as_ref().map(|b| {
                                                    match &b.score {
                                                        crate::search_session::Score::Cp(cp) => {
                                                            format!("cp {cp}")
                                                        }
                                                        crate::search_session::Score::Mate(
                                                            mate,
                                                        ) => format!("mate {mate}"),
                                                    }
                                                });

                                            let seldepth = session
                                                .committed_best
                                                .as_ref()
                                                .and_then(|b| b.seldepth);

                                            // Use stop_info values if available, otherwise use defaults
                                            let (elapsed_ms, nodes, reason, hard_timeout) =
                                                if let Some(ref info) = stop_info {
                                                    (
                                                        info.elapsed_ms,
                                                        info.nodes,
                                                        info.reason,
                                                        info.hard_timeout,
                                                    )
                                                } else {
                                                    // stop_info is None: use 0 to let emitter complement
                                                    (0, 0, TerminationReason::UserStop, false)
                                                };

                                            let meta = create_bestmove_meta_with_seldepth(
                                                BestmoveSource::SessionInSearchFinished,
                                                reason,
                                                elapsed_ms,
                                                depth,
                                                seldepth,
                                                score_str,
                                                nodes,
                                                hard_timeout,
                                            );

                                            emitter.emit(best_move, ponder, meta)?;
                                            ctx.finalize_search("BestmoveEmitter");
                                        } else {
                                            log::error!(
                                                "BestmoveEmitter not available for SearchFinished; sending bestmove directly"
                                            );
                                            send_response(UsiResponse::BestMove {
                                                best_move,
                                                ponder,
                                            })?;
                                            ctx.finalize_search("DirectSend");
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
                                    let (from, depth, score_str) =
                                        if let Some((_, d, s)) = partial_result {
                                            (
                                                BestmoveSource::PartialResultOnFinish,
                                                d,
                                                Some(format!("cp {s}")),
                                            )
                                        } else {
                                            (BestmoveSource::EmergencyFallbackOnFinish, 0, None)
                                        };

                                    let meta = create_bestmove_meta(
                                        from,
                                        TerminationReason::UserStop,
                                        0, // BestmoveEmitter に補完させる
                                        depth,
                                        score_str,
                                        0, // TODO: Get actual node count
                                        false,
                                    );

                                    emitter.emit(move_str, None, meta)?;
                                    ctx.finalize_search("BestmoveEmitter");
                                } else {
                                    log::error!("BestmoveEmitter not available for finish handler; sending bestmove directly");
                                    send_response(UsiResponse::BestMove {
                                        best_move: move_str,
                                        ponder: None,
                                    })?;
                                    ctx.finalize_search("DirectSend");
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
                                    let meta = create_bestmove_meta(
                                        BestmoveSource::ResignOnFinish,
                                        TerminationReason::Error,
                                        0, // BestmoveEmitter に補完させる
                                        0,
                                        None,
                                        0,
                                        false,
                                    );

                                    emitter.emit("resign".to_string(), None, meta)?;
                                    ctx.finalize_search("BestmoveEmitter");
                                } else {
                                    log::error!(
                                        "BestmoveEmitter not available for resign on finish; sending bestmove directly"
                                    );
                                    send_response(UsiResponse::BestMove {
                                        best_move: "resign".to_string(),
                                        ponder: None,
                                    })?;
                                    ctx.finalize_search("DirectSend");
                                }
                            }
                        }
                        break;
                    }
                }
                Ok(WorkerMessage::SearchStarted {
                    search_id,
                    start_time,
                }) => {
                    // Update BestmoveEmitter with accurate start time if it's for current search
                    if search_id == *ctx.current_search_id {
                        if let Some(ref mut emitter) = ctx.current_bestmove_emitter {
                            emitter.set_start_time(start_time);
                            log::debug!("Updated BestmoveEmitter with worker start time in stop handler for search {search_id}");
                        }
                    } else {
                        log::trace!("Ignoring SearchStarted from old search in stop handler: {search_id} (current: {})", *ctx.current_search_id);
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
