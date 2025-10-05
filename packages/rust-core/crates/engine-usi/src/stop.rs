use std::sync::{atomic::Ordering, Arc};
use std::thread;
use std::time::{Duration, Instant};

use engine_core::time_management::{
    detect_game_phase_for_time, TimeControl, TimeLimits, TimeManager, TimeParametersBuilder,
};
use engine_core::{engine::session::SearchSession, search::SearchResult};

use crate::finalize::{emit_bestmove_once, finalize_and_send, finalize_and_send_fast, fmt_hash};
use crate::io::info_string;
use crate::state::EngineState;
use engine_core::search::parallel::FinalizeReason;
use engine_core::search::types::StopInfo;
use engine_core::usi::move_to_usi;

pub(crate) const WAIT_CHUNK_MS: u64 = 50;

pub(crate) fn is_pure_byoyomi(state: &EngineState) -> bool {
    matches!(
        state.current_time_control,
        Some(TimeControl::Byoyomi { main_time_ms, .. }) if main_time_ms == 0
            || main_time_ms <= state.opts.network_delay2_ms
    )
}

pub(crate) fn compute_wait_budget(is_pure_byoyomi: bool, stop_wait_ms: u64) -> (u64, u64) {
    if stop_wait_ms == 0 {
        return (0, 0);
    }
    let chunk_ms = WAIT_CHUNK_MS;
    let budget = if is_pure_byoyomi {
        let min_budget = chunk_ms * 3; // 保守的に 150ms を下限に保持
        let max_budget = chunk_ms * 5; // 上限は 250ms 程度に丸める
        stop_wait_ms.clamp(min_budget, max_budget)
    } else {
        stop_wait_ms
    };
    (budget, chunk_ms)
}

pub(crate) fn wait_for_result_with_budget<F>(
    session: &SearchSession,
    wait_budget_ms: u64,
    chunk_ms: u64,
    mut on_wait: F,
) -> Option<(SearchResult, u64)>
where
    F: FnMut(u64, u64),
{
    if wait_budget_ms == 0 || chunk_ms == 0 {
        return None;
    }

    let mut waited_ms = 0u64;
    let mut round = 0u64;
    while waited_ms < wait_budget_ms {
        let slice = chunk_ms.min(wait_budget_ms - waited_ms);
        if let Some(result) = session.recv_result_timeout(Duration::from_millis(slice)) {
            waited_ms += slice;
            return Some((result, waited_ms));
        }
        waited_ms += slice;
        round += 1;
        on_wait(round, waited_ms);
    }
    None
}

pub(crate) fn compute_wait_budget_from_state(
    state: &EngineState,
    reason: Option<FinalizeReason>,
) -> (u64, u64, bool) {
    let pure = is_pure_byoyomi(state);

    if let Some(reason) = reason {
        match reason {
            FinalizeReason::Hard => return (0, 0, pure),
            FinalizeReason::NearHard => return (WAIT_CHUNK_MS, WAIT_CHUNK_MS, pure),
            _ => {}
        }
    }

    if matches!(
        state.current_time_control,
        Some(TimeControl::FixedTime { .. } | TimeControl::Infinite)
    ) {
        return (0, 0, pure);
    }

    let (budget, chunk) = compute_wait_budget(pure, state.opts.stop_wait_ms);
    (budget, chunk, pure)
}

pub fn handle_stop(state: &mut EngineState) {
    if let (true, Some(flag)) = (state.searching, &state.stop_flag) {
        flag.store(true, Ordering::SeqCst);
        info_string("stop_requested");

        // Use SearchSession API instead of manual channel
        if let Some(session) = state.search_session.take() {
            let (wait_budget_ms, chunk_ms, pure_byo) = compute_wait_budget_from_state(state, None);
            if wait_budget_ms == 0 {
                if let Some(tc) = &state.current_time_control {
                    match tc {
                        TimeControl::FixedTime { .. } | TimeControl::Infinite => {
                            info_string("stop_fast_finalize=fixed_or_infinite");
                        }
                        TimeControl::Byoyomi { .. } if pure_byo => {
                            info_string("stop_fast_finalize=byoyomi");
                        }
                        _ => {}
                    }
                }
            }

            info_string(format!(
                "stop_wait_budget budget_ms={} is_pure_byo={} stop_wait_ms={} chunk_ms={}",
                wait_budget_ms, pure_byo as u8, state.opts.stop_wait_ms, chunk_ms
            ));

            // Wait for result with timeout using SearchSession API
            let mut finalized = false;
            if wait_budget_ms > 0 {
                let sid = session.session_id();
                info_string(format!(
                    "stop_recv_wait_start sid={} budget_ms={} chunk_ms={}",
                    sid, wait_budget_ms, chunk_ms
                ));
                let log_wait = |round: u64, waited_ms: u64| {
                    if round.is_multiple_of(4) || waited_ms >= wait_budget_ms {
                        info_string(format!(
                            "stop_recv_waiting sid={} round={} waited_ms={}",
                            sid, round, waited_ms
                        ));
                    }
                };

                if let Some((result, waited)) = wait_for_result_with_budget(
                    &session,
                    wait_budget_ms,
                    chunk_ms,
                    log_wait,
                ) {
                    info_string(format!("stop_recv_result sid={} waited_ms={}", sid, waited));
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
                        let _ = emit_bestmove_once(state, fallback, None);
                    }
                    state.current_is_ponder = false;
                    state.current_root_hash = None;
                    state.current_time_control = None;
                    state.notify_idle();
                    finalized = true;
                } else {
                    info_string(format!(
                        "stop_recv_timeout_all sid={} budget_ms={}",
                        sid, wait_budget_ms
                    ));
                }
            }
            // Timeout expired - try immediate stop and quick polling
            if !finalized {
                state.stop_controller.request_stop();

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

                let snapshot = state.stop_controller.snapshot();
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
                        let _ = emit_bestmove_once(state, fallback, None);
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
    } else if !state.searching && !state.bestmove_emitted {
        let sid = state.current_session_core_id.unwrap_or(0);
        let root = state.current_root_hash.unwrap_or_else(|| state.position.zobrist_hash());
        info_string(format!(
            "stop_post_completion sid={} root={} current_is_ponder={}",
            sid,
            fmt_hash(root),
            state.current_is_ponder as u8
        ));

        state.stop_controller.request_finalize(FinalizeReason::UserStop);
        state.finalize_time_manager();
        finalize_and_send_fast(
            state,
            "stop_post_completion_finalize",
            Some(FinalizeReason::UserStop),
        );

        if !state.bestmove_emitted {
            // As a last resort fall back to legal move selection directly.
            let fallback = {
                let eng = state.engine.lock().unwrap();
                eng.choose_final_bestmove(&state.position, None)
            };
            let final_usi = fallback
                .best_move
                .map(|mv| move_to_usi(&mv))
                .unwrap_or_else(|| "resign".to_string());
            let ponder = None;
            let _ = emit_bestmove_once(state, final_usi, ponder);
        }

        state.current_time_control = None;
        state.current_root_hash = None;
        state.active_time_manager = None;
        state.notify_idle();
    } else {
        info_string(format!(
            "stop_ignored searching={} bestmove_emitted={}",
            state.searching as u8, state.bestmove_emitted as u8
        ));
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

        if state.active_time_manager.is_none() {
            if let Some(ref tc) = state.current_time_control {
                let mut builder = TimeParametersBuilder::new()
                    .overhead_ms(state.opts.overhead_ms)
                    .unwrap()
                    .network_delay_ms(state.opts.network_delay_ms)
                    .unwrap()
                    .network_delay2_ms(state.opts.network_delay2_ms)
                    .unwrap()
                    .byoyomi_safety_ms(state.opts.byoyomi_safety_ms)
                    .unwrap()
                    .byoyomi_early_finish_ratio(state.opts.byoyomi_early_finish_ratio)
                    .unwrap()
                    .pv_stability_base(state.opts.pv_stability_base)
                    .unwrap()
                    .pv_stability_slope(state.opts.pv_stability_slope)
                    .unwrap()
                    .slow_mover_pct(state.opts.slow_mover_pct)
                    .unwrap()
                    .max_time_ratio(state.opts.max_time_ratio_pct as f64 / 100.0)
                    .unwrap();
                if state.opts.move_horizon_trigger_ms > 0 {
                    builder = builder
                        .move_horizon_guard(
                            state.opts.move_horizon_trigger_ms,
                            state.opts.move_horizon_min_moves,
                        )
                        .unwrap();
                }
                let mut tp = builder.build();
                tp.min_think_ms = state.opts.min_think_ms;

                let go_snapshot = state.last_go_params.clone();
                let pending_limits = TimeLimits {
                    time_control: tc.clone(),
                    moves_to_go: go_snapshot.as_ref().and_then(|gp| gp.moves_to_go),
                    depth: go_snapshot.as_ref().and_then(|gp| gp.depth),
                    nodes: go_snapshot.as_ref().and_then(|gp| gp.nodes),
                    time_parameters: Some(tp),
                    random_time_ms: go_snapshot.as_ref().and_then(|gp| gp.rtime),
                };

                let phase = detect_game_phase_for_time(&state.position, state.position.ply as u32);
                let tm_new = Arc::new(TimeManager::new(
                    &pending_limits,
                    state.position.side_to_move,
                    state.position.ply as u32,
                    phase,
                ));
                state.active_time_manager = Some(Arc::clone(&tm_new));
                info_string("ponderhit_time_manager_created=1");
            } else {
                info_string("ponderhit_time_manager_missing=1");
            }
        }

        let mut soft_hard: Option<(u64, u64)> = None;

        if let Some(tm) = state.active_time_manager.as_ref() {
            let tc_str = state
                .current_time_control
                .as_ref()
                .map(|tc| format!("{tc:?}"))
                .unwrap_or_else(|| "None".to_string());
            if tm.is_pondering() {
                let elapsed_ms = tm.elapsed_ms();
                tm.ponder_hit(None, elapsed_ms);
                let soft_ms = tm.soft_limit_ms();
                let hard_ms = tm.hard_limit_ms();
                info_string(format!(
                    "time_budget soft_ms={} hard_ms={} source=ponderhit elapsed_ms={} tc={}",
                    soft_ms, hard_ms, elapsed_ms, tc_str
                ));
                soft_hard = Some((soft_ms, hard_ms));
            } else {
                let soft_ms = tm.soft_limit_ms();
                let hard_ms = tm.hard_limit_ms();
                info_string(format!(
                    "time_budget soft_ms={} hard_ms={} source=ponderhit_reinit tc={}",
                    soft_ms, hard_ms, tc_str
                ));
                soft_hard = Some((soft_ms, hard_ms));
            }
        } else {
            info_string("ponderhit_time_manager_unavailable=1");
        }

        if let Some((soft_ms, hard_ms)) = soft_hard {
            if hard_ms != u64::MAX && hard_ms > 0 {
                let now = Instant::now();
                let hard_deadline = now + Duration::from_millis(hard_ms);
                state.deadline_hard = Some(hard_deadline);
                let lead_ms = if state.opts.byoyomi_deadline_lead_ms > 0 {
                    state.opts.byoyomi_deadline_lead_ms
                } else if matches!(state.current_time_control, Some(TimeControl::Byoyomi { .. })) {
                    200
                } else {
                    0
                };
                state.deadline_near = if lead_ms > 0 {
                    hard_deadline.checked_sub(Duration::from_millis(lead_ms))
                } else {
                    None
                };
                state.deadline_near_notified = false;

                let stop_info = StopInfo {
                    soft_limit_ms: if soft_ms != u64::MAX { soft_ms } else { 0 },
                    hard_limit_ms: hard_ms,
                    ..Default::default()
                };
                state.stop_controller.prime_stop_info(stop_info);
            } else {
                state.deadline_hard = None;
                state.deadline_near = None;
                state.deadline_near_notified = false;
            }
        } else {
            state.deadline_hard = None;
            state.deadline_near = None;
            state.deadline_near_notified = false;
        }
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
                let (wait_budget_ms, chunk_ms, pure_byo) =
                    compute_wait_budget_from_state(state, None);
                if wait_budget_ms == 0 {
                    if let Some(tc) = &state.current_time_control {
                        match tc {
                            TimeControl::FixedTime { .. } | TimeControl::Infinite => {
                                info_string("gameover_fast_finalize=fixed_or_infinite");
                            }
                            TimeControl::Byoyomi { .. } if pure_byo => {
                                info_string("gameover_fast_finalize=byoyomi");
                            }
                            _ => {}
                        }
                    }
                }

                info_string(format!(
                    "gameover_wait_budget budget_ms={} is_pure_byo={} stop_wait_ms={} chunk_ms={}",
                    wait_budget_ms, pure_byo as u8, state.opts.stop_wait_ms, chunk_ms
                ));

                let mut finalized = false;
                if wait_budget_ms > 0 {
                    let log_wait = |round: u64, waited_ms: u64| {
                        if round.is_multiple_of(4) || waited_ms >= wait_budget_ms {
                            info_string(format!(
                                "gameover_recv_waiting round={} waited_ms={}",
                                round, waited_ms
                            ));
                        }
                    };

                    if let Some((result, waited)) =
                        wait_for_result_with_budget(&session, wait_budget_ms, chunk_ms, log_wait)
                    {
                        info_string(format!("gameover_recv_result waited_ms={}", waited));
                        state.searching = false;
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
                            let _ = emit_bestmove_once(state, fallback, None);
                        }
                        state.current_is_ponder = false;
                        state.current_root_hash = None;
                        state.current_time_control = None;
                        state.notify_idle();
                        finalized = true;
                    } else {
                        info_string(format!(
                            "gameover_recv_timeout_all budget_ms={}",
                            wait_budget_ms
                        ));
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
                            let _ = emit_bestmove_once(state, fallback, None);
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
        info_string("stop_ignored search_inactive=1");
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

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::time_management::{
        detect_game_phase_for_time, TimeControl, TimeLimits, TimeManager, TimeParameters,
    };
    use std::sync::{atomic::AtomicBool, Arc};

    #[test]
    fn handle_ponderhit_reinitializes_time_manager_and_deadlines() {
        let mut state = EngineState::new();
        state.opts.ponder = true;
        state.searching = true;
        state.current_is_ponder = true;
        state.current_time_control = Some(TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 10_000,
            periods: 1,
        });

        let pending_limits = TimeLimits {
            time_control: state.current_time_control.as_ref().unwrap().clone(),
            moves_to_go: None,
            depth: None,
            nodes: None,
            time_parameters: Some(TimeParameters::default()),
            random_time_ms: None,
        };

        let phase = detect_game_phase_for_time(&state.position, state.position.ply as u32);
        let tm = Arc::new(TimeManager::new_ponder(
            &pending_limits,
            state.position.side_to_move,
            state.position.ply as u32,
            phase,
        ));
        state.active_time_manager = Some(Arc::clone(&tm));

        state.ponder_hit_flag = Some(Arc::new(AtomicBool::new(false)));

        handle_ponderhit(&mut state);

        assert!(!tm.is_pondering(), "TimeManager should exit ponder mode after ponderhit");

        let hard_ms = tm.hard_limit_ms();
        assert!(hard_ms != u64::MAX && hard_ms > 0);
        assert!(state.deadline_hard.is_some());
        assert!(!state.deadline_near_notified);
        assert!(!state.current_is_ponder);
    }

    #[test]
    fn handle_stop_emits_bestmove_after_ponder_completion() {
        let mut state = EngineState::new();
        state.searching = false;
        state.bestmove_emitted = false;
        state.current_is_ponder = false;
        state.current_session_core_id = Some(77);
        state.current_root_hash = Some(state.position.zobrist_hash());

        let limits = TimeLimits {
            time_control: TimeControl::FixedTime { ms_per_move: 1000 },
            ..Default::default()
        };
        let phase = detect_game_phase_for_time(&state.position, state.position.ply as u32);
        let tm = Arc::new(TimeManager::new(
            &limits,
            state.position.side_to_move,
            state.position.ply as u32,
            phase,
        ));
        state.active_time_manager = Some(Arc::clone(&tm));

        handle_stop(&mut state);

        assert!(state.bestmove_emitted, "stop should emit fallback bestmove");
        assert!(state.current_root_hash.is_none());
        assert!(state.active_time_manager.is_none());
    }
}
