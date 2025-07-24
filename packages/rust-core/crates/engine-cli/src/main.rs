// USI (Universal Shogi Interface) adapter

mod engine_adapter;
mod usi;

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::{bounded, select, unbounded, Receiver, Sender};
use engine_adapter::EngineAdapter;
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

/// Messages from worker thread to main thread
#[derive(Debug, Clone)]
enum WorkerMessage {
    Info(SearchInfo),
    BestMove {
        best_move: String,
        ponder_move: Option<String>,
    },
    Finished, // Thread finished successfully
    Error(String),
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

    log::info!("USI Engine starting...");

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
                        log::debug!("Processing command: {cmd:?}");

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
                        searching = false;
                    }
                    Ok(WorkerMessage::Finished) => {
                        log::debug!("Worker thread finished");
                    }
                    Ok(WorkerMessage::Error(err)) => {
                        send_info_string(format!("Error: {err}"));
                        if let Err(e) = stdout.flush() {
                            eprintln!("Failed to flush stdout after sending error: {e}");
                            return Err(e.into());
                        }
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
            let mut engine = engine.lock().unwrap();
            engine.set_position(startpos, sfen.as_deref(), &moves)?;
        }

        UsiCommand::Go(params) => {
            // Stop any ongoing search
            if *searching {
                stop_flag.store(true, Ordering::Release);
                if let Some(handle) = worker_handle.take() {
                    let _ = handle.join();
                }
            }

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
            // Convert ponder search to normal search
            let mut engine = engine.lock().unwrap();
            engine.ponder_hit();
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
    engine: Arc<Mutex<EngineAdapter>>,
    params: usi::GoParams,
    stop_flag: Arc<AtomicBool>,
    tx: Sender<WorkerMessage>,
) {
    log::debug!("Search worker started with params: {params:?}");

    // Set up info callback
    let tx_info = tx.clone();
    let info_callback = move |info: SearchInfo| {
        let _ = tx_info.send(WorkerMessage::Info(info));
    };

    // Run search
    let result = {
        let mut engine = engine.lock().unwrap();
        engine.search(params, stop_flag.clone(), Box::new(info_callback))
    };

    // Send result
    match result {
        Ok((best_move, ponder_move)) => {
            let _ = tx.send(WorkerMessage::BestMove {
                best_move,
                ponder_move,
            });
        }
        Err(e) => {
            log::error!("Search error: {e}");
            // Even on error, send a fallback bestmove to satisfy USI protocol
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
