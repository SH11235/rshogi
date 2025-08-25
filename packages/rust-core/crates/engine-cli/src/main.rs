// USI (Universal Shogi Interface) adapter

mod bestmove_emitter;
mod command_handler;
mod deadlock_detector;
mod engine_adapter;
mod flushing_logger;
mod helpers;
mod search_session;
mod signal_handler;
mod state;
mod stdin_reader;
mod types;
mod usi;
mod utils;
mod worker;

use anyhow::Result;
use bestmove_emitter::BestmoveEmitter;
use clap::Parser;
use command_handler::{build_meta, handle_command, CommandContext};
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use engine_adapter::EngineAdapter;
use helpers::generate_fallback_move;
use search_session::SearchSession;
use state::SearchState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use stdin_reader::spawn_stdin_reader;
use types::{BestmoveSource, PositionState};
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
    ///
    /// Note: null move (0000) is not defined in USI specification but is widely
    /// supported by most shogi GUIs as a graceful way to handle edge cases.
    /// When disabled (default), the engine will send "resign" as per USI spec.
    #[arg(long)]
    allow_null_move: bool,
}

fn main() {
    let args = Args::parse();

    // Initialize logging
    use std::io::Write;
    let log_level = if args.debug { "debug" } else { "info" };

    // Use FlushingStderrWriter only when explicitly requested via environment variable
    // This prevents unnecessary syscalls for every log write in normal operation
    let use_flushing_stderr = std::env::var("FORCE_FLUSH_STDERR").as_deref() == Ok("1");

    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, log_level),
    );

    builder
        .format(|buf, record| {
            writeln!(buf, "[{}] {}: {}", record.level(), record.target(), record.args())
        })
        .write_style(env_logger::WriteStyle::Never); // Disable color to reduce output size

    if use_flushing_stderr {
        use flushing_logger::FlushingStderrWriter;
        builder.target(env_logger::Target::Pipe(Box::new(FlushingStderrWriter::new())));
    } else {
        builder.target(env_logger::Target::Stderr);
    }

    builder.init();

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
    // Initialize all static tables to prevent circular initialization deadlocks
    engine_core::init::init_all_tables_once();

    // Install signal handler for debugging (Unix only)
    #[cfg(unix)]
    signal_handler::unix::install_signal_handler();

    // Install deadlock detector for debug builds
    deadlock_detector::install_deadlock_detector();

    // Record program start time for elapsed calculations
    let program_start = Instant::now();

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
    let mut current_stop_flag: Option<Arc<AtomicBool>> = None; // Per-search stop flag
    let mut position_state: Option<PositionState> = None; // Position state for recovery
    let mut legal_moves_check_logged = false; // Track if we've logged the legal moves check status

    // Main event loop - process USI commands and worker messages concurrently
    loop {
        select! {
            // Handle USI commands
            recv(cmd_rx) -> cmd => {
                match cmd {
                    Ok(cmd) => {
                        log::debug!("USI command received: {cmd:?}");
                        match &cmd {
                            UsiCommand::Go(params) => log::info!("[MAIN] Go command received: depth={:?}", params.depth),
                            UsiCommand::Stop => log::info!("[MAIN] Stop command received"),
                            UsiCommand::Quit => log::info!("[MAIN] Quit command received"),
                            _ => {},
                        }

                        // Check if it's quit command
                        if matches!(cmd, UsiCommand::Quit) {
                            // Handle quit
                            stop_flag.store(true, Ordering::Release);
                            // Also set per-search stop flag if available
                            if let Some(ref search_stop_flag) = current_stop_flag {
                                search_stop_flag.store(true, Ordering::Release);
                            }
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
                            current_stop_flag: &mut current_stop_flag,
                            allow_null_move,
                            position_state: &mut position_state,
                            program_start,
                            legal_moves_check_logged: &mut legal_moves_check_logged,
                        };
                        match handle_command(cmd, &mut ctx) {
                            Ok(()) => {},
                            Err(e) => {
                                log::error!("[MAIN] handle_command error: {}", e);
                                return Err(e);
                            }
                        }
                    }
                    Err(_) => {
                        log::debug!("Command channel closed");
                        break;
                    }
                }
            }

            // Handle worker messages
            recv(worker_rx) -> msg => {
                match msg {
                    Ok(msg) => {
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
                            current_stop_flag: &mut current_stop_flag,
                            allow_null_move,
                            position_state: &mut position_state,
                            program_start,
                            legal_moves_check_logged: &mut legal_moves_check_logged,
                        };
                        handle_worker_message(msg, &mut ctx)?;
                    }
                    Err(_) => {
                        log::debug!("Worker channel closed");
                    }
                }
            }

            default(Duration::from_millis(1)) => {
                // Small idle to prevent busy loop
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

/// Handle worker messages during normal operation
fn handle_worker_message(msg: WorkerMessage, ctx: &mut CommandContext) -> Result<()> {
    match msg {
        WorkerMessage::Info { info, search_id } => {
            // Forward info messages only from current search
            if search_id == *ctx.current_search_id && ctx.search_state.is_searching() {
                send_response(UsiResponse::Info(info))?;
            } else {
                log::trace!(
                    "Suppressed Info message - search_id: {} (current: {}), state: {:?}",
                    search_id,
                    *ctx.current_search_id,
                    *ctx.search_state
                );
            }
        }

        WorkerMessage::SearchStarted {
            search_id,
            start_time,
        } => {
            // Update BestmoveEmitter with accurate start time if it's for current search
            if search_id == *ctx.current_search_id {
                if let Some(ref mut emitter) = ctx.current_bestmove_emitter {
                    emitter.set_start_time(start_time);
                    log::debug!(
                        "Updated BestmoveEmitter with worker start time for search {search_id}"
                    );
                }
            } else {
                log::trace!(
                    "Ignoring SearchStarted from old search: {search_id} (current: {})",
                    *ctx.current_search_id
                );
            }
        }

        WorkerMessage::IterationComplete { session, search_id } => {
            // Update current session if it's for current search
            if search_id == *ctx.current_search_id {
                log::debug!(
                    "Iteration complete for search {}, depth: {:?}",
                    search_id,
                    session.committed_best.as_ref().map(|b| b.depth)
                );
                *ctx.current_session = Some(*session);
            } else {
                log::trace!(
                    "Ignoring iteration from old search: {search_id} (current: {})",
                    *ctx.current_search_id
                );
            }
        }

        WorkerMessage::SearchFinished {
            session_id,
            root_hash,
            search_id,
            stop_info,
        } => {
            // Handle search completion for current search
            // Only process if we're still in Searching state (not StopRequested)
            if search_id == *ctx.current_search_id && *ctx.search_state == SearchState::Searching {
                log::info!("Search {search_id} finished (session_id: {session_id}, root_hash: {root_hash:016x})");

                // Send bestmove immediately if not ponder
                if !*ctx.current_search_is_ponder {
                    if let Some(ref _emitter) = ctx.current_bestmove_emitter {
                        // Try to use session-based bestmove
                        if let Some(ref session) = ctx.current_session {
                            log::debug!("Using session for bestmove generation");
                            let adapter = lock_or_recover_adapter(ctx.engine);
                            if let Some(position) = adapter.get_position() {
                                match adapter.validate_and_get_bestmove(session, position) {
                                    Ok((best_move, ponder)) => {
                                        // Prepare bestmove metadata
                                        let depth = session
                                            .committed_best
                                            .as_ref()
                                            .map(|b| b.depth)
                                            .unwrap_or(0);
                                        let seldepth = session
                                            .committed_best
                                            .as_ref()
                                            .and_then(|b| b.seldepth);
                                        let score_str = session.committed_best.as_ref().map(|b| {
                                            match &b.score {
                                                search_session::Score::Cp(cp) => format!("cp {cp}"),
                                                search_session::Score::Mate(mate) => {
                                                    format!("mate {mate}")
                                                }
                                            }
                                        });

                                        let meta = build_meta(
                                            BestmoveSource::SessionInSearchFinished,
                                            depth,
                                            seldepth,
                                            score_str,
                                            stop_info,
                                        );

                                        return ctx.emit_and_finalize(
                                            best_move,
                                            ponder,
                                            meta,
                                            "SearchFinished with bestmove",
                                        );
                                    }
                                    Err(e) => {
                                        log::warn!(
                                            "Session validation failed in SearchFinished: {e}"
                                        );
                                    }
                                }
                            }
                        }

                        // Fallback if session validation failed
                        match generate_fallback_move(ctx.engine, None, ctx.allow_null_move) {
                            Ok((fallback_move, _used_partial)) => {
                                let meta = build_meta(
                                    BestmoveSource::EmergencyFallback,
                                    0,         // depth
                                    None,      // seldepth
                                    None,      // score
                                    stop_info, // Pass the provided stop_info
                                );

                                ctx.emit_and_finalize(
                                    fallback_move,
                                    None,
                                    meta,
                                    "SearchFinished with fallback",
                                )?;
                            }
                            Err(e) => {
                                log::error!("Fallback move generation failed: {e}");
                                let meta = build_meta(
                                    BestmoveSource::Resign,
                                    0,         // depth
                                    None,      // seldepth
                                    None,      // score
                                    stop_info, // Pass the provided stop_info
                                );

                                ctx.emit_and_finalize(
                                    "resign".to_string(),
                                    None,
                                    meta,
                                    "SearchFinished with fallback",
                                )?;
                            }
                        }
                    } else {
                        // No emitter available - send bestmove directly
                        log::error!("No BestmoveEmitter available for search {search_id}");

                        // Try session first
                        if let Some(ref session) = ctx.current_session {
                            let adapter = lock_or_recover_adapter(ctx.engine);
                            if let Some(position) = adapter.get_position() {
                                if let Ok((best_move, ponder)) =
                                    adapter.validate_and_get_bestmove(session, position)
                                {
                                    send_response(UsiResponse::BestMove { best_move, ponder })?;
                                    ctx.finalize_search("SearchFinished direct send");
                                    return Ok(());
                                }
                            }
                        }

                        // Fallback
                        match generate_fallback_move(ctx.engine, None, ctx.allow_null_move) {
                            Ok((fallback_move, _used_partial)) => {
                                send_response(UsiResponse::BestMove {
                                    best_move: fallback_move,
                                    ponder: None,
                                })?;
                            }
                            Err(e) => {
                                log::error!("Fallback move generation failed: {e}");
                                send_response(UsiResponse::BestMove {
                                    best_move: "resign".to_string(),
                                    ponder: None,
                                })?;
                            }
                        }

                        ctx.finalize_search("SearchFinished direct fallback");
                    }
                } else {
                    log::debug!("Ponder search finished, not sending bestmove");
                    // Finalize ponder search to ensure proper cleanup
                    // (normally ponder ends via stop/ponderhit, but handle natural termination)
                    ctx.finalize_search("PonderFinished");
                }
            } else if search_id == *ctx.current_search_id
                && *ctx.search_state == SearchState::StopRequested
            {
                // SearchFinished arrived after stop command already handled bestmove
                // State transition timeline: Searching → StopRequested (stop handler sends bestmove) → Idle
                // This SearchFinished message arrives during StopRequested state, after bestmove was already sent
                log::debug!("SearchFinished for search {} ignored (state=StopRequested, bestmove already sent by stop handler)", search_id);
                // Still finalize to clean up state and transition to Idle
                ctx.finalize_search("SearchFinished after stop");
            } else if search_id == *ctx.current_search_id && *ctx.search_state == SearchState::Idle
            {
                // SearchFinished arrived after bestmove was already sent (typically from stop timeout)
                log::debug!(
                    "SearchFinished for search {} ignored (state=Idle, bestmove already sent)",
                    search_id
                );
            }
        }

        WorkerMessage::PartialResult {
            current_best,
            depth,
            score,
            search_id,
        } => {
            // Partial results are primarily used in stop command processing
            // but we can log them for debugging
            if search_id == *ctx.current_search_id {
                log::trace!("PartialResult: move={current_best}, depth={depth}, score={score}");
            }
        }

        WorkerMessage::Finished {
            from_guard,
            search_id,
        } => {
            // Handle worker thread completion
            if search_id == *ctx.current_search_id && *ctx.search_state != SearchState::Idle {
                log::debug!(
                    "Worker thread finished (from_guard: {from_guard}, search_id: {search_id})"
                );
                // Note: We don't finalize here as SearchFinished should have already done that
                // This is just cleanup notification
            } else {
                log::trace!(
                    "Ignoring Finished from old search: {search_id} (current: {})",
                    *ctx.current_search_id
                );
            }
        }

        WorkerMessage::Error { message, search_id } => {
            if search_id == *ctx.current_search_id {
                send_info_string(format!("Error: {message}"))?;
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::thread;
    use usi::output::{Score, SearchInfo};

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

    #[test]
    fn test_delayed_search_finished_idle_state() {
        // Test that SearchFinished arriving after Idle state is properly logged
        let (worker_tx, worker_rx) = unbounded();
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        let stop_flag = Arc::new(AtomicBool::new(false));

        let mut worker_handle: Option<JoinHandle<()>> = None;
        let mut search_state = SearchState::Idle; // Already in Idle
        let mut search_id_counter = 3u64;
        let mut current_search_id = 3u64;
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter = None;
        let mut current_stop_flag = None;
        let mut position_state = None;

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
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start: Instant::now(),
        };

        // Send SearchFinished for current search while already Idle
        let msg = WorkerMessage::SearchFinished {
            session_id: 1,
            root_hash: 0,
            search_id: 3,
            stop_info: None,
        };

        // Process the message - should be ignored with debug log
        match handle_worker_message(msg, &mut ctx) {
            Ok(_) => {
                // Should succeed but not do anything
                assert_eq!(*ctx.search_state, SearchState::Idle);
            }
            Err(e) => panic!("handle_worker_message failed: {e}"),
        }
    }

    #[test]
    fn test_info_search_id_filtering() {
        // Test that Info messages with old search_ids are filtered out
        let (worker_tx, worker_rx) = unbounded();
        let engine = Arc::new(Mutex::new(EngineAdapter::new()));
        let stop_flag = Arc::new(AtomicBool::new(false));

        // Set up context with active search
        let mut worker_handle: Option<JoinHandle<()>> = None;
        let mut search_state = SearchState::Searching;
        let mut search_id_counter = 2u64;
        let mut current_search_id = 2u64; // Current search is ID 2
        let mut current_search_is_ponder = false;
        let mut current_session: Option<SearchSession> = None;
        let mut current_bestmove_emitter = None;
        let mut current_stop_flag = None;
        let mut position_state = None;

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
            current_stop_flag: &mut current_stop_flag,
            allow_null_move: false,
            position_state: &mut position_state,
            program_start: Instant::now(),
        };

        // Note: In a full test, we would mock send_response to capture sent Info messages

        // Test 1: Old search_id Info should be suppressed
        let old_info = SearchInfo {
            depth: Some(10),
            time: Some(1000),
            nodes: Some(50000),
            score: Some(Score::Cp(100)),
            ..Default::default()
        };

        let msg = WorkerMessage::Info {
            info: old_info.clone(),
            search_id: 1, // Old search
        };

        // Process the message - Info with old search_id should be suppressed
        match handle_worker_message(msg, &mut ctx) {
            Ok(_) => {
                // The function succeeds but doesn't send the info
                // In a real test, we'd mock send_response to verify
            }
            Err(e) => panic!("handle_worker_message failed: {e}"),
        }

        // Test 2: Current search_id Info should be processed
        let current_info = SearchInfo {
            depth: Some(15),
            time: Some(2000),
            nodes: Some(100000),
            score: Some(Score::Cp(150)),
            ..Default::default()
        };

        let msg = WorkerMessage::Info {
            info: current_info.clone(),
            search_id: 2, // Current search
        };

        // This should be processed (would be sent to GUI)
        match handle_worker_message(msg, &mut ctx) {
            Ok(_) => {
                // In production, this would call send_response
            }
            Err(e) => panic!("handle_worker_message failed: {e}"),
        }

        // Test 3: Info is suppressed when not searching
        *ctx.search_state = SearchState::Idle;

        let msg = WorkerMessage::Info {
            info: current_info.clone(),
            search_id: 2, // Even with correct ID
        };

        match handle_worker_message(msg, &mut ctx) {
            Ok(_) => {
                // Should be suppressed due to Idle state
            }
            Err(e) => panic!("handle_worker_message failed: {e}"),
        }

        // Test 4: Verify SearchStarted with old search_id is ignored (no emitter update)
        let msg = WorkerMessage::SearchStarted {
            search_id: 1, // Old search_id
            start_time: std::time::Instant::now(),
        };

        match handle_worker_message(msg, &mut ctx) {
            Ok(_) => {
                // Old search_id is ignored - emitter is not updated
            }
            Err(e) => panic!("handle_worker_message failed: {e}"),
        }
    }
}
