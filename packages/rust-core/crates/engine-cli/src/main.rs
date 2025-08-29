// USI (Universal Shogi Interface) adapter

mod bestmove_emitter;
mod command_handler;
mod emit_utils;
mod engine_adapter;
mod flushing_logger;
mod handlers;
mod helpers;
// mod search_session; // removed after migrating to core committed iterations
mod state;
mod stdin_reader;
mod types;
mod usi;
mod utils;
mod worker;

use crate::emit_utils::build_meta;
// use crate::usi::output::Score; // not needed after session removal
use anyhow::Result;
use bestmove_emitter::BestmoveEmitter;
use clap::Parser;
use command_handler::{handle_command, CommandContext};
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use engine_adapter::EngineAdapter;
use engine_core::search::CommittedIteration;
use helpers::generate_fallback_move;
// use search_session::SearchSession;
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
                                              // let mut current_session: Option<SearchSession> = None; // legacy session (removed)
    let mut current_committed: Option<CommittedIteration> = None; // Latest committed iteration
    let mut current_bestmove_emitter: Option<BestmoveEmitter> = None; // Current search's emitter
    let mut current_stop_flag: Option<Arc<AtomicBool>> = None; // Per-search stop flag
    let mut position_state: Option<PositionState> = None; // Position state for recovery
    let mut last_partial_result: Option<(String, u8, i32)> = None; // Cache latest partial result
    let mut pre_session_fallback: Option<String> = None; // Precomputed fallback move at go-time
    let mut pre_session_fallback_hash: Option<u64> = None; // Hash when pre_session_fallback was computed

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
                            // Terminate emitter first to prevent any bestmove output
                            if let Some(ref emitter) = current_bestmove_emitter {
                                emitter.terminate();
                                log::debug!("Terminated bestmove emitter for quit");
                            }
                            // Handle quit
                            stop_flag.store(true, Ordering::Release);
                            // Also set per-search stop flag if available
                            if let Some(ref search_stop_flag) = current_stop_flag {
                                search_stop_flag.store(true, Ordering::Release);
                            }
                            break;
                        }

                        // Handle other commands
                        let mut _legacy_session: Option<()> = None;
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
                            current_session: &mut _legacy_session,
                            current_bestmove_emitter: &mut current_bestmove_emitter,
                            current_stop_flag: &mut current_stop_flag,
                            allow_null_move,
                            position_state: &mut position_state,
                            program_start,
                            last_partial_result: &mut last_partial_result,
                            pre_session_fallback: &mut pre_session_fallback,
                            pre_session_fallback_hash: &mut pre_session_fallback_hash,
                            current_committed: &mut current_committed,
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
                        let mut _legacy_session2: Option<()> = None;
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
                            current_session: &mut _legacy_session2,
                            current_bestmove_emitter: &mut current_bestmove_emitter,
                            current_stop_flag: &mut current_stop_flag,
                            allow_null_move,
                            position_state: &mut position_state,
                            program_start,
                            last_partial_result: &mut last_partial_result,
                            pre_session_fallback: &mut pre_session_fallback,
                            pre_session_fallback_hash: &mut pre_session_fallback_hash,
                            current_committed: &mut current_committed,
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

        // IterationComplete removed
        WorkerMessage::IterationCommitted {
            committed,
            search_id,
        } => {
            if search_id == *ctx.current_search_id {
                if let Some(ref emitter) = ctx.current_bestmove_emitter {
                    if emitter.is_finalized() || emitter.is_terminated() {
                        log::debug!(
                            "Ignoring IterationCommitted: emitter finalized={} terminated={}",
                            emitter.is_finalized(),
                            emitter.is_terminated()
                        );
                        return Ok(());
                    }
                }
                log::debug!(
                    "IterationCommitted for search {}, depth: {}",
                    search_id,
                    committed.depth
                );
                *ctx.current_committed = Some(committed);
            } else {
                log::trace!(
                    "Ignoring IterationCommitted from old search: {search_id} (current: {})",
                    *ctx.current_search_id
                );
            }
        }

        WorkerMessage::SearchFinished {
            root_hash,
            search_id,
            stop_info,
        } => {
            // Handle search completion for current search
            // Only process if we're still in Searching state (not StopRequested)
            if search_id == *ctx.current_search_id && *ctx.search_state == SearchState::Searching {
                log::info!("Search {search_id} finished (root_hash: {root_hash:016x})");

                // Check if emitter is finalized or terminated
                if let Some(ref emitter) = ctx.current_bestmove_emitter {
                    if emitter.is_finalized() || emitter.is_terminated() {
                        log::debug!(
                            "Ignoring SearchFinished: emitter finalized={} terminated={}",
                            emitter.is_finalized(),
                            emitter.is_terminated()
                        );
                        return Ok(());
                    }
                }

                // Send bestmove immediately if not ponder
                if !*ctx.current_search_is_ponder {
                    if let Some(ref _emitter) = ctx.current_bestmove_emitter {
                        // Prefer committed iteration
                        if let Some(committed) = ctx.current_committed.clone() {
                            log::debug!("Using committed iteration for bestmove generation");
                            if ctx.emit_best_from_committed(
                                &committed,
                                BestmoveSource::SessionInSearchFinished,
                                stop_info.clone(),
                                "SearchFinishedCommitted",
                            )? {
                                return Ok(());
                            }
                        }

                        // Try committed-based bestmove
                        if let Some(committed) = ctx.current_committed.clone() {
                            if ctx.emit_best_from_committed(
                                &committed,
                                BestmoveSource::SessionInSearchFinished,
                                stop_info.clone(),
                                "SearchFinishedCommitted",
                            )? {
                                return Ok(());
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

                        // Try committed first
                        if let Some(ref committed) = ctx.current_committed {
                            let adapter = lock_or_recover_adapter(ctx.engine);
                            if let Some(position) = adapter.get_position() {
                                if let Ok((best_move, ponder, _)) = adapter
                                    .validate_and_get_bestmove_from_committed(committed, position)
                                {
                                    send_response(UsiResponse::BestMove { best_move, ponder })?;
                                    ctx.finalize_search("SearchFinished direct send committed");
                                    return Ok(());
                                }
                            }
                        }

                        // No committed, fall back to emergency

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
            // Partial results are used by stop handler; cache the latest for current search
            if search_id == *ctx.current_search_id && ctx.search_state.is_searching() {
                log::trace!("PartialResult: move={current_best}, depth={depth}, score={score}");
                *ctx.last_partial_result = Some((current_best, depth, score));
            } else {
                log::trace!(
                    "Ignored PartialResult from search_id={}, current={}, state={:?}",
                    search_id,
                    *ctx.current_search_id,
                    *ctx.search_state
                );
            }
        }

        WorkerMessage::Finished {
            from_guard,
            search_id,
        } => {
            // Handle worker thread completion
            if search_id == *ctx.current_search_id && *ctx.search_state == SearchState::Searching {
                log::warn!(
                    "Worker Finished without SearchFinished (from_guard: {from_guard}), emitting fallback"
                );

                // Try committed-based bestmove first
                if let Some(committed) = ctx.current_committed.clone() {
                    if let Some(ref _emitter) = ctx.current_bestmove_emitter {
                        if ctx.emit_best_from_committed(
                            &committed,
                            BestmoveSource::EmergencyFallbackOnFinish,
                            None,
                            "FinishedCommittedFallback",
                        )? {
                            return Ok(());
                        }
                    }
                }

                // Session path removed

                // Fallback: use cached partial result if available
                if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
                    match generate_fallback_move(ctx.engine, Some((mv, d, s)), false) {
                        Ok((move_str, _)) => {
                            let meta = build_meta(
                                BestmoveSource::EmergencyFallbackOnFinish,
                                d,
                                None,
                                Some(format!("cp {s}")),
                                None,
                            );
                            ctx.emit_and_finalize(move_str, None, meta, "FinishedPartialFallback")?;
                            return Ok(());
                        }
                        Err(e) => {
                            log::warn!("Finished: partial fallback failed: {e}");
                        }
                    }
                }

                // Emergency last resort
                match generate_fallback_move(ctx.engine, None, false) {
                    Ok((move_str, _)) => {
                        let meta = build_meta(
                            BestmoveSource::EmergencyFallbackOnFinish,
                            0,
                            None,
                            None,
                            None,
                        );
                        ctx.emit_and_finalize(move_str, None, meta, "FinishedEmergencyFallback")?;
                    }
                    Err(e) => {
                        log::error!("Finished: emergency fallback failed: {e}");
                        let meta = build_meta(BestmoveSource::ResignOnFinish, 0, None, None, None);
                        ctx.emit_and_finalize("resign".to_string(), None, meta, "FinishedResign")?;
                    }
                }
            } else {
                log::trace!(
                    "Ignoring Finished from search_id={} (current={}, state={:?})",
                    search_id,
                    *ctx.current_search_id,
                    *ctx.search_state
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
