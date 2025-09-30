mod finalize;
mod io;
mod oob;
mod options;
mod search;
mod state;
mod stop;
mod util;

use anyhow::Result;
use engine_core::evaluation::nnue;
use log::info;
use std::io::{self as stdio, BufRead};
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use io::{info_string, usi_println};
use oob::poll_oob_finalize;
use options::{apply_options_to_engine, handle_setoption, send_id_and_options};
use search::{handle_go, parse_position, poll_search_completion};
use state::{EngineState, ReaperJob};
use stop::{handle_gameover, handle_ponderhit, handle_stop};
use util::join_search_handle;

fn main() -> Result<()> {
    env_logger::init();
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

    let (reaper_tx, reaper_rx) = mpsc::channel::<ReaperJob>();
    let reaper_queue_len = Arc::clone(&state.reaper_queue_len);
    let idle_sync = Arc::clone(&state.idle_sync);
    let reaper_handle = thread::Builder::new()
        .name("usi-reaper".to_string())
        .spawn(move || {
            let mut cum_ms: u128 = 0;
            for job in reaper_rx {
                let ReaperJob { handle, label } = job;
                let start = Instant::now();
                join_search_handle(handle, &label);
                let dur = start.elapsed().as_millis();
                reaper_queue_len.fetch_sub(1, Ordering::SeqCst);
                idle_sync.notify_all();
                if dur > 50 {
                    info_string(format!("reaper_join label={} waited_ms={}", label, dur));
                }
                cum_ms += dur;
                if cum_ms >= 1000 {
                    info_string(format!("reaper_cum_join_ms={cum_ms}"));
                    cum_ms = 0;
                }
            }
            idle_sync.notify_all();
        })
        .expect("failed to spawn reaper thread");
    state.reaper_tx = Some(reaper_tx);
    state.reaper_handle = Some(reaper_handle);

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
        // Handle out-of-band finalize requests emitted by time manager
        poll_oob_finalize(&mut state);

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
                        let bridge = {
                            let engine = state.engine.lock().unwrap();
                            engine.stop_bridge_handle()
                        };

                        // Try to get result with 1200ms timeout
                        let _ = session.request_stop_and_wait(&bridge, Duration::from_millis(1200));

                        // Join the thread to ensure complete cleanup
                        session.join_blocking();
                    }

                    state.searching = false;
                    state.stop_flag = None;
                    state.ponder_hit_flag = None;
                    state.current_time_control = None;
                }

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
                    let bridge = {
                        let engine = state.engine.lock().unwrap();
                        engine.stop_bridge_handle()
                    };

                    // Try to get result with 1500ms timeout
                    let _ = session.request_stop_and_wait(&bridge, Duration::from_millis(1500));

                    // Join the thread to ensure complete cleanup before quit
                    session.join_blocking();
                }

                state.notify_idle();

                state.reaper_tx.take();
                if let Some(h) = state.reaper_handle.take() {
                    let _ = h.join();
                }
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
            thread::sleep(Duration::from_millis(2));
        }
    }

    Ok(())
}
