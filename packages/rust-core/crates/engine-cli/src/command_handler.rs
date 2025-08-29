use crate::engine_adapter::EngineAdapter;
use crate::helpers::{generate_fallback_move, wait_for_search_completion};
use crate::search_session::SearchSession;
use crate::state::SearchState;
use crate::types::{BestmoveSource, PositionState, ResignReason};
use crate::usi::{
    canonicalize_position_cmd, send_info_string, send_response, GoParams, UsiCommand, UsiResponse,
};
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

/// Create a TSV-formatted log string from key-value pairs
/// Values are sanitized to prevent TSV format corruption
fn log_tsv(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| {
            // Sanitize value by replacing tabs and newlines with spaces
            let sanitized = v.replace(['\t', '\n', '\r'], " ");
            format!("{k}={sanitized}")
        })
        .collect::<Vec<_>>()
        .join("\t")
}

/// Handle position restoration failure by logging and sending resign
fn fail_position_restore(reason: ResignReason, log_reason: &str) -> Result<()> {
    send_info_string(log_tsv(&[("kind", "position_restore_fail"), ("reason", log_reason)]))?;
    send_info_string(log_tsv(&[("kind", "resign"), ("resign_reason", &reason.to_string())]))?;
    send_response(UsiResponse::BestMove {
        best_move: "resign".to_string(),
        ponder: None,
    })?;
    Ok(())
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
    pub position_state: &'a mut Option<PositionState>, // Store position state for recovery
    pub program_start: Instant, // Program start time for elapsed calculations
    pub legal_moves_check_logged: &'a mut bool, // Track if we've logged the legal moves check status
    /// Last received partial result (move, depth, score) for current search
    pub last_partial_result: &'a mut Option<(String, u8, i32)>,
    /// Precomputed root fallback move captured at go-time for stop-time emergencies
    pub pre_session_fallback: &'a mut Option<String>,
    /// Hash of the position when pre_session_fallback was computed
    pub pre_session_fallback_hash: &'a mut Option<u64>,
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
            | BestmoveSource::SessionInSearchFinished => TerminationReason::Completed,
            // User stop cases -> UserStop
            BestmoveSource::SessionOnStop => TerminationReason::UserStop,
            // Error cases -> Error
            BestmoveSource::Resign | BestmoveSource::ResignOnFinish => TerminationReason::Error,
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
        soft_limit_ms: 0,
        hard_limit_ms: 0,
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
            if let Ok((best_move, ponder, ponder_source)) =
                adapter.validate_and_get_bestmove(session, position)
            {
                // Extract common score formatting and metadata
                let depth = session.committed_best.as_ref().map(|b| b.depth).unwrap_or(0);
                let seldepth = session.committed_best.as_ref().and_then(|b| b.seldepth);
                let score_str = session.committed_best.as_ref().map(|b| match &b.score {
                    crate::search_session::Score::Cp(cp) => format!("cp {cp}"),
                    crate::search_session::Score::Mate(mate) => format!("mate {mate}"),
                });

                log::debug!("Validated bestmove from session: depth={depth}");

                // Metrics: PV長・Ponderソース
                let pv_len_str = session
                    .committed_best
                    .as_ref()
                    .map(|b| b.pv.len().to_string())
                    .unwrap_or_else(|| "0".to_string());
                let ponder_src_str = ponder_source.to_string();
                let metrics = log_tsv(&[
                    ("kind", "bestmove_metrics"),
                    ("search_id", &self.current_search_id.to_string()),
                    ("pv_len", &pv_len_str),
                    ("ponder_source", &ponder_src_str),
                    ("ponder_present", if ponder.is_some() { "true" } else { "false" }),
                ]);
                let _ = send_info_string(metrics);

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
        self.search_state.set_idle();
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
        // Metrics logging is handled before this call in emit_best_from_session
        // Try to emit via BestmoveEmitter if available
        if let Some(ref emitter) = self.current_bestmove_emitter {
            match emitter.emit(best_move.clone(), ponder.clone(), meta.clone()) {
                Ok(()) => {
                    self.finalize_search(finalize_label);
                    Ok(())
                }
                Err(e) => {
                    log::error!("BestmoveEmitter::emit failed: {e}");
                    // Send TSV log for fallback
                    self.send_fallback_tsv_log(
                        &best_move,
                        ponder.as_deref(),
                        Some(&meta),
                        "emitter_failed",
                    );
                    // Try direct send as fallback
                    if let Err(e) = send_response(UsiResponse::BestMove { best_move, ponder }) {
                        log::error!("Failed to send bestmove even with direct fallback: {e}");
                        // Continue without propagating error - USI requires best effort
                    }
                    // Always finalize search after attempting to emit
                    self.finalize_search(finalize_label);
                    Ok(())
                }
            }
        } else {
            log::warn!("BestmoveEmitter not available; sending bestmove directly");
            // Send TSV log for direct send
            self.send_fallback_tsv_log(&best_move, ponder.as_deref(), Some(&meta), "no_emitter");
            if let Err(e) = send_response(UsiResponse::BestMove { best_move, ponder }) {
                log::error!("Failed to send bestmove directly: {e}");
                // Continue without propagating error - USI requires best effort
            }
            // Always finalize search after attempting to emit
            self.finalize_search(finalize_label);
            Ok(())
        }
    }

    /// Send TSV log for direct fallback bestmove (when BestmoveEmitter is not available or fails)
    fn send_fallback_tsv_log(
        &self,
        best_move: &str,
        ponder: Option<&str>,
        meta: Option<&BestmoveMeta>,
        fallback_reason: &str,
    ) {
        // Prepare TSV log similar to BestmoveEmitter's format
        let search_id_str = self.current_search_id.to_string();
        let ponder_str = ponder.unwrap_or("none");

        let info_string = if let Some(m) = meta {
            // Format metadata values as strings for log_tsv
            let from_str = m.from.to_string();
            let stop_reason_str = m.stop_info.reason.to_string();
            let depth_str = m.stats.depth.to_string();
            let seldepth_str =
                m.stats.seldepth.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
            let depth_reached_str = m.stop_info.depth_reached.to_string();
            let nodes_str = m.stats.nodes.to_string();
            let nps_str = m.stats.nps.to_string();
            let elapsed_ms_str = m.stop_info.elapsed_ms.to_string();
            let hard_timeout_str = m.stop_info.hard_timeout.to_string();

            log_tsv(&[
                ("kind", "bestmove_direct_fallback"),
                ("search_id", &search_id_str),
                ("bestmove_from", &from_str),
                ("stop_reason", &stop_reason_str),
                ("depth", &depth_str),
                ("seldepth", &seldepth_str),
                ("depth_reached", &depth_reached_str),
                ("score", &m.stats.score),
                ("nodes", &nodes_str),
                ("nps", &nps_str),
                ("elapsed_ms", &elapsed_ms_str),
                ("hard_timeout", &hard_timeout_str),
                ("bestmove", best_move),
                ("ponder", ponder_str),
                ("fallback_reason", fallback_reason),
            ])
        } else {
            // Default values when no metadata is available
            log_tsv(&[
                ("kind", "bestmove_direct_fallback"),
                ("search_id", &search_id_str),
                ("bestmove_from", fallback_reason),
                ("stop_reason", "error"),
                ("depth", "0"),
                ("seldepth", "-"),
                ("depth_reached", "0"),
                ("score", "none"),
                ("nodes", "0"),
                ("nps", "0"),
                ("elapsed_ms", "0"),
                ("hard_timeout", "false"),
                ("bestmove", best_move),
                ("ponder", ponder_str),
                ("fallback_reason", fallback_reason),
            ])
        };

        if let Err(e) = send_info_string(info_string) {
            log::warn!("Failed to send fallback TSV log: {e}");
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
            // Note: Static tables are already initialized in main() using init_all_tables_once()
            // which is idempotent (using std::sync::Once internally)
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

            // Build the canonical position command string
            let cmd_canonical = canonicalize_position_cmd(startpos, sfen.as_deref(), &moves);

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

            // Clear pre-session fallback as position has changed
            *ctx.pre_session_fallback = None;
            *ctx.pre_session_fallback_hash = None;

            let mut engine = lock_or_recover_adapter(ctx.engine);
            match engine.set_position(startpos, sfen.as_deref(), &moves) {
                Ok(()) => {
                    // Get position info and create PositionState
                    if let Some(pos) = engine.get_position() {
                        let sfen_snapshot = position_to_sfen(pos);
                        let root_hash = pos.zobrist_hash();
                        let move_len = moves.len();

                        // Store the position state
                        let position_state = PositionState::new(
                            cmd_canonical.clone(),
                            root_hash,
                            move_len,
                            sfen_snapshot.clone(),
                        );

                        *ctx.position_state = Some(position_state);

                        log::debug!(
                            "Stored position state: cmd={}, hash={:#016x}",
                            cmd_canonical,
                            root_hash
                        );
                        log::info!(
                            "Position command completed - SFEN: {}, root_hash: {:#016x}, side_to_move: {:?}, move_count: {}",
                            sfen_snapshot, root_hash, pos.side_to_move, move_len
                        );

                        // Send structured log for position store
                        let stored_ms = ctx.program_start.elapsed().as_millis();
                        send_info_string(log_tsv(&[
                            ("kind", "position_store"),
                            ("root_hash", &format!("{:#016x}", root_hash)),
                            ("move_len", &move_len.to_string()),
                            ("sfen_first_20", &sfen_snapshot.chars().take(20).collect::<String>()),
                            ("stored_ms_since_start", &stored_ms.to_string()),
                        ]))?;
                    } else {
                        log::error!("Position set but unable to retrieve for state storage");
                    }
                }
                Err(e) => {
                    // Log error but don't crash - USI engines should be robust
                    log::error!("Failed to set position: {e}");
                    send_info_string(format!("Error: Failed to set position - {e}"))?;
                    // Don't update position_state on failure - keep the previous valid one
                    log::debug!("Keeping previous position state due to error");
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
                    Ok(()) => {
                        log::debug!("Ponder hit successfully processed");
                        // Emit USI-visible info for diagnostics (core also logs to stderr)
                        let _ = crate::usi::send_info_string(
                            "ponder_hit: converted to normal search (time budgets updated)"
                                .to_string(),
                        );
                    }
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
            // Terminate emitter first to prevent any bestmove output
            if let Some(ref emitter) = ctx.current_bestmove_emitter {
                emitter.terminate();
                log::debug!("Terminated bestmove emitter for gameover");
            }

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

            // Clear position state to avoid carrying over to next game
            *ctx.position_state = None;
            log::debug!("Cleared position_state for new game");

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

            // Clear position state for fresh start
            *ctx.position_state = None;
            log::debug!("Cleared position_state for new game");

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

    // Clear any pending messages from previous search to prevent interference
    let mut cleared_messages = 0;
    while let Ok(_msg) = ctx.worker_rx.try_recv() {
        cleared_messages += 1;
    }
    if cleared_messages > 0 {
        log::debug!("Cleared {cleared_messages} old messages from worker queue");
    }

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
    *ctx.pre_session_fallback = None; // Clear previous pre-session fallback
    *ctx.pre_session_fallback_hash = None; // Clear previous hash

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
            log::warn!("Position not set - attempting recovery from position state");

            // Try to recover from position state
            if let Some(pos_state) = ctx.position_state.as_ref() {
                let elapsed_ms = pos_state.elapsed().as_millis();
                log::debug!(
                    "Attempting to rebuild position from state: cmd={}, moves={}, age_ms={}",
                    pos_state.cmd_canonical,
                    pos_state.move_len,
                    elapsed_ms
                );
                send_info_string(log_tsv(&[
                    ("kind", "position_restore_try"),
                    ("move_len", &pos_state.move_len.to_string()),
                    ("age_ms", &elapsed_ms.to_string()),
                ]))?;

                // Parse and apply the canonical position command
                let mut need_fallback = false;
                match crate::usi::parse_usi_command(&pos_state.cmd_canonical) {
                    Ok(UsiCommand::Position {
                        startpos,
                        sfen,
                        moves,
                    }) => {
                        // First check move_len consistency
                        if moves.len() != pos_state.move_len {
                            log::warn!(
                                "Move count mismatch in stored command: expected {}, got {}. Attempting fallback.",
                                pos_state.move_len,
                                moves.len()
                            );
                            send_info_string(log_tsv(&[
                                ("kind", "position_restore_fallback"),
                                ("reason", "move_len_mismatch"),
                                ("expected", &pos_state.move_len.to_string()),
                                ("actual", &moves.len().to_string()),
                            ]))?;
                            need_fallback = true;
                        } else {
                            // Try to apply the canonical command
                            match engine.set_position(startpos, sfen.as_deref(), &moves) {
                                Ok(()) => {
                                    // Verify hash matches
                                    if let Some(pos) = engine.get_position() {
                                        let current_hash = pos.zobrist_hash();
                                        if current_hash == pos_state.root_hash {
                                            log::info!(
                                                "Successfully rebuilt position with matching hash"
                                            );
                                            send_info_string(log_tsv(&[
                                                ("kind", "position_restore_success"),
                                                ("source", "command"),
                                            ]))?;
                                        } else {
                                            log::warn!(
                                                "Position rebuilt but hash mismatch: expected {:#016x}, got {:#016x}, move_len={}",
                                                pos_state.root_hash, current_hash, pos_state.move_len
                                            );
                                            send_info_string(log_tsv(&[
                                                ("kind", "position_restore_fallback"),
                                                ("reason", "hash_mismatch"),
                                            ]))?;
                                            need_fallback = true;
                                        }
                                    } else {
                                        log::error!("Position set but unable to verify hash");
                                        need_fallback = true;
                                    }
                                }
                                Err(e) => {
                                    log::error!("Failed to rebuild position: {}", e);
                                    send_info_string(log_tsv(&[
                                        ("kind", "position_restore_fallback"),
                                        ("reason", "rebuild_failed"),
                                    ]))?;
                                    need_fallback = true;
                                }
                            }
                        }

                        // Attempt fallback if needed
                        if need_fallback {
                            log::debug!(
                                "Attempting fallback with sfen_snapshot: {}",
                                pos_state.sfen_snapshot
                            );

                            // Directly use sfen_snapshot without parsing
                            match engine.set_position(false, Some(&pos_state.sfen_snapshot), &[]) {
                                Ok(()) => {
                                    // Verify hash after fallback
                                    if let Some(pos) = engine.get_position() {
                                        let current_hash = pos.zobrist_hash();
                                        if current_hash == pos_state.root_hash {
                                            log::info!("Successfully restored position from sfen_snapshot with matching hash");
                                            send_info_string(log_tsv(&[
                                                ("kind", "position_restore_success"),
                                                ("source", "sfen_snapshot"),
                                            ]))?;
                                        } else {
                                            log::error!(
                                                "SFEN fallback hash mismatch: expected {:#016x}, got {:#016x}",
                                                pos_state.root_hash, current_hash
                                            );
                                            send_info_string(log_tsv(&[
                                                ("kind", "position_restore_fail"),
                                                ("reason", "sfen_hash_mismatch"),
                                                (
                                                    "expected",
                                                    &format!("{:#016x}", pos_state.root_hash),
                                                ),
                                                ("actual", &format!("{:#016x}", current_hash)),
                                            ]))?;
                                            return fail_position_restore(
                                                ResignReason::PositionRebuildFailed {
                                                    error:
                                                        "hash verification failed after fallback",
                                                },
                                                "sfen_hash_mismatch",
                                            );
                                        }
                                    } else {
                                        log::error!("Failed to get position after sfen_snapshot restoration");
                                        return fail_position_restore(
                                            ResignReason::PositionRebuildFailed {
                                                error: "no position after sfen restoration",
                                            },
                                            "no_position_after_sfen",
                                        );
                                    }
                                }
                                Err(e) => {
                                    log::error!("Failed to set position from sfen_snapshot: {}", e);
                                    return fail_position_restore(
                                        ResignReason::PositionRebuildFailed {
                                            error: "sfen_snapshot failed",
                                        },
                                        "sfen_snapshot_failed",
                                    );
                                }
                            }
                        }
                    }
                    _ => {
                        log::error!("Invalid stored position command: {}", pos_state.cmd_canonical);
                        return fail_position_restore(
                            ResignReason::InvalidStoredPositionCmd,
                            "invalid_cmd",
                        );
                    }
                }
            } else {
                log::error!("No position set and no recovery state available");
                return fail_position_restore(ResignReason::NoPositionSet, "no_position_set");
            }
        }

        // NOTE: has_legal_moves check is implemented but disabled due to MoveGen hang issue
        //
        // EngineAdapter::has_legal_moves() exists and uses MoveGen::generate_all(),
        // but calling it from subprocess context causes a hang. The issue appears to be
        // related to complex interaction between subprocess execution and engine_core APIs.
        //
        // The check is controlled by SKIP_LEGAL_MOVES environment variable:
        // - SKIP_LEGAL_MOVES=1 (default): Skip the check to avoid hang
        // - SKIP_LEGAL_MOVES=0: Would enable the check but causes hang in subprocess
        //
        // Additionally, USE_ANY_LEGAL environment variable controls which method to use:
        // - USE_ANY_LEGAL=1: Use optimized has_any_legal_move() with early exit
        // - USE_ANY_LEGAL=0 (default): Use standard has_legal_moves() with generate_all
        //
        // This workaround is safe because:
        // - Positions without legal moves are extremely rare
        // - The search algorithm handles checkmate/stalemate naturally
        // - See docs/movegen-hang-investigation-final.md for details
        let skip_legal_moves_check = std::env::var("SKIP_LEGAL_MOVES").as_deref() != Ok("0");
        let use_any_legal = std::env::var("USE_ANY_LEGAL").as_deref() == Ok("1");

        if skip_legal_moves_check {
            // Only log once per session
            if !*ctx.legal_moves_check_logged {
                log::debug!("has_legal_moves check is disabled (SKIP_LEGAL_MOVES != 0)");
                *ctx.legal_moves_check_logged = true;
            }
        } else {
            // Check is enabled - perform it with timing
            let check_start = Instant::now();
            let has_legal_moves = if use_any_legal {
                engine.has_any_legal_move()?
            } else {
                engine.has_legal_moves()?
            };

            let check_duration = check_start.elapsed();
            if check_duration > Duration::from_millis(5) {
                log::warn!(
                    "Legal moves check took {:?} (method: {})",
                    check_duration,
                    if use_any_legal {
                        "has_any_legal_move"
                    } else {
                        "has_legal_moves"
                    }
                );
            }

            if !has_legal_moves {
                return fail_position_restore(ResignReason::Checkmate, "no_legal_moves");
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

    // Precompute a root fallback move for immediate stop-after-go scenarios (race hardening)
    // This mirrors YaneuraOu's RootMove default PV safety at the USI layer
    {
        let adapter = lock_or_recover_adapter(ctx.engine);
        if let Ok(move_str) = adapter.generate_emergency_move() {
            // Store fallback move and current position hash
            *ctx.pre_session_fallback = Some(move_str.clone());
            *ctx.pre_session_fallback_hash = adapter.get_position().map(|p| p.zobrist_hash());
            let _ = send_info_string(log_tsv(&[
                ("kind", "go_received"),
                ("ponder", if params.ponder { "1" } else { "0" }),
                ("pre_session_fallback", &move_str),
            ]));
        } else {
            // Silent if no legal moves; stop path will handle resign
        }
    }

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
    if !ctx.search_state.try_start_search() {
        log::error!("Failed to transition to searching state from {:?}", ctx.search_state);
    }
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
    // If nothing to stop, return
    if !ctx.search_state.is_searching() {
        return Ok(());
    }

    // Signal stop to worker
    ctx.search_state.request_stop();
    if let Some(ref stop_flag) = *ctx.current_stop_flag {
        stop_flag.store(true, Ordering::SeqCst);
    }

    // Emit diagnostic snapshot for race analysis
    let diag = log_tsv(&[
        ("kind", "on_stop"),
        ("state", &format!("{:?}", *ctx.search_state)),
        (
            "ponder",
            if *ctx.current_search_is_ponder {
                "1"
            } else {
                "0"
            },
        ),
        (
            "session",
            if ctx.current_session.is_some() {
                "1"
            } else {
                "0"
            },
        ),
        (
            "partial",
            if ctx.last_partial_result.is_some() {
                "1"
            } else {
                "0"
            },
        ),
        (
            "pre_session_fallback",
            if ctx.pre_session_fallback.is_some() {
                "1"
            } else {
                "0"
            },
        ),
    ]);
    let _ = send_info_string(diag);

    // Ponder stop: emit immediately for GUI compatibility
    if *ctx.current_search_is_ponder {
        *ctx.current_search_is_ponder = false;

        // 1) Committed session
        if let Some(session) = ctx.current_session.clone() {
            if ctx.emit_best_from_session(
                &session,
                BestmoveSource::SessionOnStop,
                None,
                "PonderSessionOnStop",
            )? {
                let _ =
                    send_info_string(log_tsv(&[("kind", "on_stop_source"), ("src", "session")]));
                return Ok(());
            }
        }

        // 2) Partial result
        if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
            if let Ok((move_str, _)) =
                generate_fallback_move(ctx.engine, Some((mv, d, s)), ctx.allow_null_move)
            {
                let meta = build_meta(
                    BestmoveSource::SessionOnStop,
                    d,
                    None,
                    Some(format!("cp {s}")),
                    None,
                );
                let _ =
                    send_info_string(log_tsv(&[("kind", "on_stop_source"), ("src", "partial")]));
                ctx.emit_and_finalize(move_str, None, meta, "PonderPartialOnStop")?;
                return Ok(());
            }
        }

        // 3) Pre-session fallback captured at go-time (with hash verification)
        if let Some(_move_str) = ctx.pre_session_fallback.as_ref() {
            // Verify hash matches current position
            let adapter = lock_or_recover_adapter(ctx.engine);
            let current_hash = adapter.get_position().map(|p| p.zobrist_hash());

            if current_hash == *ctx.pre_session_fallback_hash {
                let move_str = ctx.pre_session_fallback.take().unwrap();
                let meta = build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
                let _ = send_info_string(log_tsv(&[
                    ("kind", "on_stop_source"),
                    ("src", "pre_session"),
                ]));
                ctx.emit_and_finalize(move_str, None, meta, "PonderPreSessionOnStop")?;
                return Ok(());
            } else {
                log::debug!(
                    "Pre-session fallback hash mismatch: expected {:?}, got {:?}",
                    ctx.pre_session_fallback_hash,
                    current_hash
                );
                // Clear invalid fallback
                *ctx.pre_session_fallback = None;
                *ctx.pre_session_fallback_hash = None;
            }
        }

        // 4) Emergency fallback
        let (move_str, from) = match generate_fallback_move(ctx.engine, None, ctx.allow_null_move) {
            Ok((m, _)) => (m, BestmoveSource::SessionOnStop),
            Err(_) => ("resign".to_string(), BestmoveSource::SessionOnStop),
        };
        let meta = build_meta(from, 0, None, None, None);
        let _ = send_info_string(log_tsv(&[("kind", "on_stop_source"), ("src", "emergency")]));
        ctx.emit_and_finalize(move_str, None, meta, "PonderEmergencyOnStop")?;
        return Ok(());
    }

    // Normal stop: emit immediately (session → partial → pre_session → emergency)
    if let Some(session) = ctx.current_session.clone() {
        if ctx.emit_best_from_session(
            &session,
            BestmoveSource::SessionOnStop,
            None,
            "SessionOnStop",
        )? {
            let _ = send_info_string(log_tsv(&[("kind", "on_stop_source"), ("src", "session")]));
            return Ok(());
        }
    }

    if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
        if let Ok((move_str, _)) =
            generate_fallback_move(ctx.engine, Some((mv, d, s)), ctx.allow_null_move)
        {
            let meta = build_meta(
                BestmoveSource::PartialResultTimeout,
                d,
                None,
                Some(format!("cp {s}")),
                None,
            );
            let _ = send_info_string(log_tsv(&[("kind", "on_stop_source"), ("src", "partial")]));
            ctx.emit_and_finalize(move_str, None, meta, "ImmediatePartialOnStop")?;
            return Ok(());
        }
    }

    // Pre-session fallback captured at go-time (with hash verification)
    if let Some(_move_str) = ctx.pre_session_fallback.as_ref() {
        // Verify hash matches current position
        let adapter = lock_or_recover_adapter(ctx.engine);
        let current_hash = adapter.get_position().map(|p| p.zobrist_hash());

        if current_hash == *ctx.pre_session_fallback_hash {
            let move_str = ctx.pre_session_fallback.take().unwrap();
            let meta = build_meta(BestmoveSource::SessionOnStop, 0, None, None, None);
            let _ =
                send_info_string(log_tsv(&[("kind", "on_stop_source"), ("src", "pre_session")]));
            ctx.emit_and_finalize(move_str, None, meta, "ImmediatePreSessionOnStop")?;
            return Ok(());
        } else {
            log::debug!(
                "Pre-session fallback hash mismatch (normal stop): expected {:?}, got {:?}",
                ctx.pre_session_fallback_hash,
                current_hash
            );
            // Clear invalid fallback
            *ctx.pre_session_fallback = None;
            *ctx.pre_session_fallback_hash = None;
        }
    }

    let (move_str, source) = match generate_fallback_move(ctx.engine, None, ctx.allow_null_move) {
        Ok((m, _)) => (m, BestmoveSource::EmergencyFallbackTimeout),
        Err(_) => ("resign".to_string(), BestmoveSource::ResignTimeout),
    };
    let _ = send_info_string(log_tsv(&[("kind", "on_stop_source"), ("src", "emergency")]));
    let meta = build_meta(source, 0, None, None, None);
    ctx.emit_and_finalize(move_str, None, meta, "ImmediateEmergencyOnStop")?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_adapter::EngineAdapter;
    use crate::usi::output::{test_info_from, test_info_len};
    use crossbeam_channel::unbounded;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_build_meta_reason_mapping() {
        use crate::types::BestmoveSource as S;

        // Timeout sources map to TimeLimit and hard_timeout true only for explicit timeout variants
        let timeout_sources = [
            S::ResignTimeout,
            S::EmergencyFallbackTimeout,
            S::PartialResultTimeout,
        ];
        for &src in &timeout_sources {
            let m = build_meta(src, 7, Some(9), Some("cp 10".into()), None);
            assert_eq!(m.stop_info.reason, TerminationReason::TimeLimit);
            assert!(m.stop_info.hard_timeout);
            assert_eq!(m.stop_info.depth_reached, 7);
        }

        // Normal completion
        for &src in &[
            S::EmergencyFallback,
            S::EmergencyFallbackOnFinish,
            S::SessionInSearchFinished,
        ] {
            let m = build_meta(src, 12, None, None, None);
            assert_eq!(m.stop_info.reason, TerminationReason::Completed);
            assert!(!m.stop_info.hard_timeout);
            assert_eq!(m.stop_info.depth_reached, 12);
        }

        // User stop
        let m = build_meta(S::SessionOnStop, 5, None, None, None);
        assert_eq!(m.stop_info.reason, TerminationReason::UserStop);
        assert!(!m.stop_info.hard_timeout);

        // Error
        for &src in &[S::Resign, S::ResignOnFinish] {
            let m = build_meta(src, 3, Some(4), None, None);
            assert_eq!(m.stop_info.reason, TerminationReason::Error);
            assert!(!m.stop_info.hard_timeout);
            assert_eq!(m.stats.depth, 3);
        }
    }

    #[test]
    fn test_build_meta_keeps_stopinfo_when_provided() {
        use crate::types::BestmoveSource as S;
        let si = StopInfo {
            reason: TerminationReason::Completed,
            elapsed_ms: 123,
            nodes: 456,
            depth_reached: 8,
            hard_timeout: false,
            soft_limit_ms: 111,
            hard_limit_ms: 222,
        };
        let m =
            build_meta(S::SessionInSearchFinished, 1, None, Some("cp 0".into()), Some(si.clone()));
        assert_eq!(m.stop_info.elapsed_ms, 123);
        assert_eq!(m.stop_info.nodes, 456);
        assert_eq!(m.stop_info.soft_limit_ms, 111);
        assert_eq!(m.stop_info.hard_limit_ms, 222);
        assert_eq!(m.stats.depth, 1);
        assert_eq!(m.stats.score, "cp 0");
    }

    /// Verify that normal stop uses pre_session fallback when available and hashes match
    #[test]
    fn test_on_stop_source_pre_session_normal() {
        // Avoid actual stdout writes
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }
        let root_hash = { engine.lock().unwrap().get_position().unwrap().zobrist_hash() };

        // Channels (not used by stop path, but required by types)
        let (tx, rx) = unbounded();

        // Per-search stop flag
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context fields
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 1u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut legal_moves_check_logged = false;
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = Some("7g7f".to_string());
        let mut pre_session_fallback_hash: Option<u64> = Some(root_hash);

        // Clear test hooks
        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
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
            position_state: &mut position_state,
            program_start,
            legal_moves_check_logged: &mut legal_moves_check_logged,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        // Execute stop
        handle_stop_command(&mut ctx).unwrap();

        // Verify bestmove_sent for this search_id exactly once and on_stop_source=pre_session
        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=1\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=pre_session"));
        assert!(found, "on_stop_source=pre_session not found in infos: {:?}", infos);
    }

    /// Verify that when pre_session hash mismatches, normal stop skips it and logs emergency
    #[test]
    fn test_on_stop_source_emergency_when_hash_mismatch() {
        // Avoid actual stdout writes
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Channels (not used by stop path, but required by types)
        let (tx, rx) = unbounded();

        // Per-search stop flag
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context fields
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 2u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut legal_moves_check_logged = false;
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = Some("7g7f".to_string());
        let mut pre_session_fallback_hash: Option<u64> = Some(0); // Intentional mismatch

        // Clear test hooks
        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
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
            allow_null_move: true, // permit null move emergency if needed
            position_state: &mut position_state,
            program_start,
            legal_moves_check_logged: &mut legal_moves_check_logged,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        // Execute stop
        handle_stop_command(&mut ctx).unwrap();

        // Verify bestmove_sent for this search_id exactly once and on_stop_source=emergency
        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=2\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=emergency"));
        assert!(found, "on_stop_source=emergency not found in infos: {:?}", infos);
    }

    /// Verify stop prefers session when committed best exists
    #[test]
    fn test_on_stop_source_session_committed() {
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }
        let root_hash = { engine.lock().unwrap().get_position().unwrap().zobrist_hash() };

        // Build a session with committed best
        let mut session = SearchSession::new(10, root_hash);
        let mv = engine_core::usi::parse_usi_move("7g7f").unwrap();
        session.update_current_best_with_seldepth(12, Some(14), 32, vec![mv]);
        session.commit_iteration();

        // Channels and stop flag
        let (tx, rx) = unbounded();
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 10u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = Some(session);
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut legal_moves_check_logged = false;
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
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
            position_state: &mut position_state,
            program_start,
            legal_moves_check_logged: &mut legal_moves_check_logged,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_stop_command(&mut ctx).unwrap();

        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=10\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=session"));
        assert!(found, "on_stop_source=session not found in infos: {:?}", infos);
    }

    /// Verify stop uses partial result when available and no committed session exists
    #[test]
    fn test_on_stop_source_partial_with_last_result() {
        std::env::set_var("USI_DRY_RUN", "1");

        // Engine and position
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Channels and stop flag
        let (tx, rx) = unbounded();
        let search_stop_flag = Arc::new(AtomicBool::new(false));

        // Context
        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 11u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(search_stop_flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut legal_moves_check_logged = false;
        let mut last_partial_result: Option<(String, u8, i32)> =
            Some(("7g7f".to_string(), 12, 100));
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
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
            position_state: &mut position_state,
            program_start,
            legal_moves_check_logged: &mut legal_moves_check_logged,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_stop_command(&mut ctx).unwrap();

        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=11\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=partial"));
        assert!(found, "on_stop_source=partial not found in infos: {:?}", infos);
    }

    /// Ponder stop should use pre_session if available (hash match)
    #[test]
    fn test_ponder_stop_uses_pre_session() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }
        let root_hash = { engine.lock().unwrap().get_position().unwrap().zobrist_hash() };

        let (tx, rx) = unbounded();
        let flag = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 20u64;
        let mut current_search_is_ponder = true;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut legal_moves_check_logged = false;
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = Some("7g7f".to_string());
        let mut pre_session_fallback_hash: Option<u64> = Some(root_hash);

        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
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
            position_state: &mut position_state,
            program_start,
            legal_moves_check_logged: &mut legal_moves_check_logged,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_stop_command(&mut ctx).unwrap();

        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=20\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=pre_session"));
        assert!(found, "ponder on_stop_source=pre_session not found in infos: {:?}", infos);
    }

    /// Ponder stop with no session/partial/pre_session should use emergency
    #[test]
    fn test_ponder_stop_emergency() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        let (tx, rx) = unbounded();
        let flag = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 21u64;
        let mut current_search_is_ponder = true;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut legal_moves_check_logged = false;
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
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
            position_state: &mut position_state,
            program_start,
            legal_moves_check_logged: &mut legal_moves_check_logged,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_stop_command(&mut ctx).unwrap();

        let infos = test_info_from(start_idx);
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=21\t"))
            .count();
        assert_eq!(sent_count, 1, "expected 1 bestmove_sent: {:?}", infos);
        let found = infos
            .iter()
            .any(|s| s.contains("kind=on_stop_source") && s.contains("src=emergency"));
        assert!(found, "ponder on_stop_source=emergency not found in infos: {:?}", infos);
    }

    /// GameOver should finalize without emitting bestmove
    #[test]
    fn test_gameover_finalizes_without_bestmove() {
        std::env::set_var("USI_DRY_RUN", "1");

        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        let (tx, rx) = unbounded();
        let flag = Arc::new(AtomicBool::new(false));

        let mut worker_handle = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 0u64;
        let mut current_search_id = 30u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter: Option<BestmoveEmitter> =
            Some(BestmoveEmitter::new(current_search_id));
        let mut current_stop_flag: Option<Arc<AtomicBool>> = Some(flag);
        let mut position_state: Option<PositionState> = None;
        let program_start = Instant::now();
        let mut legal_moves_check_logged = false;
        let mut last_partial_result: Option<(String, u8, i32)> = None;
        let mut pre_session_fallback: Option<String> = None;
        let mut pre_session_fallback_hash: Option<u64> = None;

        let start_idx = test_info_len();

        // Invoke GameOver
        let mut ctx = CommandContext {
            engine: &engine,
            stop_flag: &Arc::new(AtomicBool::new(false)),
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
            position_state: &mut position_state,
            program_start,
            legal_moves_check_logged: &mut legal_moves_check_logged,
            last_partial_result: &mut last_partial_result,
            pre_session_fallback: &mut pre_session_fallback,
            pre_session_fallback_hash: &mut pre_session_fallback_hash,
        };

        handle_command(
            UsiCommand::GameOver {
                result: crate::usi::commands::GameResult::Win,
            },
            &mut ctx,
        )
        .unwrap();

        let infos = test_info_from(start_idx);
        // No bestmove_sent for search_id=30
        let sent_count = infos
            .iter()
            .filter(|s| s.contains("kind=bestmove_sent") && s.contains("search_id=30\t"))
            .count();
        assert_eq!(sent_count, 0, "bestmove_sent should NOT be emitted on gameover: {:?}", infos);
        // Ensure search finalized to idle
        assert_eq!(*ctx.search_state, SearchState::Idle);
        assert!(ctx.current_bestmove_emitter.is_none());
    }
}
