// USI (Universal Shogi Interface) adapter

mod bestmove_emitter;
mod command_handler;
mod engine_adapter;
mod helpers;
mod search_session;
mod state;
mod stdin_reader;
mod types;
mod usi;
mod utils;
mod worker;

use anyhow::Result;
use bestmove_emitter::{BestmoveEmitter, BestmoveMeta, BestmoveStats};
use clap::Parser;
use command_handler::{handle_command, CommandContext};
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use engine_adapter::EngineAdapter;
use engine_core::search::types::{StopInfo, TerminationReason};
use helpers::generate_fallback_move;
use search_session::SearchSession;
use state::SearchState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use stdin_reader::spawn_stdin_reader;
use types::BestmoveSource;
use usi::{
    ensure_flush_on_exit, flush_final, send_info_string, send_response, UsiCommand, UsiResponse,
};
use worker::{lock_or_recover_adapter, WorkerMessage};

// Constants for timeout and channel management
const CHANNEL_SIZE: usize = 1024;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,

    /// Use null move (0000) instead of resign for fallback
    /// Note: null move is not defined in USI spec but handled by most GUIs
    #[arg(long)]
    allow_null_move: bool,
}

fn main() {
    let args = Args::parse();

    // Initialize logging
    if args.debug {
        env_logger::init_from_env(
            env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "debug"),
        );
    } else {
        env_logger::init_from_env(
            env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
        );
    }

    // Set up flush on exit hooks
    ensure_flush_on_exit();

    // IMPORTANT: Do not output any log messages to stdout before USI protocol starts
    // ShogiGUI expects only USI protocol messages on stdout
    // log::info!("Shogi USI Engine starting (version 1.0)");

    // Run the main loop and handle any errors
    if let Err(e) = run_engine(args.allow_null_move) {
        log::error!("Fatal error: {e}");
        std::process::exit(1);
    }
}

fn run_engine(allow_null_move: bool) -> Result<()> {
    // Create communication channels
    let (worker_tx, worker_rx): (Sender<WorkerMessage>, Receiver<WorkerMessage>) = unbounded();
    let (cmd_tx, cmd_rx) = bounded::<UsiCommand>(CHANNEL_SIZE);

    // Create engine adapter (thread-safe)
    let engine = Arc::new(Mutex::new(EngineAdapter::new()));

    // Create stop flag for search control
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Spawn stdin reader thread
    let stdin_handle = spawn_stdin_reader(cmd_tx.clone());

    // Store active worker thread handle
    let mut worker_handle: Option<JoinHandle<()>> = None;
    let mut search_state = SearchState::Idle;
    let mut search_id_counter = 0u64;
    let mut current_search_id = 0u64;
    let mut current_search_is_ponder = false; // Track if current search is ponder
    let mut current_session: Option<SearchSession> = None; // Current search session
    let mut current_bestmove_emitter: Option<BestmoveEmitter> = None; // Current search's emitter

    // Main event loop
    let mut should_quit = false;
    loop {
        // First, drain all pending commands to ensure FIFO ordering
        while let Ok(cmd) = cmd_rx.try_recv() {
            log::debug!("USI command received: {cmd:?}");

            // Check if it's quit command
            if matches!(cmd, UsiCommand::Quit) {
                // Handle quit
                stop_flag.store(true, Ordering::Release);
                should_quit = true;
                break;
            }

            // Handle other commands
            let mut ctx = CommandContext {
                engine: &engine,
                stop_flag: &stop_flag,
                worker_tx: &worker_tx,
                worker_rx: &worker_rx,
                worker_handle: &mut worker_handle,
                search_state: &mut search_state,
                search_id_counter: &mut search_id_counter,
                current_search_id: &mut current_search_id,
                current_search_is_ponder: &mut current_search_is_ponder,
                current_session: &mut current_session,
                current_bestmove_emitter: &mut current_bestmove_emitter,
                allow_null_move,
            };
            handle_command(cmd, &mut ctx)?;
        }

        if should_quit {
            break;
        }

        // Then handle worker messages with timeout
        select! {
            recv(worker_rx) -> msg => {
                match msg {
                    Ok(WorkerMessage::Info(info)) => {
                        send_response(UsiResponse::Info(info))?;
                    }
                    Ok(WorkerMessage::SearchStarted { search_id, start_time }) => {
                        // Update BestmoveEmitter with accurate start time
                        if search_id == current_search_id {
                            if let Some(ref mut emitter) = current_bestmove_emitter {
                                *emitter = BestmoveEmitter::with_start_time(search_id, start_time);
                                log::debug!("Updated BestmoveEmitter with worker start time for search {search_id}");
                            }
                        } else {
                            log::trace!("Ignoring SearchStarted from old search: {search_id} (current: {current_search_id})");
                        }
                    }
                    Ok(WorkerMessage::IterationComplete { session, search_id }) => {
                        // Update session if it's for current search
                        if search_id == current_search_id {
                            log::debug!("Iteration complete for search {}, depth: {:?}",
                                search_id,
                                session.committed_best.as_ref().map(|b| b.depth)
                            );
                            current_session = Some(*session);
                        } else {
                            log::trace!("Ignoring iteration from old search: {search_id} (current: {current_search_id})");
                        }
                    }
                    Ok(WorkerMessage::SearchFinished { session_id, root_hash, search_id, stop_info }) => {
                        // Mark search as finished
                        if search_id == current_search_id && search_state.can_accept_bestmove() {
                            log::info!("Search {search_id} finished (session_id: {session_id}, root_hash: {root_hash:016x})");

                            // Send bestmove immediately if not ponder
                            if !current_search_is_ponder {
                                if let Some(ref emitter) = current_bestmove_emitter {
                                    // Try to use session-based bestmove
                                    if let Some(ref session) = current_session {
                                        log::debug!("Using session for bestmove generation");
                                        let adapter = lock_or_recover_adapter(&engine);
                                        if let Some(position) = adapter.get_position() {
                                            match adapter.validate_and_get_bestmove(session, position) {
                                                Ok((best_move, ponder)) => {
                                                    // Prepare bestmove metadata
                                                    let depth = session.committed_best.as_ref().map(|b| b.depth).unwrap_or(0);
                                                    let nodes = stop_info.as_ref().map(|s| s.nodes).unwrap_or(0);
                                                    let elapsed_ms = stop_info.as_ref().map(|s| s.elapsed_ms).unwrap_or(0);
                                                    let nps = if elapsed_ms > 0 { nodes.saturating_mul(1000) / elapsed_ms } else { 0 };

                                                    let score_str = session.committed_best.as_ref()
                                                        .map(|b| match &b.score {
                                                            search_session::Score::Cp(cp) => format!("cp {cp}"),
                                                            search_session::Score::Mate(mate) => format!("mate {mate}"),
                                                        })
                                                        .unwrap_or_else(|| "unknown".to_string());

                                                    // Use stop_info or create default one
                                                    let final_stop_info = stop_info.unwrap_or(StopInfo {
                                                        reason: TerminationReason::Completed,
                                                        elapsed_ms,
                                                        nodes,
                                                        depth_reached: depth,
                                                        hard_timeout: false,
                                                    });

                                                    // Get seldepth from session
                                                    let seldepth = session.committed_best.as_ref().and_then(|b| b.seldepth);

                                                    // Emit bestmove with metadata
                                                    let meta = BestmoveMeta {
                                                        from: BestmoveSource::Session,
                                                        stop_info: final_stop_info,
                                                        stats: BestmoveStats {
                                                            depth,
                                                            seldepth,
                                                            score: score_str,
                                                            nodes,
                                                            nps,
                                                        },
                                                    };

                                                    log::info!("Session bestmove ready: {best_move}, ponder: {ponder:?}");
                                                    emitter.emit(best_move, ponder, meta)?;

                                                    // Update state
                                                    search_state = SearchState::Idle;
                                                                                                        current_search_is_ponder = false;
                                                    current_bestmove_emitter = None;
                                            }
                                                Err(e) => {
                                                    log::warn!("Session validation failed on finish: {e}");
                                                    if let Err(e) = send_info_string("Warning: Bestmove validation failed, using fallback") {
                                                        log::warn!("Failed to send info string: {e}");
                                                    }
                                                    // Try fallback move generation
                                                    match generate_fallback_move(&engine, None, allow_null_move) {
                                                        Ok(fallback_move) => {
                                                            let final_stop_info = stop_info.unwrap_or(StopInfo {
                                                                reason: TerminationReason::Error,
                                                                elapsed_ms: 0,
                                                                nodes: 0,
                                                                depth_reached: 0,
                                                                hard_timeout: false,
                                                            });

                                                            let meta = BestmoveMeta {
                                                                from: BestmoveSource::EmergencyFallback,
                                                                stop_info: final_stop_info,
                                                                stats: BestmoveStats {
                                                                    depth: 0,
                                                                    seldepth: None,
                                                                    score: "unknown".to_string(),
                                                                    nodes: 0,
                                                                    nps: 0,
                                                                },
                                                            };

                                                            log::info!("Fallback move ready: {fallback_move}");
                                                            emitter.emit(fallback_move, None, meta)?;
                                                            search_state = SearchState::Idle;
                                                                                                                        current_search_is_ponder = false;
                                                            current_bestmove_emitter = None;
                                                        }
                                                        Err(e) => {
                                                            log::error!("Fallback move generation failed: {e}");
                                                            let final_stop_info = stop_info.unwrap_or(StopInfo {
                                                                reason: TerminationReason::Error,
                                                                elapsed_ms: 0,
                                                                nodes: 0,
                                                                depth_reached: 0,
                                                                hard_timeout: false,
                                                            });

                                                            let meta = BestmoveMeta {
                                                                from: BestmoveSource::Resign,
                                                                stop_info: final_stop_info,
                                                                stats: BestmoveStats {
                                                                    depth: 0,
                                                                    seldepth: None,
                                                                    score: "unknown".to_string(),
                                                                    nodes: 0,
                                                                    nps: 0,
                                                                },
                                                            };

                                                            emitter.emit("resign".to_string(), None, meta)?;
                                                            search_state = SearchState::Idle;
                                                                                                                        current_search_is_ponder = false;
                                                        }
                                                    }
                                                }
                                        }
                                        } else {
                                            log::error!("No position available for bestmove validation");
                                            let final_stop_info = stop_info.unwrap_or(StopInfo {
                                                reason: TerminationReason::Error,
                                                elapsed_ms: 0,
                                                nodes: 0,
                                                depth_reached: 0,
                                                hard_timeout: false,
                                            });

                                            let meta = BestmoveMeta {
                                                from: BestmoveSource::ResignNoPosition,
                                                stop_info: final_stop_info,
                                                stats: BestmoveStats {
                                                    depth: 0,
                                                    seldepth: None,
                                                    score: "unknown".to_string(),
                                                    nodes: 0,
                                                    nps: 0,
                                                },
                                            };

                                            emitter.emit("resign".to_string(), None, meta)?;
                                            search_state = SearchState::Idle;
                                                                                        current_search_is_ponder = false;
                                            current_bestmove_emitter = None;
                                        }
                                    } else {
                                        log::warn!("No session available on search finish");
                                        // Try emergency move generation
                                        match generate_fallback_move(&engine, None, allow_null_move) {
                                            Ok(fallback_move) => {
                                                let final_stop_info = stop_info.unwrap_or(StopInfo {
                                                    reason: TerminationReason::Error,
                                                    elapsed_ms: 0,
                                                    nodes: 0,
                                                    depth_reached: 0,
                                                    hard_timeout: false,
                                                });

                                                let meta = BestmoveMeta {
                                                    from: BestmoveSource::EmergencyFallbackNoSession,
                                                    stop_info: final_stop_info,
                                                    stats: BestmoveStats {
                                                        depth: 0,
                                                        seldepth: None,
                                                        score: "unknown".to_string(),
                                                        nodes: 0,
                                                        nps: 0,
                                                    },
                                                };

                                                log::info!("Emergency fallback move: {fallback_move}");
                                                emitter.emit(fallback_move, None, meta)?;
                                                search_state = SearchState::Idle;
                                                                                                current_search_is_ponder = false;
                                                current_bestmove_emitter = None;
                                            }
                                            Err(e) => {
                                                log::error!("Emergency fallback move failed: {e}");
                                                let final_stop_info = stop_info.unwrap_or(StopInfo {
                                                    reason: TerminationReason::Error,
                                                    elapsed_ms: 0,
                                                    nodes: 0,
                                                    depth_reached: 0,
                                                    hard_timeout: false,
                                                });

                                                let meta = BestmoveMeta {
                                                    from: BestmoveSource::ResignFallbackFailed,
                                                    stop_info: final_stop_info,
                                                    stats: BestmoveStats {
                                                        depth: 0,
                                                        seldepth: None,
                                                        score: "unknown".to_string(),
                                                        nodes: 0,
                                                        nps: 0,
                                                    },
                                                };

                                                emitter.emit("resign".to_string(), None, meta)?;
                                                search_state = SearchState::Idle;
                                                                                                current_search_is_ponder = false;
                                            }
                                        }
                                    }
                                } else {
                                    log::error!("No BestmoveEmitter available for search {search_id}");
                                }
                            } else {
                                log::debug!("Ponder search finished, not sending bestmove (USI protocol)");
                            }
                        }
                    }
                    // WorkerMessage::BestMove has been completely removed.
                    // All bestmove emissions now go through the session-based approach
                    // (IterationComplete + SearchFinished messages)
                    Ok(WorkerMessage::PartialResult { .. }) => {
                        // Partial results are handled in stop command processing
                        log::trace!("PartialResult received in main loop");
                    }
                    Ok(WorkerMessage::Finished { from_guard, search_id }) => {
                        // Only process the first Finished message for the current search
                        if search_id == current_search_id && search_state != SearchState::Idle {
                            log::debug!("Worker thread finished (from_guard: {from_guard}, search_id: {search_id}, transitioning from {search_state:?} to Idle)");
                            // Transition from Searching/StopRequested/FallbackSent to Idle
                            search_state = SearchState::Idle;
                        } else {
                            log::trace!("Ignoring duplicate or late Finished message (from_guard: {from_guard}, search_id: {search_id}, current_search_id: {current_search_id}, search_state: {search_state:?})");
                            continue;
                        }

                        // Drain any remaining messages including EngineReturn
                        let mut engine_returned = false;
                        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);

                        while std::time::Instant::now() < deadline && !engine_returned {
                            match worker_rx.try_recv() {
                                Ok(WorkerMessage::Info(info)) => {
                                    // During shutdown, ignore send errors
                                    let _ = send_response(UsiResponse::Info(info));
                                }
                                Ok(WorkerMessage::EngineReturn(returned_engine)) => {
                                    log::debug!("Engine returned after Finished");
                                    let mut adapter = lock_or_recover_adapter(&engine);
                                    adapter.return_engine(returned_engine);
                                    engine_returned = true;
                                }
                                Ok(other) => {
                                    log::debug!("Unexpected message after Finished: {:?}",
                                               std::any::type_name_of_val(&other));
                                }
                                Err(_) => {
                                    thread::sleep(std::time::Duration::from_millis(10));
                                }
                            }
                        }

                        if !engine_returned {
                            log::warn!("Engine not returned within timeout after Finished");
                        }
                    }
                    Ok(WorkerMessage::Error { message, search_id }) => {
                        if let Err(e) = send_info_string(format!("Error (search_id: {search_id}): {message}")) {
                            log::warn!("Failed to send info string: {e}");
                        }
                    }
                    Ok(WorkerMessage::EngineReturn(returned_engine)) => {
                        log::debug!("Engine returned from worker");
                        let mut adapter = lock_or_recover_adapter(&engine);
                        adapter.return_engine(returned_engine);
                    }
                    Err(_) => {
                        log::debug!("Worker channel closed");
                    }
                }
            }

            default(Duration::from_millis(5)) => {
                // Check for new commands before idling
                if !cmd_rx.is_empty() {
                    continue; // Process commands first
                }
                // Idle - prevents busy loop
            }
        }
    }

    // Clean shutdown
    log::debug!("Starting shutdown sequence");

    // Stop any ongoing search with timeout
    stop_flag.store(true, Ordering::Release);
    if search_state.is_searching() {
        worker::wait_for_worker_with_timeout(
            &mut worker_handle,
            &worker_rx,
            &engine,
            &mut search_state,
            helpers::MIN_JOIN_TIMEOUT,
        )?;
    }

    // Stop stdin reader thread by closing the channel
    drop(cmd_tx);
    match stdin_handle.join() {
        Ok(()) => log::debug!("Stdin reader thread joined successfully"),
        Err(_) => log::error!("Stdin reader thread panicked"),
    }

    // Ensure all buffered output is flushed before exit
    if let Err(e) = flush_final() {
        log::warn!("Failed to flush final output: {e}");
    }

    log::debug!("Shutdown complete");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::thread;

    #[test]
    fn test_finished_message_multiple_delivery() {
        // Test that the main loop correctly handles multiple Finished messages
        let (tx, rx) = unbounded();

        // Simulate sending multiple Finished messages with same search_id
        let search_id = 1;

        // First Finished from guard
        tx.send(WorkerMessage::Finished {
            from_guard: true,
            search_id,
        })
        .unwrap();

        // Second Finished from worker
        tx.send(WorkerMessage::Finished {
            from_guard: false,
            search_id,
        })
        .unwrap();

        // Process messages
        let mut search_state = SearchState::Searching;
        let current_search_id = 1;
        let mut finished_count = 0;

        while let Ok(msg) = rx.try_recv() {
            if let WorkerMessage::Finished {
                from_guard,
                search_id: msg_id,
            } = msg
            {
                if msg_id == current_search_id && search_state != SearchState::Idle {
                    finished_count += 1;
                    search_state = SearchState::Idle;
                    log::debug!(
                        "Processed Finished message {finished_count} (from_guard: {from_guard})"
                    );
                } else {
                    log::debug!("Ignored duplicate Finished message (from_guard: {from_guard})");
                }
            }
        }

        // Verify only one Finished message was processed
        assert_eq!(finished_count, 1, "Only one Finished message should be processed");
        assert_eq!(search_state, SearchState::Idle, "State should be Idle after processing");
    }

    #[test]
    fn test_finished_message_different_search_ids() {
        // Test handling of Finished messages from different searches
        let (tx, rx) = unbounded();

        // Send Finished from old search
        tx.send(WorkerMessage::Finished {
            from_guard: false,
            search_id: 1,
        })
        .unwrap();

        // Send Finished from current search
        tx.send(WorkerMessage::Finished {
            from_guard: false,
            search_id: 2,
        })
        .unwrap();

        let mut search_state = SearchState::Searching;
        let current_search_id = 2;
        let mut processed_ids = Vec::new();

        while let Ok(msg) = rx.try_recv() {
            if let WorkerMessage::Finished {
                from_guard: _,
                search_id,
            } = msg
            {
                if search_id == current_search_id && search_state != SearchState::Idle {
                    search_state = SearchState::Idle;
                    processed_ids.push(search_id);
                }
            }
        }

        // Verify only current search's Finished was processed
        assert_eq!(processed_ids, vec![2], "Only current search should be processed");
    }

    #[test]
    fn test_worker_message_channel_behavior() {
        // Property test: channel should handle rapid message delivery
        let (tx, rx) = unbounded();
        let tx_clone = tx.clone();

        // Spawn thread to send messages rapidly
        let sender = thread::spawn(move || {
            for i in 0..100 {
                let search_id = (i % 3) as u64; // Simulate 3 different searches

                // Send various message types
                if i % 10 == 0 {
                    tx_clone
                        .send(WorkerMessage::Finished {
                            from_guard: i % 2 == 0,
                            search_id,
                        })
                        .unwrap();
                }

                // WorkerMessage::BestMove has been removed - using SearchFinished instead
                if i % 7 == 0 {
                    tx_clone
                        .send(WorkerMessage::SearchFinished {
                            session_id: search_id,
                            root_hash: 0,
                            search_id,
                            stop_info: None,
                        })
                        .unwrap();
                }
            }
        });

        // Process messages with state tracking
        let mut finished_per_search = [0; 3];
        let mut search_finished_per_search = [0; 3];

        sender.join().unwrap();

        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMessage::Finished {
                    from_guard: _,
                    search_id,
                } => {
                    finished_per_search[search_id as usize] += 1;
                }
                WorkerMessage::SearchFinished { search_id, .. } => {
                    search_finished_per_search[search_id as usize] += 1;
                }
                _ => {}
            }
        }

        // Verify all messages were received
        let total_finished: i32 = finished_per_search.iter().sum();
        let total_search_finished: i32 = search_finished_per_search.iter().sum();

        assert!(total_finished > 0, "Should have received Finished messages");
        assert!(total_search_finished > 0, "Should have received SearchFinished messages");

        // Each search should have received messages
        for (i, &count) in finished_per_search.iter().enumerate() {
            assert!(count > 0, "Search {i} should have Finished messages");
        }
    }
}
