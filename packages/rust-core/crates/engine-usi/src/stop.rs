use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};

use engine_core::time_management::TimeControl;

use crate::finalize::{emit_bestmove_once, finalize_and_send, finalize_and_send_fast};
use crate::io::info_string;
use crate::state::EngineState;
use engine_core::search::parallel::FinalizeReason;
use engine_core::usi::move_to_usi;

pub fn handle_stop(state: &mut EngineState) {
    if let (true, Some(flag)) = (state.searching, &state.stop_flag) {
        flag.store(true, Ordering::SeqCst);
        info_string("stop_requested");

        // Use SearchSession API instead of manual channel
        if let Some(session) = state.search_session.take() {
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

            // Wait for result with timeout using SearchSession API
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

                // Use SearchSession::recv_result_timeout() instead of manual channel
                match session.recv_result_timeout(slice) {
                    Some(result) => {
                        // No session_id check needed - SearchSession manages this internally
                        // No worker join needed - SearchSession manages thread lifecycle
                        state.searching = false;
                        // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;

                        if let Some(tm) = state.active_time_manager.take() {
                            let elapsed_ms = result.stats.elapsed.as_millis() as u64;
                            let time_state = state.time_state_for_update(elapsed_ms);
                            tm.update_after_move(elapsed_ms, time_state);
                        }
                        let stale = state
                            .current_root_hash
                            .map(|h| h != state.position.zobrist_hash())
                            .unwrap_or(false);
                        finalize_and_send(
                            state,
                            "stop_finalize",
                            Some(&result),
                            stale,
                            Some(FinalizeReason::UserStop),
                        );
                        if !state.bestmove_emitted {
                            let fallback = result
                                .best_move
                                .map(|mv| move_to_usi(&mv))
                                .unwrap_or_else(|| "resign".to_string());
                            emit_bestmove_once(state, fallback, None);
                        }
                        state.current_is_ponder = false;
                        state.current_root_hash = None;
                        state.current_time_control = None;
                        state.notify_idle();
                        finalized = true;
                    }
                    None => {
                        // Timeout or disconnected - continue waiting
                    }
                }
            }
            // Timeout expired - try immediate stop and quick polling
            if !finalized {
                state.stop_bridge.request_stop();

                let mut waited_after_stop_ms = 0u64;
                let mut finalize_candidate: Option<engine_core::search::SearchResult> = None;

                // Try a few quick polls with backoff
                for backoff in [5u64, 10, 20, 40] {
                    use engine_core::engine::TryResult;
                    match session.try_poll() {
                        TryResult::Ok(result) => {
                            finalize_candidate = Some(result);
                            break;
                        }
                        TryResult::Pending => {
                            thread::sleep(Duration::from_millis(backoff));
                            waited_after_stop_ms += backoff;
                        }
                        TryResult::Disconnected => {
                            // Thread died - will use fallback
                            break;
                        }
                    }
                }

                // One final check
                if finalize_candidate.is_none() {
                    use engine_core::engine::TryResult;
                    if let TryResult::Ok(result) = session.try_poll() {
                        finalize_candidate = Some(result);
                    }
                }

                let snapshot = state.stop_bridge.snapshot();
                let has_result = finalize_candidate.is_some();

                info_string(format!(
                    "fast_finalize active_threads={} pending={} waited_ms={} has_result={}",
                    snapshot.active_workers,
                    snapshot.pending_work_items,
                    waited_after_stop_ms,
                    if has_result { 1 } else { 0 }
                ));

                // Finalize with result if available
                if let Some(result) = finalize_candidate {
                    state.searching = false;
                    // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                    state.stop_flag = None;
                    state.ponder_hit_flag = None;

                    let stale = state
                        .current_root_hash
                        .map(|h| h != state.position.zobrist_hash())
                        .unwrap_or(false);
                    if let Some(tm) = state.active_time_manager.take() {
                        let elapsed_ms = result.stats.elapsed.as_millis() as u64;
                        let time_state = state.time_state_for_update(elapsed_ms);
                        tm.update_after_move(elapsed_ms, time_state);
                    }
                    finalize_and_send(
                        state,
                        "stop_finalize",
                        Some(&result),
                        stale,
                        Some(FinalizeReason::UserStop),
                    );
                    if !state.bestmove_emitted {
                        let fallback = result
                            .best_move
                            .map(|mv| move_to_usi(&mv))
                            .unwrap_or_else(|| "resign".to_string());
                        emit_bestmove_once(state, fallback, None);
                    }
                    state.current_is_ponder = false;
                    state.current_root_hash = None;
                    state.current_time_control = None;
                    state.notify_idle();
                    return;
                }

                // No result available - use fast finalize
                // SearchSession will clean up automatically on drop
                state.searching = false;
                // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                state.finalize_time_manager();
                finalize_and_send_fast(
                    state,
                    "stop_timeout_finalize",
                    Some(FinalizeReason::UserStop),
                );
                state.current_is_ponder = false;
                state.current_root_hash = None;
                state.current_time_control = None;
                state.notify_idle();
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
            // No worker to clean up - SearchSession handles this automatically
            state.searching = false;
            // Clear stop_flag - each session gets a fresh flag to avoid race conditions
            state.stop_flag = None;
            state.ponder_hit_flag = None;
            state.current_time_control = None;
            state.notify_idle();
            return;
        }
        if let Some(flag) = &state.stop_flag {
            flag.store(true, Ordering::SeqCst);
        }
        if state.searching {
            // Use SearchSession API instead of manual channel
            if let Some(session) = state.search_session.take() {
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

                    // Use SearchSession::recv_result_timeout() instead of manual channel
                    match session.recv_result_timeout(slice) {
                        Some(result) => {
                            // No session_id check needed - SearchSession manages this internally
                            // No worker join needed - SearchSession manages thread lifecycle
                            state.searching = false;
                            // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                            state.stop_flag = None;
                            state.ponder_hit_flag = None;

                            let stale = state
                                .current_root_hash
                                .map(|h| h != state.position.zobrist_hash())
                                .unwrap_or(false);
                            if let Some(tm) = state.active_time_manager.take() {
                                let elapsed_ms = result.stats.elapsed.as_millis() as u64;
                                let time_state = state.time_state_for_update(elapsed_ms);
                                tm.update_after_move(elapsed_ms, time_state);
                            }
                            finalize_and_send(
                                state,
                                "gameover_finalize",
                                Some(&result),
                                stale,
                                Some(FinalizeReason::UserStop),
                            );
                            if !state.bestmove_emitted {
                                let fallback = result
                                    .best_move
                                    .map(|mv| move_to_usi(&mv))
                                    .unwrap_or_else(|| "resign".to_string());
                                emit_bestmove_once(state, fallback, None);
                            }
                            state.current_is_ponder = false;
                            state.current_root_hash = None;
                            state.current_time_control = None;
                            state.notify_idle();
                            finalized = true;
                        }
                        None => {
                            // Timeout or disconnected - continue waiting
                        }
                    }
                }

                // Timeout expired - try quick polling
                if !finalized {
                    use engine_core::engine::TryResult;
                    if let TryResult::Ok(result) = session.try_poll() {
                        state.searching = false;
                        // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                        state.stop_flag = None;
                        state.ponder_hit_flag = None;

                        let stale = state
                            .current_root_hash
                            .map(|h| h != state.position.zobrist_hash())
                            .unwrap_or(false);
                        if let Some(tm) = state.active_time_manager.take() {
                            let elapsed_ms = result.stats.elapsed.as_millis() as u64;
                            let time_state = state.time_state_for_update(elapsed_ms);
                            tm.update_after_move(elapsed_ms, time_state);
                        }
                        finalize_and_send(
                            state,
                            "gameover_finalize",
                            Some(&result),
                            stale,
                            Some(FinalizeReason::UserStop),
                        );
                        if !state.bestmove_emitted {
                            let fallback = result
                                .best_move
                                .map(|mv| move_to_usi(&mv))
                                .unwrap_or_else(|| "resign".to_string());
                            emit_bestmove_once(state, fallback, None);
                        }
                        state.current_is_ponder = false;
                        state.current_root_hash = None;
                        state.current_time_control = None;
                        state.notify_idle();
                        return;
                    }

                    // No result available - use fast finalize
                    // SearchSession will clean up automatically on drop
                    state.searching = false;
                    // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                    state.stop_flag = None;
                    state.ponder_hit_flag = None;
                    state.finalize_time_manager();
                    finalize_and_send_fast(
                        state,
                        "gameover_timeout_finalize",
                        Some(FinalizeReason::UserStop),
                    );
                    state.current_is_ponder = false;
                    state.current_root_hash = None;
                    state.current_time_control = None;
                    state.notify_idle();
                }
            } else {
                state.searching = false;
                // Clear stop_flag - each session gets a fresh flag to avoid race conditions
                state.stop_flag = None;
                state.ponder_hit_flag = None;
                state.finalize_time_manager();
                finalize_and_send_fast(
                    state,
                    "gameover_immediate_finalize",
                    Some(FinalizeReason::UserStop),
                );
                state.current_is_ponder = false;
                state.current_root_hash = None;
                state.current_time_control = None;
                state.notify_idle();
            }
        } else {
            state.searching = false;
            // Clear stop_flag - each session gets a fresh flag to avoid race conditions
            state.stop_flag = None;
            state.ponder_hit_flag = None;
            state.finalize_time_manager();
            finalize_and_send_fast(
                state,
                "gameover_immediate_finalize",
                Some(FinalizeReason::UserStop),
            );
            state.current_is_ponder = false;
            state.current_root_hash = None;
            state.current_time_control = None;
            state.notify_idle();
        }
    } else {
        if let Some(flag) = &state.stop_flag {
            flag.store(true, Ordering::SeqCst);
        }
        // SearchSession will clean up automatically on drop
        state.search_session = None;
        state.searching = false;
        // Clear stop_flag - each session gets a fresh flag to avoid race conditions
        state.stop_flag = None;
        state.ponder_hit_flag = None;
        state.current_time_control = None;
        state.notify_idle();
    }
}
