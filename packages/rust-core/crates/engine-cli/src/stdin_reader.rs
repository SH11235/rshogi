use crate::emit_utils::log_tsv;
use crate::usi::{parse_usi_command, send_info_string, UsiCommand};
use crossbeam_channel::Sender;
use std::io::{self, BufRead};
use std::thread::{self, JoinHandle};

/// Spawn stdin reader thread
pub fn spawn_stdin_reader(
    cmd_tx: Sender<UsiCommand>,
    ctrl_tx: Sender<UsiCommand>,
) -> JoinHandle<()> {
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
                            // Diagnostic: emit an info string when a command is parsed from stdin
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
                            let _ = send_info_string(log_tsv(&[
                                ("kind", "stdin_parsed"),
                                ("cmd", cmd_name),
                            ]));
                            // Use try_send to avoid blocking (control-plane commands to ctrl_tx)
                            let is_ctrl = matches!(
                                cmd,
                                UsiCommand::Stop | UsiCommand::GameOver { .. } | UsiCommand::Quit
                            );
                            let target = if is_ctrl { &ctrl_tx } else { &cmd_tx };
                            match target.try_send(cmd) {
                                Ok(()) => {}
                                Err(crossbeam_channel::TrySendError::Full(_)) => {
                                    // Log drop with USI-visible info so we can diagnose saturation
                                    let _ = send_info_string(log_tsv(&[
                                        ("kind", "cmd_drop"),
                                        ("cmd", cmd_name),
                                    ]));
                                    log::warn!(
                                        "Command channel full, dropping command: {}",
                                        cmd_name
                                    );
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
                    match ctrl_tx.try_send(UsiCommand::Quit) {
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
        match ctrl_tx.try_send(UsiCommand::Quit) {
            Ok(()) => log::info!("Sent quit command after EOF"),
            Err(_) => log::debug!("Channel closed before quit after EOF"),
        }

        log::debug!("Stdin reader thread exiting (EOF)");
    })
}
