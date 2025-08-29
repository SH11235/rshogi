use crate::bestmove_emitter::BestmoveEmitter;
use crate::command_handler::CommandContext;
use crate::emit_utils::log_tsv;
use crate::helpers::wait_for_search_completion;
use crate::usi::{send_info_string, send_response, GoParams, UsiResponse};
use crate::worker::{lock_or_recover_adapter, search_worker, WorkerMessage};
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use engine_core::usi::position_to_sfen;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

pub(crate) fn handle_go_command(params: GoParams, ctx: &mut CommandContext) -> Result<()> {
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
                    Ok(crate::usi::UsiCommand::Position {
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
                                            return super::super::command_handler::fail_position_restore(
                                                crate::types::ResignReason::PositionRebuildFailed {
                                                    error: "hash verification failed after fallback",
                                                },
                                                "sfen_hash_mismatch",
                                            );
                                        }
                                    } else {
                                        log::error!("Failed to get position after sfen_snapshot restoration");
                                        return super::super::command_handler::fail_position_restore(
                                            crate::types::ResignReason::PositionRebuildFailed {
                                                error: "no position after sfen restoration",
                                            },
                                            "no_position_after_sfen",
                                        );
                                    }
                                }
                                Err(e) => {
                                    log::error!("Failed to set position from sfen_snapshot: {}", e);
                                    return super::super::command_handler::fail_position_restore(
                                        crate::types::ResignReason::PositionRebuildFailed {
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
                        return super::super::command_handler::fail_position_restore(
                            crate::types::ResignReason::InvalidStoredPositionCmd,
                            "invalid_cmd",
                        );
                    }
                }
            } else {
                log::error!("No position set and no recovery state available");
                return super::super::command_handler::fail_position_restore(
                    crate::types::ResignReason::NoPositionSet,
                    "no_position_set",
                );
            }
        }

        // NOTE: has_legal_moves check disabled as in original
        let skip_legal_moves_check = std::env::var("SKIP_LEGAL_MOVES").as_deref() != Ok("0");
        if !skip_legal_moves_check {
            let use_any_legal = std::env::var("USE_ANY_LEGAL").as_deref() == Ok("1");
            let check_start = Instant::now();
            let has_legal_moves = if use_any_legal {
                engine.has_any_legal_move()?
            } else {
                engine.has_legal_moves()?
            };
            let check_duration = check_start.elapsed();
            if check_duration > std::time::Duration::from_millis(5) {
                log::warn!("Legal moves check took {:?}", check_duration);
            }
            if !has_legal_moves {
                return super::super::command_handler::fail_position_restore(
                    crate::types::ResignReason::Checkmate,
                    "no_legal_moves",
                );
            }
        }
    }

    // Clean up old stop flag before creating new one
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

    // Precompute a root fallback move
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
        }
    }

    // Clone necessary data for worker thread
    let engine_clone = Arc::clone(ctx.engine);
    let stop_clone = search_stop_flag.clone();
    let tx_clone: Sender<WorkerMessage> = ctx.worker_tx.clone();
    log::debug!("Using per-search stop flag for search_id={search_id}");
    log::debug!("About to spawn worker thread for search_id={search_id}");

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

    // Send immediate info depth 1 to confirm search started
    send_response(UsiResponse::Info(crate::usi::output::SearchInfo {
        depth: Some(1),
        time: Some(0),
        nodes: Some(0),
        string: Some("search starting".to_string()),
        ..Default::default()
    }))?;
    log::debug!("Sent initial info depth 1 heartbeat to GUI");

    Ok(())
}
