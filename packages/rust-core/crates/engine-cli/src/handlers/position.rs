use crate::command_handler::CommandContext;
use crate::emit_utils::log_tsv;
use crate::usi::send_info_string;
use engine_core::usi::canonicalize_position_cmd;
use crate::worker::lock_or_recover_adapter;
use crate::helpers::wait_for_search_completion;
use engine_core::usi::position_to_sfen;

pub(crate) fn handle_position_command(
    startpos: bool,
    sfen: Option<String>,
    moves: Vec<String>,
    ctx: &mut CommandContext,
) -> anyhow::Result<()> {
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
                let position_state = crate::types::PositionState::new(
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
                    (
                        "sfen_first_20",
                        &sfen_snapshot.chars().take(20).collect::<String>(),
                    ),
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

    Ok(())
}
