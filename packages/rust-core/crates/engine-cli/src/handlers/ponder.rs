use crate::command_handler::CommandContext;
use crate::state::SearchState;
use crate::usi::send_info_string;
use crate::worker::lock_or_recover_adapter;

pub(crate) fn handle_ponder_hit(ctx: &mut CommandContext) -> anyhow::Result<()> {
    // Handle ponder hit only if we're actively pondering
    if *ctx.current_search_is_ponder && *ctx.search_state == SearchState::Searching {
        let mut engine = lock_or_recover_adapter(ctx.engine);
        // Mark that we're no longer in pure ponder mode
        *ctx.current_search_is_ponder = false;
        match engine.ponder_hit() {
            Ok(()) => {
                log::debug!("Ponder hit successfully processed");
                // Emit USI-visible info for diagnostics (core also logs to stderr)
                let _ = send_info_string(
                    "ponder_hit: converted to normal search (time budgets updated)".to_string(),
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
    Ok(())
}

