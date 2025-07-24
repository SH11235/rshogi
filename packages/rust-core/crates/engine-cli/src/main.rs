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
use std::time::Duration;
use usi::output::SearchInfo;
use usi::{parse_usi_command, send_info_string, send_response, UsiCommand, UsiResponse};

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
        // In normal operation, engine should never be None when deref is called
        // If it is None, it's a programming error, so panic is appropriate
        match self.engine.as_ref() {
            Some(engine) => engine,
            None => {
                // This should never happen in correct usage
                log::error!("EngineReturnGuard::deref called but engine is None");
                panic!("EngineReturnGuard: engine already taken or not initialized")
            }
        }
    }
}

impl std::ops::DerefMut for EngineReturnGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // In normal operation, engine should never be None when deref_mut is called
        // If it is None, it's a programming error, so panic is appropriate
        match self.engine.as_mut() {
            Some(engine) => engine,
            None => {
                // This should never happen in correct usage
                log::error!("EngineReturnGuard::deref_mut called but engine is None");
                panic!("EngineReturnGuard: engine already taken or not initialized")
            }
        }
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

/// Wait for any ongoing search to complete
fn wait_for_search_completion(
    searching: &mut bool,
    stop_flag: &Arc<AtomicBool>,
    worker_handle: &mut Option<JoinHandle<()>>,
) {
    if *searching {
        stop_flag.store(true, Ordering::Release);
        if let Some(handle) = worker_handle.take() {
            let _ = handle.join();
        }
        *searching = false;
    }
}

/// Drain remaining messages from worker thread
#[cfg(test)]
#[allow(dead_code)]
fn flush_worker_queue(rx: &Receiver<WorkerMessage>, stdout: &mut impl Write) -> Result<()> {
    while let Ok(msg) = rx.try_recv() {
        match msg {
            WorkerMessage::BestMove {
                best_move,
                ponder_move,
            } => {
                send_response(UsiResponse::BestMove {
                    best_move,
                    ponder: ponder_move,
                });
                if let Err(e) = stdout.flush() {
                    eprintln!("Failed to flush stdout after sending best move: {e}");
                    return Err(e.into());
                }
            }
            WorkerMessage::Info(info) => {
                send_response(UsiResponse::Info(info));
                if let Err(e) = stdout.flush() {
                    eprintln!("Failed to flush stdout after sending info: {e}");
                    return Err(e.into());
                }
            }
            WorkerMessage::Error(err) => {
                send_info_string(format!("Error: {err}"));
                if let Err(e) = stdout.flush() {
                    eprintln!("Failed to flush stdout after sending error: {e}");
                    return Err(e.into());
                }
            }
            WorkerMessage::Finished => {} // Ignore finished message in drain
            WorkerMessage::EngineReturn(_) => {} // Ignore engine return in drain
        }
    }
    Ok(())
}

/// Pumps messages from worker thread until bestmove or termination
/// Returns true if bestmove was sent, false otherwise
#[cfg(test)]
#[allow(dead_code)]
fn pump_messages(
    rx: &Receiver<WorkerMessage>,
    stdout: &mut impl Write,
    until_bestmove: bool,
) -> Result<bool> {
    let mut bestmove_sent = false;

    loop {
        select! {
            recv(rx) -> msg => {
                match msg {
                    Ok(WorkerMessage::Info(info)) => {
                        send_response(UsiResponse::Info(info));
                        if let Err(e) = stdout.flush() {
                            eprintln!("Failed to flush stdout after sending info: {e}");
                            return Err(e.into());
                        }
                    }
                    Ok(WorkerMessage::BestMove { best_move, ponder_move }) => {
                        send_response(UsiResponse::BestMove {
                            best_move,
                            ponder: ponder_move,
                        });
                        if let Err(e) = stdout.flush() {
                            eprintln!("Failed to flush stdout after sending best move: {e}");
                            return Err(e.into());
                        }
                        bestmove_sent = true;
                        if until_bestmove {
                            break;  // Exit after bestmove if requested
                        }
                    }
                    Ok(WorkerMessage::Finished) => {
                        log::debug!("Worker thread finished");
                        break;
                    }
                    Ok(WorkerMessage::Error(err)) => {
                        send_info_string(format!("Error: {err}"));
                        if let Err(e) = stdout.flush() {
                            eprintln!("Failed to flush stdout after sending error: {e}");
                            return Err(e.into());
                        }
                        break;
                    }
                    Ok(WorkerMessage::EngineReturn(_)) => {
                        // EngineReturn is handled in the main loop, not in test utilities
                        log::debug!("EngineReturn message in pump_messages (unexpected)");
                    }
                    Err(_) => {
                        log::debug!("Channel disconnected");
                        break;
                    }
                }
            }
            default(Duration::from_millis(10)) => {
                // Just continue - this prevents blocking
            }
        }
    }

    Ok(bestmove_sent)
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

fn main() -> Result<()> {
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

    // Create communication channels
    let (worker_tx, worker_rx): (Sender<WorkerMessage>, Receiver<WorkerMessage>) = unbounded();
    let (cmd_tx, cmd_rx) = bounded::<UsiCommand>(1024);

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
                        handle_command(
                            cmd,
                            &engine,
                            &stop_flag,
                            &worker_tx,
                            &mut worker_handle,
                            &mut searching,
                            &mut stdout,
                        )?;
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
                            eprintln!("Failed to flush stdout after sending info: {e}");
                            return Err(e.into());
                        }
                    }
                    Ok(WorkerMessage::BestMove { best_move, ponder_move }) => {
                        send_response(UsiResponse::BestMove {
                            best_move,
                            ponder: ponder_move,
                        });
                        if let Err(e) = stdout.flush() {
                            eprintln!("Failed to flush stdout after sending best move: {e}");
                            return Err(e.into());
                        }
                    }
                    Ok(WorkerMessage::Finished) => {
                        log::debug!("Worker thread finished");
                        searching = false;

                        // Drain any remaining Info messages in the queue
                        while let Ok(msg) = worker_rx.try_recv() {
                            match msg {
                                WorkerMessage::Info(info) => {
                                    send_response(UsiResponse::Info(info));
                                    if let Err(e) = stdout.flush() {
                                        log::debug!("Failed to flush stdout while draining info: {e}");
                                        // Continue draining even if flush fails
                                    }
                                }
                                _ => {
                                    // Other message types shouldn't be in queue after Finished
                                    // If they are, it indicates an error condition, so we break
                                    log::debug!("Unexpected message type after Finished: {:?}",
                                               std::any::type_name_of_val(&msg));
                                    break;
                                }
                            }
                        }
                    }
                    Ok(WorkerMessage::Error(err)) => {
                        send_info_string(format!("Error: {err}"));
                        if let Err(e) = stdout.flush() {
                            eprintln!("Failed to flush stdout after sending error: {e}");
                            return Err(e.into());
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

    // Stop any ongoing search
    stop_flag.store(true, Ordering::Release);
    if let Some(handle) = worker_handle.take() {
        let _ = handle.join();
    }

    // Stop stdin reader thread by closing the channel
    drop(cmd_tx);
    let _ = stdin_handle.join();

    log::debug!("Shutdown complete");
    Ok(())
}

fn handle_command(
    command: UsiCommand,
    engine: &Arc<Mutex<EngineAdapter>>,
    stop_flag: &Arc<AtomicBool>,
    worker_tx: &Sender<WorkerMessage>,
    worker_handle: &mut Option<JoinHandle<()>>,
    searching: &mut bool,
    stdout: &mut impl Write,
) -> Result<()> {
    match command {
        UsiCommand::Usi => {
            send_response(UsiResponse::Id {
                name: "RustShogi 1.0".to_string(),
                author: "RustShogi Team".to_string(),
            });

            // Send available options
            {
                let engine = engine.lock().unwrap();
                for option in engine.get_options() {
                    send_response(UsiResponse::Option(option.to_usi_string()));
                }
            }

            send_response(UsiResponse::UsiOk);
            if let Err(e) = stdout.flush() {
                eprintln!("Failed to flush stdout after sending uciok: {e}");
                return Err(e.into());
            }
        }

        UsiCommand::IsReady => {
            // Initialize engine if needed
            {
                let mut engine = engine.lock().unwrap();
                engine.initialize()?;
            }
            send_response(UsiResponse::ReadyOk);
            if let Err(e) = stdout.flush() {
                eprintln!("Failed to flush stdout after sending readyok: {e}");
                return Err(e.into());
            }
        }

        UsiCommand::Position {
            startpos,
            sfen,
            moves,
        } => {
            // Wait for any ongoing search to complete before updating position
            wait_for_search_completion(searching, stop_flag, worker_handle);

            let mut engine = engine.lock().unwrap();
            engine.set_position(startpos, sfen.as_deref(), &moves)?;
        }

        UsiCommand::Go(params) => {
            // Stop any ongoing search
            wait_for_search_completion(searching, stop_flag, worker_handle);

            // Reset stop flag
            stop_flag.store(false, Ordering::Release);

            // Clone necessary data for worker thread
            let engine_clone = Arc::clone(engine);
            let stop_clone = Arc::clone(stop_flag);
            let tx_clone = worker_tx.clone();

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

            *worker_handle = Some(handle);
            *searching = true;

            // Don't block - return immediately
        }

        UsiCommand::Stop => {
            // Signal stop to worker thread
            if *searching {
                stop_flag.store(true, Ordering::Release);
                // Don't wait - bestmove will come through the channel
            } else {
                // Not searching - send dummy bestmove to satisfy USI protocol
                log::debug!("Stop command received while not searching, sending resign");
                send_response(UsiResponse::BestMove {
                    best_move: "resign".to_string(),
                    ponder: None,
                });
                if let Err(e) = stdout.flush() {
                    eprintln!("Failed to flush stdout after sending resign: {e}");
                    return Err(e.into());
                }
            }
        }

        UsiCommand::PonderHit => {
            // Handle ponder hit
            let mut engine = engine.lock().unwrap();
            match engine.ponder_hit() {
                Ok(()) => log::debug!("Ponder hit successfully processed"),
                Err(e) => log::debug!("Ponder hit ignored: {e}"),
            }
        }

        UsiCommand::SetOption { name, value } => {
            let mut engine = engine.lock().unwrap();
            engine.set_option(&name, value.as_deref())?;
        }

        UsiCommand::GameOver { result } => {
            // Stop any ongoing search
            stop_flag.store(true, Ordering::Release);

            // Notify engine of game result
            let mut engine = engine.lock().unwrap();
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

    // Always send Finished at the end
    let _ = tx.send(WorkerMessage::Finished);

    log::debug!("Search worker finished");
}
