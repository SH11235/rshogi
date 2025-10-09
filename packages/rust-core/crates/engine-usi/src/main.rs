mod finalize;
mod io;
mod oob;
mod options;
mod search;
mod state;
mod stop;
mod usi_adapter;
mod util;

use anyhow::Result;
use engine_core::evaluation::nnue;
use log::info;
use std::io::{self as stdio, BufRead};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use io::{info_string, usi_println};
use oob::{enforce_deadline, poll_instant_mate, poll_oob_finalize};
use options::{apply_options_to_engine, handle_setoption, send_id_and_options};
use search::{handle_go, parse_position, poll_search_completion, tick_time_watchdog};
use state::EngineState;
use stop::{handle_gameover, handle_ponderhit, handle_stop};

fn main() -> Result<()> {
    env_logger::init();
    engine_core::util::panic::install_panic_hook();
    let stdin = stdio::stdin();
    let mut state = EngineState::new();

    let feat = nnue::enabled_features_str();
    info_string(format!("core_features={feat}"));
    match std::env::var("SHOGI_SIMD_MAX") {
        Ok(v) => info_string(format!("simd_clamp={v}")),
        Err(_) => info_string("simd_clamp=auto"),
    }
    match std::env::var("SHOGI_NNUE_SIMD") {
        Ok(v) => info_string(format!("nnue_simd_clamp={v}")),
        Err(_) => info_string("nnue_simd_clamp=auto"),
    }

    let (line_tx, line_rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        for line in stdin.lock().lines() {
            match line {
                Ok(s) => {
                    if line_tx.send(s).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    loop {
        poll_search_completion(&mut state);
        // YO互換: 短手数詰みは時間配分より優先（非ponderは即返答／ponderは保持→ponderhit）
        poll_instant_mate(&mut state);
        // Handle out-of-band finalize requests emitted by time manager
        poll_oob_finalize(&mut state);
        // Enforce locally computed deadlines (USI-side OOB finalize)
        enforce_deadline(&mut state);
        // Watchdog based on TimeManager state
        tick_time_watchdog(&mut state);

        if let Ok(line) = line_rx.try_recv() {
            let cmd = line.trim();
            if cmd.is_empty() {
                continue;
            }

            if cmd == "usi" {
                send_id_and_options(&state.opts);
                usi_println("usiok");
                continue;
            }

            if cmd == "isready" {
                // Ensure any ongoing search completes before readyok
                if state.searching {
                    if let Some(flag) = &state.stop_flag {
                        flag.store(true, Ordering::SeqCst);
                    }

                    if let Some(session) = state.search_session.take() {
                        let stop_ctrl = {
                            let engine = state.engine.lock().unwrap();
                            engine.stop_controller_handle()
                        };

                        // Try to get result with 1200ms timeout
                        let got_result = session
                            .request_stop_and_wait(stop_ctrl.as_ref(), Duration::from_millis(1200));

                        // Join only if search completed or disconnected; detach if still pending
                        if got_result.is_some() {
                            session.join_blocking();
                        } else {
                            use engine_core::engine::TryResult;
                            match session.try_poll() {
                                TryResult::Pending => {
                                    // Detach: drop session without joining to avoid hang
                                    info_string("isready_join_skipped pending=1");
                                    // session drops here, JoinHandle detaches
                                }
                                _ => {
                                    // Disconnected or late result: safe to join
                                    session.join_blocking();
                                }
                            }
                        }
                    }

                    // Reset state before notifying idle
                    state.searching = false;
                    // Keep stop_flag for reuse in next session (don't set to None)
                    state.ponder_hit_flag = None;
                    state.current_time_control = None;
                }

                // Notify idle after state is consistent
                state.notify_idle();
                apply_options_to_engine(&mut state);
                usi_println("readyok");
                continue;
            }

            if cmd.starts_with("setoption ") {
                handle_setoption(cmd, &mut state)?;
                continue;
            }

            if cmd.starts_with("position ") {
                parse_position(cmd, &mut state)?;
                continue;
            }

            if cmd == "usinewgame" {
                continue;
            }

            if cmd == "quit" {
                if let Some(flag) = &state.stop_flag {
                    flag.store(true, Ordering::SeqCst);
                }

                // Ensure any ongoing search completes before quit
                if let Some(session) = state.search_session.take() {
                    let stop_ctrl = {
                        let engine = state.engine.lock().unwrap();
                        engine.stop_controller_handle()
                    };

                    // Try to get result with 1500ms timeout
                    let got_result = session
                        .request_stop_and_wait(stop_ctrl.as_ref(), Duration::from_millis(1500));

                    // Join only if search completed or disconnected; detach if still pending
                    if got_result.is_some() {
                        session.join_blocking();
                    } else {
                        use engine_core::engine::TryResult;
                        match session.try_poll() {
                            TryResult::Pending => {
                                // Detach: drop session without joining to avoid hang
                                info_string("quit_join_skipped pending=1");
                                // session drops here, JoinHandle detaches
                            }
                            _ => {
                                // Disconnected or late result: safe to join
                                session.join_blocking();
                            }
                        }
                    }
                }

                // Notify idle after cleanup is complete
                state.notify_idle();
                break;
            }

            if cmd.starts_with("go") {
                handle_go(cmd, &mut state)?;
                continue;
            }

            if cmd == "stop" {
                handle_stop(&mut state);
                continue;
            }

            if cmd == "ponderhit" {
                handle_ponderhit(&mut state);
                continue;
            }

            if cmd.starts_with("gameover ") {
                handle_gameover(&mut state);
                continue;
            }

            info!("Ignoring command: {cmd}");
        } else {
            let poll_ms = state.opts.watchdog_poll_ms.max(1);
            thread::sleep(Duration::from_millis(poll_ms));
        }
    }

    Ok(())
}
