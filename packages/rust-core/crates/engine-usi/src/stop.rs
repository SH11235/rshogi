use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use engine_core::time_management::TimeControl;

use crate::finalize::{finalize_and_send, finalize_and_send_fast};
use crate::io::info_string;
use crate::state::EngineState;

pub fn handle_stop(state: &mut EngineState) {
    if let (true, Some(flag)) = (state.searching, &state.stop_flag) {
        flag.store(true, Ordering::SeqCst);
        info_string("stop_requested");
        if let Some(rx) = state.result_rx.take() {
            let mut wait_ms = state.opts.stop_wait_ms;
            if let Some(tc) = &state.current_time_control {
                match tc {
                    TimeControl::FixedTime { .. } | TimeControl::Infinite => {
                        wait_ms = 0;
                        info_string("stop_fast_finalize=fixed_or_infinite");
                    }
                    TimeControl::Byoyomi { main_time_ms, .. } => {
                        if *main_time_ms == 0 || *main_time_ms <= state.opts.network_delay2_ms {
                            wait_ms = 0;
                            info_string("stop_fast_finalize=byoyomi");
                        }
                    }
                    _ => {}
                }
            }

            let deadline = Instant::now() + Duration::from_millis(wait_ms);
            let mut finalized = false;
            while Instant::now() < deadline && !finalized {
                let now = Instant::now();
                let remain = if deadline > now {
                    deadline - now
                } else {
                    Duration::from_millis(0)
                };
                let slice = if remain > Duration::from_millis(20) {
                    Duration::from_millis(20)
                } else {
                    remain
                };
                match rx.recv_timeout(slice) {
                    Ok((sid, result)) => {
                        if sid != state.current_search_id {
                            info_string(format!(
                                "ignore_result stale_sid={} current_sid={}",
                                sid, state.current_search_id
                            ));
                            continue;
                        }
                        if let Some(h) = state.worker.take() {
                            let _ = h.join();
                        }
                        state.searching = false;
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;

                        let stale = state
                            .current_root_hash
                            .map(|h| h != state.position.zobrist_hash())
                            .unwrap_or(false);
                        finalize_and_send(state, "stop_finalize", Some(&result), stale);
                        state.current_is_ponder = false;
                        state.current_root_hash = None;
                        state.current_time_control = None;
                        finalized = true;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            if !finalized {
                if let Ok((sid, result)) = rx.try_recv() {
                    if sid == state.current_search_id {
                        if let Some(h) = state.worker.take() {
                            let _ = h.join();
                        }
                        state.searching = false;
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;

                        let stale = state
                            .current_root_hash
                            .map(|h| h != state.position.zobrist_hash())
                            .unwrap_or(false);
                        finalize_and_send(state, "stop_finalize", Some(&result), stale);
                        state.current_is_ponder = false;
                        state.current_root_hash = None;
                        state.current_time_control = None;
                        return;
                    } else {
                        info_string(format!(
                            "ignore_result stale_sid={} current_sid={}",
                            sid, state.current_search_id
                        ));
                    }
                }
                if let Some(h) = state.worker.take() {
                    if let Some(tx) = &state.reaper_tx {
                        let q = state.reaper_queue_len.fetch_add(1, Ordering::SeqCst) + 1;
                        let _ = tx.send(h);
                        const REAPER_QUEUE_SOFT_MAX: usize = 128;
                        if q > REAPER_QUEUE_SOFT_MAX {
                            info_string(format!("reaper_queue_len_high len={}", q));
                        } else {
                            info_string(format!("reaper_detach queued len={}", q));
                        }
                    }
                }
                state.searching = false;
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                finalize_and_send_fast(state, "stop_timeout_finalize");
                state.current_is_ponder = false;
                state.current_root_hash = None;
                state.current_time_control = None;
            }
        }
    }
}

pub fn handle_ponderhit(state: &mut EngineState) {
    if state.opts.stochastic_ponder && state.searching && state.current_is_stochastic_ponder {
        state.stoch_suppress_result = true;
        state.pending_research_after_ponderhit = true;
        if let Some(flag) = &state.stop_flag {
            flag.store(true, Ordering::SeqCst);
        }
    } else {
        if let Some(flag) = &state.ponder_hit_flag {
            flag.store(true, Ordering::SeqCst);
        }
        state.current_is_ponder = false;
    }
}

pub fn handle_gameover(state: &mut EngineState) {
    if state.opts.gameover_sends_bestmove {
        if !state.searching && state.bestmove_emitted {
            if let Some(flag) = &state.stop_flag {
                flag.store(true, Ordering::SeqCst);
            }
            if let Some(h) = state.worker.take() {
                let _ = h.join();
            }
            state.searching = false;
            state.stop_flag = None;
            state.ponder_hit_flag = None;
            state.current_time_control = None;
            return;
        }
        if let Some(flag) = &state.stop_flag {
            flag.store(true, Ordering::SeqCst);
        }
        if state.searching {
            if let Some(rx) = state.result_rx.take() {
                let mut wait_ms = state.opts.stop_wait_ms;
                if let Some(tc) = &state.current_time_control {
                    match tc {
                        TimeControl::FixedTime { .. } | TimeControl::Infinite => {
                            wait_ms = 0;
                            info_string("gameover_fast_finalize=fixed_or_infinite");
                        }
                        TimeControl::Byoyomi { main_time_ms, .. } => {
                            if *main_time_ms == 0 || *main_time_ms <= state.opts.network_delay2_ms {
                                wait_ms = 0;
                                info_string("gameover_fast_finalize=byoyomi");
                            }
                        }
                        _ => {}
                    }
                }

                let deadline = Instant::now() + Duration::from_millis(wait_ms);
                let mut finalized = false;
                while Instant::now() < deadline && !finalized {
                    let now = Instant::now();
                    let remain = if deadline > now {
                        deadline - now
                    } else {
                        Duration::from_millis(0)
                    };
                    let slice = if remain > Duration::from_millis(20) {
                        Duration::from_millis(20)
                    } else {
                        remain
                    };
                    match rx.recv_timeout(slice) {
                        Ok((sid, result)) => {
                            if sid != state.current_search_id {
                                info_string(format!(
                                    "ignore_result stale_sid={} current_sid={}",
                                    sid, state.current_search_id
                                ));
                                continue;
                            }
                            if let Some(h) = state.worker.take() {
                                let _ = h.join();
                            }
                            state.searching = false;
                            state.stop_flag = None;
                            state.ponder_hit_flag = None;
                            let stale = state
                                .current_root_hash
                                .map(|h| h != state.position.zobrist_hash())
                                .unwrap_or(false);
                            finalize_and_send(state, "gameover_finalize", Some(&result), stale);
                            state.current_is_ponder = false;
                            state.current_root_hash = None;
                            state.current_time_control = None;
                            finalized = true;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                if !finalized {
                    if let Ok((sid, result)) = rx.try_recv() {
                        if sid == state.current_search_id {
                            if let Some(h) = state.worker.take() {
                                let _ = h.join();
                            }
                            state.searching = false;
                            state.stop_flag = None;
                            state.ponder_hit_flag = None;
                            let stale = state
                                .current_root_hash
                                .map(|h| h != state.position.zobrist_hash())
                                .unwrap_or(false);
                            finalize_and_send(state, "gameover_finalize", Some(&result), stale);
                            state.current_is_ponder = false;
                            state.current_root_hash = None;
                            state.current_time_control = None;
                            return;
                        } else {
                            info_string(format!(
                                "ignore_result stale_sid={} current_sid={}",
                                sid, state.current_search_id
                            ));
                        }
                    }
                    if let Some(h) = state.worker.take() {
                        if let Some(tx) = &state.reaper_tx {
                            let q = state.reaper_queue_len.fetch_add(1, Ordering::SeqCst) + 1;
                            let _ = tx.send(h);
                            const REAPER_QUEUE_SOFT_MAX: usize = 128;
                            if q > REAPER_QUEUE_SOFT_MAX {
                                info_string(format!("reaper_queue_len_high len={}", q));
                            } else {
                                info_string(format!("reaper_detach queued len={}", q));
                            }
                        }
                    }
                    state.searching = false;
                    state.stop_flag = None;
                    state.ponder_hit_flag = None;
                    finalize_and_send_fast(state, "gameover_timeout_finalize");
                    state.current_is_ponder = false;
                    state.current_root_hash = None;
                    state.current_time_control = None;
                }
            } else {
                state.searching = false;
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                finalize_and_send_fast(state, "gameover_immediate_finalize");
                state.current_is_ponder = false;
                state.current_root_hash = None;
                state.current_time_control = None;
            }
        } else {
            state.searching = false;
            state.stop_flag = None;
            state.ponder_hit_flag = None;
            finalize_and_send_fast(state, "gameover_immediate_finalize");
            state.current_is_ponder = false;
            state.current_root_hash = None;
            state.current_time_control = None;
        }
    } else {
        if let Some(flag) = &state.stop_flag {
            flag.store(true, Ordering::SeqCst);
        }
        if let Some(h) = state.worker.take() {
            let _ = h.join();
        }
        state.searching = false;
        state.stop_flag = None;
        state.ponder_hit_flag = None;
        state.current_time_control = None;
    }
}
