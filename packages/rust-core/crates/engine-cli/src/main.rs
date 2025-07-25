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
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use usi::output::SearchInfo;
use usi::{parse_usi_command, send_info_string, send_response, UsiCommand, UsiResponse};

// Constants for timeout and channel management
const JOIN_TIMEOUT: Duration = Duration::from_secs(5);
const CHANNEL_SIZE: usize = 1024;
const SELECT_TIMEOUT: Duration = Duration::from_millis(50);

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
}

/// Wait for worker thread to finish with timeout
fn wait_for_worker_with_timeout(
    worker_handle: &mut Option<JoinHandle<()>>,
    worker_rx: &Receiver<WorkerMessage>,
    engine: &Arc<Mutex<EngineAdapter>>,
    searching: &mut bool,
    stdout: &mut dyn Write,
) -> Result<()> {
    let deadline = Instant::now() + JOIN_TIMEOUT;
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
                        let mut adapter = engine.lock().unwrap();
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
                        send_response(UsiResponse::BestMove {
                            best_move,
                            ponder: ponder_move,
                        });
                        stdout.flush()?;
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
                    log::error!("Worker thread timeout after {JOIN_TIMEOUT:?}");
                    // Fatal error - exit process
                    std::process::exit(1);
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
                let mut adapter = engine.lock().unwrap();
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
        wait_for_worker_with_timeout(worker_handle, worker_rx, engine, searching, stdout)?;
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
                    log::debug!("Stdin read error (EOF?): {e}");
                    // Try to send quit command on EOF
                    match cmd_tx.try_send(UsiCommand::Quit) {
                        Ok(()) => {}
                        Err(_) => {
                            log::debug!("Failed to send quit command, channel likely closed");
                        }
                    }
                    break;
                }
            }
        }

        log::debug!("Stdin reader thread exiting");
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

    log::info!("Shogi USI Engine starting (version 1.0)");

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
                        send_response(UsiResponse::Info(info));
                        if let Err(e) = stdout.flush() {
                            log::error!("Failed to flush stdout after sending info: {e}");
                            return Err(anyhow::anyhow!("Failed to flush stdout: {}", e));
                        }
                    }
                    Ok(WorkerMessage::BestMove { best_move, ponder_move }) => {
                        send_response(UsiResponse::BestMove {
                            best_move,
                            ponder: ponder_move,
                        });
                        if let Err(e) = stdout.flush() {
                            log::error!("Failed to flush stdout after sending best move: {e}");
                            return Err(anyhow::anyhow!("Failed to flush stdout: {}", e));
                        }
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
                                    send_response(UsiResponse::Info(info));
                                    if let Err(e) = stdout.flush() {
                                        log::debug!("Failed to flush stdout while draining info: {e}");
                                    }
                                }
                                Ok(WorkerMessage::EngineReturn(returned_engine)) => {
                                    log::debug!("Engine returned after Finished");
                                    let mut adapter = engine.lock().unwrap();
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
                        send_info_string(format!("Error: {err}"));
                        if let Err(e) = stdout.flush() {
                            log::error!("Failed to flush stdout after sending error: {e}");
                            return Err(anyhow::anyhow!("Failed to flush stdout: {}", e));
                        }
                    }
                    Ok(WorkerMessage::EngineReturn(returned_engine)) => {
                        log::debug!("Engine returned from worker");
                        let mut adapter = engine.lock().unwrap();
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
        )?;
    }

    // Stop stdin reader thread by closing the channel
    drop(cmd_tx);
    match stdin_handle.join() {
        Ok(()) => log::debug!("Stdin reader thread joined successfully"),
        Err(_) => log::error!("Stdin reader thread panicked"),
    }

    log::debug!("Shutdown complete");
    Ok(())
}

fn handle_command(command: UsiCommand, ctx: &mut CommandContext) -> Result<()> {
    match command {
        UsiCommand::Usi => {
            send_response(UsiResponse::Id {
                name: "RustShogi 1.0".to_string(),
                author: "RustShogi Team".to_string(),
            });

            // Send available options
            {
                let engine = ctx.engine.lock().unwrap();
                for option in engine.get_options() {
                    send_response(UsiResponse::Option(option.to_usi_string()));
                }
            }

            send_response(UsiResponse::UsiOk);
            if let Err(e) = ctx.stdout.flush() {
                log::error!("Failed to flush stdout after sending uciok: {e}");
                return Err(anyhow::anyhow!("Failed to flush stdout: {}", e));
            }
        }

        UsiCommand::IsReady => {
            // Initialize engine if needed
            {
                let mut engine = ctx.engine.lock().unwrap();
                engine.initialize()?;
            }
            send_response(UsiResponse::ReadyOk);
            if let Err(e) = ctx.stdout.flush() {
                log::error!("Failed to flush stdout after sending readyok: {e}");
                return Err(anyhow::anyhow!("Failed to flush stdout: {}", e));
            }
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

            let mut engine = ctx.engine.lock().unwrap();
            engine.set_position(startpos, sfen.as_deref(), &moves)?;
        }

        UsiCommand::Go(params) => {
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

            // Clone necessary data for worker thread
            let engine_clone = Arc::clone(ctx.engine);
            let stop_clone = Arc::clone(ctx.stop_flag);
            let tx_clone = ctx.worker_tx.clone();

            // Spawn worker thread for search with panic safety
            let handle = thread::spawn(move || {
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

            // Don't block - return immediately
        }

        UsiCommand::Stop => {
            // Signal stop to worker thread
            if *ctx.searching {
                ctx.stop_flag.store(true, Ordering::Release);
                // Don't wait - bestmove will come through the channel
            } else {
                // Not searching - send dummy bestmove to satisfy USI protocol
                log::debug!("Stop command received while not searching, sending resign");
                send_response(UsiResponse::BestMove {
                    best_move: "resign".to_string(),
                    ponder: None,
                });
                if let Err(e) = ctx.stdout.flush() {
                    log::error!("Failed to flush stdout after sending resign: {e}");
                    return Err(anyhow::anyhow!("Failed to flush stdout: {}", e));
                }
            }
        }

        UsiCommand::PonderHit => {
            // Handle ponder hit
            let mut engine = ctx.engine.lock().unwrap();
            match engine.ponder_hit() {
                Ok(()) => log::debug!("Ponder hit successfully processed"),
                Err(e) => log::debug!("Ponder hit ignored: {e}"),
            }
        }

        UsiCommand::SetOption { name, value } => {
            let mut engine = ctx.engine.lock().unwrap();
            engine.set_option(&name, value.as_deref())?;
        }

        UsiCommand::GameOver { result } => {
            // Stop any ongoing search
            ctx.stop_flag.store(true, Ordering::Release);

            // Notify engine of game result
            let mut engine = ctx.engine.lock().unwrap();
            engine.game_over(result);
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
    log::debug!("Search worker thread started");

    // Set up info callback
    let tx_info = tx.clone();
    let info_callback = move |info: SearchInfo| {
        let _ = tx_info.send(WorkerMessage::Info(info));
    };

    // Take engine out and prepare search
    let was_ponder = params.ponder;
    let (engine, position, limits, ponder_hit_flag) = {
        let mut adapter = engine_adapter.lock().unwrap();
        match adapter.take_engine() {
            Ok(engine) => {
                match adapter.prepare_search(&params, stop_flag.clone()) {
                    Ok((pos, lim, flag)) => (engine, pos, lim, flag),
                    Err(e) => {
                        // Return engine and send error
                        adapter.return_engine(engine);
                        log::error!("Search preparation error: {e}");
                        let _ = tx.send(WorkerMessage::Error(e.to_string()));
                        let _ = tx.send(WorkerMessage::BestMove {
                            best_move: "resign".to_string(),
                            ponder_move: None,
                        });
                        let _ = tx.send(WorkerMessage::Finished);
                        return;
                    }
                }
            }
            Err(e) => {
                log::warn!("Failed to take engine: {e}");
                let _ = tx.send(WorkerMessage::Error(e.to_string()));
                let _ = tx.send(WorkerMessage::BestMove {
                    best_move: "resign".to_string(),
                    ponder_move: None,
                });
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
                let mut adapter = engine_adapter.lock().unwrap();
                adapter.cleanup_after_search(was_ponder);
            }

            // Send best move
            let _ = tx.send(WorkerMessage::BestMove {
                best_move,
                ponder_move,
            });
        }
        Err(e) => {
            log::error!("Search error: {e}");
            // Engine will be returned automatically by EngineReturnGuard::drop

            // Clean up ponder state if needed
            {
                let mut adapter = engine_adapter.lock().unwrap();
                adapter.cleanup_after_search(was_ponder);
            }

            // Send error and resign
            if stop_flag.load(Ordering::Acquire) {
                // Stopped by user - send resign
                let _ = tx.send(WorkerMessage::BestMove {
                    best_move: "resign".to_string(),
                    ponder_move: None,
                });
            } else {
                // Other error - send error and resign
                let _ = tx.send(WorkerMessage::Error(e.to_string()));
                let _ = tx.send(WorkerMessage::BestMove {
                    best_move: "resign".to_string(),
                    ponder_move: None,
                });
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
