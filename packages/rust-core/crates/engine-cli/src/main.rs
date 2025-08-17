// USI (Universal Shogi Interface) adapter

mod command_handler;
mod engine_adapter;
mod helpers;
mod search_session;
mod state;
mod stdin_reader;
mod usi;
mod utils;
mod worker;

use anyhow::Result;
use clap::Parser;
use command_handler::{handle_command, CommandContext};
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use engine_adapter::EngineAdapter;
use engine_core::usi::move_to_usi;
use helpers::{generate_fallback_move, send_bestmove_once};
use search_session::SearchSession;
use state::SearchState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use stdin_reader::spawn_stdin_reader;
use usi::output::SearchInfo;
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
    let mut bestmove_sent = false; // Track if bestmove has been sent for current search
    let mut current_search_timeout = helpers::MIN_JOIN_TIMEOUT;
    let mut search_id_counter = 0u64;
    let mut current_search_id = 0u64;
    let mut current_search_is_ponder = false; // Track if current search is ponder
    let mut current_session: Option<SearchSession> = None; // Current search session

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
                bestmove_sent: &mut bestmove_sent,
                current_search_timeout: &mut current_search_timeout,
                search_id_counter: &mut search_id_counter,
                current_search_id: &mut current_search_id,
                current_search_is_ponder: &mut current_search_is_ponder,
                current_session: &mut current_session,
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
                    Ok(WorkerMessage::SearchFinished { session_id, root_hash, search_id }) => {
                        // Mark search as finished
                        if search_id == current_search_id && search_state.can_accept_bestmove() {
                            log::info!("Search {search_id} finished (session_id: {session_id}, root_hash: {root_hash:016x})");

                            // Send bestmove immediately if not ponder
                            if !current_search_is_ponder {
                                // Try to use session-based bestmove
                                if let Some(ref session) = current_session {
                                    log::debug!("Using session for bestmove generation");
                                    let adapter = lock_or_recover_adapter(&engine);
                                    if let Some(position) = adapter.get_position() {
                                        match adapter.validate_and_get_bestmove(session, position) {
                                            Ok((best_move, ponder)) => {
                                                // Send info string about bestmove source
                                                let depth = session.committed_best.as_ref().map(|b| b.depth).unwrap_or(0);
                                                let score_str = session.committed_best.as_ref()
                                                    .map(|b| match &b.score {
                                                        search_session::Score::Cp(cp) => format!("cp {cp}"),
                                                        search_session::Score::Mate(mate) => format!("mate {mate}"),
                                                    })
                                                    .unwrap_or_else(|| "unknown".to_string());
                                                send_info_string(format!("bestmove_from=session depth={depth} score={score_str}"))?;

                                                // Also output a final PV info line to ensure consistency with bestmove
                                                if let Some(committed) = session.committed_best.as_ref() {
                                                    let pv_usi: Vec<String> = committed.pv.iter().map(move_to_usi).collect();
                                                    let info = SearchInfo {
                                                        depth: Some(committed.depth),
                                                        pv: pv_usi,
                                                        ..Default::default()
                                                    };
                                                    let _ = send_response(UsiResponse::Info(info));
                                                }

                                                log::info!("Sending bestmove on search finish: {best_move}, ponder: {ponder:?}");
                                                send_bestmove_once(best_move, ponder, &mut search_state, &mut bestmove_sent)?;
                                            }
                                            Err(e) => {
                                                log::warn!("Session validation failed on finish: {e}");
                                                send_info_string("Warning: Bestmove validation failed, using fallback")?;
                                                // Try fallback move generation
                                                match generate_fallback_move(&engine, None, allow_null_move) {
                                                    Ok(fallback_move) => {
                                                        send_info_string("bestmove_from=emergency_fallback")?;
                                                        log::info!("Sending fallback move on search finish: {fallback_move}");
                                                        send_bestmove_once(fallback_move, None, &mut search_state, &mut bestmove_sent)?;
                                                    }
                                                    Err(e) => {
                                                        log::error!("Fallback move generation failed: {e}");
                                                        send_bestmove_once("resign".to_string(), None, &mut search_state, &mut bestmove_sent)?;
                                                    }
                                                }
                                            }
                                        }
                                    } else {
                                        log::error!("No position available for bestmove validation");
                                        send_bestmove_once("resign".to_string(), None, &mut search_state, &mut bestmove_sent)?;
                                    }
                                } else {
                                    log::warn!("No session available on search finish");
                                    // Try emergency move generation
                                    match generate_fallback_move(&engine, None, allow_null_move) {
                                        Ok(fallback_move) => {
                                            send_info_string("bestmove_from=emergency_fallback_no_session")?;
                                            send_bestmove_once(fallback_move, None, &mut search_state, &mut bestmove_sent)?;
                                        }
                                        Err(_) => {
                                            send_bestmove_once("resign".to_string(), None, &mut search_state, &mut bestmove_sent)?;
                                        }
                                    }
                                }
                            } else {
                                log::debug!("Ponder search finished, not sending bestmove (USI protocol)");
                            }
                        }
                    }
                    Ok(WorkerMessage::BestMove { best_move, ponder_move, search_id }) => {
                        // Only send bestmove if:
                        // 1. We're still searching AND haven't sent one yet
                        // 2. The search_id matches current search (prevents old search results)
                        // 3. NOT a pure ponder search (USI protocol: no bestmove during ponder)
                        if search_state.can_accept_bestmove() && !bestmove_sent && search_id == current_search_id && !current_search_is_ponder {
                            // Log position state for debugging (debug level)
                            if log::log_enabled!(log::Level::Debug) {
                                let adapter = lock_or_recover_adapter(&engine);
                                adapter.log_position_state("BestMove validation");
                            }

                            // Validate bestmove before sending
                            let is_valid = {
                                let adapter = lock_or_recover_adapter(&engine);
                                adapter.is_legal_move(&best_move)
                            };

                            if is_valid {
                                log::info!("Sending validated bestmove: {best_move}");
                                send_bestmove_once(best_move, ponder_move, &mut search_state, &mut bestmove_sent)?;
                            } else {
                                // Log detailed error information
                                log::error!("Invalid bestmove detected: {best_move}");
                                let adapter = lock_or_recover_adapter(&engine);
                                adapter.log_position_state("Invalid bestmove context");

                                // Try to generate a fallback move
                                log::warn!("Attempting to generate fallback move after invalid bestmove");
                                match generate_fallback_move(&engine, None, allow_null_move) {
                                    Ok(fallback_move) => {
                                        log::info!("Sending fallback move: {fallback_move}");
                                        send_bestmove_once(fallback_move, None, &mut search_state, &mut bestmove_sent)?;
                                    }
                                    Err(e) => {
                                        log::error!("Failed to generate fallback move: {e}");
                                        // As last resort, send resign
                                        send_bestmove_once("resign".to_string(), None, &mut search_state, &mut bestmove_sent)?;
                                    }
                                }
                            }
                            current_search_is_ponder = false; // Reset ponder flag
                        } else {
                            log::warn!("Ignoring late/ponder bestmove: {best_move} (search_state={search_state:?}, bestmove_sent={bestmove_sent}, search_id={search_id}, current={current_search_id}, is_ponder={current_search_is_ponder})");
                        }
                    }
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
                        send_info_string(format!("Error (search_id: {search_id}): {message}"))?;
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

                if i % 7 == 0 {
                    tx_clone
                        .send(WorkerMessage::BestMove {
                            best_move: format!("7g7f_{i}"),
                            ponder_move: None,
                            search_id,
                        })
                        .unwrap();
                }
            }
        });

        // Process messages with state tracking
        let mut finished_per_search = [0; 3];
        let mut bestmoves_per_search = [0; 3];

        sender.join().unwrap();

        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMessage::Finished {
                    from_guard: _,
                    search_id,
                } => {
                    finished_per_search[search_id as usize] += 1;
                }
                WorkerMessage::BestMove { search_id, .. } => {
                    bestmoves_per_search[search_id as usize] += 1;
                }
                _ => {}
            }
        }

        // Verify all messages were received
        let total_finished: i32 = finished_per_search.iter().sum();
        let total_bestmoves: i32 = bestmoves_per_search.iter().sum();

        assert!(total_finished > 0, "Should have received Finished messages");
        assert!(total_bestmoves > 0, "Should have received BestMove messages");

        // Each search should have received messages
        for (i, &count) in finished_per_search.iter().enumerate() {
            assert!(count > 0, "Search {i} should have Finished messages");
        }
    }
}
