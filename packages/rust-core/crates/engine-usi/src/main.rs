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

use crate::finalize::{emit_bestmove_once, fmt_hash};
use engine_core::movegen::MoveGenerator;
use engine_core::usi::move_to_usi;
use io::{info_string, usi_println};
use oob::{enforce_deadline, poll_instant_mate, poll_oob_finalize};
use options::{apply_options_to_engine, handle_setoption, send_id_and_options};
use search::{handle_go, parse_position, poll_search_completion, tick_time_watchdog};
use state::EngineState;
use std::panic::{catch_unwind, AssertUnwindSafe};
use stop::{handle_gameover, handle_ponderhit, handle_stop};

fn emit_fallback_bestmove(state: &mut EngineState, note: &str) {
    // 既に送信済みなら何もしない
    if state.bestmove_emitted {
        info_string(format!("fallback_skip already_emitted=1 note={note}"));
        return;
    }
    // エンジンから安全な最終手を取得（TT/合法手）。Poison時や内部panicも救済。
    let best = match state.engine.lock() {
        Ok(eng) => {
            match catch_unwind(AssertUnwindSafe(|| {
                eng.choose_final_bestmove(&state.position, None)
            })) {
                Ok(fb) => fb.best_move.map(|mv| move_to_usi(&mv)),
                Err(_) => {
                    info_string("fallback_tt_panic_caught=1");
                    None
                }
            }
        }
        Err(poison) => {
            let eng = poison.into_inner();
            match catch_unwind(AssertUnwindSafe(|| {
                eng.choose_final_bestmove(&state.position, None)
            })) {
                Ok(fb) => fb.best_move.map(|mv| move_to_usi(&mv)),
                Err(_) => {
                    info_string("fallback_tt_panic_caught=1");
                    None
                }
            }
        }
    }
    .or_else(|| {
        let mg = MoveGenerator::new();
        if let Ok(list) = mg.generate_all(&state.position) {
            list.as_slice().first().map(move_to_usi)
        } else {
            None
        }
    })
    .unwrap_or_else(|| "resign".to_string());
    let root = fmt_hash(state.position.zobrist_hash());
    let sid = state.current_session_core_id.unwrap_or(0);
    info_string(format!(
        "fallback_bestmove_emit=1 reason={note} move={} sid={} root={}",
        best, sid, root
    ));
    let _ = emit_bestmove_once(state, best, None);
    state.notify_idle();
}

fn force_cleanup_after_go_failure(state: &mut EngineState) {
    // Signal stop to any ongoing search
    if let Some(flag) = &state.stop_flag {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    if let Some(session) = state.search_session.take() {
        // Proactively request backend stop
        session.request_stop();
        // Try to request stop via StopController if available
        let stop_ctrl = {
            let eng = state.lock_engine();
            Some(eng.stop_controller_handle())
        };

        if let Some(sc) = stop_ctrl {
            let _ =
                session.request_stop_and_wait(sc.as_ref(), std::time::Duration::from_millis(300));
        }

        // Join only if completed or disconnected; otherwise detach to avoid hangs
        use engine_core::engine::TryResult;
        match session.try_poll() {
            TryResult::Pending => {
                info_string("go_failure_join_skipped pending=1");
                // drop without join to detach
            }
            _ => {
                session.join_blocking();
            }
        }
    }

    // Reset search-related state to idle
    state.searching = false;
    state.stop_flag = None;
    state.ponder_hit_flag = None;
    // Flush TimeManager metrics if any
    state.finalize_time_manager();
    state.current_is_ponder = false;
    state.current_is_stochastic_ponder = false;
    state.deadline_hard = None;
    state.deadline_near = None;
    state.deadline_near_notified = false;
    state.current_root_hash = None;
    state.current_time_control = None;
    state.pending_ponder_result = None;
}

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
                            let engine = state.lock_engine();
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

                // Threads連動の自動既定（ユーザー上書きが無い項目のみ）を適用し、エンジンへ反映
                options::maybe_apply_thread_based_defaults(&mut state);
                // Notify idle after state is consistent
                state.notify_idle();
                apply_options_to_engine(&mut state);
                usi_println("readyok");
                continue;
            }

            if cmd.starts_with("setoption ") {
                // setoption の安全ラップ（パニック・エラーを吸収し、ログのみ残す）
                match catch_unwind(AssertUnwindSafe(|| handle_setoption(cmd, &mut state))) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        info_string(format!("setoption_error err={}", e));
                    }
                    Err(_) => {
                        info_string("setoption_panic_caught=1");
                    }
                }
                continue;
            }

            if cmd.starts_with("position ") {
                // position の安全ラップ（パニック・エラーを吸収し、旧局面を保持）
                match catch_unwind(AssertUnwindSafe(|| parse_position(cmd, &mut state))) {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        info_string(format!("position_error err={}", e));
                    }
                    Err(_) => {
                        info_string("position_panic_caught=1");
                    }
                }
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
                        let engine = state.lock_engine();
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
                // go 実行の安全ラッパー。パニック/エラー伝播でプロセスが落ちないようにする。
                info_string("go_dispatch_enter");
                match catch_unwind(AssertUnwindSafe(|| {
                    // テスト用: 環境変数でpanicを強制（リリースでも有効にしてフォールバック経路を検証可能にする）
                    if std::env::var("USI_TEST_GO_PANIC").ok().as_deref() == Some("1") {
                        panic!("testhook: forced panic before handle_go");
                    }
                    handle_go(cmd, &mut state)
                })) {
                    Ok(Ok(())) => {
                        // 正常終了
                    }
                    Ok(Err(e)) => {
                        info_string(format!("go_error err={}", e));
                        force_cleanup_after_go_failure(&mut state);
                        emit_fallback_bestmove(&mut state, "go_error");
                    }
                    Err(_) => {
                        info_string("go_panic_caught=1");
                        force_cleanup_after_go_failure(&mut state);
                        emit_fallback_bestmove(&mut state, "go_panic");
                    }
                }
                continue;
            }

            // テスト用: エンジンMutexをPoisonさせる隠しコマンド（リリースでも実行可にして回帰テストを容易に）
            if cmd == "debug_poison_engine" {
                let engine = state.engine.clone();
                std::thread::spawn(move || {
                    let _guard = engine.lock().unwrap();
                    panic!("testhook: poison engine mutex");
                })
                .join()
                .ok();
                // 直後にロックを取りにいき、Poison復帰ログを確実に出す
                // 非保持で即時ドロップ（ログ目的のみ）
                drop(state.lock_engine());
                info_string("debug_poison_engine_done=1");
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
