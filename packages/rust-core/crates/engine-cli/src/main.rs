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
use engine_core::search::CommittedIteration;
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
    // Armed worker watchdog threshold (None when not armed): used to suppress wall watchdog
    let mut worker_watchdog_threshold: Option<u64> = None;

    // Main event loop - process USI commands and worker messages concurrently
    // Strategy: always drain cmd/ctrl first (non-blocking), then handle a bounded number
    // of worker messages to avoid starvation, then fall back to a short select tick.
    let mut pending_quit = false;
    // Log wall watchdog suppression only once per armed period
    let mut wall_watchdog_suppressed_logged = false;
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
                        pre_session_fallback: &mut pre_session_fallback,
                        pre_session_fallback_hash: &mut pre_session_fallback_hash,
                        current_committed: &mut current_committed,
                        last_bestmove_sent_at: &mut last_bestmove_sent_at,
                        last_go_begin_at: &mut last_go_begin_at,
                        current_worker_watchdog_threshold: &mut worker_watchdog_threshold,
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
                        pre_session_fallback: &mut pre_session_fallback,
                        pre_session_fallback_hash: &mut pre_session_fallback_hash,
                        current_committed: &mut current_committed,
                        last_bestmove_sent_at: &mut last_bestmove_sent_at,
                        last_go_begin_at: &mut last_go_begin_at,
                        current_worker_watchdog_threshold: &mut worker_watchdog_threshold,
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
                        pre_session_fallback: &mut pre_session_fallback,
                        pre_session_fallback_hash: &mut pre_session_fallback_hash,
                        current_committed: &mut current_committed,
                        last_bestmove_sent_at: &mut last_bestmove_sent_at,
                        last_go_begin_at: &mut last_go_begin_at,
                        current_worker_watchdog_threshold: &mut worker_watchdog_threshold,
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
                        pre_session_fallback: &mut pre_session_fallback,
                        pre_session_fallback_hash: &mut pre_session_fallback_hash,
                        current_committed: &mut current_committed,
                        last_bestmove_sent_at: &mut last_bestmove_sent_at,
                        last_go_begin_at: &mut last_go_begin_at,
                        current_worker_watchdog_threshold: &mut worker_watchdog_threshold,
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
                            pre_session_fallback: &mut pre_session_fallback,
                            pre_session_fallback_hash: &mut pre_session_fallback_hash,
                            current_committed: &mut current_committed,
                            last_bestmove_sent_at: &mut last_bestmove_sent_at,
                            last_go_begin_at: &mut last_go_begin_at,
                            current_worker_watchdog_threshold: &mut worker_watchdog_threshold,
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
                            pre_session_fallback: &mut pre_session_fallback,
                            pre_session_fallback_hash: &mut pre_session_fallback_hash,
                            current_committed: &mut current_committed,
                            last_bestmove_sent_at: &mut last_bestmove_sent_at,
                            last_go_begin_at: &mut last_go_begin_at,
                            current_worker_watchdog_threshold: &mut worker_watchdog_threshold,
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
                // Small idle to prevent busy loop
                // Wall-clock watchdog: default有効（Byoyomi前提 4.4s）。環境変数で上書き可能。
                let thr_ms = std::env::var("WALL_WATCHDOG_MS")
                    .ok()
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(4400);
                if thr_ms > 0 && search_state.is_searching() && worker_watchdog_threshold.is_none() {
                    // Leaving armed period: reset suppression log flag
                    wall_watchdog_suppressed_logged = false;
                    if let Some(t0) = last_go_begin_at {
                        let elapsed = t0.elapsed().as_millis() as u64;
                        // Only fire if no bestmove has been sent since go-begin
                        let best_after_begin = last_bestmove_sent_at.map(|tb| tb >= t0).unwrap_or(false);
                        if elapsed > thr_ms && !best_after_begin {
                            let _ = send_info_string(log_tsv(&[("kind", "wall_watchdog_fire"), ("elapsed_ms", &elapsed.to_string()), ("threshold_ms", &thr_ms.to_string())]));
                            // Build a context and immediately handle stop
                            let mut _legacy_session3: Option<()> = None;
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
                                current_session: &mut _legacy_session3,
                                current_bestmove_emitter: &mut current_bestmove_emitter,
                                current_finalized_flag: &mut current_finalized_flag,
                                current_stop_flag: &mut current_stop_flag,
                                allow_null_move,
                                position_state: &mut position_state,
                                program_start,
                                last_partial_result: &mut last_partial_result,
                                pre_session_fallback: &mut pre_session_fallback,
                                pre_session_fallback_hash: &mut pre_session_fallback_hash,
                                current_committed: &mut current_committed,
                                last_bestmove_sent_at: &mut last_bestmove_sent_at,
                                last_go_begin_at: &mut last_go_begin_at,
                                current_worker_watchdog_threshold: &mut worker_watchdog_threshold,
                                final_pv_injected: &mut final_pv_injected,
                            };
                            // Force stop handling (best-effort fallback emission inside)
                            let _ = crate::handlers::stop::handle_stop_command(&mut ctx);
                        }
                    }
                } else if thr_ms > 0 && search_state.is_searching() && worker_watchdog_threshold.is_some() {
                    // Suppress wall watchdog when worker watchdog is armed - log once per armed period
                    if !wall_watchdog_suppressed_logged {
                        let _ = send_info_string(log_tsv(&[("kind", "wall_watchdog_suppress"), ("reason", "worker_watchdog_active")]));
                        wall_watchdog_suppressed_logged = true;
                    }
                }
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
                WorkerMessage::WatchdogFired { search_id, .. } => *search_id,
                WorkerMessage::SearchStarted { search_id, .. } => *search_id,
                WorkerMessage::IterationCommitted { search_id, .. } => *search_id,
                WorkerMessage::SearchFinished { search_id, .. } => *search_id,
                WorkerMessage::PartialResult { search_id, .. } => *search_id,
                WorkerMessage::Finished { search_id, .. } => *search_id, // already excluded above
                WorkerMessage::Error { search_id, .. } => *search_id,
            };
            if id == current_id {
                // Log and drop
                let tag = match &msg {
                    WorkerMessage::Info { .. } => "info",
                    WorkerMessage::WatchdogFired { .. } => "watchdog",
                    WorkerMessage::SearchStarted { .. } => "started",
                    WorkerMessage::IterationCommitted { .. } => "committed",
                    WorkerMessage::SearchFinished { .. } => "finished",
                    WorkerMessage::PartialResult { .. } => "partial",
                    WorkerMessage::Error { .. } => "error",
                    WorkerMessage::Finished { .. } => "finished", // unreachable here
                };
                log_drop(tag, &[("search_id", id.to_string())]);
                return Ok(());
            }
        }
        _ => {}
    }

    match msg {
        WorkerMessage::WatchdogFired {
            search_id,
            soft_ms,
            hard_ms,
        } => {
            let emit_start = Instant::now();
            // Emit immediately on watchdog fire (state非依存、emitter未finalizeで一度だけ)
            if search_id != *ctx.current_search_id {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "watchdog_emit_drop"),
                    ("reason", "id_mismatch"),
                ]));
                return Ok(());
            }
            if let Some(ref emitter) = ctx.current_bestmove_emitter {
                if emitter.is_finalized() || emitter.is_terminated() {
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "watchdog_emit_drop"),
                        ("reason", "finalized_or_terminated"),
                    ]));
                    return Ok(());
                }
            } else {
                let _ = send_info_string(log_tsv(&[
                    ("kind", "watchdog_emit_drop"),
                    ("reason", "no_emitter"),
                ]));
                return Ok(());
            }

            // Prefer committed iteration
            if let Some(committed) = ctx.current_committed.clone() {
                let stop_info = engine_core::search::types::StopInfo {
                    reason: engine_core::search::types::TerminationReason::TimeLimit,
                    elapsed_ms: 0,
                    nodes: 0,
                    depth_reached: committed.depth,
                    hard_timeout: false,
                    soft_limit_ms: soft_ms,
                    hard_limit_ms: hard_ms,
                };
                if ctx.emit_best_from_committed(
                    &committed,
                    BestmoveSource::PartialResultTimeout,
                    Some(stop_info),
                    "WatchdogCommitted",
                )? {
                    // Emit-latency (watchdog_fire → bestmove_sent) observation
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "watchdog_emit_latency"),
                        ("ms", &emit_start.elapsed().as_millis().to_string()),
                    ]));
                    return Ok(());
                }
            }

            // Fallback chain: partial → pre_session → emergency
            if let Some((mv, d, s)) = ctx.last_partial_result.clone() {
                if let Ok((move_str, _)) =
                    generate_fallback_move(ctx.engine, Some((mv, d, s)), ctx.allow_null_move, true)
                {
                    // Emit a final info pv reflecting the partial result used for bestmove
                    // so that the last PV seen by GUIs matches the emitted bestmove.
                    let (time_opt, nodes_opt, nps_opt, depth_opt) =
                        if let Some(committed) = ctx.current_committed.clone() {
                            let ems = committed.elapsed.as_millis() as u64;
                            let nps = if ems > 0 && committed.nodes > 0 {
                                Some(committed.nodes.saturating_mul(1000) / ems)
                            } else {
                                None
                            };
                            (Some(ems), Some(committed.nodes), nps, Some(committed.depth as u32))
                        } else {
                            (None, None, None, Some(d as u32))
                        };
                    let info = crate::usi::output::SearchInfo {
                        multipv: Some(1),
                        depth: depth_opt,
                        time: time_opt,
                        nodes: nodes_opt,
                        nps: nps_opt,
                        score: Some(crate::utils::to_usi_score(s)),
                        pv: vec![move_str.clone()],
                        ..Default::default()
                    };
                    ctx.inject_final_pv(info, "watchdog_partial");
                    let meta = build_meta(
                        BestmoveSource::PartialResultTimeout,
                        d,
                        None,
                        Some(format!("cp {s}")),
                        Some(engine_core::search::types::StopInfo {
                            reason: engine_core::search::types::TerminationReason::TimeLimit,
                            elapsed_ms: 0,
                            nodes: 0,
                            depth_reached: d,
                            hard_timeout: false,
                            soft_limit_ms: soft_ms,
                            hard_limit_ms: hard_ms,
                        }),
                    );
                    ctx.emit_and_finalize(move_str, None, meta, "WatchdogPartial")?;
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "watchdog_emit_latency"),
                        ("ms", &emit_start.elapsed().as_millis().to_string()),
                    ]));
                    return Ok(());
                }
            }

            // Pre-session fallback（try_lockで一致時のみ使用。1ms超ならスキップ）
            if let Some(saved_mv) = ctx.pre_session_fallback.clone() {
                if let Ok(adapter) = ctx.engine.try_lock() {
                    let t0 = std::time::Instant::now();
                    if let Some(h) = adapter.get_position().map(|p| p.zobrist_hash()) {
                        if Some(h) == *ctx.pre_session_fallback_hash {
                            // 軽い再正規化（1ms 以内のみ）
                            if let Some(pos) = adapter.get_position() {
                                if let Some(norm) =
                                    engine_core::util::usi_helpers::normalize_usi_move_str_logged(
                                        pos, &saved_mv,
                                    )
                                {
                                    let us = t0.elapsed().as_micros();
                                    if us <= 1000 {
                                        // Inject final PV for pre_session watchdog path
                                        let info = crate::usi::output::SearchInfo {
                                            multipv: Some(1),
                                            pv: vec![norm.clone()],
                                            ..Default::default()
                                        };
                                        ctx.inject_final_pv(info, "watchdog_pre_session");
                                        let meta = build_meta(
                                            BestmoveSource::EmergencyFallbackTimeout,
                                            0,
                                            None,
                                            None,
                                            Some(engine_core::search::types::StopInfo {
                                                reason: engine_core::search::types::TerminationReason::TimeLimit,
                                                elapsed_ms: 0,
                                                nodes: 0,
                                                depth_reached: 0,
                                                hard_timeout: false,
                                                soft_limit_ms: soft_ms,
                                                hard_limit_ms: hard_ms,
                                            }),
                                        );
                                        ctx.emit_and_finalize(
                                            norm,
                                            None,
                                            meta,
                                            "WatchdogPreSession",
                                        )?;
                                        let _ = send_info_string(log_tsv(&[
                                            ("kind", "watchdog_emit_latency"),
                                            ("ms", &emit_start.elapsed().as_millis().to_string()),
                                        ]));
                                        return Ok(());
                                    } else {
                                        let _ = send_info_string(log_tsv(&[
                                            ("kind", "watchdog_pre_session_skip"),
                                            ("reason", "recheck_slow"),
                                            ("us", &us.to_string()),
                                        ]));
                                    }
                                } else {
                                    let _ = send_info_string(log_tsv(&[
                                        ("kind", "watchdog_pre_session_skip"),
                                        ("reason", "normalize_failed"),
                                    ]));
                                }
                            }
                        } else {
                            let _ = send_info_string(log_tsv(&[
                                ("kind", "watchdog_pre_session_skip"),
                                ("reason", "hash_mismatch"),
                            ]));
                        }
                    }
                } else {
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "watchdog_pre_session_skip"),
                        ("reason", "adapter_lock_busy"),
                    ]));
                }
            }

            // Emergency（PositionState 優先でロック不要経路を試す）
            if let Some(state) = ctx.position_state.as_ref() {
                if let Some(m) = crate::helpers::emergency_move_from_state(state) {
                    // Inject final PV for emergency(state) watchdog path
                    let info = crate::usi::output::SearchInfo {
                        multipv: Some(1),
                        pv: vec![m.clone()],
                        ..Default::default()
                    };
                    ctx.inject_final_pv(info, "watchdog_emergency_state");
                    let meta = build_meta(
                        BestmoveSource::EmergencyFallbackTimeout,
                        0,
                        None,
                        None,
                        Some(engine_core::search::types::StopInfo {
                            reason: engine_core::search::types::TerminationReason::TimeLimit,
                            elapsed_ms: 0,
                            nodes: 0,
                            depth_reached: 0,
                            hard_timeout: false,
                            soft_limit_ms: soft_ms,
                            hard_limit_ms: hard_ms,
                        }),
                    );
                    ctx.emit_and_finalize(m, None, meta, "WatchdogEmergencyState")?;
                    let _ = send_info_string(log_tsv(&[
                        ("kind", "watchdog_emit_latency"),
                        ("ms", &emit_start.elapsed().as_millis().to_string()),
                    ]));
                } else {
                    match generate_fallback_move(ctx.engine, None, false, true) {
                        Ok((move_str, _)) => {
                            let meta = build_meta(
                                BestmoveSource::EmergencyFallbackTimeout,
                                0,
                                None,
                                None,
                                Some(engine_core::search::types::StopInfo {
                                    reason:
                                        engine_core::search::types::TerminationReason::TimeLimit,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 0,
                                    hard_timeout: false,
                                    soft_limit_ms: soft_ms,
                                    hard_limit_ms: hard_ms,
                                }),
                            );
                            ctx.emit_and_finalize(move_str, None, meta, "WatchdogEmergency")?;
                            let _ = send_info_string(log_tsv(&[
                                ("kind", "watchdog_emit_latency"),
                                ("ms", &emit_start.elapsed().as_millis().to_string()),
                            ]));
                        }
                        Err(_) => {
                            let meta = build_meta(
                                BestmoveSource::ResignTimeout,
                                0,
                                None,
                                None,
                                Some(engine_core::search::types::StopInfo {
                                    reason:
                                        engine_core::search::types::TerminationReason::TimeLimit,
                                    elapsed_ms: 0,
                                    nodes: 0,
                                    depth_reached: 0,
                                    hard_timeout: true,
                                    soft_limit_ms: soft_ms,
                                    hard_limit_ms: hard_ms,
                                }),
                            );
                            ctx.emit_and_finalize(
                                "resign".to_string(),
                                None,
                                meta,
                                "WatchdogResign",
                            )?;
                            let _ = send_info_string(log_tsv(&[
                                ("kind", "watchdog_emit_latency"),
                                ("ms", &emit_start.elapsed().as_millis().to_string()),
                            ]));
                        }
                    }
                }
            } else {
                match generate_fallback_move(ctx.engine, None, false, true) {
                    Ok((move_str, _)) => {
                        // Inject final PV for emergency watchdog path
                        let info = crate::usi::output::SearchInfo {
                            multipv: Some(1),
                            pv: vec![move_str.clone()],
                            ..Default::default()
                        };
                        ctx.inject_final_pv(info, "watchdog_emergency");
                        let meta = build_meta(
                            BestmoveSource::EmergencyFallbackTimeout,
                            0,
                            None,
                            None,
                            Some(engine_core::search::types::StopInfo {
                                reason: engine_core::search::types::TerminationReason::TimeLimit,
                                elapsed_ms: 0,
                                nodes: 0,
                                depth_reached: 0,
                                hard_timeout: false,
                                soft_limit_ms: soft_ms,
                                hard_limit_ms: hard_ms,
                            }),
                        );
                        ctx.emit_and_finalize(move_str, None, meta, "WatchdogEmergency")?;
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "watchdog_emit_latency"),
                            ("ms", &emit_start.elapsed().as_millis().to_string()),
                        ]));
                    }
                    Err(_) => {
                        // Inject final PV for resign watchdog path
                        let info = crate::usi::output::SearchInfo {
                            multipv: Some(1),
                            pv: vec!["resign".to_string()],
                            ..Default::default()
                        };
                        ctx.inject_final_pv(info, "watchdog_resign");
                        let meta = build_meta(
                            BestmoveSource::ResignTimeout,
                            0,
                            None,
                            None,
                            Some(engine_core::search::types::StopInfo {
                                reason: engine_core::search::types::TerminationReason::TimeLimit,
                                elapsed_ms: 0,
                                nodes: 0,
                                depth_reached: 0,
                                hard_timeout: true,
                                soft_limit_ms: soft_ms,
                                hard_limit_ms: hard_ms,
                            }),
                        );
                        ctx.emit_and_finalize("resign".to_string(), None, meta, "WatchdogResign")?;
                        let _ = send_info_string(log_tsv(&[
                            ("kind", "watchdog_emit_latency"),
                            ("ms", &emit_start.elapsed().as_millis().to_string()),
                        ]));
                    }
                }
            }
            return Ok(());
        }
        WorkerMessage::Info { info, search_id } => {
            // Forward info messages only from current search
            if search_id == *ctx.current_search_id && ctx.search_state.is_searching() {
                // Detect worker watchdog arming and record threshold to suppress wall watchdog
                if let Some(ref s) = info.string {
                    if s.contains("kind=watchdog_start") {
                        // Parse threshold_ms from TSV
                        let mut threshold: Option<u64> = None;
                        for kv in s.split('\t') {
                            if let Some((k, v)) = kv.split_once('=') {
                                if k == "threshold_ms" {
                                    if let Ok(ms) = v.parse::<u64>() {
                                        threshold = Some(ms);
                                        break;
                                    }
                                }
                            }
                        }
                        *ctx.current_worker_watchdog_threshold = threshold;
                    }
                }
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
            // Clear any armed worker watchdog for this search
            *ctx.current_worker_watchdog_threshold = None;
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
                // Send bestmove immediately if not ponder (Core finalize)
                if !*ctx.current_search_is_ponder {
                    if let Some(ref _emitter) = ctx.current_bestmove_emitter {
                        let adapter = crate::worker::lock_or_recover_adapter(ctx.engine);
                        if let Some((bm, pv, src)) =
                            adapter.choose_final_bestmove_core(ctx.current_committed.as_ref())
                        {
                            let info = crate::usi::output::SearchInfo {
                                multipv: Some(1),
                                pv,
                                ..Default::default()
                            };
                            ctx.inject_final_pv(info, "searchfinished_core_finalize");
                            let meta = build_meta(
                                BestmoveSource::CoreFinalize,
                                0,
                                None,
                                Some(format!("string core_src={src}")),
                                stop_info,
                            );
                            ctx.emit_and_finalize(bm, None, meta, "SearchFinishedCoreFinalize")?;
                            return Ok(());
                        } else {
                            log::warn!("Core finalize selection unavailable at SearchFinished; emitter present but engine/position missing");
                            return Ok(());
                        }
                    } else {
                        log::debug!("SearchFinished: emitter not available; ignoring direct send (likely already emitted)");
                        return Ok(());
                    }
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

                // Try core finalize first (book→committed→TT→legal/resign)
                if !*ctx.current_search_is_ponder {
                    let adapter = crate::worker::lock_or_recover_adapter(ctx.engine);
                    if let Some((bm, pv, src)) =
                        adapter.choose_final_bestmove_core(ctx.current_committed.as_ref())
                    {
                        let info = crate::usi::output::SearchInfo {
                            multipv: Some(1),
                            pv,
                            ..Default::default()
                        };
                        ctx.inject_final_pv(info, "core_finalize_on_finished");
                        let meta = build_meta(
                            BestmoveSource::CoreFinalize,
                            0,
                            None,
                            Some(format!("string core_src={src}")),
                            None,
                        );
                        ctx.emit_and_finalize(bm, None, meta, "CoreFinalizeOnFinished")?;
                        return Ok(());
                    }
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
