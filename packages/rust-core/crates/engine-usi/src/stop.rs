use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use engine_core::time_management::TimeControl;

use crate::finalize::{finalize_and_send, finalize_and_send_fast};
use crate::io::info_string;
use crate::state::EngineState;
use crate::util::enqueue_reaper;

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
                            state.notify_idle();
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
                state.stop_bridge.request_stop_immediate();

                let mut waited_after_stop_ms = 0u64;
                let mut finalize_candidate: Option<(u64, engine_core::search::SearchResult)> = None;

                for backoff in [5u64, 10, 20, 40] {
                    match rx.try_recv() {
                        Ok(pair) => {
                            finalize_candidate = Some(pair);
                            break;
                        }
                        Err(mpsc::TryRecvError::Empty) => {
                            thread::sleep(Duration::from_millis(backoff));
                            waited_after_stop_ms += backoff;
                        }
                        Err(mpsc::TryRecvError::Disconnected) => break,
                    }
                }

                if finalize_candidate.is_none() {
                    match rx.try_recv() {
                        Ok(pair) => finalize_candidate = Some(pair),
                        Err(mpsc::TryRecvError::Empty) => {}
                        Err(mpsc::TryRecvError::Disconnected) => {}
                    }
                }

                let snapshot = state.stop_bridge.snapshot();
                let detach_flag = finalize_candidate
                    .as_ref()
                    .map(|(sid, _)| *sid != state.current_search_id)
                    .unwrap_or(true);

                info_string(format!(
                    "fast_finalize active_threads={} pending={} waited_ms={} detach={}",
                    snapshot.active_workers,
                    snapshot.pending_work_items,
                    waited_after_stop_ms,
                    if detach_flag { 1 } else { 0 }
                ));

                if let Some((sid, result)) = finalize_candidate {
                    if sid == state.current_search_id {
                        if let Some(h) = state.worker.take() {
                            let _ = h.join();
                            state.notify_idle();
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

                let worker = state.worker.take();
                state.searching = false;
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                finalize_and_send_fast(state, "stop_timeout_finalize");
                state.current_is_ponder = false;
                state.current_root_hash = None;
                state.current_time_control = None;
                if let Some(handle) = worker {
                    enqueue_reaper(state, handle, "stop_timeout_finalize");
                } else {
                    state.notify_idle();
                }
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
                enqueue_reaper(state, h, "gameover_post_emit_join");
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
                                state.notify_idle();
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
                                state.notify_idle();
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
                    let worker = state.worker.take();
                    state.searching = false;
                    state.stop_flag = None;
                    state.ponder_hit_flag = None;
                    finalize_and_send_fast(state, "gameover_timeout_finalize");
                    state.current_is_ponder = false;
                    state.current_root_hash = None;
                    state.current_time_control = None;
                    if let Some(handle) = worker {
                        enqueue_reaper(state, handle, "gameover_timeout_finalize");
                    } else {
                        state.notify_idle();
                    }
                }
            } else {
                state.searching = false;
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                finalize_and_send_fast(state, "gameover_immediate_finalize");
                state.current_is_ponder = false;
                state.current_root_hash = None;
                state.current_time_control = None;
                state.notify_idle();
            }
        } else {
            state.searching = false;
            state.stop_flag = None;
            state.ponder_hit_flag = None;
            finalize_and_send_fast(state, "gameover_immediate_finalize");
            state.current_is_ponder = false;
            state.current_root_hash = None;
            state.current_time_control = None;
            state.notify_idle();
        }
    } else {
        if let Some(flag) = &state.stop_flag {
            flag.store(true, Ordering::SeqCst);
        }
        if let Some(h) = state.worker.take() {
            enqueue_reaper(state, h, "gameover_no_bestmove_join");
        } else {
            state.notify_idle();
        }
        state.searching = false;
        state.stop_flag = None;
        state.ponder_hit_flag = None;
        state.current_time_control = None;
    }
}
