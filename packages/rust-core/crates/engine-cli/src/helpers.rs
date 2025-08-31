use crate::emit_utils::log_tsv;
use crate::engine_adapter::{EngineAdapter, EngineError};
use crate::state::SearchState;
use crate::types::PositionState;
use crate::usi::send_info_string;
use crate::worker::{
    lock_or_recover_adapter, wait_for_worker_sync, wait_for_worker_with_timeout, WorkerMessage,
};
use anyhow::Result;
use crossbeam_channel::Receiver;
use engine_core::usi::position_to_sfen;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

// Constants for timeout and channel management
// Long timeout used only for shutdown paths
pub const MIN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);
// NOTE: GO path now uses full synchronous wait (no timeout). Keep only MIN_JOIN_TIMEOUT for shutdown.

/// Perform fallback move generation with graduated strategy
///
/// This function attempts to generate a move using increasingly simple methods:
/// 1. Use partial result from interrupted search (instant)
/// 2. Run quick shallow search (depth 3, ~10-100ms)
/// 3. Generate emergency move using heuristics only (~1ms)
///
/// All operations are synchronous but designed to be fast.
/// Total worst-case time: ~100ms (dominated by quick_search)
///
/// Returns: (move_string, used_partial_result)
pub fn generate_fallback_move(
    engine: &Arc<Mutex<EngineAdapter>>,
    partial_result: Option<(String, u8, i32)>,
    allow_null_move: bool,
    fast: bool,
) -> Result<(String, bool)> {
    // Stage 1: Use partial result if available (instant)
    if let Some((best_move, depth, score)) = partial_result {
        // Validate the partial result move before using it
        let adapter = lock_or_recover_adapter(engine);

        // First check if it's a well-formed USI move string
        if engine_core::usi::parse_usi_move(&best_move).is_err() {
            log::warn!(
                "Partial result move {best_move} has invalid USI format, proceeding to Stage 2"
            );
        } else if adapter.is_legal_move(&best_move) {
            log::info!(
                "Using validated partial result: move={best_move}, depth={depth}, score={score}"
            );
            return Ok((best_move, true));
        } else {
            // Include position SFEN in warning
            let sfen = adapter
                .get_position()
                .map(position_to_sfen)
                .unwrap_or_else(|| "<no position>".to_string());
            log::warn!("Partial result move {best_move} is illegal in position {sfen}, proceeding to Stage 2");
        }
    }

    if !fast {
        // Stage 2: Try quick shallow search (depth 3, typically 10-50ms, max 100ms)
        log::info!("Attempting quick shallow search");
        let shallow_result = {
            let mut adapter = lock_or_recover_adapter(engine);
            match adapter.quick_search() {
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
            return Ok((move_str, false));
        }
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
            Ok((move_str, false))
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
            Ok(("resign".to_string(), false))
        }
        Err(EngineError::EngineNotAvailable(msg)) if msg.contains("Position not set") => {
            if allow_null_move {
                log::error!("Position not set - returning null move (0000)");
                // Return null move (0000) which most GUIs handle gracefully
                // Note: This is not defined in USI spec but widely supported
                Ok(("0000".to_string(), false))
            } else {
                log::error!("Position not set - returning resign");
                Ok(("resign".to_string(), false))
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
                Ok(("0000".to_string(), false))
            } else {
                // Return resign as per USI spec
                Ok(("resign".to_string(), false))
            }
        }
    }
}

/// Generate an emergency move directly from PositionState without touching the adapter lock.
/// Returns Some(usi_move) if a legal move exists; None if no legal moves (resign upstream).
pub fn emergency_move_from_state(pos_state: &PositionState) -> Option<String> {
    // Try fast snapshot restore first
    if let Ok(pos) =
        engine_core::usi::restore_snapshot_and_verify(&pos_state.sfen_snapshot, pos_state.root_hash)
    {
        return engine_core::util::emergency::emergency_move_usi(&pos);
    }
    // Fallback: parse canonical command and rebuild position
    if let Ok(crate::usi::UsiCommand::Position {
        startpos,
        sfen,
        moves,
    }) = crate::usi::parse_usi_command(&pos_state.cmd_canonical)
    {
        if let Ok((pos_verified, _)) = engine_core::usi::rebuild_then_snapshot_fallback(
            startpos,
            sfen.as_deref(),
            &moves,
            Some(&pos_state.sfen_snapshot),
            pos_state.root_hash,
        ) {
            return engine_core::util::emergency::emergency_move_usi(&pos_verified);
        }
    }
    None
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
    // USI-visible diagnostic begin
    let _ = send_info_string(log_tsv(&[
        ("kind", "wait_for_search_begin"),
        ("state", &format!("{:?}", *search_state)),
    ]));
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

        // Wait synchronously for worker to finish (no timeout)
        log::debug!("Waiting synchronously for worker to finish (go-path)");
        let wait_result = wait_for_worker_sync(worker_handle, worker_rx, search_state, _engine);

        let stop_duration = stop_start.elapsed();
        log::info!("wait_for_search_completion: completed in {stop_duration:?}");
        let _ = send_info_string(log_tsv(&[
            ("kind", "wait_for_search_done"),
            ("elapsed_ms", &stop_duration.as_millis().to_string()),
        ]));

        // Even if wait failed, ensure we're in a clean state
        if let Err(e) = wait_result {
            // Include diagnostic info about current position
            let position_info = {
                let adapter = lock_or_recover_adapter(_engine);
                adapter
                    .get_position()
                    .map(position_to_sfen)
                    .unwrap_or_else(|| "<no position>".to_string())
            };
            log::error!("wait_for_worker_with_timeout failed at position {position_info}: {e}, forcing clean state");
            *search_state = SearchState::Idle;
            // Drain any remaining messages
            let mut drained = 0usize;
            while worker_rx.try_recv().is_ok() {
                drained += 1;
            }
            let _ = send_info_string(log_tsv(&[
                ("kind", "wait_for_search_drained"),
                ("count", &drained.to_string()),
            ]));
        }

        // Always reset stop flag after completion
        stop_flag.store(false, Ordering::Release);
        log::debug!("wait_for_search_completion: reset stop_flag to false after stopping search");
    } else {
        log::debug!("wait_for_search_completion: no active search state");
        // Even if state is Idle, there might still be a worker finalizing (post-finalize join not done).
        if worker_handle.is_some() {
            log::info!("wait_for_search_completion: waiting for post-finalize worker join (sync)");
            // Do NOT touch stop flags here; just wait for clean completion and engine return.
            wait_for_worker_sync(worker_handle, worker_rx, search_state, _engine)?;
        } else {
            // Ensure stop flag is false even if no search was running
            let was_true = stop_flag.load(Ordering::Acquire);
            stop_flag.store(false, Ordering::Release);
            if was_true {
                log::warn!("wait_for_search_completion: stop_flag was true even though no search was running, reset to false");
            }
        }
    }
    Ok(())
}

/// Wait for any ongoing search to complete with a custom timeout
pub fn wait_for_search_completion_with_timeout(
    search_state: &mut SearchState,
    stop_flag: &Arc<AtomicBool>,
    current_stop_flag: Option<&Arc<AtomicBool>>, // Per-search stop flag
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    _engine: &Arc<Mutex<EngineAdapter>>,
    timeout: Duration,
) -> Result<()> {
    let _ = send_info_string(log_tsv(&[
        ("kind", "wait_for_search_begin"),
        ("state", &format!("{:?}", *search_state)),
    ]));
    if search_state.is_searching() {
        log::info!(
            "wait_for_search_completion_with_timeout: stopping ongoing search, state={:?}",
            search_state
        );
        let stop_start = std::time::Instant::now();
        *search_state = SearchState::StopRequested;
        stop_flag.store(true, Ordering::Release);
        if let Some(search_flag) = current_stop_flag {
            search_flag.store(true, Ordering::Release);
            log::debug!(
                "wait_for_search_completion_with_timeout: set per-search stop flag to true"
            );
        }
        log::debug!("Waiting up to {:?} for worker to finish (custom)", timeout);
        let wait_result = wait_for_worker_with_timeout(
            worker_handle,
            worker_rx,
            search_state,
            timeout,
            Some(_engine),
        );
        let stop_duration = stop_start.elapsed();
        log::info!("wait_for_search_completion_with_timeout: completed in {stop_duration:?}");
        let _ = send_info_string(log_tsv(&[
            ("kind", "wait_for_search_done"),
            ("elapsed_ms", &stop_duration.as_millis().to_string()),
        ]));

        if let Err(e) = wait_result {
            let position_info = {
                let adapter = lock_or_recover_adapter(_engine);
                adapter
                    .get_position()
                    .map(position_to_sfen)
                    .unwrap_or_else(|| "<no position>".to_string())
            };
            log::error!("wait_for_worker_with_timeout failed at position {position_info}: {e}, forcing clean state");
            *search_state = SearchState::Idle;
        }
        // Reset stop flag to false after completion
        stop_flag.store(false, Ordering::Release);
        log::debug!("wait_for_search_completion_with_timeout: reset stop_flag to false after stopping search");
    } else {
        let _ =
            send_info_string(log_tsv(&[("kind", "state_idle_after_finalize"), ("search_id", "0")]));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine_adapter::EngineAdapter;
    use std::sync::{Arc, Mutex};

    #[test]
    fn test_fallback_move_invalid_usi_format() {
        // Create a test engine adapter
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));

        // Set up a valid position
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Test with invalid USI format in partial result
        let partial_result = Some(("invalid-move-format".to_string(), 5, 100));

        // Generate fallback move - should skip Stage 1 and use Stage 2 or 3
        let result = generate_fallback_move(&engine, partial_result, false, false);

        // Should not fail, but return a valid move from Stage 2 or 3
        assert!(result.is_ok());
        let (move_str, used_partial) = result.unwrap();

        // Should not have used the partial result since it's invalid
        assert!(!used_partial);

        // The move should not be the invalid format we provided
        assert_ne!(move_str, "invalid-move-format");

        // It should be either a valid move or "resign"
        assert!(move_str == "resign" || engine_core::usi::parse_usi_move(&move_str).is_ok());
    }

    #[test]
    fn test_fallback_move_illegal_but_valid_format() {
        // Create a test engine adapter
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));

        // Set up initial position
        {
            let mut adapter = engine.lock().unwrap();
            adapter.set_position(true, None, &[]).unwrap();
        }

        // Test with a well-formed but illegal move (e.g., moving opponent's piece)
        let partial_result = Some(("1a1b".to_string(), 5, 100)); // Valid format but illegal move

        // Generate fallback move
        let result = generate_fallback_move(&engine, partial_result, false, false);

        // Should not fail
        assert!(result.is_ok());
        let (move_str, used_partial) = result.unwrap();

        // Should not have used the partial result since it's illegal
        assert!(!used_partial);

        // Should not return the illegal move
        assert_ne!(move_str, "1a1b");
    }
}
