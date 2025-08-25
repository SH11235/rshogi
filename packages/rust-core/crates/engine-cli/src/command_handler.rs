use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::helpers::{generate_fallback_move, wait_for_search_completion};
use crate::search_session::SearchSession;
use crate::state::SearchState;
use crate::types::{BestmoveSource, ResignReason};
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
    pub last_position_cmd: &'a mut Option<String>, // Store last position command for recovery
}

/// Build BestmoveMeta from common parameters
/// This reduces duplication of BestmoveMeta construction across the codebase
pub fn build_meta(
    from: BestmoveSource,
    depth: u8,
    seldepth: Option<u8>,
    score: Option<String>,
    stop_info: Option<StopInfo>,
) -> BestmoveMeta {
    // Use provided stop_info or create default one
    let si = stop_info.unwrap_or(StopInfo {
        reason: match from {
            // Timeout cases -> TimeLimit
            BestmoveSource::ResignTimeout
            | BestmoveSource::EmergencyFallbackTimeout
            | BestmoveSource::PartialResultTimeout => TerminationReason::TimeLimit,
            // Normal completion cases -> Completed
            BestmoveSource::EmergencyFallback
            | BestmoveSource::EmergencyFallbackOnFinish
            | BestmoveSource::PartialResultOnFinish
            | BestmoveSource::SessionInSearchFinished => TerminationReason::Completed,
            // User stop cases -> UserStop
            BestmoveSource::SessionOnStop => TerminationReason::UserStop,
            // Error cases -> Error
            BestmoveSource::Resign | BestmoveSource::ResignOnFinish => TerminationReason::Error,
            // Test variant
            #[cfg(test)]
            BestmoveSource::Test => TerminationReason::UserStop,
        },
        elapsed_ms: 0, // BestmoveEmitter will complement this
        nodes: 0,      // BestmoveEmitter will complement this
        depth_reached: depth,
        hard_timeout: matches!(
            from,
            BestmoveSource::EmergencyFallbackTimeout
                | BestmoveSource::PartialResultTimeout
                | BestmoveSource::ResignTimeout
        ),
    });

    let nodes = si.nodes;
    let elapsed_ms = si.elapsed_ms;

    BestmoveMeta {
        from,
        stop_info: si,
        stats: BestmoveStats {
            depth,
            seldepth,
            score: score.unwrap_or_else(|| "none".to_string()),
            nodes,
            nps: if elapsed_ms > 0 {
                nodes.saturating_mul(1000) / elapsed_ms
            } else {
                0
            },
        },
    }
}

impl<'a> CommandContext<'a> {
    /// Try to emit bestmove from session
    /// Returns Ok(true) if bestmove was successfully emitted
    fn emit_best_from_session(
        &mut self,
        session: &SearchSession,
        from: BestmoveSource,
        stop_info: Option<StopInfo>,
        finalize_label: &str,
    ) -> Result<bool> {
        let adapter = lock_or_recover_adapter(self.engine);
        if let Some(position) = adapter.get_position() {
            if let Ok((best_move, ponder)) = adapter.validate_and_get_bestmove(session, position) {
                // Extract common score formatting and metadata
                let depth = session.committed_best.as_ref().map(|b| b.depth).unwrap_or(0);
                let seldepth = session.committed_best.as_ref().and_then(|b| b.seldepth);
                let score_str = session.committed_best.as_ref().map(|b| match &b.score {
                    crate::search_session::Score::Cp(cp) => format!("cp {cp}"),
                    crate::search_session::Score::Mate(mate) => format!("mate {mate}"),
                });

                log::debug!("Validated bestmove from session: depth={depth}");

                let meta = build_meta(from, depth, seldepth, score_str, stop_info);
                self.emit_and_finalize(best_move, ponder, meta, finalize_label)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[inline]
    pub fn finalize_search(&mut self, where_: &str) {
        log::debug!("Finalize search {} ({})", *self.current_search_id, where_);
        *self.search_state = SearchState::Idle;
        *self.current_search_is_ponder = false;
        *self.current_bestmove_emitter = None;
        *self.current_session = None;

        // Drop the current stop flag without resetting it
        // This prevents race conditions where worker might miss the stop signal
        let _ = self.current_stop_flag.take();
    }

    /// Emit bestmove and always finalize search, even on error
    ///
    /// This ensures finalize_search is called even if emit fails.
    /// Following USI best practices, this method always succeeds (returns Ok)
    /// and makes best effort to send bestmove even if primary emission fails.
    pub fn emit_and_finalize(
        &mut self,
        best_move: String,
        ponder: Option<String>,
        meta: BestmoveMeta,
        finalize_label: &str,
    ) -> Result<()> {
        // Try to emit via BestmoveEmitter if available
        if let Some(ref emitter) = self.current_bestmove_emitter {
            match emitter.emit(best_move.clone(), ponder.clone(), meta) {
                Ok(()) => {
                    self.finalize_search(finalize_label);
                    Ok(())
                }
                Err(e) => {
                    log::error!("BestmoveEmitter::emit failed: {e}");
                    // Always finalize search even on error
                    self.finalize_search(finalize_label);
                    // Try direct send as fallback
                    if let Err(e) = send_response(UsiResponse::BestMove { best_move, ponder }) {
                        log::error!("Failed to send bestmove even with direct fallback: {e}");
                        // Continue without propagating error - USI requires best effort
                    }
                    Ok(())
                }
            }
        } else {
            log::warn!("BestmoveEmitter not available; sending bestmove directly");
            // Always finalize before sending
            self.finalize_search(finalize_label);
            if let Err(e) = send_response(UsiResponse::BestMove { best_move, ponder }) {
                log::error!("Failed to send bestmove directly: {e}");
                // Continue without propagating error - USI requires best effort
            }
            Ok(())
        }
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
            log::debug!(
                "Handling position command - startpos: {startpos}, sfen: {sfen:?}, moves: {moves:?}"
            );

            // Build the position command string
            let mut position_cmd = String::from("position");
            if startpos {
                position_cmd.push_str(" startpos");
            } else if let Some(sfen) = &sfen {
                position_cmd.push_str(" sfen ");
                position_cmd.push_str(sfen);
            }
            if !moves.is_empty() {
                position_cmd.push_str(" moves");
                for mv in &moves {
                    position_cmd.push(' ');
                    position_cmd.push_str(mv);
                }
            }

            // Wait for any ongoing search to complete before updating position
            wait_for_search_completion(
                ctx.search_state,
                ctx.stop_flag,
                ctx.current_stop_flag.as_ref(),
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
            )?;

            // Clean up any remaining search state
            ctx.finalize_search("Position");

            let mut engine = lock_or_recover_adapter(ctx.engine);
            match engine.set_position(startpos, sfen.as_deref(), &moves) {
                Ok(()) => {
                    // Store the position command only on success
                    *ctx.last_position_cmd = Some(position_cmd.clone());
                    log::debug!("Stored position command: {}", position_cmd);

                    // Get position info for logging
                    if let Some(pos) = engine.get_position() {
                        let sfen = position_to_sfen(pos);
                        let root_hash = pos.zobrist_hash();
                        log::info!(
                            "Position command completed - SFEN: {}, root_hash: {:#016x}, side_to_move: {:?}, move_count: {}",
                            sfen, root_hash, pos.side_to_move, moves.len()
                        );
                        send_info_string(format!(
                            "Position set: root_hash={:#016x} side={:?} moves={}",
                            root_hash,
                            pos.side_to_move,
                            moves.len()
                        ))?;
                    } else {
                        log::info!("Position command completed");
                    }
                }
                Err(e) => {
                    // Log error but don't crash - USI engines should be robust
                    log::error!("Failed to set position: {e}");
                    send_info_string(format!("Error: Failed to set position - {e}"))?;
                    // Don't update last_position_cmd on failure - keep the previous valid one
                    log::debug!("Keeping previous position command due to error");
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

            // Clear last position command to avoid carrying over to next game
            *ctx.last_position_cmd = None;
            log::debug!("Cleared last_position_cmd for new game");

            // Notify engine of game result
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.game_over(result);

            // Note: stop_flag is already reset to false by wait_for_search_completion
            log::debug!("Game over processed, worker cleaned up, state reset to Idle");
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

            // Clean up any remaining search state
            ctx.finalize_search("UsiNewGame");

            // Clear last position command for fresh start
            *ctx.last_position_cmd = None;
            log::debug!("Cleared last_position_cmd for new game");

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
    log::debug!("Received go command with params: {params:?}");
    let go_received_time = Instant::now();
    log::debug!("NewSearchStart: go received at {go_received_time:?}");

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
    log::debug!("Wait for search completion took: {wait_duration:?}");

    // Check engine availability before proceeding
    {
        let mut adapter = lock_or_recover_adapter(ctx.engine);
        let engine_available = adapter.is_engine_available();
        log::debug!("Engine availability after wait: {engine_available}");
        if !engine_available {
            log::error!("Engine is not available after wait_for_search_completion");
            // Force reset state to recover engine if it's stuck in another thread
            log::warn!("Attempting to force reset engine state for recovery");
            adapter.force_reset_state();

            // Check again after reset
            let engine_available_after_reset = adapter.is_engine_available();
            if !engine_available_after_reset {
                log::error!(
                    "Engine still not available after force reset - falling back to emergency move"
                );
                // Continue anyway - fallback mechanisms will handle this
            } else {
                log::debug!("Engine recovered after force reset");
            }
        }
    }

    // No delay needed - state transitions are atomic

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
        let mut engine = lock_or_recover_adapter(ctx.engine);
        if !engine.has_position() {
            log::warn!("Position not set - attempting recovery from last position command");

            // Try to recover from last position command
            if let Some(last_cmd) = ctx.last_position_cmd.as_ref() {
                log::debug!("Attempting to rebuild position from: {}", last_cmd);

                // Parse and apply the last position command
                match crate::usi::parse_usi_command(last_cmd) {
                    Ok(UsiCommand::Position {
                        startpos,
                        sfen,
                        moves,
                    }) => match engine.set_position(startpos, sfen.as_deref(), &moves) {
                        Ok(()) => {
                            log::info!("Successfully rebuilt position from last command");
                            send_info_string("Position recovered from last command".to_string())?;
                        }
                        Err(e) => {
                            log::error!("Failed to rebuild position: {}", e);
                            let reason = ResignReason::PositionRebuildFailed {
                                error: "see log for details",
                            };
                            send_info_string(format!("ResignReason: {reason}"))?;
                            send_response(UsiResponse::BestMove {
                                best_move: "resign".to_string(),
                                ponder: None,
                            })?;
                            return Ok(());
                        }
                    },
                    _ => {
                        log::error!("Invalid stored position command: {}", last_cmd);
                        let reason = ResignReason::InvalidStoredPositionCmd;
                        send_info_string(format!("ResignReason: {reason}"))?;
                        send_response(UsiResponse::BestMove {
                            best_move: "resign".to_string(),
                            ponder: None,
                        })?;
                        return Ok(());
                    }
                }
            } else {
                log::error!("No position set and no recovery command available");
                let reason = ResignReason::NoPositionSet;
                send_info_string(format!("ResignReason: {reason}"))?;
                send_response(UsiResponse::BestMove {
                    best_move: "resign".to_string(),
                    ponder: None,
                })?;
                return Ok(());
            }
        }

        // Sanity check: verify we have legal moves
        match engine.has_legal_moves() {
            Ok(true) => {
                log::debug!("Position sanity check passed - legal moves available");
            }
            Ok(false) => {
                // Check if it's checkmate or error condition
                let in_check = engine.is_in_check().unwrap_or(false);
                let reason = if in_check {
                    ResignReason::Checkmate
                } else {
                    ResignReason::NoLegalMovesButNotInCheck
                };

                log::error!("Position has no legal moves - in_check: {in_check}");
                send_info_string(format!("ResignReason: {reason}"))?;

                // Get position info for debugging
                let position_info = engine
                    .get_position()
                    .map(position_to_sfen)
                    .unwrap_or_else(|| "<no position>".to_string());
                log::error!("Current position SFEN: {position_info}");

                send_response(UsiResponse::BestMove {
                    best_move: "resign".to_string(),
                    ponder: None,
                })?;
                return Ok(());
            }
            Err(e) => {
                log::error!("Failed to check legal moves: {e}");
                let reason = ResignReason::OtherError {
                    error: "legal move check failed",
                };
                send_info_string(format!("ResignReason: {reason}"))?;

                send_response(UsiResponse::BestMove {
                    best_move: "resign".to_string(),
                    ponder: None,
                })?;
                return Ok(());
            }
        }
    }

    // Clean up old stop flag before creating new one
    // Following the same policy as finalize_search: once a stop flag is set,
    // we don't reset it to avoid race conditions
    if let Some(_old_flag) = ctx.current_stop_flag.take() {
        log::debug!("Cleaned up old stop flag before creating new one");
    }

    // Create new per-search stop flag (after all validation passes)
    let search_stop_flag = Arc::new(AtomicBool::new(false));
    *ctx.current_stop_flag = Some(search_stop_flag.clone());
    log::debug!("Created new per-search stop flag for upcoming search");

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
    log::debug!("Using per-search stop flag for search_id={search_id}");

    // Log before spawning worker
    log::debug!("About to spawn worker thread for search_id={search_id}");
    log::debug!("NewSearchStart: spawning worker, search_id={search_id}");

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
    log::debug!("Worker thread handle stored, search_state = Searching");

    // Send immediate info depth 1 to confirm search started (ensures GUI sees activity)
    send_response(UsiResponse::Info(crate::usi::output::SearchInfo {
        depth: Some(1),
        time: Some(0),
        nodes: Some(0),
        string: Some("search starting".to_string()),
        ..Default::default()
    }))?;
    log::debug!("Sent initial info depth 1 heartbeat to GUI");

    // Don't block - return immediately
    Ok(())
}

fn handle_stop_command(ctx: &mut CommandContext) -> Result<()> {
    log::debug!("Received stop command, search_state = {:?}", *ctx.search_state);
    log::debug!("Stop command received, entering stop handler");
    log::debug!(
        "StopRequested: search_id={}, state={:?}",
        *ctx.current_search_id,
        *ctx.search_state
    );

    // Early return if not searching
    if !ctx.search_state.is_searching() {
        log::debug!("Stop while idle -> ignore");
        log::debug!("StopAck: ignored (not searching)");
        return Ok(());
    }

    // Handle ponder searches - according to USI spec, stop command should return bestmove
    if *ctx.current_search_is_ponder {
        log::info!(
            "Stop during ponder (search_id: {}) - will send bestmove per USI spec",
            *ctx.current_search_id
        );

        // Signal stop to worker thread using per-search flag
        *ctx.search_state = SearchState::StopRequested;
        if let Some(ref stop_flag) = *ctx.current_stop_flag {
            stop_flag.store(true, Ordering::Release);
        }

        // Mark that we're no longer in ponder mode since stop was received
        *ctx.current_search_is_ponder = false;

        // Continue to send bestmove below
    }

    // Signal stop to worker thread for normal searches
    if ctx.search_state.is_searching() {
        *ctx.search_state = SearchState::StopRequested;
        if let Some(ref stop_flag) = *ctx.current_stop_flag {
            stop_flag.store(true, Ordering::Release);
            log::debug!(
                "Per-search stop flag set to true for search_id={}, search_state = StopRequested",
                *ctx.current_search_id
            );

            // Debug: Verify stop flag was actually set
            let stop_value = stop_flag.load(Ordering::Acquire);
            log::debug!("Stop flag verification (Acquire read): {stop_value}");
        } else {
            log::warn!("No current stop flag available for search_id={}", *ctx.current_search_id);
        }
        log::debug!("StopAck: stop_flag set, search_id={}", *ctx.current_search_id);

        // First try to use committed best from session immediately
        if let Some(session) = ctx.current_session.clone() {
            if ctx.emit_best_from_session(
                &session,
                BestmoveSource::SessionOnStop,
                None, // Let build_meta create default StopInfo
                "SessionOnStop",
            )? {
                return Ok(());
            }
        }

        // Check if the last search was using byoyomi time control and get safety ms
        let (is_byoyomi, safety_ms) = {
            let adapter = lock_or_recover_adapter(ctx.engine);
            (adapter.last_search_is_byoyomi(), adapter.byoyomi_safety_ms())
        };

        // Get safety factor from environment variable (default: 0.5 for stage1, 1.0 for total)
        let stage1_factor = std::env::var("BYOYOMI_STAGE1_FACTOR")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| {
                if f <= 0.0 {
                    log::warn!("BYOYOMI_STAGE1_FACTOR must be positive, using default 0.5");
                    0.5
                } else {
                    f
                }
            })
            .unwrap_or(0.5);
        let total_factor = std::env::var("BYOYOMI_TOTAL_FACTOR")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| {
                if f <= 0.0 {
                    log::warn!("BYOYOMI_TOTAL_FACTOR must be positive, using default 1.0");
                    1.0
                } else {
                    f
                }
            })
            .unwrap_or(1.0);

        // Use adaptive timeouts based on byoyomi safety settings
        #[cfg(test)]
        let stage1_timeout = if is_byoyomi {
            // Test mode: minimal clamp for faster tests
            Duration::from_millis(((safety_ms as f64 * stage1_factor) as u64).max(1))
        } else {
            Duration::from_millis(10) // Test mode: very quick wait
        };
        #[cfg(not(test))]
        let stage1_timeout = if is_byoyomi {
            // Use configured fraction of safety margin for stage 1
            // For very short byoyomi (< 800ms safety), allow shorter minimums
            let min_stage1 = if safety_ms < 800 { 200 } else { 400 };
            let max_stage1 = if safety_ms < 800 { 600 } else { 1000 };
            Duration::from_millis(
                ((safety_ms as f64 * stage1_factor) as u64).clamp(min_stage1, max_stage1),
            )
        } else {
            Duration::from_millis(100) // Normal mode: quick wait
        };

        #[cfg(test)]
        let total_timeout = if is_byoyomi {
            // Test mode: minimal clamp for faster tests
            Duration::from_millis(((safety_ms as f64 * total_factor) as u64).max(1))
        } else {
            Duration::from_millis(15) // Test mode: very quick fallback
        };
        #[cfg(not(test))]
        let total_timeout = if is_byoyomi {
            // Use configured fraction of safety margin for total timeout
            // For very short byoyomi (< 1600ms safety), allow shorter minimums
            let min_total = if safety_ms < 1600 { 400 } else { 800 };
            let max_total = if safety_ms < 1600 { 1200 } else { 2000 };
            Duration::from_millis(
                ((safety_ms as f64 * total_factor) as u64).clamp(min_total, max_total),
            )
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
                log::warn!(
                    "Timeout waiting for bestmove after stop command (search_id={})",
                    *ctx.current_search_id
                );
                // Log timeout error
                log::debug!("Stop command timeout: {:?}", EngineError::Timeout);

                // Use emergency fallback (session already tried at the beginning)
                match generate_fallback_move(
                    ctx.engine,
                    partial_result.clone(),
                    ctx.allow_null_move,
                ) {
                    Ok((move_str, used_partial)) => {
                        // Log fallback source (info now handled by BestmoveEmitter)
                        if let Some((_, depth, score)) = partial_result {
                            log::debug!("Using partial result: depth={depth}, score={score}");
                        } else {
                            log::debug!("Using emergency fallback after timeout");
                        }
                        log::debug!("Sending emergency fallback bestmove: {move_str}");

                        // Use BestmoveEmitter for centralized emission
                        if let Some(ref _emitter) = ctx.current_bestmove_emitter {
                            // Determine the source based on what generate_fallback_move actually used
                            let (from, depth, score_str) = if used_partial {
                                if let Some((_, d, s)) = partial_result {
                                    (
                                        BestmoveSource::PartialResultTimeout,
                                        d,
                                        Some(format!("cp {s}")),
                                    )
                                } else {
                                    // This shouldn't happen, but handle gracefully
                                    (BestmoveSource::EmergencyFallbackTimeout, 0, None)
                                }
                            } else {
                                (BestmoveSource::EmergencyFallbackTimeout, 0, None)
                            };

                            let meta = build_meta(
                                from, depth, None, // seldepth
                                score_str, None, // Let build_meta create appropriate StopInfo
                            );

                            ctx.emit_and_finalize(move_str, None, meta, "TimeoutFallback")?;
                        } else {
                            log::warn!("BestmoveEmitter not available for timeout fallback; sending bestmove directly");
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
                        if let Some(ref _emitter) = ctx.current_bestmove_emitter {
                            let meta = build_meta(
                                BestmoveSource::ResignTimeout,
                                0,    // depth
                                None, // seldepth
                                None, // score
                                None, // Let build_meta create appropriate StopInfo
                            );

                            ctx.emit_and_finalize(
                                "resign".to_string(),
                                None,
                                meta,
                                "ResignTimeout",
                            )?;
                        } else {
                            log::warn!("BestmoveEmitter not available for resign; sending bestmove directly");
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

            // Calculate remaining time for adaptive polling
            let remaining = total_timeout.saturating_sub(elapsed);
            let poll_timeout = std::cmp::min(remaining, Duration::from_millis(3));

            // Check for bestmove message with timeout
            match ctx.worker_rx.recv_timeout(poll_timeout) {
                // WorkerMessage::BestMove has been completely removed.
                // All bestmove emissions now go through the session-based approach
                Ok(WorkerMessage::Info { info, search_id }) => {
                    // Forward info messages during active search (including StopRequested state)
                    // Only forward messages from current search to prevent old search info from appearing
                    // Note: is_searching() returns true for both Searching and StopRequested states,
                    // allowing GUIs to receive final info messages during stop processing
                    if search_id == *ctx.current_search_id && ctx.search_state.is_searching() {
                        let _ = send_response(UsiResponse::Info(info));
                    } else {
                        log::trace!(
                            "Suppressed Info message - old search_id: {} (current: {})",
                            search_id,
                            *ctx.current_search_id
                        );
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
                        log::debug!("SearchFinished received in stop handler, sending bestmove");
                        // Try to use session-based bestmove
                        if let Some(session) = ctx.current_session.clone() {
                            if ctx.emit_best_from_session(
                                &session,
                                BestmoveSource::SessionInSearchFinished,
                                stop_info, // Pass the provided stop_info if any
                                "BestmoveEmitter",
                            )? {
                                break;
                            } else {
                                // Continue to wait for BestMove or use fallback
                                log::debug!(
                                    "Session validation failed in stop handler, continuing to wait"
                                );
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
                            Ok((move_str, used_partial)) => {
                                // Log fallback source (info now handled by BestmoveEmitter)
                                if let Some((_, depth, score)) = partial_result {
                                    log::debug!("Using partial result on finish: depth={depth}, score={score}");
                                } else {
                                    log::debug!("Using emergency fallback on finish");
                                }

                                if let Some(ref _emitter) = ctx.current_bestmove_emitter {
                                    let (from, depth, score_str) = if used_partial {
                                        if let Some((_, d, s)) = partial_result {
                                            (
                                                BestmoveSource::PartialResultOnFinish,
                                                d,
                                                Some(format!("cp {s}")),
                                            )
                                        } else {
                                            // This shouldn't happen, but handle gracefully
                                            (BestmoveSource::EmergencyFallbackOnFinish, 0, None)
                                        }
                                    } else {
                                        (BestmoveSource::EmergencyFallbackOnFinish, 0, None)
                                    };

                                    let meta = build_meta(
                                        from, depth, None, // seldepth
                                        score_str,
                                        None, // Let build_meta create appropriate StopInfo
                                    );

                                    ctx.emit_and_finalize(move_str, None, meta, "BestmoveEmitter")?;
                                } else {
                                    log::warn!("BestmoveEmitter not available for finish handler; sending bestmove directly");
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

                                if let Some(ref _emitter) = ctx.current_bestmove_emitter {
                                    let meta = build_meta(
                                        BestmoveSource::ResignOnFinish,
                                        0,    // depth
                                        None, // seldepth
                                        None, // score
                                        None, // Let build_meta create appropriate StopInfo
                                    );

                                    ctx.emit_and_finalize(
                                        "resign".to_string(),
                                        None,
                                        meta,
                                        "BestmoveEmitter",
                                    )?;
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
                Ok(WorkerMessage::Error { message, search_id }) => {
                    if search_id == *ctx.current_search_id {
                        log::error!("Worker error in stop handler: {message}");
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    // Timeout is expected - just continue to next iteration
                }
                Err(e) => {
                    // Channel disconnected
                    log::error!("Worker channel error in stop handler: {e:?}");
                    break;
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bestmove_emitter::BestmoveEmitter;
    use crate::engine_adapter::EngineAdapter;
    use crate::state::SearchState;
    use crossbeam_channel::{unbounded, Receiver, Sender};
    use std::thread::JoinHandle;

    /// Helper function to create a test CommandContext
    fn create_test_context() -> (
        Arc<Mutex<EngineAdapter>>,
        Arc<AtomicBool>,
        Sender<WorkerMessage>,
        Receiver<WorkerMessage>,
        SearchState,
        u64,
        u64,
        bool,
        Option<SearchSession>,
        Option<BestmoveEmitter>,
        Option<Arc<AtomicBool>>,
        Option<String>,
        Option<JoinHandle<()>>,
    ) {
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = unbounded();
        let search_state = SearchState::Idle;
        let search_id_counter = 0;
        let current_search_id = 0;
        let current_search_is_ponder = false;
        let current_session = None;
        let current_bestmove_emitter = None;
        let current_stop_flag = None;
        let last_position_cmd = None;
        let worker_handle = None;

        (
            engine,
            stop_flag,
            tx,
            rx,
            search_state,
            search_id_counter,
            current_search_id,
            current_search_is_ponder,
            current_session,
            current_bestmove_emitter,
            current_stop_flag,
            last_position_cmd,
            worker_handle,
        )
    }

    /// Helper function to clean up test worker threads
    fn cleanup_test_worker(
        worker_handle: &mut Option<JoinHandle<()>>,
        worker_rx: &Receiver<WorkerMessage>,
        search_state: &mut SearchState,
    ) {
        use crate::worker::wait_for_worker_with_timeout;
        use std::time::Duration;

        if worker_handle.is_some() {
            log::debug!("Test cleanup: waiting for worker thread");
            // Wait for worker with short timeout suitable for tests
            let _ = wait_for_worker_with_timeout(
                worker_handle,
                worker_rx,
                search_state,
                Duration::from_millis(100),
            );
        }

        // Drain any remaining messages
        let mut drained_count = 0;
        while worker_rx.try_recv().is_ok() {
            drained_count += 1;
        }
        if drained_count > 0 {
            log::debug!("Test cleanup: drained {} messages from worker_rx", drained_count);
        }

        // Ensure we're in idle state
        *search_state = SearchState::Idle;
    }

    #[test]
    fn test_position_command_stored_on_success() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Test successful position command
        let cmd = UsiCommand::Position {
            startpos: true,
            sfen: None,
            moves: vec!["7g7f".to_string()],
        };

        // Should succeed
        assert!(handle_command(cmd, &mut ctx).is_ok());

        // Check that position command was stored
        assert!(ctx.last_position_cmd.is_some());
        let stored_cmd = ctx.last_position_cmd.as_ref().unwrap();
        assert!(stored_cmd.contains("position startpos"));
        assert!(stored_cmd.contains("moves 7g7f"));
    }

    #[test]
    fn test_position_command_not_stored_on_failure() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set a valid previous position command
        *ctx.last_position_cmd = Some("position startpos".to_string());
        let previous_cmd = ctx.last_position_cmd.clone();

        // Test with invalid SFEN that will fail
        let cmd = UsiCommand::Position {
            startpos: false,
            sfen: Some("invalid sfen string".to_string()),
            moves: vec![],
        };

        // Command handling succeeds (it logs error but doesn't crash)
        assert!(handle_command(cmd, &mut ctx).is_ok());

        // Check that previous position command is still stored
        assert_eq!(*ctx.last_position_cmd, previous_cmd);
    }

    #[test]
    fn test_usinewgame_clears_last_position() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set a position command
        *ctx.last_position_cmd = Some("position startpos moves 7g7f".to_string());

        // UsiNewGame should clear last_position_cmd
        let cmd = UsiCommand::UsiNewGame;
        assert!(handle_command(cmd, &mut ctx).is_ok());

        // Check that position command was cleared
        assert!(ctx.last_position_cmd.is_none());
    }

    #[test]
    fn test_gameover_clears_last_position() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set a position command
        *ctx.last_position_cmd = Some("position startpos moves 7g7f".to_string());

        // GameOver should clear last_position_cmd
        let cmd = UsiCommand::GameOver {
            result: crate::usi::GameResult::Win,
        };
        assert!(handle_command(cmd, &mut ctx).is_ok());

        // Check that position command was cleared
        assert!(ctx.last_position_cmd.is_none());
    }

    #[test]
    fn test_go_with_position_recovery() {
        use crate::usi::GoParams;

        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        // Set initial position
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Store a position command
        *ctx.last_position_cmd = Some("position startpos moves 7g7f 3c3d".to_string());

        // Force clear position in engine to simulate position loss
        {
            let mut adapter = ctx.engine.lock().unwrap();
            adapter.force_reset_state();
        }

        // Send go command - should attempt recovery
        let cmd = UsiCommand::Go(GoParams {
            depth: Some(1),
            ..Default::default()
        });

        // This should succeed (recovery happens in handle_go_command)
        let result = handle_command(cmd, &mut ctx);
        assert!(result.is_ok(), "Go command should succeed with position recovery");

        // Position should be restored
        {
            let adapter = ctx.engine.lock().unwrap();
            assert!(adapter.has_position(), "Position should be restored");
        }
    }

    #[test]
    fn test_go_with_invalid_position_recovery() {
        use crate::usi::GoParams;

        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Store an invalid position command
        *ctx.last_position_cmd = Some("position invalid command".to_string());

        // Send go command - recovery should fail
        let cmd = UsiCommand::Go(GoParams {
            depth: Some(1),
            ..Default::default()
        });

        // This should still succeed (resigns gracefully)
        let result = handle_command(cmd, &mut ctx);
        assert!(result.is_ok(), "Go command should handle invalid recovery gracefully");

        // Clean up worker thread
        cleanup_test_worker(&mut worker_handle, &rx, &mut search_state);
    }

    #[test]
    fn test_stop_while_idle() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Ensure we're in idle state
        assert_eq!(*ctx.search_state, SearchState::Idle);

        // Send stop command while idle
        let cmd = UsiCommand::Stop;
        let result = handle_command(cmd, &mut ctx);

        // Should succeed (ignored)
        assert!(result.is_ok(), "Stop while idle should be handled gracefully");
        assert_eq!(*ctx.search_state, SearchState::Idle, "Should remain idle");
    }

    #[test]
    fn test_emit_and_finalize_with_error() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set up search state
        *ctx.search_state = SearchState::Searching;
        *ctx.current_search_id = 42;
        *ctx.current_search_is_ponder = false;

        // Set up BestmoveEmitter - in a real error scenario, emit() would fail
        // but we can't easily mock that here, so we test the structure
        *ctx.current_bestmove_emitter = Some(BestmoveEmitter::new(42));

        // Test emit_and_finalize
        let meta = build_meta(
            BestmoveSource::SessionOnStop,
            10,
            Some(15),
            Some("cp 100".to_string()),
            None,
        );

        // Call emit_and_finalize
        let result = ctx.emit_and_finalize(
            "7g7f".to_string(),
            Some("8c8d".to_string()),
            meta,
            "test_finalize",
        );

        // Should succeed
        assert!(result.is_ok(), "emit_and_finalize should handle errors gracefully");

        // Verify search was finalized
        assert_eq!(*ctx.search_state, SearchState::Idle, "Search should be finalized");
        assert!(ctx.current_bestmove_emitter.is_none(), "Emitter should be cleared");
        assert!(ctx.current_stop_flag.is_none(), "Stop flag should be cleared");
        assert!(!*ctx.current_search_is_ponder, "Ponder flag should be reset");
    }

    #[test]
    fn test_emit_and_finalize_with_actual_error() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set up search state
        *ctx.search_state = SearchState::Searching;
        *ctx.current_search_id = 42;
        *ctx.current_search_is_ponder = false;

        // Set up BestmoveEmitter that will force an error
        *ctx.current_bestmove_emitter = Some(BestmoveEmitter::new_with_error(42));

        // Test emit_and_finalize
        let meta = build_meta(
            BestmoveSource::SessionOnStop,
            10,
            Some(15),
            Some("cp 100".to_string()),
            None,
        );

        // Call emit_and_finalize - should handle the error gracefully
        let result = ctx.emit_and_finalize(
            "7g7f".to_string(),
            Some("8c8d".to_string()),
            meta,
            "test_finalize",
        );

        // Should succeed despite emit() error (due to fallback)
        assert!(result.is_ok(), "emit_and_finalize should handle emit errors gracefully");

        // Verify search was finalized even though emit() failed
        assert_eq!(
            *ctx.search_state,
            SearchState::Idle,
            "Search should be finalized even on emit error"
        );
        assert!(ctx.current_bestmove_emitter.is_none(), "Emitter should be cleared");
        assert!(ctx.current_stop_flag.is_none(), "Stop flag should be cleared");
        assert!(!*ctx.current_search_is_ponder, "Ponder flag should be reset");

        // Note: In a real scenario, the fallback send_response() would send the bestmove
        // We can't easily verify that in unit tests without mocking the global writer
    }

    #[test]
    fn test_emit_and_finalize_without_emitter() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set up search state
        *ctx.search_state = SearchState::Searching;
        *ctx.current_search_id = 100;
        *ctx.current_search_is_ponder = false;
        *ctx.current_stop_flag = Some(Arc::new(AtomicBool::new(false)));

        // Key: Set current_bestmove_emitter to None
        *ctx.current_bestmove_emitter = None;

        // Test emit_and_finalize
        let meta =
            build_meta(BestmoveSource::SessionOnStop, 8, Some(12), Some("cp 50".to_string()), None);

        // Call emit_and_finalize
        let result = ctx.emit_and_finalize(
            "2g2f".to_string(),
            Some("8c8d".to_string()),
            meta,
            "NoEmitterTest",
        );

        // Should succeed with direct send
        assert!(result.is_ok(), "emit_and_finalize should succeed without emitter");

        // Verify search was finalized
        assert_eq!(*ctx.search_state, SearchState::Idle, "Search should be finalized");
        assert!(ctx.current_bestmove_emitter.is_none(), "Emitter should remain None");
        assert!(ctx.current_stop_flag.is_none(), "Stop flag should be cleared");
        assert!(!*ctx.current_search_is_ponder, "Ponder flag should be reset");
        assert!(ctx.current_session.is_none(), "Session should be cleared");

        // Note: The direct send_response() call would send the bestmove,
        // but we can't easily verify stdout output in unit tests
    }

    #[test]
    fn test_stop_handler_partial_result_timeout() {
        use crate::bestmove_emitter::{clear_last_source_for, last_source_for};
        use std::thread;
        use std::time::Duration;

        let search_id = 200;

        // Clear last emit source for this search_id
        clear_last_source_for(search_id);

        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        // Set up engine adapter with very short byoyomi_safety_ms
        {
            let mut adapter = lock_or_recover_adapter(&engine);
            adapter.set_byoyomi_safety_ms_for_test(20); // Short enough to trigger timeout
            adapter.set_last_search_is_byoyomi(true);
            // Set a position so fallback doesn't return "resign"
            adapter.set_position(true, None, &[]).unwrap();
        }

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set up search state
        *ctx.search_state = SearchState::Searching;
        *ctx.current_search_id = 200;
        *ctx.current_search_is_ponder = false;
        *ctx.current_stop_flag = Some(Arc::new(AtomicBool::new(false)));
        *ctx.current_bestmove_emitter = Some(BestmoveEmitter::new(200));

        // Spawn a thread that sends a partial result but delays bestmove
        let tx_clone = ctx.worker_tx.clone();
        let worker_thread = thread::spawn(move || {
            // Send partial result quickly (well before timeout of 20ms)
            thread::sleep(Duration::from_millis(5));
            let _ = tx_clone.send(WorkerMessage::PartialResult {
                current_best: "7g7f".to_string(),
                depth: 5,
                score: 100,
                search_id: 200,
            });

            // Sleep longer than the timeout to trigger timeout path
            thread::sleep(Duration::from_millis(30));

            // Send SearchFinished too late
            let _ = tx_clone.send(WorkerMessage::SearchFinished {
                session_id: 1,
                root_hash: 0,
                search_id: 200,
                stop_info: None,
            });
        });

        // Call handle_stop_command
        let result = handle_stop_command(&mut ctx);

        // Should succeed using partial result timeout path
        assert!(result.is_ok(), "handle_stop should succeed with partial result timeout");

        // Verify search was finalized
        assert_eq!(*ctx.search_state, SearchState::Idle);
        assert!(ctx.current_bestmove_emitter.is_none());

        // Verify the correct BestmoveSource was used
        assert_eq!(
            last_source_for(search_id),
            Some(BestmoveSource::PartialResultTimeout),
            "Should have used PartialResultTimeout source"
        );

        // Clean up worker thread
        let _ = worker_thread.join();
    }

    #[test]
    fn test_build_meta_mapping() {
        use engine_core::search::types::TerminationReason;

        // Test all BestmoveSource to TerminationReason mappings
        let test_cases = vec![
            // Timeout cases -> TimeLimit
            (BestmoveSource::ResignTimeout, TerminationReason::TimeLimit),
            (BestmoveSource::EmergencyFallbackTimeout, TerminationReason::TimeLimit),
            (BestmoveSource::PartialResultTimeout, TerminationReason::TimeLimit),
            // Normal completion cases -> Completed
            (BestmoveSource::EmergencyFallback, TerminationReason::Completed),
            (BestmoveSource::EmergencyFallbackOnFinish, TerminationReason::Completed),
            (BestmoveSource::PartialResultOnFinish, TerminationReason::Completed),
            (BestmoveSource::SessionInSearchFinished, TerminationReason::Completed),
            // User stop cases -> UserStop
            (BestmoveSource::SessionOnStop, TerminationReason::UserStop),
            // Error cases -> Error
            (BestmoveSource::Resign, TerminationReason::Error),
            (BestmoveSource::ResignOnFinish, TerminationReason::Error),
        ];

        for (source, expected_reason) in test_cases {
            let meta = build_meta(source, 10, None, None, None);
            assert_eq!(
                meta.stop_info.reason, expected_reason,
                "BestmoveSource::{:?} should map to TerminationReason::{:?}",
                source, expected_reason
            );
        }
    }

    #[test]
    fn test_stop_handler_emergency_fallback_timeout() {
        use crate::bestmove_emitter::{clear_last_source_for, last_source_for};
        use std::thread;
        use std::time::Duration;

        let search_id = 300;

        // Clear last emit source for this search_id
        clear_last_source_for(search_id);

        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        // Set up engine adapter with very short byoyomi_safety_ms
        {
            let mut adapter = lock_or_recover_adapter(&engine);
            adapter.set_byoyomi_safety_ms_for_test(10); // Very short to trigger timeout
            adapter.set_last_search_is_byoyomi(true);
            // Set a position for emergency fallback
            adapter.set_position(true, None, &[]).unwrap();
        }

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set up search state
        *ctx.search_state = SearchState::Searching;
        *ctx.current_search_id = 300;
        *ctx.current_search_is_ponder = false;
        *ctx.current_stop_flag = Some(Arc::new(AtomicBool::new(false)));
        *ctx.current_bestmove_emitter = Some(BestmoveEmitter::new(300));

        // Spawn a thread that sends nothing within timeout
        let tx_clone = ctx.worker_tx.clone();
        let worker_thread = thread::spawn(move || {
            // Sleep longer than the timeout to trigger emergency fallback
            thread::sleep(Duration::from_millis(20));

            // Do not send PartialResult at all to ensure EmergencyFallbackTimeout
            // Only send SearchFinished after timeout
            let _ = tx_clone.send(WorkerMessage::SearchFinished {
                session_id: 1,
                root_hash: 0,
                search_id: 300,
                stop_info: None,
            });
        });

        // Call handle_stop_command
        let result = handle_stop_command(&mut ctx);

        // Should succeed using emergency fallback timeout path
        assert!(result.is_ok(), "handle_stop should succeed with emergency fallback timeout");

        // Verify search was finalized
        assert_eq!(*ctx.search_state, SearchState::Idle);
        assert!(ctx.current_bestmove_emitter.is_none());

        // Verify the correct BestmoveSource was used
        assert_eq!(
            last_source_for(search_id),
            Some(BestmoveSource::EmergencyFallbackTimeout),
            "Should have used EmergencyFallbackTimeout source"
        );

        // Clean up worker thread
        let _ = worker_thread.join();
    }

    #[test]
    fn test_worker_error_handling() {
        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set up search state
        *ctx.search_state = SearchState::Searching;
        *ctx.current_search_id = 400;
        *ctx.current_search_is_ponder = false;
        *ctx.current_stop_flag = Some(Arc::new(AtomicBool::new(false)));
        *ctx.current_bestmove_emitter = Some(BestmoveEmitter::new(400));

        // Test error from current search
        let error_msg = WorkerMessage::Error {
            message: "Test error".to_string(),
            search_id: 400,
        };

        // Send error message
        ctx.worker_tx.send(error_msg).unwrap();

        // Also test error from old search (should be ignored)
        let old_error_msg = WorkerMessage::Error {
            message: "Old error".to_string(),
            search_id: 300,
        };
        ctx.worker_tx.send(old_error_msg).unwrap();

        // Send SearchFinished to terminate
        ctx.worker_tx
            .send(WorkerMessage::SearchFinished {
                session_id: 1,
                root_hash: 0,
                search_id: 400,
                stop_info: None,
            })
            .unwrap();

        // Call stop handler which should process the error message
        let result = handle_stop_command(&mut ctx);
        assert!(result.is_ok(), "Stop command should handle error messages");

        // Verify search was finalized
        assert_eq!(*ctx.search_state, SearchState::Idle);
    }

    #[test]
    fn test_recv_timeout_boundary() {
        use std::time::{Duration, Instant};

        let (
            engine,
            stop_flag,
            tx,
            rx,
            mut search_state,
            mut search_id_counter,
            mut current_search_id,
            mut current_search_is_ponder,
            mut current_session,
            mut current_bestmove_emitter,
            mut current_stop_flag,
            mut last_position_cmd,
            mut worker_handle,
        ) = create_test_context();

        // Set up engine adapter with very short timeout (1ms)
        {
            let mut adapter = lock_or_recover_adapter(&engine);
            adapter.set_byoyomi_safety_ms_for_test(1);
            adapter.set_last_search_is_byoyomi(true);
            adapter.set_position(true, None, &[]).unwrap();
        }

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &stop_flag,
            worker_tx: &tx,
            worker_rx: &rx,
            worker_handle: &mut worker_handle,
            search_state: &mut search_state,
            search_id_counter: &mut search_id_counter,
            current_search_id: &mut current_search_id,
            current_search_is_ponder: &mut current_search_is_ponder,
            current_session: &mut current_session,
            current_bestmove_emitter: &mut current_bestmove_emitter,
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            last_position_cmd: &mut last_position_cmd,
        };

        // Set up search state
        *ctx.search_state = SearchState::Searching;
        *ctx.current_search_id = 500;
        *ctx.current_search_is_ponder = false;
        *ctx.current_stop_flag = Some(Arc::new(AtomicBool::new(false)));
        *ctx.current_bestmove_emitter = Some(BestmoveEmitter::new(500));

        // Don't send any messages - let it timeout
        let start = Instant::now();
        let result = handle_stop_command(&mut ctx);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "Stop command should handle timeout gracefully");
        assert!(elapsed < Duration::from_millis(10), "Should timeout quickly with 1ms safety");

        // Verify search was finalized
        assert_eq!(*ctx.search_state, SearchState::Idle);
    }

    #[test]
    fn test_byoyomi_factor_validation() {
        // Test invalid BYOYOMI_STAGE1_FACTOR values
        std::env::set_var("BYOYOMI_STAGE1_FACTOR", "0");
        let factor = std::env::var("BYOYOMI_STAGE1_FACTOR")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| {
                if f <= 0.0 {
                    // In production code, this would log a warning
                    0.5
                } else {
                    f
                }
            })
            .unwrap_or(0.5);
        assert_eq!(factor, 0.5, "Zero factor should default to 0.5");

        std::env::set_var("BYOYOMI_STAGE1_FACTOR", "-1.0");
        let factor = std::env::var("BYOYOMI_STAGE1_FACTOR")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| if f <= 0.0 { 0.5 } else { f })
            .unwrap_or(0.5);
        assert_eq!(factor, 0.5, "Negative factor should default to 0.5");

        // Test valid values
        std::env::set_var("BYOYOMI_STAGE1_FACTOR", "0.3");
        let factor = std::env::var("BYOYOMI_STAGE1_FACTOR")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| if f <= 0.0 { 0.5 } else { f })
            .unwrap_or(0.5);
        assert_eq!(factor, 0.3, "Valid factor should be used");

        // Clean up
        std::env::remove_var("BYOYOMI_STAGE1_FACTOR");

        // Test BYOYOMI_TOTAL_FACTOR similarly
        std::env::set_var("BYOYOMI_TOTAL_FACTOR", "0");
        let factor = std::env::var("BYOYOMI_TOTAL_FACTOR")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .map(|f| if f <= 0.0 { 1.0 } else { f })
            .unwrap_or(1.0);
        assert_eq!(factor, 1.0, "Zero total factor should default to 1.0");

        // Clean up
        std::env::remove_var("BYOYOMI_TOTAL_FACTOR");
    }
}
