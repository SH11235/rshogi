use std::sync::mpsc;
use std::time::Duration;

use engine_core::search::parallel::{FinalizeReason, FinalizerMsg};

use crate::finalize::{finalize_and_send, finalize_and_send_fast};
use crate::io::info_string;
use crate::state::EngineState;
use crate::util::{enqueue_reaper, join_search_handle};

/// Poll and handle out-of-band finalize requests coming from engine-core.
///
/// This function is cheap (non-blocking) and is intended to be called frequently
/// from the USI main loop. It ensures exactly-once bestmove emission per session
/// by respecting `state.bestmove_emitted` and matching the engine-core session id.
pub fn poll_oob_finalize(state: &mut EngineState) {
    let Some(rx) = state.finalizer_rx.take() else {
        return;
    };

    // Drain at most a few messages to keep the loop responsive
    // 増やして取りこぼしを抑制（E2E検証支援）
    for _ in 0..16 {
        let msg = match rx.try_recv() {
            Ok(m) => m,
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                state.finalizer_rx = Some(rx);
                return;
            }
        };

        match msg {
            FinalizerMsg::SessionStart { session_id } => {
                state.current_session_core_id = Some(session_id);
                info_string(format!("oob_session_start id={}", session_id));
            }
            FinalizerMsg::Finalize { session_id, reason } => {
                // Accept only if this session is active and we haven't emitted yet
                if !state.searching || state.bestmove_emitted {
                    continue;
                }
                // Late-bind if SessionStart hasn't arrived yet
                if state.current_session_core_id.is_none() {
                    state.current_session_core_id = Some(session_id);
                    info_string(format!("oob_session_late_bind id={}", session_id));
                }
                if state.current_session_core_id != Some(session_id) {
                    // Stale or mismatched session; ignore
                    info_string(format!(
                        "oob_finalize_ignored stale=1 sid={} cur={:?}",
                        session_id, state.current_session_core_id
                    ));
                    continue;
                }

                let label = match reason {
                    FinalizeReason::Hard => "oob_hard_finalize",
                    FinalizeReason::NearHard => "oob_near_hard_finalize",
                    FinalizeReason::Planned => "oob_planned_finalize",
                    FinalizeReason::TimeManagerStop => "oob_tm_finalize",
                    FinalizeReason::UserStop => "oob_user_finalize",
                };

                info_string(format!("oob_finalize_request reason={:?} sid={}", reason, session_id));

                // Step 1: broadcast immediate stop to search threads
                state.stop_bridge.request_stop_immediate();

                // Step 2: compute wait budget based on time control and StopWaitMs
                // Stage 1: Prefer in-place join with extended waiting
                let stop_wait_ms = state.opts.stop_wait_ms;
                let is_pure_byoyomi = if let Some(ref tc) = state.current_time_control {
                    use engine_core::time_management::TimeControl;
                    matches!(
                        tc,
                        TimeControl::Byoyomi {
                            main_time_ms: 0,
                            ..
                        }
                    )
                } else {
                    false
                };

                let wait_budget_ms = if is_pure_byoyomi {
                    // Pure byoyomi: allow longer wait (150-250ms range)
                    stop_wait_ms.clamp(150, 250)
                } else {
                    // Other time controls: conservative wait
                    stop_wait_ms.clamp(50, 150)
                };

                info_string(format!(
                    "oob_finalize_wait_budget budget_ms={} is_pure_byo={} stop_wait_ms={}",
                    wait_budget_ms, is_pure_byoyomi as u8, stop_wait_ms
                ));

                // Step 3: try to receive result with bounded waiting
                // Review feedback: Limit max consecutive blocking to 2 rounds (100ms)
                // to keep USI main loop responsive
                let mut finalize_candidate: Option<(u64, engine_core::search::SearchResult)> = None;
                if let Some(rx_res) = &state.result_rx {
                    let chunk_ms = 50u64; // Wait in 50ms chunks
                    let max_rounds = wait_budget_ms.div_ceil(chunk_ms);
                    let max_consecutive_rounds = 2; // Max 100ms consecutive blocking
                    info_string(format!(
                        "oob_recv_wait_start budget_ms={} max_rounds={}",
                        wait_budget_ms, max_rounds
                    ));

                    let mut rounds_done = 0;
                    while rounds_done < max_rounds {
                        let rounds_this_batch =
                            (max_rounds - rounds_done).min(max_consecutive_rounds);

                        for batch_round in 0..rounds_this_batch {
                            let round = rounds_done + batch_round;
                            match rx_res.recv_timeout(Duration::from_millis(chunk_ms)) {
                                Ok(pair) => {
                                    info_string(format!(
                                        "oob_recv_result round={} waited_ms={}",
                                        round,
                                        (round + 1) * chunk_ms
                                    ));
                                    finalize_candidate = Some(pair);
                                    break;
                                }
                                Err(mpsc::RecvTimeoutError::Timeout) => {
                                    // Log every 4 rounds (200ms) to track progress
                                    if round % 4 == 3 || round == max_rounds - 1 {
                                        info_string(format!(
                                            "oob_recv_waiting round={}/{} waited_ms={}",
                                            round + 1,
                                            max_rounds,
                                            (round + 1) * chunk_ms
                                        ));
                                    }
                                    continue;
                                }
                                Err(mpsc::RecvTimeoutError::Disconnected) => {
                                    info_string("oob_recv_disconnected");
                                    break;
                                }
                            }
                        }

                        if finalize_candidate.is_some() {
                            break;
                        }

                        rounds_done += rounds_this_batch;

                        // Early return point: put rx back and exit to keep main loop responsive
                        if rounds_done < max_rounds {
                            state.finalizer_rx = Some(rx);
                            return;
                        }
                    }

                    if finalize_candidate.is_none() {
                        info_string(format!(
                            "oob_recv_timeout_all budget_ms={} max_rounds={}",
                            wait_budget_ms, max_rounds
                        ));
                    }
                }

                // Step 4: finalize with result or detach as last resort
                if let Some((sid, result)) = finalize_candidate {
                    if sid == state.current_search_id {
                        info_string(format!("oob_finalize_joined sid={} label={}", sid, label));
                        if let Some(h) = state.worker.take() {
                            join_search_handle(h, label);
                            state.notify_idle();
                        }
                        state.searching = false;
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;
                        let stale = state
                            .current_root_hash
                            .map(|h| h != state.position.zobrist_hash())
                            .unwrap_or(false);
                        finalize_and_send(state, label, Some(&result), stale);
                        state.current_is_ponder = false;
                        state.current_root_hash = None;
                        state.current_time_control = None;
                        state.notify_idle();
                    } else {
                        // Stale result id; fall back to fast finalize path
                        info_string(format!(
                            "oob_finalize_stale sid={} cur_sid={} fast_path=1",
                            sid, state.current_search_id
                        ));
                        fast_finalize_and_detach(state, label);
                    }
                } else {
                    // Review feedback: For pure byoyomi with sufficient margin, prohibit detach
                    // and extend wait instead of giving up
                    let should_prohibit_detach = if is_pure_byoyomi {
                        // Check if we have at least 300ms margin left
                        if let Some(ref tc) = state.current_time_control {
                            use engine_core::time_management::TimeControl;
                            if let TimeControl::Byoyomi { byoyomi_ms, .. } = tc {
                                // Estimate elapsed: start time not available here, use conservative heuristic
                                // If we reach here with budget ≥ 200ms, likely still have margin
                                wait_budget_ms >= 200 && *byoyomi_ms >= 1000
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if should_prohibit_detach {
                        info_string(format!(
                            "oob_finalize_prohibit_detach pure_byo=1 budget={} retry=1",
                            wait_budget_ms
                        ));
                        // Put rx back and retry on next poll
                        state.finalizer_rx = Some(rx);
                        return;
                    }

                    info_string(format!(
                        "oob_finalize_timeout no_result=1 sid={} detach=1",
                        session_id
                    ));
                    fast_finalize_and_detach(state, label);
                }
            }
        }
    }

    // Put receiver back
    state.finalizer_rx = Some(rx);
}

fn fast_finalize_and_detach(state: &mut EngineState, label: &str) {
    use std::sync::atomic::Ordering;

    let worker = state.worker.take();
    state.searching = false;
    state.stop_flag = None;
    state.ponder_hit_flag = None;
    state.result_rx = None;
    finalize_and_send_fast(state, label);
    state.current_is_ponder = false;
    state.current_root_hash = None;
    state.current_time_control = None;
    if let Some(handle) = worker {
        // Increment detach counter for diagnostics
        let count = state.oob_detach_count.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        info_string(format!("oob_detach label={} detach_count={}", label, count));
        enqueue_reaper(state, handle, label);
    } else {
        state.notify_idle();
    }
}
