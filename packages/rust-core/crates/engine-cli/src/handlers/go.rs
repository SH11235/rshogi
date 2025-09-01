use crate::bestmove_emitter::BestmoveEmitter;
use crate::command_handler::CommandContext;
use crate::emit_utils::log_tsv;
use crate::emit_utils::{
    log_go_received, log_position_restore_fallback, log_position_restore_resign,
    log_position_restore_success, log_position_restore_try,
};
use crate::handlers::common::resign_on_position_restore_fail;
use crate::helpers::{transition_to_idle_if_finalized, wait_for_search_completion};
use crate::state::SearchState;
use crate::usi::{send_info_string, send_response, GoParams, UsiCommand, UsiResponse};
use crate::worker::{lock_or_recover_adapter, search_worker, WorkerMessage};
use anyhow::{anyhow, Result};
use crossbeam_channel::Sender;
use engine_core::movegen::MoveGenerator;
use engine_core::usi::position_to_sfen;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

pub(crate) fn handle_go_command(params: GoParams, ctx: &mut CommandContext) -> Result<()> {
    log::debug!("Received go command with params: {params:?}");
    log::debug!(
        "Global stop flag ptr: {:p}, value: {}",
        ctx.stop_flag.as_ref(),
        ctx.stop_flag.load(std::sync::atomic::Ordering::Acquire)
    );
    let go_received_time = Instant::now();
    log::debug!("NewSearchStart: go received at {go_received_time:?}");
    // Reset per-search final PV injection guard
    *ctx.final_pv_injected = false;
    // Reset hard deadline backstop guard and legal-move snapshot
    *ctx.hard_deadline_taken = false;
    *ctx.root_legal_moves = None;
    // Reset per-search metrics
    *ctx.search_start_time = None;
    *ctx.latest_nodes = 0;
    *ctx.soft_limit_ms_ctx = 0;
    // Clear any pending stop info from previous search
    *ctx.pending_stop_info = None;

    // USI-visible diagnostic: go handler entry
    let now = Instant::now();
    let _ = send_info_string(log_tsv(&[
        ("kind", "go_begin"),
        ("ponder", if params.ponder { "1" } else { "0" }),
    ]));
    // Record accept gate (finalized/idle) for diagnostics
    let gate = if ctx.search_state.is_searching() {
        "searching"
    } else {
        "idle"
    };
    let finalized_flag = ctx
        .current_finalized_flag
        .as_ref()
        .map(|f| f.load(std::sync::atomic::Ordering::Acquire))
        .unwrap_or(false);
    let _ = send_info_string(log_tsv(&[
        ("kind", "cmd_accept_gate"),
        ("gate", gate),
        ("finalized", if finalized_flag { "1" } else { "0" }),
    ]));
    // Track go-begin timestamp for SearchStarted delta measurement
    *ctx.last_go_begin_at = Some(now);

    // Acceptance gate: only accept go when idle or finalized
    if !ctx.search_state.can_start_search() {
        log::warn!(
            "Rejecting go command in state: {:?} (only Idle/Finalized allowed)",
            ctx.search_state
        );
        let _ = send_info_string(log_tsv(&[
            ("kind", "go_rejected"),
            ("state", &format!("{:?}", ctx.search_state)),
            ("reason", "not_idle_or_finalized"),
        ]));
        return Ok(()); // Silently reject - don't send error to GUI
    }

    // Process any pending worker messages FIRST to handle Finished/ReturnEngine
    // This ensures Finalized->Idle transition happens before state checks
    {
        let mut processed_count = 0;
        while let Ok(msg) = ctx.worker_rx.try_recv() {
            processed_count += 1;
            // Handle all messages through the central handler
            if let Err(e) = crate::handle_worker_message(msg, ctx) {
                log::error!("Error handling worker message during pre-go cleanup: {e}");
            }
        }
        if processed_count > 0 {
            log::debug!("Processed {processed_count} pending worker messages before go");
            let _ = send_info_string(log_tsv(&[
                ("kind", "go_pre_pump_messages"),
                ("count", &processed_count.to_string()),
            ]));
        }
    }

    // After processing messages, handle Finalized state if needed
    if *ctx.search_state == SearchState::Finalized {
        log::info!("Go command received in Finalized state, transitioning to Idle");
        // Transition directly to Idle without waiting
        transition_to_idle_if_finalized(ctx.search_state, "go_handler");
        let _ = send_info_string(log_tsv(&[
            ("kind", "go_finalized_to_idle"),
            ("search_id", &ctx.current_search_id.to_string()),
        ]));

        // Do one more quick pump after Idle transition to catch any late ReturnEngine
        while let Ok(msg) = ctx.worker_rx.try_recv() {
            if let Err(e) = crate::handle_worker_message(msg, ctx) {
                log::error!("Error handling worker message after Finalized->Idle transition: {e}");
            }
        }
    }

    // If still searching after message pump, wait for completion
    if ctx.search_state.is_searching() {
        log::info!("Go command received while searching, waiting for completion");
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
            ("kind", "go_wait_for_search_done"),
            ("elapsed_ms", &wait_duration.as_millis().to_string()),
        ]));
    }

    // Check engine availability after processing messages
    {
        let mut adapter = lock_or_recover_adapter(ctx.engine);
        let mut engine_available = adapter.is_engine_available();
        log::debug!("Engine availability after message processing: {engine_available}");
        if !engine_available {
            // Short grace period: the previous worker may be returning the engine via guard drop
            let grace_ms = std::env::var("ENGINE_RECOVERY_GRACE_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(50); // Reduced from 300ms to minimize startup delay
            if grace_ms > 0 {
                let start = std::time::Instant::now();
                drop(adapter); // avoid holding lock while waiting
                let _ = send_info_string(log_tsv(&[
                    ("kind", "engine_recovery_grace_wait"),
                    ("ms", &grace_ms.to_string()),
                ]));
                while start.elapsed().as_millis() as u64 <= grace_ms {
                    // Continue processing messages during grace period
                    if let Ok(msg) = ctx.worker_rx.try_recv() {
                        // Handle ALL messages through the central handler
                        if let Err(e) = crate::handle_worker_message(msg, ctx) {
                            log::error!("Error handling worker message during grace period: {e}");
                        }
                        // Check if engine became available after processing
                        let a = lock_or_recover_adapter(ctx.engine);
                        if a.is_engine_available() {
                            // Engine is now available, exit grace period
                            engine_available = true;
                            drop(a);
                            break;
                        }
                    } else {
                        // No message available, sleep briefly
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                }
                // Re-check engine availability after grace period
                if !engine_available {
                    adapter = lock_or_recover_adapter(ctx.engine);
                    engine_available = adapter.is_engine_available();
                } else {
                    adapter = lock_or_recover_adapter(ctx.engine);
                }
            }

            if !engine_available {
                log::error!("Engine is not available after grace wait; attempting force reset");
                adapter.force_reset_state();
                let _ = send_info_string(log_tsv(&[("kind", "engine_recovery_force_reset")]));
                // Check again after reset
                engine_available = adapter.is_engine_available();
                if engine_available {
                    log::debug!("Engine recovered after force reset");
                } else {
                    log::error!(
                        "Engine still not available after force reset - proceeding with fallback paths"
                    );
                }
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

    // Fast path: if PositionState indicates no legal moves, resign immediately (no worker)
    if let Some(pos_state) = ctx.position_state.as_ref() {
        if let Ok(pos_verified) = engine_core::usi::restore_snapshot_and_verify(
            &pos_state.sfen_snapshot,
            pos_state.root_hash,
        ) {
            let mg = MoveGenerator::new();
            if let Ok(legal) = mg.generate_all(&pos_verified) {
                if legal.as_slice().is_empty() {
                    let _ = send_info_string(crate::emit_utils::log_tsv(&[(
                        "kind",
                        "go_no_legal_moves",
                    )]));
                    let meta = crate::emit_utils::build_meta(
                        crate::types::BestmoveSource::Resign,
                        0,
                        None,
                        None,
                        None,
                    );
                    // Inject final PV for resign to align GUI display
                    let info = crate::usi::output::SearchInfo {
                        multipv: Some(1),
                        pv: vec!["resign".to_string()],
                        ..Default::default()
                    };
                    ctx.inject_final_pv(info, "go_no_legal_moves");
                    // Emit bestmove resign and finalize immediately
                    ctx.emit_and_finalize("resign".to_string(), None, meta, "GoNoLegalMoves")?;
                    return Ok(());
                } else if legal.as_slice().len() == 1 {
                    // Special one-move case: return immediately without search
                    let only_move = &legal.as_slice()[0];
                    let move_str = engine_core::usi::move_to_usi(only_move);

                    let _ = send_info_string(crate::emit_utils::log_tsv(&[
                        ("kind", "go_only_one_move"),
                        ("move", &move_str),
                    ]));

                    let meta = crate::emit_utils::build_meta(
                        crate::types::BestmoveSource::OnlyMove,
                        0,
                        None,
                        None,
                        None,
                    );

                    // Inject final PV for the only move
                    let info = crate::usi::output::SearchInfo {
                        multipv: Some(1),
                        depth: Some(1),
                        pv: vec![move_str.clone()],
                        ..Default::default()
                    };
                    ctx.inject_final_pv(info, "go_only_one_move");

                    // Emit bestmove and finalize immediately
                    ctx.emit_and_finalize(move_str, None, meta, "GoOnlyOneMove")?;
                    return Ok(());
                }
            }
        }
    }

    // Verify position is set and consistent before starting search
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
        } else {
            // Consistency check: adapter position hash must match PositionState when available
            if let Some(pos_state) = ctx.position_state.as_ref() {
                let current_hash = engine.get_position().map(|p| p.zobrist_hash());
                if current_hash != Some(pos_state.root_hash) {
                    log::warn!(
                        "Adapter position hash mismatch (adapter={:?}, state={:#016x}) - rebuilding from PositionState",
                        current_hash,
                        pos_state.root_hash
                    );
                    // Try fast snapshot verify first
                    match engine_core::usi::restore_snapshot_and_verify(
                        &pos_state.sfen_snapshot,
                        pos_state.root_hash,
                    ) {
                        Ok(pos_verified) => {
                            engine.set_raw_position(pos_verified);
                            log_position_restore_success("sfen_snapshot_consistency");
                        }
                        Err(e) => {
                            log::error!("Consistency rebuild via snapshot failed: {e}");
                            // Fall back: parse cmd_canonical and rebuild
                            match crate::usi::parse_usi_command(&pos_state.cmd_canonical) {
                                Ok(UsiCommand::Position {
                                    startpos,
                                    sfen,
                                    moves,
                                }) => {
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
                                                    log_position_restore_success(
                                                        "command_consistency",
                                                    )
                                                }
                                                engine_core::usi::RestoreSource::Snapshot => {
                                                    log_position_restore_success(
                                                        "sfen_snapshot_consistency",
                                                    )
                                                }
                                            }
                                        }
                                        Err(e2) => {
                                            log::error!(
                                                "Consistency rebuild+snapshot failed: {e2}"
                                            );
                                            let _ = send_info_string(log_tsv(&[(
                                                "kind",
                                                "go_position_consistency_failed",
                                            )]));
                                        }
                                    }
                                }
                                Ok(_) | Err(_) => {
                                    let _ = send_info_string(log_tsv(&[(
                                        "kind",
                                        "go_position_consistency_parse_failed",
                                    )]));
                                }
                            }
                        }
                    }
                }
                // Log positive consistency
                let _ = send_info_string(crate::emit_utils::log_tsv(&[
                    ("kind", "go_position_consistency_ok"),
                    ("adapter_hash", &format!("{:?}", current_hash.map(|h| format!("{h:#016x}")))),
                    ("state_hash", &format!("{:#016x}", pos_state.root_hash)),
                ]));
            }
        }
    }

    // Clean up old stop flag before creating new one
    if let Some(old_flag) = ctx.current_stop_flag.take() {
        let old_value = old_flag.load(std::sync::atomic::Ordering::Acquire);
        // Ensure the old flag is reset to false before dropping it
        old_flag.store(false, std::sync::atomic::Ordering::Release);
        log::debug!(
            "Cleaned up old stop flag (was: {}, ptr: {:p}) and reset it to false",
            old_value,
            old_flag.as_ref()
        );
    }

    // Create new per-search stop flag (after all validation passes)
    let search_stop_flag = Arc::new(AtomicBool::new(false));

    // Verify the new flag is actually false
    let initial_value = search_stop_flag.load(std::sync::atomic::Ordering::Acquire);
    if initial_value {
        log::error!("BUG: Newly created stop flag has value true! This should never happen.");
    }

    log::debug!(
        "Created new per-search stop flag (ptr: {:p}, initial_value: {}) for upcoming search",
        search_stop_flag.as_ref(),
        initial_value
    );
    *ctx.current_stop_flag = Some(search_stop_flag.clone());

    // Increment search ID for new search
    *ctx.search_id_counter += 1;
    *ctx.current_search_id = *ctx.search_id_counter;
    let search_id = *ctx.current_search_id;
    log::info!("Starting new search with ID: {search_id}, ponder: {}", params.ponder);
    // Reset final PV guard and log for diagnostics
    *ctx.final_pv_injected = false;
    let _ = send_info_string(log_tsv(&[
        ("kind", "final_pv_guard_reset"),
        ("search_id", &search_id.to_string()),
    ]));

    // Create new BestmoveEmitter and finalized flag for this search
    *ctx.current_bestmove_emitter = Some(BestmoveEmitter::new(search_id));
    *ctx.current_finalized_flag =
        Some(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)));

    // Track if this is a ponder search
    *ctx.current_search_is_ponder = params.ponder;
    // 旧pre_session系は段階撤去。ここでは受領ログのみ。
    log_go_received(params.ponder, None);

    // Clone necessary data for worker thread
    let engine_clone = Arc::clone(ctx.engine);
    let stop_clone = search_stop_flag.clone();
    let global_stop_clone = Arc::clone(ctx.stop_flag);

    // Double-check the stop flag value right before passing to worker
    let pre_spawn_value = stop_clone.load(std::sync::atomic::Ordering::Acquire);
    log::debug!(
        "Stop flag value right before spawning worker: {} (ptr: {:p})",
        pre_spawn_value,
        stop_clone.as_ref()
    );

    let tx_clone: Sender<WorkerMessage> = ctx.worker_tx.clone();
    let finalized_flag = ctx.current_finalized_flag.as_ref().cloned();
    log::debug!("Using per-search stop flag for search_id={search_id}");
    log::debug!("About to spawn worker thread for search_id={search_id}");
    let _ = send_info_string(log_tsv(&[
        ("kind", "go_spawn_worker"),
        ("search_id", &search_id.to_string()),
    ]));

    // Removed: pre-commit tiny quick_search iteration to avoid go-path latency

    // Record last GoParams in adapter (for stochastic ponder restart)
    {
        let mut adapter = lock_or_recover_adapter(ctx.engine);
        adapter.set_last_go_params(&params);
    }

    // Clear global stop flag right before spawning worker
    // This ensures no race condition with quit command
    ctx.stop_flag.store(false, std::sync::atomic::Ordering::Release);
    log::debug!(
        "Cleared global stop flag before spawning worker (ptr: {:p})",
        ctx.stop_flag.as_ref()
    );

    // Spawn worker thread for search with panic safety
    let handle = thread::spawn(move || {
        log::debug!("Worker thread spawned");
        let result = std::panic::catch_unwind(|| {
            search_worker(
                engine_clone,
                params,
                stop_clone,
                global_stop_clone,
                tx_clone.clone(),
                search_id,
                finalized_flag,
                now,
            );
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
