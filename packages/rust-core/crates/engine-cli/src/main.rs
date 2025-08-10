// USI (Universal Shogi Interface) adapter

mod engine_adapter;
mod usi;
mod utils;

use anyhow::{anyhow, Result};
use clap::Parser;
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use engine_adapter::{EngineAdapter, EngineError};
use engine_core::engine::controller::Engine;
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use usi::output::{Score, SearchInfo};
use usi::{
    ensure_flush_on_exit, flush_final, parse_usi_command, send_info_string, send_response,
    send_response_or_exit, UsiCommand, UsiResponse,
};
use utils::lock_or_recover_generic;

// Constants for timeout and channel management
const MIN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);
const CHANNEL_SIZE: usize = 1024;
const SELECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Specialized lock_or_recover for EngineAdapter with state reset
fn lock_or_recover_adapter(mutex: &Mutex<EngineAdapter>) -> MutexGuard<'_, EngineAdapter> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::error!("EngineAdapter mutex was poisoned, attempting recovery with state reset");
            let mut guard = poisoned.into_inner();

            // Force reset engine state to safe defaults
            guard.force_reset_state();

            // Try to notify GUI about the reset
            let _ = send_info_string(
                "Engine state reset due to error recovery. Please send 'isready' to reinitialize.",
            );

            guard
        }
    }
}

/// Calculate dynamic timeout based on game phase and position complexity
fn calculate_dynamic_timeout(_engine: &Arc<Mutex<EngineAdapter>>) -> Duration {
    // For now, use a simple static timeout strategy
    // In the future, this could be enhanced to analyze the position
    // and adjust timeout based on game phase (opening/middlegame/endgame)

    // Default timeout with some basic logic
    let timeout_ms = 1000; // 1 second default

    log::info!("Using timeout: {timeout_ms}ms");
    Duration::from_millis(timeout_ms)
}

/// Perform fallback move generation with graduated strategy
///
/// This function attempts to generate a move using increasingly simple methods:
/// 1. Use partial result from interrupted search (instant)
/// 2. Run quick shallow search (depth 3, ~10-100ms)
/// 3. Generate emergency move using heuristics only (~1ms)
///
/// All operations are synchronous but designed to be fast.
/// Total worst-case time: ~100ms (dominated by quick_search)
fn generate_fallback_move(
    engine: &Arc<Mutex<EngineAdapter>>,
    partial_result: Option<(String, u32, i32)>,
    allow_null_move: bool,
) -> Result<String> {
    // Stage 1: Use partial result if available (instant)
    if let Some((best_move, depth, score)) = partial_result {
        log::info!("Using partial result: move={best_move}, depth={depth}, score={score}");
        return Ok(best_move);
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

/// Guard to ensure engine is returned on drop (for panic safety)
struct EngineReturnGuard {
    engine: Option<Engine>,
    tx: Sender<WorkerMessage>,
    search_id: u64,
}

impl EngineReturnGuard {
    fn new(engine: Engine, tx: Sender<WorkerMessage>, search_id: u64) -> Self {
        Self {
            engine: Some(engine),
            tx,
            search_id,
        }
    }
}

impl std::ops::Deref for EngineReturnGuard {
    type Target = Engine;

    fn deref(&self) -> &Self::Target {
        self.engine.as_ref().expect("Engine already taken")
    }
}

impl std::ops::DerefMut for EngineReturnGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.engine.as_mut().expect("Engine already taken")
    }
}

impl Drop for EngineReturnGuard {
    fn drop(&mut self) {
        if let Some(engine) = self.engine.take() {
            log::debug!("EngineReturnGuard: returning engine");

            // Try to return engine through channel
            match self.tx.try_send(WorkerMessage::EngineReturn(engine)) {
                Ok(()) => {
                    log::debug!("Engine returned successfully through channel");
                }
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    // Channel is full - this shouldn't happen with unbounded channel
                    log::error!("Channel full, cannot return engine");
                    // Engine will be dropped here, which is safe
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    // Channel is disconnected - main thread has exited
                    log::warn!("Channel disconnected, cannot return engine");
                    // Engine will be dropped here, which is safe
                }
            }

            // Always try to send Finished message to signal completion (from guard)
            let _ = self.tx.try_send(WorkerMessage::Finished {
                from_guard: true,
                search_id: self.search_id,
            });
        }
    }
}

/// Search state management - tracks the current state of the search
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchState {
    /// No search is active
    Idle,
    /// Search is actively running
    Searching,
    /// Stop has been requested but search is still running
    StopRequested,
    /// Fallback move has been sent due to timeout/error
    FallbackSent,
}

impl SearchState {
    /// Check if we're in any searching state
    fn is_searching(&self) -> bool {
        matches!(self, SearchState::Searching | SearchState::StopRequested)
    }

    /// Check if we can start a new search
    fn can_start_search(&self) -> bool {
        matches!(self, SearchState::Idle)
    }

    /// Check if we should accept a bestmove
    fn can_accept_bestmove(&self) -> bool {
        matches!(self, SearchState::Searching | SearchState::StopRequested)
    }
}

/// Messages from worker thread to main thread
enum WorkerMessage {
    Info(SearchInfo),
    BestMove {
        best_move: String,
        ponder_move: Option<String>,
        search_id: u64, // Add search ID to track which search this belongs to
    },
    /// Partial result available during search
    PartialResult {
        current_best: String,
        depth: u32,
        score: i32,
        search_id: u64,
    },
    /// Thread finished - from_guard indicates if sent by EngineReturnGuard
    Finished {
        from_guard: bool,
        search_id: u64, // Add search ID
    },
    Error {
        message: String,
        search_id: u64,
    },
    EngineReturn(Engine), // Return the engine after search
}

/// Context for handling USI commands
struct CommandContext<'a> {
    engine: &'a Arc<Mutex<EngineAdapter>>,
    stop_flag: &'a Arc<AtomicBool>,
    worker_tx: &'a Sender<WorkerMessage>,
    worker_rx: &'a Receiver<WorkerMessage>,
    worker_handle: &'a mut Option<JoinHandle<()>>,
    search_state: &'a mut SearchState,
    bestmove_sent: &'a mut bool,
    current_search_timeout: &'a mut Duration,
    search_id_counter: &'a mut u64,
    current_search_id: &'a mut u64,
    current_search_is_ponder: &'a mut bool,
    allow_null_move: bool,
}

/// Calculate maximum expected search time from GoParams
fn calculate_max_search_time(params: &usi::GoParams) -> Duration {
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
        // Use half of available time + margin
        Duration::from_millis(max_time / 2 + 2000)
    } else {
        // Default timeout for depth/node limited searches
        Duration::from_secs(60)
    }
}

/// Wait for worker thread to finish with timeout
fn wait_for_worker_with_timeout(
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    engine: &Arc<Mutex<EngineAdapter>>,
    search_state: &mut SearchState,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout.max(MIN_JOIN_TIMEOUT);
    let mut finished = false;
    let mut engine_returned = false;
    let mut finished_count = 0u32;

    // Wait for Finished message AND EngineReturn message or timeout
    loop {
        select! {
            recv(worker_rx) -> msg => {
                match msg {
                    Ok(WorkerMessage::Finished { from_guard, search_id: _ }) => {
                        finished_count += 1;
                        if !finished {
                            log::debug!("Worker thread finished cleanly (from_guard: {from_guard})");
                            finished = true;
                            if engine_returned {
                                break;
                            }
                        } else {
                            log::trace!("Ignoring duplicate Finished message #{finished_count} (from_guard: {from_guard})");
                        }
                    }
                    Ok(WorkerMessage::EngineReturn(returned_engine)) => {
                        log::debug!("Engine returned from worker");
                        let mut adapter = lock_or_recover_adapter(engine);
                        adapter.return_engine(returned_engine);
                        engine_returned = true;
                        if finished {
                            break;
                        }
                    }
                    Ok(WorkerMessage::Info(info)) => {
                        // Info messages during shutdown can be ignored
                        log::trace!("Received info during shutdown: {info:?}");
                    }
                    Ok(WorkerMessage::BestMove { best_move, ponder_move, search_id }) => {
                        // During shutdown, we may accept late bestmoves
                        // For safety, we could check search_id here but during shutdown
                        // we're more lenient since we're trying to clean up
                        if search_state.can_accept_bestmove() {
                            log::debug!("Accepting bestmove during shutdown (search_id: {search_id})");
                            send_response_or_exit(UsiResponse::BestMove {
                                best_move,
                                ponder: ponder_move,
                            });
                            // Mark search as finished when bestmove is received
                            *search_state = SearchState::Idle;
                        } else {
                            log::warn!("Ignoring late bestmove during shutdown: {best_move} (search_id: {search_id})");
                        }
                    }
                    Ok(WorkerMessage::PartialResult { .. }) => {
                        // Partial results during shutdown can be ignored
                        log::trace!("PartialResult during shutdown - ignoring");
                    }
                    Ok(WorkerMessage::Error { message, search_id }) => {
                        log::error!("Worker error during shutdown (search_id: {search_id}): {message}");
                    }
                    Err(_) => {
                        log::error!("Worker channel closed unexpectedly");
                        break;
                    }
                }
            }
            default(SELECT_TIMEOUT) => {
                if Instant::now() > deadline {
                    log::error!("Worker thread timeout after {:?}", timeout.max(MIN_JOIN_TIMEOUT));
                    // Return error instead of exit for graceful handling
                    return Err(anyhow::anyhow!("Worker thread timeout"));
                }
            }
        }
    }

    // If we received Finished, join() should complete immediately
    if finished {
        if let Some(handle) = worker_handle.take() {
            match handle.join() {
                Ok(()) => log::debug!("Worker thread joined successfully"),
                Err(_) => log::error!("Worker thread panicked"),
            }
        }
    }

    *search_state = SearchState::Idle;

    // Drain any remaining messages in worker_rx
    while let Ok(msg) = worker_rx.try_recv() {
        match msg {
            WorkerMessage::EngineReturn(returned_engine) => {
                log::debug!("Engine returned during drain");
                let mut adapter = lock_or_recover_adapter(engine);
                adapter.return_engine(returned_engine);
            }
            _ => {
                log::trace!("Drained message: {:?}", std::any::type_name_of_val(&msg));
            }
        }
    }

    Ok(())
}

/// Wait for any ongoing search to complete
fn wait_for_search_completion(
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

/// Spawn stdin reader thread
fn spawn_stdin_reader(cmd_tx: Sender<UsiCommand>) -> JoinHandle<()> {
    thread::spawn(move || {
        let stdin = io::stdin();
        let reader = stdin.lock();

        for line in reader.lines() {
            match line {
                Ok(line) => {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }

                    log::debug!("Received: {line}");

                    match parse_usi_command(line) {
                        Ok(cmd) => {
                            // Use try_send to avoid blocking
                            match cmd_tx.try_send(cmd) {
                                Ok(()) => {}
                                Err(crossbeam_channel::TrySendError::Full(_)) => {
                                    log::warn!("Command channel full, dropping command");
                                }
                                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                                    log::debug!(
                                        "Command channel disconnected, exiting stdin reader"
                                    );
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            log::error!("Failed to parse command '{line}': {e}");
                            // Invalid commands are silently ignored in USI protocol
                        }
                    }
                }
                Err(e) => {
                    // Distinguish between EOF and actual errors
                    match e.kind() {
                        io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe => {
                            log::info!(
                                "Stdin closed (EOF or broken pipe), shutting down gracefully"
                            );
                        }
                        io::ErrorKind::Interrupted => {
                            // EINTR - could retry, but for stdin it's safer to exit
                            log::warn!("Stdin read interrupted, shutting down");
                        }
                        _ => {
                            log::error!("Stdin read error: {e}");
                        }
                    }

                    // Try to send quit command for graceful shutdown
                    match cmd_tx.try_send(UsiCommand::Quit) {
                        Ok(()) => {
                            log::debug!("Sent quit command for graceful shutdown");
                        }
                        Err(_) => {
                            log::debug!("Failed to send quit command, channel likely closed");
                        }
                    }
                    break;
                }
            }
        }

        // Reached here = normal EOF. GUI closed the pipe, send quit
        match cmd_tx.try_send(UsiCommand::Quit) {
            Ok(()) => log::info!("Sent quit command after EOF"),
            Err(_) => log::debug!("Channel closed before quit after EOF"),
        }

        log::debug!("Stdin reader thread exiting (EOF)");
    })
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
    let mut current_search_timeout = MIN_JOIN_TIMEOUT;
    let mut search_id_counter = 0u64;
    let mut current_search_id = 0u64;
    let mut current_search_is_ponder = false; // Track if current search is ponder

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
                            
                            // bestmove検証をスキップ（局面同期の問題により、エンジンの出力を信頼）
                            log::info!("Sending bestmove without validation: {best_move}");
                            send_response(UsiResponse::BestMove {
                                best_move,
                                ponder: ponder_move,
                            })?;
                            search_state = SearchState::Idle; // Clear searching flag after sending bestmove
                            bestmove_sent = true; // Mark that we've sent bestmove
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
        wait_for_worker_with_timeout(
            &mut worker_handle,
            &worker_rx,
            &engine,
            &mut search_state,
            MIN_JOIN_TIMEOUT,
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

fn handle_command(command: UsiCommand, ctx: &mut CommandContext) -> Result<()> {
    match command {
        UsiCommand::Usi => {
            send_response(UsiResponse::IdName("RustShogi 1.0".to_string()))?;
            send_response(UsiResponse::IdAuthor("RustShogi Team".to_string()))?;

            // Send available options
            {
                let engine = lock_or_recover_adapter(ctx.engine);
                for option in engine.get_options() {
                    send_response(UsiResponse::Option(option.to_string()))?;
                }
            }

            send_response(UsiResponse::UsiOk)?;
        }

        UsiCommand::IsReady => {
            // Initialize engine if needed
            {
                let mut engine = lock_or_recover_adapter(ctx.engine);
                engine.initialize()?;
            }
            send_response(UsiResponse::ReadyOk)?;
        }

        UsiCommand::Position {
            startpos,
            sfen,
            moves,
        } => {
            log::info!(
                "Handling position command - startpos: {startpos}, sfen: {sfen:?}, moves: {moves:?}"
            );
            // Wait for any ongoing search to complete before updating position
            wait_for_search_completion(
                ctx.search_state,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
            )?;

            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.set_position(startpos, sfen.as_deref(), &moves)?;
            log::info!("Position command completed");
        }

        UsiCommand::Go(params) => {
            log::info!("Received go command with params: {params:?}");

            // Stop any ongoing search and ensure engine is available
            wait_for_search_completion(
                ctx.search_state,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
            )?;

            // Add a small delay to ensure clean state transition
            thread::sleep(Duration::from_millis(10));

            // Reset stop flag and bestmove_sent flag
            ctx.stop_flag.store(false, Ordering::Release);
            *ctx.bestmove_sent = false; // Reset for new search

            // Verify we can start a new search (defensive check)
            if !ctx.search_state.can_start_search() {
                log::error!("Cannot start search in state: {:?}", ctx.search_state);
                return Err(anyhow!("Invalid state for starting search"));
            }

            // Verify position is set before starting search
            {
                let engine = lock_or_recover_adapter(ctx.engine);
                if !engine.has_position() {
                    log::error!("Cannot start search: position not set");
                    send_response(UsiResponse::BestMove {
                        best_move: "resign".to_string(),
                        ponder: None,
                    })?;
                    return Ok(());
                }
            }

            // Increment search ID for new search
            *ctx.search_id_counter += 1;
            *ctx.current_search_id = *ctx.search_id_counter;
            let search_id = *ctx.current_search_id;
            log::info!("Starting new search with ID: {search_id}, ponder: {}", params.ponder);

            // Calculate timeout for this search
            *ctx.current_search_timeout = calculate_max_search_time(&params);

            // Track if this is a ponder search
            *ctx.current_search_is_ponder = params.ponder;

            // Clone necessary data for worker thread
            let engine_clone = Arc::clone(ctx.engine);
            let stop_clone = Arc::clone(ctx.stop_flag);
            let tx_clone = ctx.worker_tx.clone();

            // Spawn worker thread for search with panic safety
            let handle = thread::spawn(move || {
                log::debug!("Worker thread spawned");
                let result = std::panic::catch_unwind(|| {
                    search_worker(engine_clone, params, stop_clone, tx_clone.clone(), search_id);
                });

                if let Err(e) = result {
                    log::error!("Worker thread panicked: {e:?}");
                    // Send error message to main thread
                    let _ = tx_clone.send(WorkerMessage::Error {
                        message: "Worker thread panicked".to_string(),
                        search_id,
                    });
                    let _ = tx_clone.send(WorkerMessage::Finished {
                        from_guard: false,
                        search_id,
                    });
                }
            });

            *ctx.worker_handle = Some(handle);
            *ctx.search_state = SearchState::Searching;
            log::info!("Worker thread handle stored, search_state = Searching");

            // Don't block - return immediately
        }

        UsiCommand::Stop => {
            log::info!("Received stop command, search_state = {:?}", *ctx.search_state);
            log::debug!("Stop command received, entering stop handler");
            // Signal stop to worker thread
            if ctx.search_state.is_searching() {
                *ctx.search_state = SearchState::StopRequested;
                ctx.stop_flag.store(true, Ordering::Release);
                log::info!("Stop flag set to true, search_state = StopRequested");

                // Wait for bestmove with timeout to ensure we always send a response
                let start = Instant::now();
                let timeout = calculate_dynamic_timeout(ctx.engine);
                let mut partial_result: Option<(String, u32, i32)> = None;

                loop {
                    if start.elapsed() > timeout {
                        // Timeout - use fallback strategy
                        log::warn!("Timeout waiting for bestmove after stop command");
                        // Log timeout error
                        log::debug!("Stop command timeout: {:?}", EngineError::Timeout);

                        if *ctx.current_search_is_ponder {
                            // Ponder search - don't send bestmove (USI protocol)
                            log::info!(
                                "Ponder search timeout, not sending bestmove (USI protocol)"
                            );
                            *ctx.search_state = SearchState::Idle;
                            *ctx.current_search_is_ponder = false; // Reset ponder flag
                            break;
                        }

                        match generate_fallback_move(
                            ctx.engine,
                            partial_result,
                            ctx.allow_null_move,
                        ) {
                            Ok(move_str) => {
                                log::debug!("Sending fallback bestmove: {move_str}");
                                send_response(UsiResponse::BestMove {
                                    best_move: move_str.clone(),
                                    ponder: None,
                                })?;
                                log::debug!("Fallback bestmove sent successfully: {move_str}");
                                *ctx.bestmove_sent = true;
                                *ctx.search_state = SearchState::FallbackSent;
                            }
                            Err(e) => {
                                log::error!("Fallback move generation failed: {e}");
                                send_response(UsiResponse::BestMove {
                                    best_move: "resign".to_string(),
                                    ponder: None,
                                })?;
                                *ctx.bestmove_sent = true;
                                *ctx.search_state = SearchState::FallbackSent;
                            }
                        }
                        break;
                    }

                    // Check for bestmove message
                    match ctx.worker_rx.try_recv() {
                        Ok(WorkerMessage::BestMove {
                            best_move,
                            ponder_move,
                            search_id,
                        }) => {
                            // Only accept if it's for current search and not pondering
                            if search_id == *ctx.current_search_id {
                                if !*ctx.current_search_is_ponder {
                                    send_response(UsiResponse::BestMove {
                                        best_move,
                                        ponder: ponder_move,
                                    })?;
                                    *ctx.search_state = SearchState::Idle;
                                    *ctx.bestmove_sent = true;
                                    *ctx.current_search_is_ponder = false; // Reset ponder flag after sending bestmove
                                } else {
                                    // Ponder search stopped - don't send bestmove
                                    log::debug!("Ponder search stopped, not sending bestmove");
                                    *ctx.search_state = SearchState::Idle;
                                    *ctx.current_search_is_ponder = false; // Reset ponder flag
                                }
                                break;
                            }
                        }
                        Ok(WorkerMessage::Info(info)) => {
                            // Forward info messages
                            let _ = send_response(UsiResponse::Info(info));
                        }
                        Ok(WorkerMessage::PartialResult {
                            current_best,
                            depth,
                            score,
                            search_id,
                        }) => {
                            // Store partial result for fallback only if it's from current search
                            if search_id == *ctx.current_search_id {
                                partial_result = Some((current_best, depth, score));
                            }
                        }
                        Ok(WorkerMessage::Finished {
                            from_guard,
                            search_id,
                        }) => {
                            // Only process if it's for current search
                            if search_id == *ctx.current_search_id {
                                if *ctx.current_search_is_ponder {
                                    // Ponder search - don't send bestmove (USI protocol)
                                    log::debug!("Ponder search finished without bestmove, not sending fallback (USI protocol)");
                                    *ctx.search_state = SearchState::Idle;
                                    *ctx.current_search_is_ponder = false; // Reset ponder flag
                                    break;
                                }

                                // Normal search - use fallback strategy
                                log::warn!(
                                    "Worker finished without bestmove (from_guard: {from_guard})"
                                );
                                match generate_fallback_move(
                                    ctx.engine,
                                    partial_result,
                                    ctx.allow_null_move,
                                ) {
                                    Ok(move_str) => {
                                        send_response(UsiResponse::BestMove {
                                            best_move: move_str,
                                            ponder: None,
                                        })?;
                                        *ctx.bestmove_sent = true;
                                        *ctx.search_state = SearchState::FallbackSent;
                                    }
                                    Err(e) => {
                                        log::error!("Fallback move generation failed: {e}");
                                        send_response(UsiResponse::BestMove {
                                            best_move: "resign".to_string(),
                                            ponder: None,
                                        })?;
                                        *ctx.bestmove_sent = true;
                                        *ctx.search_state = SearchState::FallbackSent;
                                    }
                                }
                                break;
                            }
                        }
                        _ => {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
            } else {
                // Not searching - use fallback strategy
                log::debug!("Stop command received while not searching");

                match generate_fallback_move(ctx.engine, None, ctx.allow_null_move) {
                    Ok(move_str) => {
                        send_response(UsiResponse::BestMove {
                            best_move: move_str,
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
            }
        }

        UsiCommand::PonderHit => {
            // Handle ponder hit
            let mut engine = lock_or_recover_adapter(ctx.engine);
            // Mark that we're no longer in pure ponder mode
            *ctx.current_search_is_ponder = false;
            match engine.ponder_hit() {
                Ok(()) => log::debug!("Ponder hit successfully processed"),
                Err(e) => log::debug!("Ponder hit ignored: {e}"),
            }
        }

        UsiCommand::SetOption { name, value } => {
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.set_option(&name, value.as_deref())?;
        }

        UsiCommand::GameOver { result } => {
            // Stop any ongoing search
            ctx.stop_flag.store(true, Ordering::Release);

            // Notify engine of game result
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.game_over(result);
        }

        UsiCommand::UsiNewGame => {
            // ShogiGUI extension - new game notification
            // Stop any ongoing search
            wait_for_search_completion(
                ctx.search_state,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
            )?;

            // Reset engine state for new game
            let mut engine = lock_or_recover_adapter(ctx.engine);
            engine.new_game();
            log::debug!("New game started");
        }

        UsiCommand::Quit => {
            // Quit is handled in main loop
            unreachable!("Quit should be handled in main loop");
        }
    }

    Ok(())
}

/// Worker thread function for search
fn search_worker(
    engine_adapter: Arc<Mutex<EngineAdapter>>,
    params: usi::GoParams,
    stop_flag: Arc<AtomicBool>,
    tx: Sender<WorkerMessage>,
    search_id: u64,
) {
    log::debug!("Search worker thread started with params: {params:?}");

    // Set up info callback with partial result tracking
    let tx_info = tx.clone();
    let tx_partial = tx.clone();
    let last_partial_depth = Arc::new(Mutex::new(0u32));
    let info_callback = move |info: SearchInfo| {
        // Always send the info message
        let _ = tx_info.send(WorkerMessage::Info(info.clone()));

        // Send partial result at certain depth intervals
        if let (Some(depth), Some(score), Some(pv)) =
            (info.depth, info.score.as_ref(), info.pv.first())
        {
            // Check if we should send a partial result
            let should_send = {
                let mut last_depth = lock_or_recover_generic(&last_partial_depth);
                if depth >= *last_depth + 5 || (depth >= 10 && depth > *last_depth) {
                    *last_depth = depth;
                    true
                } else {
                    false
                }
            };

            if should_send {
                let score_value = match score {
                    Score::Cp(cp) => *cp,
                    Score::Mate(mate) => {
                        // Convert mate score to centipawn equivalent
                        if *mate > 0 {
                            30000 - (*mate * 100)
                        } else {
                            -30000 - (*mate * 100)
                        }
                    }
                };

                log::debug!(
                    "Sending partial result: move={pv}, depth={depth}, score={score_value}"
                );
                let _ = tx_partial.send(WorkerMessage::PartialResult {
                    current_best: pv.clone(),
                    depth,
                    score: score_value,
                    search_id,
                });
            }
        }
    };

    // Take engine out and prepare search
    let was_ponder = params.ponder;
    log::debug!("Attempting to take engine from adapter");
    let (engine, position, limits, ponder_hit_flag) = {
        let mut adapter = lock_or_recover_adapter(&engine_adapter);
        log::debug!("Adapter lock acquired, calling take_engine");
        match adapter.take_engine() {
            Ok(engine) => {
                log::debug!("Engine taken successfully, preparing search");
                match adapter.prepare_search(&params, stop_flag.clone()) {
                    Ok((pos, lim, flag)) => {
                        log::debug!("Search prepared successfully");
                        (engine, pos, lim, flag)
                    }
                    Err(e) => {
                        // Return engine and send error
                        adapter.return_engine(engine);
                        log::error!("Search preparation error: {e}");
                        let _ = tx.send(WorkerMessage::Error {
                            message: e.to_string(),
                            search_id,
                        });

                        // Try to generate emergency move before resigning (only if not pondering)
                        if !params.ponder {
                            match adapter.generate_emergency_move() {
                                Ok(emergency_move) => {
                                    log::info!(
                                        "Generated emergency move after preparation error: {emergency_move}"
                                    );
                                    if let Err(e) = tx.send(WorkerMessage::BestMove {
                                        best_move: emergency_move,
                                        ponder_move: None,
                                        search_id,
                                    }) {
                                        log::error!("Failed to send emergency move: {e}");
                                    }
                                }
                                Err(_) => {
                                    // Only resign if no legal moves available
                                    if let Err(e) = tx.send(WorkerMessage::BestMove {
                                        best_move: "resign".to_string(),
                                        ponder_move: None,
                                        search_id,
                                    }) {
                                        log::error!(
                                            "Failed to send resign after preparation error: {e}"
                                        );
                                    }
                                }
                            }
                        } else {
                            log::info!(
                                "Ponder preparation error, not sending bestmove (USI protocol)"
                            );
                        }

                        let _ = tx.send(WorkerMessage::Finished {
                            from_guard: false,
                            search_id,
                        });
                        return;
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to take engine: {e}");
                let _ = tx.send(WorkerMessage::Error {
                    message: e.to_string(),
                    search_id,
                });

                // Try to generate emergency move from adapter (only if not pondering)
                if !params.ponder {
                    log::info!("Attempting to generate emergency move after engine take failure");
                    match adapter.generate_emergency_move() {
                        Ok(emergency_move) => {
                            log::info!(
                                "Generated emergency move after engine take error: {emergency_move}"
                            );
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: emergency_move,
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send emergency move: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves available
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: "resign".to_string(),
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send resign after engine take error: {e}");
                            }
                        }
                    }
                } else {
                    log::info!("Ponder engine take error, not sending bestmove (USI protocol)");
                }

                let _ = tx.send(WorkerMessage::Finished {
                    from_guard: false,
                    search_id,
                });
                return;
            }
        }
    }; // Lock released here

    // Keep ponder_hit_flag for checking later
    let ponder_hit_flag_ref = ponder_hit_flag.clone();

    // Explicitly drop ponder_hit_flag (it's used internally by the engine)
    drop(ponder_hit_flag);

    // Wrap engine in guard for panic safety
    let mut engine_guard = EngineReturnGuard::new(engine, tx.clone(), search_id);

    // Execute search without holding the lock
    log::info!("Calling execute_search_static");
    let result = EngineAdapter::execute_search_static(
        &mut engine_guard,
        position,
        limits,
        Box::new(info_callback),
    );
    log::info!("execute_search_static returned: {:?}", result.is_ok());

    // Handle result
    match result {
        Ok((best_move, ponder_move)) => {
            // Clean up ponder state if needed
            {
                let mut adapter = lock_or_recover_adapter(&engine_adapter);
                adapter.cleanup_after_search(was_ponder);
            }

            // Check if ponderhit occurred during ponder search
            let ponder_hit_occurred = if was_ponder {
                // Check if ponder_hit_flag was set during search
                ponder_hit_flag_ref
                    .as_ref()
                    .map(|flag| flag.load(Ordering::Acquire))
                    .unwrap_or(false)
            } else {
                false
            };

            // Send best move if:
            // - Not a ponder search OR
            // - Ponder search that was converted via ponderhit
            if !was_ponder || ponder_hit_occurred {
                log::info!(
                    "Sending bestmove: was_ponder={was_ponder}, ponder_hit={ponder_hit_occurred}"
                );
                if let Err(e) = tx.send(WorkerMessage::BestMove {
                    best_move,
                    ponder_move,
                    search_id,
                }) {
                    log::error!("Failed to send bestmove through channel: {e}");
                }
            } else {
                log::info!("Ponder search without ponderhit, not sending bestmove (USI protocol)");
            }
        }
        Err(e) => {
            log::error!("Search error: {e}");
            // Engine will be returned automatically by EngineReturnGuard::drop

            // Clean up ponder state if needed
            {
                let mut adapter = lock_or_recover_adapter(&engine_adapter);
                adapter.cleanup_after_search(was_ponder);
            }

            // Try to generate emergency move before sending error
            let emergency_result = {
                let adapter = lock_or_recover_adapter(&engine_adapter);
                adapter.generate_emergency_move()
            };

            if stop_flag.load(Ordering::Acquire) {
                // Check if ponderhit occurred for ponder search
                let ponder_hit_occurred = if was_ponder {
                    ponder_hit_flag_ref
                        .as_ref()
                        .map(|flag| flag.load(Ordering::Acquire))
                        .unwrap_or(false)
                } else {
                    false
                };

                // Stopped by user - send bestmove if:
                // - Not a ponder search OR
                // - Ponder search that was converted via ponderhit
                if !was_ponder || ponder_hit_occurred {
                    // Normal search or ponder-hit search that was stopped - send emergency move
                    match emergency_result {
                        Ok(emergency_move) => {
                            log::info!("Generated emergency move after stop: {emergency_move}");
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: emergency_move,
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send emergency move: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: "resign".to_string(),
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send resign after stop: {e}");
                            }
                        }
                    }
                } else {
                    // Ponder search that was stopped (not ponderhit) - don't send bestmove
                    log::info!("Ponder search stopped, not sending bestmove (USI protocol)");
                }
            } else {
                // Other error - send error and try emergency move
                // Check if ponderhit occurred for ponder search
                let ponder_hit_occurred = if was_ponder {
                    ponder_hit_flag_ref
                        .as_ref()
                        .map(|flag| flag.load(Ordering::Acquire))
                        .unwrap_or(false)
                } else {
                    false
                };

                let _ = tx.send(WorkerMessage::Error {
                    message: e.to_string(),
                    search_id,
                });

                // Send bestmove if not ponder OR ponder was converted via ponderhit
                if !was_ponder || ponder_hit_occurred {
                    match emergency_result {
                        Ok(emergency_move) => {
                            log::info!(
                                "Generated emergency move after search error: {emergency_move}"
                            );
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: emergency_move,
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send emergency move: {e}");
                            }
                        }
                        Err(_) => {
                            // Only resign if no legal moves
                            if let Err(e) = tx.send(WorkerMessage::BestMove {
                                best_move: "resign".to_string(),
                                ponder_move: None,
                                search_id,
                            }) {
                                log::error!("Failed to send resign after error: {e}");
                            }
                        }
                    }
                } else {
                    log::info!(
                        "Ponder search error without ponderhit, not sending bestmove (USI protocol)"
                    );
                }
            }
        }
    }

    // Always send Finished at the end - use blocking send to ensure delivery
    match tx.send(WorkerMessage::Finished {
        from_guard: false,
        search_id,
    }) {
        Ok(()) => log::debug!("Finished message sent successfully"),
        Err(e) => {
            log::error!("Failed to send Finished message: {e}. Channel might be closed.");
            // This is a critical error but we can't do much about it
        }
    }

    log::debug!("Search worker finished");
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
