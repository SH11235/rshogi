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

                // compute wait budget based on time control and StopWaitMs
                // Prefer in-place join with extended waiting
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

                // Step 3: try to receive result with bounded waiting using SearchSession
                let mut finalize_candidate: Option<engine_core::search::SearchResult> = None;
                if let Some(session) = &state.search_session {
                    let chunk_ms = 50u64; // Wait in 50ms chunks
                    let max_rounds = wait_budget_ms.div_ceil(chunk_ms);
                    info_string(format!(
                        "oob_recv_wait_start budget_ms={} max_rounds={} session_id={}",
                        wait_budget_ms,
                        max_rounds,
                        session.session_id()
                    ));

                    for round in 0..max_rounds {
                        match session.recv_result_timeout(Duration::from_millis(chunk_ms)) {
                            Some(result) => {
                                info_string(format!(
                                    "oob_recv_result round={} waited_ms={}",
                                    round,
                                    (round + 1) * chunk_ms
                                ));
                                finalize_candidate = Some(result);
                                break;
                            }
                            None => {
                                // Timeout or disconnected - log every 4 rounds (200ms)
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
                        }
                    }

                    if finalize_candidate.is_none() {
                        info_string(format!(
                            "oob_recv_timeout_all budget_ms={} max_rounds={}",
                            wait_budget_ms, max_rounds
                        ));
                    }
                }

                // Step 4: finalize with result or use fast path
                if let Some(result) = finalize_candidate {
                    info_string(format!("oob_finalize_joined label={}", label));
                    state.searching = false;
                    // Keep stop_flag for reuse in next session (don't set to None)
                    state.ponder_hit_flag = None;
                    state.search_session = None;
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
                    // Result not received within wait budget → fast finalize
                    // Note: Previously tried to prohibit fast path for pure byoyomi with margin,
                    // but this caused infinite loops because Finalize message is not resent.
                    // Better to send bestmove immediately than to time-loss.
                    info_string(format!(
                        "oob_finalize_timeout no_result=1 sid={} budget_ms={}",
                        session_id, wait_budget_ms
                    ));
                    fast_finalize_no_detach(state, label);
                }
            }
        }
    }

    // Put receiver back
    state.finalizer_rx = Some(rx);
}

/// Fast finalize without waiting for result (SearchSession will clean up automatically)
fn fast_finalize_no_detach(state: &mut EngineState, label: &str) {
    state.searching = false;
    // Keep stop_flag for reuse in next session (don't set to None)
    state.ponder_hit_flag = None;
    state.search_session = None;
    finalize_and_send_fast(state, label);
    state.current_is_ponder = false;
    state.current_root_hash = None;
    state.current_time_control = None;
    state.notify_idle();
}
