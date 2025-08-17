use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::state::SearchState;
use crate::usi::{self, send_response, UsiResponse};
use crate::worker::{lock_or_recover_adapter, wait_for_worker_with_timeout, WorkerMessage};
use anyhow::Result;
use crossbeam_channel::Receiver;
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
    partial_result: Option<(String, u32, i32)>,
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
            log::warn!("Partial result move {best_move} is illegal, proceeding to Stage 2");
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
                log::warn!("Quick search failed: {e}");
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
            log::error!("No legal moves available - position is checkmate or stalemate");
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
            log::error!("Failed to generate fallback move: {e}");
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

/// Unified function to send bestmove response and update state atomically
pub fn send_bestmove_once(
    best_move: String,
    ponder: Option<String>,
    search_state: &mut SearchState,
    bestmove_sent: &mut bool,
) -> Result<()> {
    // Check if already sent
    if *bestmove_sent {
        log::debug!("Bestmove already sent, ignoring duplicate: {best_move}");
        return Ok(());
    }

    // Log before sending (while we still own the values)
    log::info!("Bestmove sent: {best_move}, ponder: {ponder:?}");

    // Send the response
    send_response(UsiResponse::BestMove { best_move, ponder })?;

    // Update state atomically
    *search_state = SearchState::Idle;
    *bestmove_sent = true;

    Ok(())
}

/// Calculate maximum expected search time from GoParams
pub fn calculate_max_search_time(params: &usi::GoParams) -> Duration {
    if params.infinite {
        // For infinite search, use a large but reasonable timeout
        return Duration::from_secs(3600); // 1 hour
    }

    if let Some(movetime) = params.movetime {
        // Fixed time per move + margin
        return Duration::from_millis(movetime + 1000);
    }

    // For time-based searches, estimate based on available time
    let mut max_time = 0u64;

    if let Some(wtime) = params.wtime {
        max_time = max_time.max(wtime);
    }
    if let Some(btime) = params.btime {
        max_time = max_time.max(btime);
    }
    if let Some(byoyomi) = params.byoyomi {
        // Byoyomi could be used multiple times
        let periods = params.periods.unwrap_or(1) as u64;
        max_time = max_time.max(byoyomi * periods);
    }

    if max_time > 0 {
        // Use half of available time + margin, with upper limit
        // Cap at 10 seconds to avoid excessively long stop waits
        Duration::from_millis((max_time / 2 + 2000).min(10000))
    } else {
        // Default timeout for depth/node limited searches
        Duration::from_secs(10) // Reduced from 60s for better responsiveness
    }
}

/// Wait for any ongoing search to complete
pub fn wait_for_search_completion(
    search_state: &mut SearchState,
    stop_flag: &Arc<AtomicBool>,
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    engine: &Arc<Mutex<EngineAdapter>>,
) -> Result<()> {
    if search_state.is_searching() {
        *search_state = SearchState::StopRequested;
        stop_flag.store(true, Ordering::Release);
        wait_for_worker_with_timeout(
            worker_handle,
            worker_rx,
            engine,
            search_state,
            MIN_JOIN_TIMEOUT,
        )?;
    }
    Ok(())
}
