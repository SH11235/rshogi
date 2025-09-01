use crate::command_handler::CommandContext;
use crate::emit_utils::log_position_store;
use crate::emit_utils::log_tsv;
use crate::helpers::wait_for_search_completion;
use crate::usi::send_info_string;
use engine_core::usi::canonicalize_position_cmd;
use engine_core::usi::{position_to_sfen, rebuild_then_snapshot_fallback};
use std::time::Instant;

pub(crate) fn handle_position_command(
    startpos: bool,
    sfen: Option<String>,
    moves: Vec<String>,
    ctx: &mut CommandContext,
) -> anyhow::Result<()> {
    log::debug!(
        "Handling position command - startpos: {startpos}, sfen: {sfen:?}, moves: {moves:?}"
    );

    // USI-visible diagnostic: position handler entry
    let _ = send_info_string(log_tsv(&[
        ("kind", "position_begin"),
        ("startpos", if startpos { "1" } else { "0" }),
        ("moves", &moves.len().to_string()),
    ]));

    // Build the canonical position command string
    let cmd_canonical = canonicalize_position_cmd(startpos, sfen.as_deref(), &moves);

    // Wait for any ongoing search to complete before updating position
    // IMPORTANT: Skip waiting if state is Finalized - the go handler will handle it
    let wait_ms = if ctx.search_state.is_searching() {
        let wait_start = Instant::now();
        wait_for_search_completion(
            ctx.search_state,
            ctx.stop_flag,
            ctx.current_stop_flag.as_ref(),
            ctx.worker_handle,
            ctx.worker_rx,
            ctx.engine,
        )?;
        wait_start.elapsed().as_millis()
    } else {
        // For Finalized or Idle state, don't wait - let go handler deal with it
        log::debug!(
            "Skipping wait_for_search_completion in position handler - state: {:?}",
            ctx.search_state
        );
        let _ = send_info_string(log_tsv(&[
            ("kind", "position_wait_skipped"),
            ("state", &format!("{:?}", ctx.search_state)),
        ]));
        0
    };
    let _ = send_info_string(log_tsv(&[
        ("kind", "position_wait_done"),
        ("elapsed_ms", &wait_ms.to_string()),
    ]));

    // Clean up any remaining search state only if a search was actually running
    if *ctx.current_search_id > 0 {
        ctx.finalize_search("Position");
    }

    // Clear pre-session fallback as position has changed
    *ctx.pre_session_fallback = None;
    *ctx.pre_session_fallback_hash = None;

    // Position store fast path with non-blocking lock attempt
    let store_start = Instant::now();
    let _ = send_info_string(log_tsv(&[("kind", "position_store_begin")]));
    // Try to acquire adapter lock without blocking; if busy, defer store using pure-core rebuild
    match ctx.engine.try_lock() {
        Ok(mut engine) => {
            // Got the lock quickly – proceed with normal set_position
            match engine.set_position(startpos, sfen.as_deref(), &moves) {
                Ok(()) => {
                    if let Some(pos) = engine.get_position() {
                        let sfen_snapshot = position_to_sfen(pos);
                        let root_hash = pos.zobrist_hash();
                        let move_len = moves.len();

                        let position_state = crate::types::PositionState::new(
                            cmd_canonical.clone(),
                            root_hash,
                            move_len,
                            sfen_snapshot.clone(),
                        );
                        *ctx.position_state = Some(position_state);

                        let stored_ms = ctx.program_start.elapsed().as_millis();
                        log_position_store(root_hash, move_len, &sfen_snapshot, stored_ms);
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "position_store_end"),
                            ("elapsed_ms", &store_start.elapsed().as_millis().to_string()),
                        ]));
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "position_end"),
                            ("move_len", &move_len.to_string()),
                        ]));
                    } else {
                        log::error!("Position set but unable to retrieve for state storage");
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "position_store_error"),
                            ("reason", "get_position_none"),
                        ]));
                    }
                }
                Err(e) => {
                    log::error!("Failed to set position: {e:?}");
                    send_info_string(format!("Error: Failed to set position - {e:?}"))?;
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "position_store_error"),
                        ("reason", "set_position_failed"),
                    ]));
                }
            }
        }
        Err(_) => {
            // Adapter lock busy – compute PositionState without touching adapter, then defer actual engine update
            let _ = send_info_string(log_tsv(&[
                ("kind", "position_store_deferred"),
                ("reason", "adapter_lock_busy"),
            ]));
            match rebuild_then_snapshot_fallback(startpos, sfen.as_deref(), &moves, None, 0) {
                Ok((pos_verified, _src)) => {
                    let sfen_snapshot = position_to_sfen(&pos_verified);
                    let root_hash = pos_verified.zobrist_hash();
                    let move_len = moves.len();
                    let position_state = crate::types::PositionState::new(
                        cmd_canonical.clone(),
                        root_hash,
                        move_len,
                        sfen_snapshot.clone(),
                    );
                    *ctx.position_state = Some(position_state);
                    let stored_ms = ctx.program_start.elapsed().as_millis();
                    log_position_store(root_hash, move_len, &sfen_snapshot, stored_ms);
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "position_store_end"),
                        ("elapsed_ms", &store_start.elapsed().as_millis().to_string()),
                    ]));
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "position_end"),
                        ("move_len", &move_len.to_string()),
                    ]));
                }
                Err(e) => {
                    log::error!("Deferred position rebuild failed: {e}");
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "position_store_error"),
                        ("reason", "deferred_rebuild_failed"),
                    ]));
                    // Keep previous position_state; do not block main loop
                }
            }
        }
    }

    Ok(())
}
