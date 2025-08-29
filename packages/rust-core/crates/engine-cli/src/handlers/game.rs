use crate::command_handler::CommandContext;
use crate::worker::lock_or_recover_adapter;
use crate::helpers::wait_for_search_completion;
use crate::state::SearchState;
use crate::usi::commands::GameResult;
use std::sync::atomic::Ordering;

pub(crate) fn handle_gameover(result: GameResult, ctx: &mut CommandContext) -> anyhow::Result<()> {
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
    Ok(())
}

pub(crate) fn handle_usi_new_game(ctx: &mut CommandContext) -> anyhow::Result<()> {
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
    Ok(())
}

