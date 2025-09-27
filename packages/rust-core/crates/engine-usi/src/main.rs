mod finalize;
mod io;
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
use options::{apply_options_to_engine, handle_setoption, send_id_and_options};
use search::{handle_go, parse_position, poll_search_completion};
use state::EngineState;
use stop::{handle_gameover, handle_ponderhit, handle_stop};

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

    let (reaper_tx, reaper_rx) = mpsc::channel::<thread::JoinHandle<()>>();
    let reaper_queue_len = Arc::clone(&state.reaper_queue_len);
    let reaper_handle = thread::Builder::new()
        .name("usi-reaper".to_string())
        .spawn(move || {
            let mut cum_ms: u128 = 0;
            for h in reaper_rx {
                let start = Instant::now();
                let _ = h.join();
                let dur = start.elapsed().as_millis();
                reaper_queue_len.fetch_sub(1, Ordering::SeqCst);
                if dur > 50 {
                    usi_println(&format!("info string reaper_join_ms={dur}"));
                }
                cum_ms += dur;
                if cum_ms >= 1000 {
                    usi_println(&format!("info string reaper_cum_join_ms={cum_ms}"));
                    cum_ms = 0;
                }
            }
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
                if let Some(h) = state.worker.take() {
                    let _ = h.join();
                }
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
