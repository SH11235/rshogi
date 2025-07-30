// USI (Universal Shogi Interface) adapter

mod engine_adapter;
mod usi;

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use engine_adapter::EngineAdapter;
use engine_core::engine::controller::Engine;
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use usi::output::SearchInfo;
use usi::{
    ensure_flush_on_exit, flush_final, parse_usi_command, send_info_string, send_response,
    send_response_or_exit, UsiCommand, UsiResponse,
};

// Constants for timeout and channel management
const MIN_JOIN_TIMEOUT: Duration = Duration::from_secs(5);
const CHANNEL_SIZE: usize = 1024;
const SELECT_TIMEOUT: Duration = Duration::from_millis(50);

/// Helper function to lock a mutex with recovery for Poisoned state
fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            log::error!("Mutex was poisoned, attempting recovery");
            poisoned.into_inner()
        }
    }
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Enable debug logging
    #[arg(short, long)]
    debug: bool,
}

/// Guard to ensure engine is returned on drop (for panic safety)
struct EngineReturnGuard {
    engine: Option<Engine>,
    tx: Sender<WorkerMessage>,
}

impl EngineReturnGuard {
    fn new(engine: Engine, tx: Sender<WorkerMessage>) -> Self {
        Self {
            engine: Some(engine),
            tx,
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
            if let Err(e) = self.tx.try_send(WorkerMessage::EngineReturn(engine)) {
                log::warn!("Failed to return engine through channel: {e:?}");
            }
        }
    }
}

/// Messages from worker thread to main thread
enum WorkerMessage {
    Info(SearchInfo),
    BestMove {
        best_move: String,
        ponder_move: Option<String>,
    },
    Finished, // Thread finished successfully
    Error(String),
    EngineReturn(Engine), // Return the engine after search
}

/// Context for handling USI commands
struct CommandContext<'a> {
    engine: &'a Arc<Mutex<EngineAdapter>>,
    stop_flag: &'a Arc<AtomicBool>,
    worker_tx: &'a Sender<WorkerMessage>,
    worker_rx: &'a Receiver<WorkerMessage>,
    worker_handle: &'a mut Option<JoinHandle<()>>,
    searching: &'a mut bool,
    stdout: &'a mut dyn Write,
    current_search_timeout: &'a mut Duration,
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
    searching: &mut bool,
    _stdout: &mut dyn Write,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout.max(MIN_JOIN_TIMEOUT);
    let mut finished = false;
    let mut engine_returned = false;

    // Wait for Finished message AND EngineReturn message or timeout
    loop {
        select! {
            recv(worker_rx) -> msg => {
                match msg {
                    Ok(WorkerMessage::Finished) => {
                        log::debug!("Worker thread finished cleanly");
                        finished = true;
                        if engine_returned {
                            break;
                        }
                    }
                    Ok(WorkerMessage::EngineReturn(returned_engine)) => {
                        log::debug!("Engine returned from worker");
                        let mut adapter = lock_or_recover(engine);
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
                    Ok(WorkerMessage::BestMove { best_move, ponder_move }) => {
                        // Forward bestmove to stdout instead of discarding it
                        send_response_or_exit(UsiResponse::BestMove {
                            best_move,
                            ponder: ponder_move,
                        });
                        // Mark search as finished when bestmove is received
                        *searching = false;
                    }
                    Ok(WorkerMessage::Error(err)) => {
                        log::error!("Worker error during shutdown: {err}");
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

    *searching = false;

    // Drain any remaining messages in worker_rx
    while let Ok(msg) = worker_rx.try_recv() {
        match msg {
            WorkerMessage::EngineReturn(returned_engine) => {
                log::debug!("Engine returned during drain");
                let mut adapter = lock_or_recover(engine);
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
    searching: &mut bool,
    stop_flag: &Arc<AtomicBool>,
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    engine: &Arc<Mutex<EngineAdapter>>,
    stdout: &mut dyn Write,
) -> Result<()> {
    if *searching {
        stop_flag.store(true, Ordering::Release);
        wait_for_worker_with_timeout(
            worker_handle,
            worker_rx,
            engine,
            searching,
            stdout,
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
    if let Err(e) = run_engine() {
        log::error!("Fatal error: {e}");
        std::process::exit(1);
    }
}

fn run_engine() -> Result<()> {
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
    let mut searching = false;
    let mut current_search_timeout = MIN_JOIN_TIMEOUT;

    // Get stdout handle
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    // Main event loop
    loop {
        select! {
            recv(cmd_rx) -> msg => {
                match msg {
                    Ok(cmd) => {
                        log::debug!("USI command received: {cmd:?}");

                        // Check if it's quit command
                        if matches!(cmd, UsiCommand::Quit) {
                            // Handle quit
                            stop_flag.store(true, Ordering::Release);
                            break;
                        }

                        // Handle other commands
                        let mut ctx = CommandContext {
                            engine: &engine,
                            stop_flag: &stop_flag,
                            worker_tx: &worker_tx,
                            worker_rx: &worker_rx,
                            worker_handle: &mut worker_handle,
                            searching: &mut searching,
                            stdout: &mut stdout,
                            current_search_timeout: &mut current_search_timeout,
                        };
                        handle_command(cmd, &mut ctx)?;
                    }
                    Err(_) => {
                        log::debug!("Command channel closed");
                        break;
                    }
                }
            }

            recv(worker_rx) -> msg => {
                match msg {
                    Ok(WorkerMessage::Info(info)) => {
                        send_response(UsiResponse::Info(info))?;
                    }
                    Ok(WorkerMessage::BestMove { best_move, ponder_move }) => {
                        send_response(UsiResponse::BestMove {
                            best_move,
                            ponder: ponder_move,
                        })?;
                        searching = false; // Clear searching flag after sending bestmove
                    }
                    Ok(WorkerMessage::Finished) => {
                        log::debug!("Worker thread finished");
                        searching = false;

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
                                    let mut adapter = lock_or_recover(&engine);
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
                    Ok(WorkerMessage::Error(err)) => {
                        send_info_string(format!("Error: {err}"))?;
                    }
                    Ok(WorkerMessage::EngineReturn(returned_engine)) => {
                        log::debug!("Engine returned from worker");
                        let mut adapter = lock_or_recover(&engine);
                        adapter.return_engine(returned_engine);
                    }
                    Err(_) => {
                        log::debug!("Worker channel closed");
                    }
                }
            }

            default(Duration::from_millis(5)) => {
                // Idle - prevents busy loop
            }
        }
    }

    // Clean shutdown
    log::debug!("Starting shutdown sequence");

    // Stop any ongoing search with timeout
    stop_flag.store(true, Ordering::Release);
    if searching {
        wait_for_worker_with_timeout(
            &mut worker_handle,
            &worker_rx,
            &engine,
            &mut searching,
            &mut stdout,
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
                let engine = lock_or_recover(ctx.engine);
                for option in engine.get_options() {
                    send_response(UsiResponse::Option(option.to_usi_string()))?;
                }
            }

            send_response(UsiResponse::UsiOk)?;
        }

        UsiCommand::IsReady => {
            // Initialize engine if needed
            {
                let mut engine = lock_or_recover(ctx.engine);
                engine.initialize()?;
            }
            send_response(UsiResponse::ReadyOk)?;
        }

        UsiCommand::Position {
            startpos,
            sfen,
            moves,
        } => {
            // Wait for any ongoing search to complete before updating position
            wait_for_search_completion(
                ctx.searching,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
                ctx.stdout,
            )?;

            let mut engine = lock_or_recover(ctx.engine);
            engine.set_position(startpos, sfen.as_deref(), &moves)?;
        }

        UsiCommand::Go(params) => {
            log::info!("Received go command with params: {:?}", params);

            // Stop any ongoing search
            wait_for_search_completion(
                ctx.searching,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
                ctx.stdout,
            )?;

            // Reset stop flag
            ctx.stop_flag.store(false, Ordering::Release);

            // Calculate timeout for this search
            *ctx.current_search_timeout = calculate_max_search_time(&params);

            // Clone necessary data for worker thread
            let engine_clone = Arc::clone(ctx.engine);
            let stop_clone = Arc::clone(ctx.stop_flag);
            let tx_clone = ctx.worker_tx.clone();

            // Spawn worker thread for search with panic safety
            let handle = thread::spawn(move || {
                log::info!("Worker thread spawned");
                let result = std::panic::catch_unwind(|| {
                    search_worker(engine_clone, params, stop_clone, tx_clone.clone());
                });

                if let Err(e) = result {
                    log::error!("Worker thread panicked: {e:?}");
                    // Send error message to main thread
                    let _ =
                        tx_clone.send(WorkerMessage::Error("Worker thread panicked".to_string()));
                    let _ = tx_clone.send(WorkerMessage::Finished);
                }
            });

            *ctx.worker_handle = Some(handle);
            *ctx.searching = true;
            log::info!("Worker thread handle stored, searching = true");

            // Don't block - return immediately
        }

        UsiCommand::Stop => {
            log::info!("Received stop command, searching = {}", *ctx.searching);
            // Signal stop to worker thread
            if *ctx.searching {
                ctx.stop_flag.store(true, Ordering::Release);
                log::info!("Stop flag set to true");

                // Wait for bestmove with timeout to ensure we always send a response
                let start = Instant::now();
                let timeout = Duration::from_millis(1000); // 1 second timeout

                loop {
                    if start.elapsed() > timeout {
                        // Timeout - try to generate a legal move instead of resigning
                        log::warn!("Timeout waiting for bestmove after stop command");

                        // Try to generate a legal move from current position
                        let emergency_move = {
                            let engine = lock_or_recover(ctx.engine);
                            engine.generate_emergency_move()
                        };

                        match emergency_move {
                            Ok(move_str) => {
                                log::info!("Generated emergency move: {}", move_str);
                                send_response(UsiResponse::BestMove {
                                    best_move: move_str,
                                    ponder: None,
                                })?;
                            }
                            Err(e) => {
                                log::error!("Failed to generate emergency move: {}", e);
                                // Only resign if we really can't generate any move
                                send_response(UsiResponse::BestMove {
                                    best_move: "resign".to_string(),
                                    ponder: None,
                                })?;
                            }
                        }

                        *ctx.searching = false;
                        break;
                    }

                    // Check for bestmove message
                    match ctx.worker_rx.try_recv() {
                        Ok(WorkerMessage::BestMove {
                            best_move,
                            ponder_move,
                        }) => {
                            send_response(UsiResponse::BestMove {
                                best_move,
                                ponder: ponder_move,
                            })?;
                            *ctx.searching = false;
                            break;
                        }
                        Ok(WorkerMessage::Info(info)) => {
                            // Forward info messages
                            let _ = send_response(UsiResponse::Info(info));
                        }
                        Ok(WorkerMessage::Finished) => {
                            // Worker finished but no bestmove - try emergency move
                            log::warn!("Worker finished without bestmove");

                            let emergency_move = {
                                let engine = lock_or_recover(ctx.engine);
                                engine.generate_emergency_move()
                            };

                            match emergency_move {
                                Ok(move_str) => {
                                    log::info!("Generated emergency move: {}", move_str);
                                    send_response(UsiResponse::BestMove {
                                        best_move: move_str,
                                        ponder: None,
                                    })?;
                                }
                                Err(e) => {
                                    log::error!("Failed to generate emergency move: {}", e);
                                    send_response(UsiResponse::BestMove {
                                        best_move: "resign".to_string(),
                                        ponder: None,
                                    })?;
                                }
                            }

                            *ctx.searching = false;
                            break;
                        }
                        _ => {
                            thread::sleep(Duration::from_millis(10));
                        }
                    }
                }
            } else {
                // Not searching - try to generate a move from current position
                log::debug!("Stop command received while not searching");

                let emergency_move = {
                    let engine = lock_or_recover(ctx.engine);
                    engine.generate_emergency_move()
                };

                match emergency_move {
                    Ok(move_str) => {
                        send_response(UsiResponse::BestMove {
                            best_move: move_str,
                            ponder: None,
                        })?;
                    }
                    Err(_) => {
                        // Only resign if no legal moves available
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
            let mut engine = lock_or_recover(ctx.engine);
            match engine.ponder_hit() {
                Ok(()) => log::debug!("Ponder hit successfully processed"),
                Err(e) => log::debug!("Ponder hit ignored: {e}"),
            }
        }

        UsiCommand::SetOption { name, value } => {
            let mut engine = lock_or_recover(ctx.engine);
            engine.set_option(&name, value.as_deref())?;
        }

        UsiCommand::GameOver { result } => {
            // Stop any ongoing search
            ctx.stop_flag.store(true, Ordering::Release);

            // Notify engine of game result
            let mut engine = lock_or_recover(ctx.engine);
            engine.game_over(result);
        }

        UsiCommand::UsiNewGame => {
            // ShogiGUI extension - new game notification
            // Stop any ongoing search
            wait_for_search_completion(
                ctx.searching,
                ctx.stop_flag,
                ctx.worker_handle,
                ctx.worker_rx,
                ctx.engine,
                ctx.stdout,
            )?;

            // Reset engine state for new game
            let mut engine = lock_or_recover(ctx.engine);
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
) {
    log::info!("Search worker thread started with params: {:?}", params);
    log::debug!("Search worker thread started");

    // Set up info callback
    let tx_info = tx.clone();
    let info_callback = move |info: SearchInfo| {
        let _ = tx_info.send(WorkerMessage::Info(info));
    };

    // Take engine out and prepare search
    let was_ponder = params.ponder;
    log::info!("Attempting to take engine from adapter");
    let (engine, position, limits, ponder_hit_flag) = {
        let mut adapter = lock_or_recover(&engine_adapter);
        log::info!("Adapter lock acquired, calling take_engine");
        match adapter.take_engine() {
            Ok(engine) => {
                log::info!("Engine taken successfully, preparing search");
                match adapter.prepare_search(&params, stop_flag.clone()) {
                    Ok((pos, lim, flag)) => {
                        log::info!("Search prepared successfully");
                        (engine, pos, lim, flag)
                    }
                    Err(e) => {
                        // Return engine and send error
                        adapter.return_engine(engine);
                        log::error!("Search preparation error: {e}");
                        let _ = tx.send(WorkerMessage::Error(e.to_string()));

                        // Try to generate emergency move before resigning
                        match adapter.generate_emergency_move() {
                            Ok(emergency_move) => {
                                log::info!(
                                    "Generated emergency move after preparation error: {}",
                                    emergency_move
                                );
                                if let Err(e) = tx.send(WorkerMessage::BestMove {
                                    best_move: emergency_move,
                                    ponder_move: None,
                                }) {
                                    log::error!("Failed to send emergency move: {e}");
                                }
                            }
                            Err(_) => {
                                // Only resign if no legal moves available
                                if let Err(e) = tx.send(WorkerMessage::BestMove {
                                    best_move: "resign".to_string(),
                                    ponder_move: None,
                                }) {
                                    log::error!(
                                        "Failed to send resign after preparation error: {e}"
                                    );
                                }
                            }
                        }

                        let _ = tx.send(WorkerMessage::Finished);
                        return;
                    }
                }
            }
            Err(e) => {
                log::error!("Failed to take engine: {e}");
                let _ = tx.send(WorkerMessage::Error(e.to_string()));

                // Try to generate emergency move from adapter
                log::info!("Attempting to generate emergency move after engine take failure");
                match adapter.generate_emergency_move() {
                    Ok(emergency_move) => {
                        log::info!(
                            "Generated emergency move after engine take error: {}",
                            emergency_move
                        );
                        if let Err(e) = tx.send(WorkerMessage::BestMove {
                            best_move: emergency_move,
                            ponder_move: None,
                        }) {
                            log::error!("Failed to send emergency move: {e}");
                        }
                    }
                    Err(_) => {
                        // Only resign if no legal moves available
                        if let Err(e) = tx.send(WorkerMessage::BestMove {
                            best_move: "resign".to_string(),
                            ponder_move: None,
                        }) {
                            log::error!("Failed to send resign after engine take error: {e}");
                        }
                    }
                }

                let _ = tx.send(WorkerMessage::Finished);
                return;
            }
        }
    }; // Lock released here

    // Explicitly drop ponder_hit_flag (it's used internally by the engine)
    drop(ponder_hit_flag);

    // Wrap engine in guard for panic safety
    let mut engine_guard = EngineReturnGuard::new(engine, tx.clone());

    // Execute search without holding the lock
    let result = EngineAdapter::execute_search_static(
        &mut engine_guard,
        position,
        limits,
        Box::new(info_callback),
    );

    // Handle result
    match result {
        Ok((best_move, ponder_move)) => {
            // Clean up ponder state if needed
            {
                let mut adapter = lock_or_recover(&engine_adapter);
                adapter.cleanup_after_search(was_ponder);
            }

            // Send best move
            if let Err(e) = tx.send(WorkerMessage::BestMove {
                best_move,
                ponder_move,
            }) {
                log::error!("Failed to send bestmove through channel: {e}");
            }
        }
        Err(e) => {
            log::error!("Search error: {e}");
            // Engine will be returned automatically by EngineReturnGuard::drop

            // Clean up ponder state if needed
            {
                let mut adapter = lock_or_recover(&engine_adapter);
                adapter.cleanup_after_search(was_ponder);
            }

            // Try to generate emergency move before sending error
            let emergency_result = {
                let adapter = lock_or_recover(&engine_adapter);
                adapter.generate_emergency_move()
            };

            if stop_flag.load(Ordering::Acquire) {
                // Stopped by user - try emergency move first
                match emergency_result {
                    Ok(emergency_move) => {
                        log::info!("Generated emergency move after stop: {}", emergency_move);
                        if let Err(e) = tx.send(WorkerMessage::BestMove {
                            best_move: emergency_move,
                            ponder_move: None,
                        }) {
                            log::error!("Failed to send emergency move: {e}");
                        }
                    }
                    Err(_) => {
                        // Only resign if no legal moves
                        if let Err(e) = tx.send(WorkerMessage::BestMove {
                            best_move: "resign".to_string(),
                            ponder_move: None,
                        }) {
                            log::error!("Failed to send resign after stop: {e}");
                        }
                    }
                }
            } else {
                // Other error - send error and try emergency move
                let _ = tx.send(WorkerMessage::Error(e.to_string()));
                match emergency_result {
                    Ok(emergency_move) => {
                        log::info!(
                            "Generated emergency move after search error: {}",
                            emergency_move
                        );
                        if let Err(e) = tx.send(WorkerMessage::BestMove {
                            best_move: emergency_move,
                            ponder_move: None,
                        }) {
                            log::error!("Failed to send emergency move: {e}");
                        }
                    }
                    Err(_) => {
                        // Only resign if no legal moves
                        if let Err(e) = tx.send(WorkerMessage::BestMove {
                            best_move: "resign".to_string(),
                            ponder_move: None,
                        }) {
                            log::error!("Failed to send resign after error: {e}");
                        }
                    }
                }
            }
        }
    }

    // Always send Finished at the end - use blocking send to ensure delivery
    match tx.send(WorkerMessage::Finished) {
        Ok(()) => log::debug!("Finished message sent successfully"),
        Err(e) => {
            log::error!("Failed to send Finished message: {e}. Channel might be closed.");
            // This is a critical error but we can't do much about it
        }
    }

    log::debug!("Search worker finished");
}
