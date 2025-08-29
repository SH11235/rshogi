use crate::bestmove_emitter::BestmoveEmitter;
use crate::command_handler::CommandContext;
use crate::emit_utils::log_tsv;
use crate::emit_utils::{
    log_go_received, log_position_restore_fallback, log_position_restore_resign,
    log_position_restore_success, log_position_restore_try,
};
use crate::handlers::common::resign_on_position_restore_fail;
use crate::helpers::wait_for_search_completion;
use crate::usi::{send_info_string, send_response, GoParams, UsiResponse};
use crate::worker::{lock_or_recover_adapter, search_worker, WorkerMessage};
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use engine_core::usi::parse_usi_move;
use engine_core::usi::position_to_sfen;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

pub(crate) fn handle_go_command(params: GoParams, ctx: &mut CommandContext) -> Result<()> {
    log::debug!("Received go command with params: {params:?}");
    let go_received_time = Instant::now();
    log::debug!("NewSearchStart: go received at {go_received_time:?}");

    // USI-visible diagnostic: go handler entry
    let now = Instant::now();
    let _ = send_info_string(log_tsv(&[
        ("kind", "go_begin"),
        ("ponder", if params.ponder { "1" } else { "0" }),
    ]));
    // Track go-begin timestamp for SearchStarted delta measurement
    *ctx.last_go_begin_at = Some(now);

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
    let _ = send_info_string(log_tsv(&[
        ("kind", "go_wait_done"),
        ("elapsed_ms", &wait_duration.as_millis().to_string()),
    ]));

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

    // legacy session removed
    *ctx.current_committed = None; // Clear any previous committed iteration
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
                log_position_restore_try(pos_state.move_len, elapsed_ms);

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
                            log_position_restore_fallback("move_len_mismatch");
                            need_fallback = true;
                        } else {
                            // Use core helper to attempt rebuild with snapshot fallback
                            match engine_core::usi::rebuild_then_snapshot_fallback(
                                startpos,
                                sfen.as_deref(),
                                &moves,
                                Some(&pos_state.sfen_snapshot),
                                pos_state.root_hash,
                            ) {
                                Ok((pos_verified, source)) => {
                                    engine.set_raw_position(pos_verified);
                                    match source {
                                        engine_core::usi::RestoreSource::Command => {
                                            log_position_restore_success("command")
                                        }
                                        engine_core::usi::RestoreSource::Snapshot => {
                                            log_position_restore_success("sfen_snapshot")
                                        }
                                    }
                                }
                                Err(e) => {
                                    log::warn!("Rebuild/snapshot failed: {e}");
                                    log_position_restore_fallback("rebuild_and_snapshot_failed");
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
                            match engine_core::usi::restore_snapshot_and_verify(
                                &pos_state.sfen_snapshot,
                                pos_state.root_hash,
                            ) {
                                Ok(pos_verified) => {
                                    engine.set_raw_position(pos_verified);
                                    log_position_restore_success("sfen_snapshot");
                                }
                                Err(e) => {
                                    log::error!("SFEN fallback verify failed: {e}");
                                    log_position_restore_resign(
                                        "sfen_hash_mismatch",
                                        Some(&format!("{:#016x}", pos_state.root_hash)),
                                        Some("unknown"),
                                    );
                                    return resign_on_position_restore_fail(
                                        crate::types::ResignReason::PositionRebuildFailed {
                                            error: "hash verification failed after fallback",
                                        },
                                        "sfen_hash_mismatch",
                                    );
                                }
                            }
                        }
                    }
                    _ => {
                        log::error!("Invalid stored position command: {}", pos_state.cmd_canonical);
                        return resign_on_position_restore_fail(
                            crate::types::ResignReason::InvalidStoredPositionCmd,
                            "invalid_cmd",
                        );
                    }
                }
            } else {
                log::error!("No position set and no recovery state available");
                return resign_on_position_restore_fail(
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
                return resign_on_position_restore_fail(
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

    // Precompute a root fallback move (normalized and verified)
    {
        let mut adapter = lock_or_recover_adapter(ctx.engine);
        let pos_opt = adapter.get_position().cloned();
        if let Some(pos_clone) = pos_opt {
            let mut fallback_usi: Option<String> = None;
            // Optional fast shallow search for a better fallback (USI options driven)
            if adapter.quick_fallback_enabled {
                if let Ok(mut eng) = adapter.take_engine() {
                    if let Some(mv) = engine_core::util::search_helpers::quick_search_move(
                        &mut eng,
                        &pos_clone,
                        adapter.quick_fallback_depth,
                        adapter.quick_fallback_time_ms,
                    ) {
                        fallback_usi = Some(engine_core::usi::move_to_usi(&mv));
                    }
                    adapter.return_engine(eng);
                }
            }
            if fallback_usi.is_none() {
                if let Ok(m) = adapter.generate_emergency_move() {
                    fallback_usi = Some(m);
                }
            }
            if let Some(mstr) = fallback_usi {
                if let Some(norm) =
                    engine_core::util::usi_helpers::normalize_usi_move_str_logged(&pos_clone, &mstr)
                {
                    *ctx.pre_session_fallback = Some(norm.clone());
                    *ctx.pre_session_fallback_hash = Some(pos_clone.zobrist_hash());
                    log_go_received(params.ponder, Some(&norm));
                } else {
                    log_go_received(params.ponder, None);
                }
            } else {
                log_go_received(params.ponder, None);
            }
        } else {
            log_go_received(params.ponder, None);
        }
    }

    // Clone necessary data for worker thread
    let engine_clone = Arc::clone(ctx.engine);
    let stop_clone = search_stop_flag.clone();
    let tx_clone: Sender<WorkerMessage> = ctx.worker_tx.clone();
    log::debug!("Using per-search stop flag for search_id={search_id}");
    log::debug!("About to spawn worker thread for search_id={search_id}");
    let _ = send_info_string(log_tsv(&[
        ("kind", "go_spawn_worker"),
        ("search_id", &search_id.to_string()),
    ]));

    // Phase 1: Pre-commit a tiny iteration result to ensure a sane fallback from normal search
    // This avoids relying on emergency_fallback at the tail end when time is tight.
    {
        let mut adapter = lock_or_recover_adapter(ctx.engine);
        // Run a tiny shallow search (depth from options, time budget small)
        if let Ok(qm) = adapter.quick_search() {
            if let Ok(parsed) = parse_usi_move(&qm) {
                let committed = engine_core::search::CommittedIteration {
                    depth: adapter.quick_fallback_depth.max(1),
                    seldepth: None,
                    score: 0, // score not available from quick_search; use neutral
                    pv: vec![parsed],
                    node_type: engine_core::search::types::NodeType::Exact,
                    nodes: 0,
                    elapsed: std::time::Duration::from_millis(0),
                };
                // Send as if from worker to unify the path
                let _ = ctx.worker_tx.send(WorkerMessage::IterationCommitted {
                    committed,
                    search_id,
                });
                log::debug!(
                    "Pre-committed tiny iteration for search_id={} with move {}",
                    search_id,
                    qm
                );
            }
        }
    }

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
    let _ = send_info_string(log_tsv(&[
        ("kind", "go_spawned"),
        ("search_state", &format!("{:?}", ctx.search_state)),
    ]));

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
