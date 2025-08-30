use crate::command_handler::CommandContext;
use crate::handlers::go::handle_go_command;
// use crate::usi::GoParams;
use crate::state::SearchState;
use crate::usi::send_info_string;
use crate::worker::lock_or_recover_adapter;

pub(crate) fn handle_ponder_hit(ctx: &mut CommandContext) -> anyhow::Result<()> {
    // Handle ponder hit only if we're actively pondering
    if *ctx.current_search_is_ponder && *ctx.search_state == SearchState::Searching {
        let stochastic = {
            let engine = lock_or_recover_adapter(ctx.engine);
            engine.is_stochastic_ponder()
        };
        if stochastic {
            // Stochastic: stop current ponder search without emitting bestmove, then restart normal go
            let _ = send_info_string("stochastic_ponder: stopping ponder for restart".to_string());
            // Keep ponder flag true to suppress bestmove on SearchFinished
            // Signal stop to worker and wait for completion (short timeout)
            // Use a slightly longer timeout to allow Ponder to unwind safely (e.g., 1200ms)
            if let Err(e) = crate::helpers::wait_for_search_completion_with_timeout(
                ctx.search_state,
                ctx.stop_flag,
                ctx.current_stop_flag.as_ref(),
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
                std::time::Duration::from_millis(1200),
            ) {
                log::warn!("stochastic ponder: wait_for_search_completion failed: {e}");
            }

            // Prepare new GoParams from last stored, switch ponder=false
            let last = {
                let engine = lock_or_recover_adapter(ctx.engine);
                engine.get_last_go_params()
            };
            if let Some(mut last) = last {
                last.ponder = false;
                // Mark no longer ponder before starting
                *ctx.current_search_is_ponder = false;
                if let Err(e) = handle_go_command(last, ctx) {
                    log::error!("stochastic ponder: failed to restart go: {e}");
                } else {
                    let _ = send_info_string(
                        "stochastic_ponder: restarted normal search after ponderhit".to_string(),
                    );
                }
            } else {
                log::warn!(
                    "stochastic ponder: no last GoParams available; falling back to convert"
                );
                // Fallback: convert in-place
                let mut adapter = lock_or_recover_adapter(ctx.engine);
                match adapter.ponder_hit() {
                    Ok(()) => {
                        *ctx.current_search_is_ponder = false;
                        let _ = send_info_string(
                            "ponder_hit: converted to normal search (fallback)".to_string(),
                        );
                    }
                    Err(e) => log::debug!("Ponder hit ignored: {e}"),
                }
            }
        } else {
            // Non-stochastic: convert in-place
            let mut adapter = lock_or_recover_adapter(ctx.engine);
            match adapter.ponder_hit() {
                Ok(()) => {
                    *ctx.current_search_is_ponder = false;
                    let _ = send_info_string(
                        "ponder_hit: converted to normal search (time budgets updated)".to_string(),
                    );
                }
                Err(e) => log::debug!("Ponder hit ignored: {e}"),
            }
        }
    } else {
        log::debug!(
            "Ponder hit ignored (state={:?}, is_ponder={})",
            *ctx.search_state,
            *ctx.current_search_is_ponder
        );
    }
    Ok(())
}
