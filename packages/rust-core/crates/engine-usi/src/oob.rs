use std::sync::mpsc;
use std::time::Duration;

use engine_core::search::parallel::{FinalizeReason, FinalizerMsg};

use crate::finalize::{finalize_and_send, finalize_and_send_fast};
use crate::io::info_string;
use crate::state::EngineState;

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

                // Step 2: try to pick up a result quickly without blocking
                let mut finalize_candidate: Option<(u64, engine_core::search::SearchResult)> = None;
                if let Some(rx_res) = &state.result_rx {
                    for backoff in [0u64, 5, 10] {
                        if backoff > 0 {
                            std::thread::sleep(Duration::from_millis(backoff));
                        }
                        match rx_res.try_recv() {
                            Ok(pair) => {
                                finalize_candidate = Some(pair);
                                break;
                            }
                            Err(mpsc::TryRecvError::Empty) => continue,
                            Err(mpsc::TryRecvError::Disconnected) => break,
                        }
                    }
                }

                // Step 3: finalize
                if let Some((sid, result)) = finalize_candidate {
                    if sid == state.current_search_id {
                        info_string(format!("oob_finalize_joined sid={} label={}", sid, label));
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
                    info_string(format!("oob_finalize_fast_path no_result=1 sid={}", session_id));
                    fast_finalize_and_detach(state, label);
                }
            }
        }
    }

    // Put receiver back
    state.finalizer_rx = Some(rx);
}

fn fast_finalize_and_detach(state: &mut EngineState, label: &str) {
    // Detach worker to reaper if exists
    if let Some(h) = state.worker.take() {
        if let Some(tx) = &state.reaper_tx {
            let q = state.reaper_queue_len.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            let _ = tx.send(h);
            const REAPER_QUEUE_SOFT_MAX: usize = 128;
            if q > REAPER_QUEUE_SOFT_MAX {
                info_string(format!("reaper_queue_len_high len={}", q));
            } else {
                info_string(format!("reaper_detach queued len={}", q));
            }
        }
        state.notify_idle();
    }

    state.searching = false;
    state.stop_flag = None;
    state.ponder_hit_flag = None;
    finalize_and_send_fast(state, label);
    state.current_is_ponder = false;
    state.current_root_hash = None;
    state.current_time_control = None;
    state.notify_idle();
}
