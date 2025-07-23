// USI (Universal Shogi Interface) adapter

mod engine_adapter;
mod usi;

use anyhow::Result;
use clap::Parser;
use crossbeam_channel::{select, unbounded, Receiver, Sender};
use engine_adapter::{EngineAdapter, SearchInfo};
use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use usi::{parse_usi_command, send_response, UsiCommand, UsiResponse};

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
                stdout.flush()?;
            }
            WorkerMessage::Info(info) => {
                send_response(UsiResponse::String(format!("info {}", info.to_usi_string())));
                stdout.flush()?;
            }
            WorkerMessage::Error(err) => {
                send_response(UsiResponse::String(format!("info string Error: {}", err)));
                stdout.flush()?;
            }
            WorkerMessage::Finished => {} // Ignore finished message in drain
        }
    }
    Ok(())
}

/// Pumps messages from worker thread until bestmove or termination
/// Returns true if bestmove was sent, false otherwise
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
                        send_response(UsiResponse::String(format!("info {}", info.to_usi_string())));
                        stdout.flush()?;
                    }
                    Ok(WorkerMessage::BestMove { best_move, ponder_move }) => {
                        send_response(UsiResponse::BestMove {
                            best_move,
                            ponder: ponder_move,
                        });
                        stdout.flush()?;
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
                        send_response(UsiResponse::String(format!("info string Error: {err}")));
                        stdout.flush()?;
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
    let (tx, rx): (Sender<WorkerMessage>, Receiver<WorkerMessage>) = unbounded();

    // Create engine adapter (thread-safe)
    let engine = Arc::new(Mutex::new(EngineAdapter::new()));

    // Create stop flag for search control
    let stop_flag = Arc::new(AtomicBool::new(false));

    // Store active worker thread handle
    let mut worker_handle: Option<thread::JoinHandle<()>> = None;

    // Main I/O loop
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        log::debug!("Received command: {line}");

        match parse_usi_command(&line) {
            Ok(command) => {
                handle_command(
                    command,
                    &engine,
                    &stop_flag,
                    &tx,
                    &rx,
                    &mut worker_handle,
                    &mut stdout,
                )?;
            }
            Err(e) => {
                // Invalid commands are reported as info string
                send_response(UsiResponse::String(format!("Error: {e}")));
            }
        }
    }

    Ok(())
}

fn handle_command(
    command: UsiCommand,
    engine: &Arc<Mutex<EngineAdapter>>,
    stop_flag: &Arc<AtomicBool>,
    tx: &Sender<WorkerMessage>,
    rx: &Receiver<WorkerMessage>,
    worker_handle: &mut Option<thread::JoinHandle<()>>,
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
            stdout.flush()?;
        }

        UsiCommand::IsReady => {
            // Initialize engine if needed
            {
                let mut engine = engine.lock().unwrap();
                engine.initialize()?;
            }
            send_response(UsiResponse::ReadyOk);
            stdout.flush()?;
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
            stop_flag.store(true, Ordering::Release);

            // Wait for previous worker to finish
            if let Some(handle) = worker_handle.take() {
                let _ = handle.join();
            }

            // Reset stop flag
            stop_flag.store(false, Ordering::Release);

            // Clone necessary data for worker thread
            let engine_clone = Arc::clone(engine);
            let stop_clone = Arc::clone(stop_flag);
            let tx_clone = tx.clone();

            // Spawn worker thread for search
            let handle = thread::spawn(move || {
                search_worker(engine_clone, params, stop_clone, tx_clone);
            });

            *worker_handle = Some(handle);

            // Process messages until bestmove is received
            pump_messages(rx, stdout, true)?;

            // Join the worker thread after loop exits
            if let Some(handle) = worker_handle.take() {
                let _ = handle.join();
            }
        }

        UsiCommand::Stop => {
            // Signal stop to worker thread
            stop_flag.store(true, Ordering::Release);

            // Wait for messages from worker (don't exit on bestmove)
            if worker_handle.is_some() {
                pump_messages(rx, stdout, false)?;

                // Join the worker thread
                if let Some(handle) = worker_handle.take() {
                    let _ = handle.join();
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
            // Stop search
            stop_flag.store(true, Ordering::Release);

            // Drain remaining messages first
            flush_worker_queue(rx, stdout)?;

            // Wait for worker to finish
            if let Some(handle) = worker_handle.take() {
                let _ = handle.join();
            }

            // Exit gracefully
            std::process::exit(0);
        }
    }

    // Process any pending messages from worker thread
    while let Ok(msg) = rx.try_recv() {
        match msg {
            WorkerMessage::Info(info) => {
                send_response(UsiResponse::String(format!("info {}", info.to_usi_string())));
            }
            WorkerMessage::BestMove {
                best_move,
                ponder_move,
            } => {
                send_response(UsiResponse::BestMove {
                    best_move,
                    ponder: ponder_move,
                });
            }
            WorkerMessage::Finished => {
                // Worker thread finished - nothing to do
            }
            WorkerMessage::Error(err) => {
                send_response(UsiResponse::String(format!("Error: {err}")));
            }
        }
        stdout.flush()?;
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
        engine.search(params, stop_flag, Box::new(info_callback))
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
            let _ = tx.send(WorkerMessage::Error(e.to_string()));
        }
    }

    // Always send Finished at the end
    let _ = tx.send(WorkerMessage::Finished);

    log::debug!("Search worker finished");
}
