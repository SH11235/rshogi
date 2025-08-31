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
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use engine_adapter::EngineAdapter;
use engine_core::movegen::MoveGenerator;
use engine_core::search::CommittedIteration;
use engine_core::usi::move_to_usi;
use helpers::generate_fallback_move;
// use search_session::SearchSession;
use crate::emit_utils::log_tsv;
use state::SearchState;
use std::backtrace::Backtrace;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use stdin_reader::spawn_stdin_reader;
use types::{BestmoveSource, PositionState};
use usi::{
    ensure_flush_on_exit, flush_final, send_info_string, send_response, UsiCommand, UsiResponse,
};
use worker::WorkerMessage;

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
    // Separate control-plane channel for prioritizing stop/gameover/quit
    let (ctrl_tx, ctrl_rx) = unbounded::<UsiCommand>();
    // Use unbounded command channel to avoid drops for normal commands
    let (cmd_tx, cmd_rx) = unbounded::<UsiCommand>();

    // Create engine adapter (thread-safe)
    let engine = Arc::new(Mutex::new(EngineAdapter::new()));

    // Create stop flag for search control
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Spawn stdin reader thread
    let stdin_handle = spawn_stdin_reader(cmd_tx.clone(), ctrl_tx.clone());

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
    let mut current_finalized_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>> = None; // Share finalize with worker
                                                                                                  // Guard: ensure final PV injection exactly once per search
    let mut final_pv_injected: bool = false;

    // Diagnostics: timestamps for cross-event deltas
    let mut last_bestmove_sent_at: Option<Instant> = None;
    let mut last_go_begin_at: Option<Instant> = None;
    // Per-search runtime metrics for HardDeadlineFire
    let mut search_start_time: Option<Instant> = None;
    let mut latest_nodes: u64 = 0;
    let mut soft_limit_ms_ctx: u64 = 0;

    // Additional per-search guards/state (reset by go handler)
    let mut hard_deadline_taken: bool = false; // exactly-once backstop for HardDeadlineFire
    let mut root_legal_moves: Option<Vec<String>> = None; // snapshot of root legal moves

    // Main event loop - process USI commands and worker messages concurrently
    // Strategy: always drain cmd/ctrl first (non-blocking), then handle a bounded number
    // of worker messages to avoid starvation, then fall back to a short select tick.
    let mut pending_quit = false;
    loop {
        // 1) Drain pending normal commands first (position/go 優先)
        'drain_cmds: loop {
            match cmd_rx.try_recv() {
                Ok(cmd) => {
                    log::debug!("USI command (drain phase): {:?}", cmd);
                    let cmd_name = match &cmd {
                        UsiCommand::Usi => "usi",
                        UsiCommand::IsReady => "isready",
                        UsiCommand::Quit => "quit",
                        UsiCommand::Stop => "stop",
                        UsiCommand::Position { .. } => "position",
                        UsiCommand::Go(_) => "go",
                        UsiCommand::SetOption { .. } => "setoption",
                        UsiCommand::GameOver { .. } => "gameover",
                        UsiCommand::PonderHit => "ponderhit",
                        UsiCommand::UsiNewGame => "usinewgame",
                    };
                    let _ = send_info_string(log_tsv(&[("kind", "cmd_rx"), ("cmd", cmd_name)]));
                    if let Some(t) = last_bestmove_sent_at {
                        let delta = t.elapsed().as_millis();
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "post_bestmove_to_cmd_rx"),
                            ("elapsed_ms", &delta.to_string()),
                            ("cmd", cmd_name),
                        ]));
                    }

                    // Explicitly log acceptance gate for diagnostics: idle vs finalized
                    let finalized_ready = current_finalized_flag
                        .as_ref()
                        .map(|f| f.load(Ordering::Acquire))
                        .unwrap_or(false);
                    let idle_ready = search_state.can_start_search();
                    let gate = if idle_ready {
                        "idle"
                    } else if finalized_ready {
                        "finalized"
                    } else {
                        "none"
                    };
                    let _ =
                        send_info_string(log_tsv(&[("kind", "cmd_accept_gate"), ("gate", gate)]));

                    // Quit is handled specially
                    if matches!(cmd, UsiCommand::Quit) {
                        if let Some(ref emitter) = current_bestmove_emitter {
                            emitter.terminate();
                        }
                        stop_flag.store(true, Ordering::Release);
                        if let Some(ref search_stop_flag) = current_stop_flag {
                            search_stop_flag.store(true, Ordering::Release);
                        }
                        pending_quit = true;
                        break 'drain_cmds; // proceed to shutdown via outer flow
                    }

                    // Handle other commands with fresh context
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
                        current_finalized_flag: &mut current_finalized_flag,
                        current_stop_flag: &mut current_stop_flag,
                        allow_null_move,
                        position_state: &mut position_state,
                        program_start,
                        last_partial_result: &mut last_partial_result,
                        search_start_time: &mut search_start_time,
                        latest_nodes: &mut latest_nodes,
                        soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
                        root_legal_moves: &mut root_legal_moves,
                        hard_deadline_taken: &mut hard_deadline_taken,
                        pre_session_fallback: &mut pre_session_fallback,
                        pre_session_fallback_hash: &mut pre_session_fallback_hash,
                        current_committed: &mut current_committed,
                        last_bestmove_sent_at: &mut last_bestmove_sent_at,
                        last_go_begin_at: &mut last_go_begin_at,
                        final_pv_injected: &mut final_pv_injected,
                    };
                    match handle_command(cmd, &mut ctx) {
                        Ok(()) => {}
                        Err(e) => {
                            log::error!("[MAIN] drain handle_command error: {}", e);
                            return Err(e);
                        }
                    }
                    let _ =
                        send_info_string(log_tsv(&[("kind", "cmd_handled"), ("cmd", cmd_name)]));
                    continue; // try drain next command
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break 'drain_cmds,
                Err(crossbeam_channel::TryRecvError::Disconnected) => break 'drain_cmds,
            }
        }

        // 2) Drain control-plane commands next（stop/gameover/quit 優先）
        'drain_ctrl: loop {
            match ctrl_rx.try_recv() {
                Ok(cmd) => {
                    log::info!("[MAIN] Ctrl command (drain phase): {:?}", cmd);
                    if matches!(cmd, UsiCommand::Quit) {
                        if let Some(ref emitter) = current_bestmove_emitter {
                            emitter.terminate();
                        }
                        stop_flag.store(true, Ordering::Release);
                        if let Some(ref search_stop_flag) = current_stop_flag {
                            search_stop_flag.store(true, Ordering::Release);
                        }
                        pending_quit = true; // allow normal shutdown path after loop
                        break 'drain_ctrl;
                    }
                    let mut _legacy_session_c: Option<()> = None;
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
                        current_session: &mut _legacy_session_c,
                        current_bestmove_emitter: &mut current_bestmove_emitter,
                        current_finalized_flag: &mut current_finalized_flag,
                        current_stop_flag: &mut current_stop_flag,
                        allow_null_move,
                        position_state: &mut position_state,
                        program_start,
                        last_partial_result: &mut last_partial_result,
                        search_start_time: &mut search_start_time,
                        latest_nodes: &mut latest_nodes,
                        soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
                        root_legal_moves: &mut root_legal_moves,
                        hard_deadline_taken: &mut hard_deadline_taken,
                        pre_session_fallback: &mut pre_session_fallback,
                        pre_session_fallback_hash: &mut pre_session_fallback_hash,
                        current_committed: &mut current_committed,
                        last_bestmove_sent_at: &mut last_bestmove_sent_at,
                        last_go_begin_at: &mut last_go_begin_at,
                        final_pv_injected: &mut final_pv_injected,
                    };
                    match handle_command(cmd, &mut ctx) {
                        Ok(()) => {}
                        Err(e) => {
                            log::error!("[MAIN] ctrl drain handle_command error: {}", e);
                            return Err(e);
                        }
                    }
                    continue; // try drain next ctrl
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break 'drain_ctrl,
                Err(crossbeam_channel::TryRecvError::Disconnected) => break 'drain_ctrl,
            }
        }

        // 3) Process a limited number of worker messages to avoid starving cmd_rx
        let worker_budget: usize =
            std::env::var("WORKER_BUDGET").ok().and_then(|v| v.parse().ok()).unwrap_or(32);
        let mut processed_worker = 0usize;
        while processed_worker < worker_budget {
            match worker_rx.try_recv() {
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
                        current_finalized_flag: &mut current_finalized_flag,
                        current_stop_flag: &mut current_stop_flag,
                        allow_null_move,
                        position_state: &mut position_state,
                        program_start,
                        last_partial_result: &mut last_partial_result,
                        search_start_time: &mut search_start_time,
                        latest_nodes: &mut latest_nodes,
                        soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
                        root_legal_moves: &mut root_legal_moves,
                        hard_deadline_taken: &mut hard_deadline_taken,
                        pre_session_fallback: &mut pre_session_fallback,
                        pre_session_fallback_hash: &mut pre_session_fallback_hash,
                        current_committed: &mut current_committed,
                        last_bestmove_sent_at: &mut last_bestmove_sent_at,
                        last_go_begin_at: &mut last_go_begin_at,
                        final_pv_injected: &mut final_pv_injected,
                    };
                    handle_worker_message(msg, &mut ctx)?;
                    processed_worker += 1;
                    continue;
                }
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }

        // 4) Break out early if quit was requested during drain phase
        if pending_quit {
            break;
        }

        // 5) Fall back to select! for blocking wait (short tick via default below)
        select! {
            // High-priority control commands
            recv(ctrl_rx) -> ctrl => {
                if let Ok(cmd) = ctrl {
                    log::info!("[MAIN] Ctrl command received: {:?}", cmd);
                    if matches!(cmd, UsiCommand::Quit) {
                        if let Some(ref emitter) = current_bestmove_emitter { emitter.terminate(); }
                        stop_flag.store(true, Ordering::Release);
                        if let Some(ref search_stop_flag) = current_stop_flag { search_stop_flag.store(true, Ordering::Release); }
                        break;
                    }
                    let mut _legacy_session_c: Option<()> = None;
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
                        current_session: &mut _legacy_session_c,
                        current_bestmove_emitter: &mut current_bestmove_emitter,
                        current_finalized_flag: &mut current_finalized_flag,
                        current_stop_flag: &mut current_stop_flag,
                        allow_null_move,
                        position_state: &mut position_state,
                        program_start,
                        last_partial_result: &mut last_partial_result,
                        search_start_time: &mut search_start_time,
                        latest_nodes: &mut latest_nodes,
                        soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
                        root_legal_moves: &mut root_legal_moves,
                        hard_deadline_taken: &mut hard_deadline_taken,
                        pre_session_fallback: &mut pre_session_fallback,
                        pre_session_fallback_hash: &mut pre_session_fallback_hash,
                        current_committed: &mut current_committed,
                        last_bestmove_sent_at: &mut last_bestmove_sent_at,
                        last_go_begin_at: &mut last_go_begin_at,
                        final_pv_injected: &mut final_pv_injected,
                    };
                    match handle_command(cmd, &mut ctx) { Ok(()) => {}, Err(e) => { log::error!("[MAIN] ctrl handle_command error: {}", e); return Err(e); } }
                    continue;
                }
            }
            // Handle USI commands
            recv(cmd_rx) -> cmd => {
                match cmd {
                    Ok(cmd) => {
                        log::debug!("USI command received: {cmd:?}");
                        // Diagnostic: mark command receipt in main loop
                        let cmd_name = match &cmd {
                            UsiCommand::Usi => "usi",
                            UsiCommand::IsReady => "isready",
                            UsiCommand::Quit => "quit",
                            UsiCommand::Stop => "stop",
                            UsiCommand::Position { .. } => "position",
                            UsiCommand::Go(_) => "go",
                            UsiCommand::SetOption { .. } => "setoption",
                            UsiCommand::GameOver { .. } => "gameover",
                            UsiCommand::PonderHit => "ponderhit",
                            UsiCommand::UsiNewGame => "usinewgame",
                        };
                        let _ = send_info_string(log_tsv(&[("kind", "cmd_rx"), ("cmd", cmd_name)]));
                        if let Some(t) = last_bestmove_sent_at {
                            let delta = t.elapsed().as_millis();
                            let _ = send_info_string(log_tsv(&[("kind", "post_bestmove_to_cmd_rx"), ("elapsed_ms", &delta.to_string()), ("cmd", cmd_name)]));
                        }
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
                            current_finalized_flag: &mut current_finalized_flag,
                            current_stop_flag: &mut current_stop_flag,
                            allow_null_move,
                            position_state: &mut position_state,
                            program_start,
                            last_partial_result: &mut last_partial_result,
                            search_start_time: &mut search_start_time,
                            latest_nodes: &mut latest_nodes,
                            soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
                            root_legal_moves: &mut root_legal_moves,
                            hard_deadline_taken: &mut hard_deadline_taken,
                            pre_session_fallback: &mut pre_session_fallback,
                            pre_session_fallback_hash: &mut pre_session_fallback_hash,
                            current_committed: &mut current_committed,
                            last_bestmove_sent_at: &mut last_bestmove_sent_at,
                            last_go_begin_at: &mut last_go_begin_at,
                            final_pv_injected: &mut final_pv_injected,
                        };
                        match handle_command(cmd, &mut ctx) {
                            Ok(()) => {},
                            Err(e) => {
                                log::error!("[MAIN] handle_command error: {}", e);
                                return Err(e);
                            }
                        }
                        // Diagnostic: mark command handled in main loop
                        let _ = send_info_string(log_tsv(&[("kind", "cmd_handled"), ("cmd", cmd_name)]));
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
                            current_finalized_flag: &mut current_finalized_flag,
                            current_stop_flag: &mut current_stop_flag,
                            allow_null_move,
                            position_state: &mut position_state,
                            program_start,
                            last_partial_result: &mut last_partial_result,
                            search_start_time: &mut search_start_time,
                            latest_nodes: &mut latest_nodes,
                            soft_limit_ms_ctx: &mut soft_limit_ms_ctx,
                            root_legal_moves: &mut root_legal_moves,
                            hard_deadline_taken: &mut hard_deadline_taken,
                            pre_session_fallback: &mut pre_session_fallback,
                            pre_session_fallback_hash: &mut pre_session_fallback_hash,
                            current_committed: &mut current_committed,
                            last_bestmove_sent_at: &mut last_bestmove_sent_at,
                            last_go_begin_at: &mut last_go_begin_at,
                            final_pv_injected: &mut final_pv_injected,
                        };
                        handle_worker_message(msg, &mut ctx)?;
                    }
                    Err(_) => {
                        log::debug!("Worker channel closed");
                    }
                }
            }

            default(Duration::from_millis(1)) => {
                // Small idle to prevent busy loop (no wall watchdog)
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
    // If bestmove has already been finalized for the current search, drop non-critical
    // worker messages for this search_id to avoid backlog starving cmd_rx.
    // Keep Finished so that join/waits can still complete cleanly.
    let drop_after_finalize = match (&ctx.current_bestmove_emitter, &ctx.search_state) {
        (Some(em), _) if em.is_finalized() || em.is_terminated() => true,
        (None, _) => true, // finalize_search() sets emitter=None
        _ => false,
    };

    // Helper to log standardized drop reason
    let log_drop = |reason: &str, extra: &[(&str, String)]| {
        let mut kv = vec![
            ("kind", "worker_drop_after_finalize".to_string()),
            ("reason", reason.to_string()),
        ];
        kv.extend(extra.iter().cloned());
        let _ = send_info_string(log_tsv(
            &kv.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>(),
        ));
    };

    // Inspect search_id for drop decision
    match &msg {
        WorkerMessage::Finished { .. } => {
            // Never drop Finished — allow cleanup/join logic to observe it later if needed.
        }
        _ if drop_after_finalize => {
            // Drop only if message is from current search; otherwise let normal path handle it
            let current_id = *ctx.current_search_id;
            let id = match &msg {
                WorkerMessage::Info { search_id, .. } => *search_id,
                WorkerMessage::SearchStarted { search_id, .. } => *search_id,
                WorkerMessage::IterationCommitted { search_id, .. } => *search_id,
                WorkerMessage::SearchFinished { search_id, .. } => *search_id,
                WorkerMessage::PartialResult { search_id, .. } => *search_id,
                WorkerMessage::Finished { search_id, .. } => *search_id, // already excluded above
                WorkerMessage::Error { search_id, .. } => *search_id,
                WorkerMessage::HardDeadlineFire { search_id, .. } => *search_id,
            };
            if id == current_id {
                // Log and drop
                let tag = match &msg {
                    WorkerMessage::Info { .. } => "info",
                    WorkerMessage::SearchStarted { .. } => "started",
                    WorkerMessage::IterationCommitted { .. } => "committed",
                    WorkerMessage::SearchFinished { .. } => "finished",
                    WorkerMessage::PartialResult { .. } => "partial",
                    WorkerMessage::Error { .. } => "error",
                    WorkerMessage::Finished { .. } => "finished", // unreachable here
                    WorkerMessage::HardDeadlineFire { .. } => "hard_deadline",
                };
                log_drop(tag, &[("search_id", id.to_string())]);
                return Ok(());
            }
        }
        _ => {}
    }

    match msg {
        WorkerMessage::HardDeadlineFire { search_id, hard_ms } => {
            // Insurance: hard deadline single-shot
            let _ = send_info_string(log_tsv(&[
                ("kind", "hard_deadline_fire"),
                ("search_id", &search_id.to_string()),
                ("hard_ms", &hard_ms.to_string()),
            ]));
            if search_id != *ctx.current_search_id {
                return Ok(());
            }

            // Check option: ForceTerminateOnHardDeadline
            if let Ok(adapter) = ctx.engine.try_lock() {
                if !adapter.force_terminate_on_hard_deadline() {
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "hard_deadline_skip"),
                        ("reason", "option_disabled"),
                    ]));
                    return Ok(());
                }
            }

            // Exactly-once backstop for this search
            if *ctx.hard_deadline_taken {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "hard_deadline_skip"),
                    ("reason", "already_taken"),
                ]));
                return Ok(());
            }
            *ctx.hard_deadline_taken = true;

            // If emitter missing or already finalized, nothing to do
            if ctx
                .current_bestmove_emitter
                .as_ref()
                .map(|e| e.is_finalized() || e.is_terminated())
                .unwrap_or(true)
            {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "hard_deadline_skip"),
                    ("reason", "no_emitter_or_finalized"),
                ]));
                return Ok(());
            }

            // Build base StopInfo (hard timeout) with best-known metrics
            let elapsed_from_start =
                ctx.search_start_time.map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
            let (comm_elapsed, comm_nodes, comm_depth) = ctx
                .current_committed
                .as_ref()
                .map(|c| (c.elapsed.as_millis() as u64, c.nodes, c.depth))
                .unwrap_or((0, 0, 0));
            let final_elapsed = if elapsed_from_start > 0 {
                elapsed_from_start
            } else {
                comm_elapsed
            };
            let final_nodes = if *ctx.latest_nodes > 0 {
                *ctx.latest_nodes
            } else {
                comm_nodes
            };
            let base_stop = engine_core::search::types::StopInfo {
                reason: engine_core::search::types::TerminationReason::TimeLimit,
                elapsed_ms: final_elapsed,
                nodes: final_nodes,
                depth_reached: comm_depth,
                hard_timeout: true,
                soft_limit_ms: *ctx.soft_limit_ms_ctx,
                hard_limit_ms: hard_ms,
            };

            // 1) Try committed iteration path first
            if let Some(committed) = ctx.current_committed.clone() {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "hard_deadline_has_committed"),
                    ("depth", &committed.depth.to_string()),
                    ("nodes", &committed.nodes.to_string()),
                    ("search_id", &search_id.to_string()),
                ]));
                if ctx.emit_best_from_committed(
                    &committed,
                    BestmoveSource::EmergencyFallbackTimeout,
                    Some(base_stop.clone()),
                    "HardCommitted",
                )? {
                    // Ensure prompt flush
                    crate::usi::output::flush_now();
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "hard_deadline_path"),
                        ("src", "committed"),
                    ]));
                    return Ok(());
                }
            } else {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "hard_deadline_no_committed"),
                    ("search_id", &search_id.to_string()),
                ]));
            }

            // 2) Use cached partial result if available
            if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
                if let Ok((move_str, _)) =
                    generate_fallback_move(ctx.engine, Some((mv, d, s)), ctx.allow_null_move, true)
                {
                    // Inject final PV to align with bestmove
                    let info = crate::usi::output::SearchInfo {
                        depth: Some(d as u32),
                        score: Some(crate::utils::to_usi_score(s)),
                        pv: vec![move_str.clone()],
                        ..Default::default()
                    };
                    ctx.inject_final_pv(info, "hard_partial");
                    let meta = build_meta(
                        BestmoveSource::EmergencyFallbackTimeout,
                        d,
                        None,
                        Some(format!("cp {s}")),
                        Some(base_stop.clone()),
                    );
                    ctx.emit_and_finalize(move_str, None, meta, "HardPartialFallback")?;
                    crate::usi::output::flush_now();
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "hard_deadline_path"),
                        ("src", "partial"),
                    ]));
                    return Ok(());
                }
            }

            // 3) Root legal move set snapshot
            if let Some(legal) = ctx.root_legal_moves.as_ref() {
                // Prefer PV head from committed when legal
                let (chosen, depth_for_meta) =
                    if let Some(committed) = ctx.current_committed.as_ref() {
                        if let Some(first) = committed.pv.first() {
                            let pv_best = move_to_usi(first);
                            if legal.iter().any(|s| s == &pv_best) {
                                (pv_best, committed.depth)
                            } else {
                                (legal[0].clone(), committed.depth)
                            }
                        } else {
                            (legal[0].clone(), committed.depth)
                        }
                    } else {
                        (legal[0].clone(), 0)
                    };

                // Inject final PV then emit
                let info = crate::usi::output::SearchInfo {
                    multipv: Some(1),
                    pv: vec![chosen.clone()],
                    ..Default::default()
                };
                ctx.inject_final_pv(info, "hard_root_legal");
                let meta = build_meta(
                    BestmoveSource::EmergencyFallbackTimeout,
                    depth_for_meta,
                    None,
                    None,
                    Some(base_stop.clone()),
                );
                ctx.emit_and_finalize(chosen.clone(), None, meta, "HardRootLegalFallback")?;
                crate::usi::output::flush_now();
                let _ = send_info_string(log_tsv(&[
                    ("kind", "hard_deadline_path"),
                    ("src", "root_legal"),
                ]));
                return Ok(());
            }

            // 4) Emergency generator
            match generate_fallback_move(ctx.engine, None, ctx.allow_null_move, true) {
                Ok((move_str, _)) => {
                    // Inject PV and emit
                    let info = crate::usi::output::SearchInfo {
                        multipv: Some(1),
                        pv: vec![move_str.clone()],
                        ..Default::default()
                    };
                    ctx.inject_final_pv(info, "hard_emergency");
                    let meta = build_meta(
                        BestmoveSource::EmergencyFallbackTimeout,
                        0,
                        None,
                        None,
                        Some(base_stop.clone()),
                    );
                    ctx.emit_and_finalize(move_str, None, meta, "HardEmergencyFallback")?;
                    crate::usi::output::flush_now();
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "hard_deadline_path"),
                        ("src", "emergency"),
                    ]));
                    return Ok(());
                }
                Err(_) => {
                    // Last resort: resign
                    let info = crate::usi::output::SearchInfo {
                        multipv: Some(1),
                        pv: vec!["resign".to_string()],
                        ..Default::default()
                    };
                    ctx.inject_final_pv(info, "hard_resign");
                    let meta =
                        build_meta(BestmoveSource::Resign, 0, None, None, Some(base_stop.clone()));
                    ctx.emit_and_finalize("resign".to_string(), None, meta, "HardResign")?;
                    crate::usi::output::flush_now();
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "hard_deadline_path"),
                        ("src", "resign"),
                    ]));
                    return Ok(());
                }
            }
        }
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
                // Record search start time for HardDeadline metrics
                *ctx.search_start_time = Some(start_time);
                // USI-visible diagnostic: SearchStarted event and delta from go_begin
                if let Some(t) = *ctx.last_go_begin_at {
                    let delta = t.elapsed().as_millis() as u64;
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "search_started"),
                        ("search_id", &search_id.to_string()),
                        ("delta_ms", &delta.to_string()),
                    ]));
                    // Threshold for slow path warning (env var SLOW_GO_THRESHOLD_MS, default 500)
                    let thr_ms = std::env::var("SLOW_GO_THRESHOLD_MS")
                        .ok()
                        .and_then(|v| v.parse::<u64>().ok())
                        .unwrap_or(500);
                    if delta > thr_ms {
                        let bt = Backtrace::force_capture();
                        log::warn!(
                            "Slow go->SearchStarted: {} ms (> {} ms). Backtrace:\n{}",
                            delta,
                            thr_ms,
                            bt
                        );
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "slow_go_start"),
                            ("delta_ms", &delta.to_string()),
                            ("threshold_ms", &thr_ms.to_string()),
                        ]));
                    }
                } else {
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "search_started"),
                        ("search_id", &search_id.to_string()),
                        ("delta_ms", "-1"),
                    ]));
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
                // Log IterationCommitted received for visibility
                let _ = send_info_string(log_tsv(&[
                    ("kind", "iteration_committed_received"),
                    ("search_id", &search_id.to_string()),
                    ("depth", &committed.depth.to_string()),
                    ("score", &format!("{:?}", committed.score)),
                    ("nodes", &committed.nodes.to_string()),
                    ("elapsed_ms", &committed.elapsed.as_millis().to_string()),
                ]));
                // Save committed and capture root legal move set (for hard-deadline backstop)
                *ctx.current_committed = Some(committed);
                // Update latest nodes snapshot
                *ctx.latest_nodes = ctx.current_committed.as_ref().map(|c| c.nodes).unwrap_or(0);
                // Best-effort snapshot (non-blocking try_lock to avoid stalls)
                if let Ok(adapter) = ctx.engine.try_lock() {
                    if let Some(pos) = adapter.get_position() {
                        let mg = MoveGenerator::new();
                        if let Ok(list) = mg.generate_all(pos) {
                            let v: Vec<String> = list.iter().map(move_to_usi).collect();
                            *ctx.root_legal_moves = Some(v);
                            if std::env::var("LOG_ROOT_LEGAL").as_deref() == Ok("1") {
                                let _ = send_info_string(log_tsv(&[
                                    ("kind", "committed_root_legal_snapshot"),
                                    (
                                        "count",
                                        &ctx.root_legal_moves
                                            .as_ref()
                                            .map(|v| v.len())
                                            .unwrap_or(0)
                                            .to_string(),
                                    ),
                                ]));
                            }
                        }
                    }
                }
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
            // Log SearchFinished received
            let _ = send_info_string(log_tsv(&[
                ("kind", "search_finished_received"),
                ("search_id", &search_id.to_string()),
                ("stop_reason", &format!("{:?}", stop_info.as_ref().map(|s| &s.reason))),
                ("depth", &stop_info.as_ref().map(|s| s.depth_reached).unwrap_or(0).to_string()),
                ("nodes", &stop_info.as_ref().map(|s| s.nodes).unwrap_or(0).to_string()),
            ]));

            // Handle search completion for current search（state非依存: emitter未finalizeなら許可）
            if search_id != *ctx.current_search_id {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "searchfinished_drop"),
                    ("reason", "id_mismatch"),
                    ("search_id", &search_id.to_string()),
                    ("current", &ctx.current_search_id.to_string()),
                ]));
                return Ok(());
            }

            if search_id == *ctx.current_search_id {
                log::info!("Search {search_id} finished (root_hash: {root_hash:016x})");

                // Check if emitter is finalized or terminated
                if let Some(ref emitter) = ctx.current_bestmove_emitter {
                    if emitter.is_finalized() || emitter.is_terminated() {
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "searchfinished_drop"),
                            ("reason", "finalized_or_terminated"),
                            ("search_id", &search_id.to_string()),
                        ]));
                        return Ok(());
                    }
                } else {
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "searchfinished_drop"),
                        ("reason", "no_emitter"),
                        ("search_id", &search_id.to_string()),
                    ]));
                    return Ok(());
                }
                // Send bestmove immediately if not ponder (Central finalize)
                if !*ctx.current_search_is_ponder {
                    let _ = ctx.finalize_emit_if_possible("search_finished", stop_info)?;
                    return Ok(());
                } else {
                    log::debug!("Ponder search finished, not sending bestmove");
                    // Finalize ponder search to ensure proper cleanup
                    // (normally ponder ends via stop/ponderhit, but handle natural termination)
                    ctx.finalize_search("PonderFinished");
                }
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
            // Handle worker thread completion (state非依存: emitter未finalizeなら許可)
            if search_id != *ctx.current_search_id {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "finished_drop"),
                    ("reason", "id_mismatch"),
                    ("search_id", &search_id.to_string()),
                    ("current", &ctx.current_search_id.to_string()),
                ]));
                return Ok(());
            }
            // If already finalized or no emitter, drop with reason
            if let Some(ref emitter) = ctx.current_bestmove_emitter {
                if emitter.is_finalized() || emitter.is_terminated() {
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "finished_drop"),
                        ("reason", "finalized_or_terminated"),
                        ("search_id", &search_id.to_string()),
                    ]));
                    return Ok(());
                }
            } else {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "finished_drop"),
                    ("reason", "no_emitter"),
                    ("search_id", &search_id.to_string()),
                ]));
                return Ok(());
            }

            if search_id == *ctx.current_search_id {
                log::warn!(
                    "Worker Finished without SearchFinished (from_guard: {from_guard}), emitting fallback"
                );

                // Try central finalize first
                if !*ctx.current_search_is_ponder
                    && ctx.finalize_emit_if_possible("finished", None)?
                {
                    return Ok(());
                }

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
                    match generate_fallback_move(ctx.engine, Some((mv, d, s)), false, false) {
                        Ok((move_str, _)) => {
                            // Emit a final info pv based on the partial result to align with bestmove
                            let info = crate::usi::output::SearchInfo {
                                depth: Some(d as u32),
                                score: Some(crate::utils::to_usi_score(s)),
                                pv: vec![move_str.clone()],
                                ..Default::default()
                            };
                            let _ = crate::usi::send_response(crate::usi::UsiResponse::Info(info));
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
                match generate_fallback_move(ctx.engine, None, false, false) {
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
