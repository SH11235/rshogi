use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::state::SearchState;
use crate::worker::{lock_or_recover_adapter, wait_for_worker_with_timeout, WorkerMessage};
use anyhow::Result;
use crossbeam_channel::Receiver;
use engine_core::usi::position_to_sfen;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

// Constants for timeout and channel management
pub const MIN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Perform fallback move generation with graduated strategy
///
/// This function attempts to generate a move using increasingly simple methods:
/// 1. Use partial result from interrupted search (instant)
/// 2. Run quick shallow search (depth 3, ~10-100ms)
/// 3. Generate emergency move using heuristics only (~1ms)
///
/// All operations are synchronous but designed to be fast.
/// Total worst-case time: ~100ms (dominated by quick_search)
pub fn generate_fallback_move(
    engine: &Arc<Mutex<EngineAdapter>>,
    partial_result: Option<(String, u8, i32)>,
    allow_null_move: bool,
) -> Result<String> {
    // Stage 1: Use partial result if available (instant)
    if let Some((best_move, depth, score)) = partial_result {
        // Validate the partial result move before using it
        let adapter = lock_or_recover_adapter(engine);
        if adapter.is_legal_move(&best_move) {
            log::info!(
                "Using validated partial result: move={best_move}, depth={depth}, score={score}"
            );
            return Ok(best_move);
        } else {
            // Include position SFEN in warning
            let sfen = adapter
                .get_position()
                .map(position_to_sfen)
                .unwrap_or_else(|| "<no position>".to_string());
            log::warn!("Partial result move {best_move} is illegal in position {sfen}, proceeding to Stage 2");
        }
    }

    // Stage 2: Try quick shallow search (depth 3, typically 10-50ms, max 100ms)
    log::info!("Attempting quick shallow search");
    let shallow_result = {
        let mut engine = lock_or_recover_adapter(engine);
        match engine.quick_search() {
            Ok(move_str) => {
                log::info!("Quick search successful: {move_str}");
                Some(move_str)
            }
            Err(e) => {
                // Log specific reason for failure
                if e.to_string().contains("Engine not available") {
                    log::info!("Quick search skipped: engine not available (likely held by timed-out worker)");
                } else {
                    log::warn!("Quick search failed: {e}");
                }
                None
            }
        }
    };

    if let Some(move_str) = shallow_result {
        return Ok(move_str);
    }

    // Stage 3: Try emergency move generation (heuristic only, ~1ms)
    log::info!("Attempting emergency move generation");
    let emergency_result = {
        let engine = lock_or_recover_adapter(engine);
        engine.generate_emergency_move()
    };

    match emergency_result {
        Ok(move_str) => {
            log::info!("Generated emergency move: {move_str}");
            Ok(move_str)
        }
        Err(EngineError::NoLegalMoves) => {
            let sfen = {
                let adapter = lock_or_recover_adapter(engine);
                adapter
                    .get_position()
                    .map(position_to_sfen)
                    .unwrap_or_else(|| "<no position>".to_string())
            };
            log::error!(
                "No legal moves available in position {sfen} - position is checkmate or stalemate"
            );
            Ok("resign".to_string())
        }
        Err(EngineError::EngineNotAvailable(msg)) if msg.contains("Position not set") => {
            if allow_null_move {
                log::error!("Position not set - returning null move (0000)");
                // Return null move (0000) which most GUIs handle gracefully
                // Note: This is not defined in USI spec but widely supported
                Ok("0000".to_string())
            } else {
                log::error!("Position not set - returning resign");
                Ok("resign".to_string())
            }
        }
        Err(e) => {
            let sfen = {
                let adapter = lock_or_recover_adapter(engine);
                adapter
                    .get_position()
                    .map(position_to_sfen)
                    .unwrap_or_else(|| "<no position>".to_string())
            };
            log::error!("Failed to generate fallback move in position {sfen}: {e}");
            if allow_null_move {
                // Return null move for better GUI compatibility
                // Note: This is not defined in USI spec but widely supported
                Ok("0000".to_string())
            } else {
                // Return resign as per USI spec
                Ok("resign".to_string())
            }
        }
    }
}

/// Wait for any ongoing search to complete
pub fn wait_for_search_completion(
    search_state: &mut SearchState,
    stop_flag: &Arc<AtomicBool>,
    current_stop_flag: Option<&Arc<AtomicBool>>, // Per-search stop flag
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    _engine: &Arc<Mutex<EngineAdapter>>,
) -> Result<()> {
    if search_state.is_searching() {
        log::info!("wait_for_search_completion: stopping ongoing search, state={:?}", search_state);
        let stop_start = std::time::Instant::now();
        *search_state = SearchState::StopRequested;
        stop_flag.store(true, Ordering::Release);

        // Also set the per-search stop flag if available
        if let Some(search_flag) = current_stop_flag {
            search_flag.store(true, Ordering::Release);
            log::debug!("wait_for_search_completion: set per-search stop flag to true");
        }

        // Wait for worker with timeout
        let wait_result =
            wait_for_worker_with_timeout(worker_handle, worker_rx, search_state, MIN_JOIN_TIMEOUT);

        let stop_duration = stop_start.elapsed();
        log::info!("wait_for_search_completion: completed in {stop_duration:?}");

        // Even if wait failed, ensure we're in a clean state
        if let Err(e) = wait_result {
            log::error!("wait_for_worker_with_timeout failed: {e}, forcing clean state");
            *search_state = SearchState::Idle;
            // Drain any remaining messages
            while worker_rx.try_recv().is_ok() {}
        }

        // Always reset stop flag after completion
        stop_flag.store(false, Ordering::Release);
        log::debug!("wait_for_search_completion: reset stop_flag to false after stopping search");
    } else {
        log::debug!("wait_for_search_completion: no search in progress");
        // Ensure stop flag is false even if no search was running
        let was_true = stop_flag.load(Ordering::Acquire);
        stop_flag.store(false, Ordering::Release);
        if was_true {
            log::warn!("wait_for_search_completion: stop_flag was true even though no search was running, reset to false");
        }
    }
    Ok(())
}
