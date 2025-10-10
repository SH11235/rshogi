use engine_core::search::parallel::{FinalizeReason, FinalizerMsg};
use std::sync::atomic::Ordering;
use std::sync::mpsc;

use crate::finalize::{emit_bestmove_once, finalize_and_send, finalize_and_send_fast, fmt_hash};
use crate::io::diag_info_string;
use crate::io::info_string;
use crate::state::EngineState;
use crate::stop::{compute_wait_budget_from_state, wait_for_result_with_budget};
use engine_core::movegen::MoveGenerator;
use engine_core::search::snapshot::SnapshotSource;
use engine_core::search::types::Bound;
use engine_core::usi::move_to_usi;
use std::time::Instant;

/// Poll and handle out-of-band finalize requests coming from engine-core.
///
/// This function is cheap (non-blocking) and is intended to be called frequently
/// from the USI main loop. It ensures exactly-once bestmove emission per session
/// by respecting `state.bestmove_emitted` and matching the engine-core session id.
pub fn poll_oob_finalize(state: &mut EngineState) {
    let Some(rx) = state.finalizer_rx.take() else {
        return;
    };

    // Drain all pending messages (try_recv はノンブロッキングなのでループで枯らしても軽い)。
    // 将来ログ量が増えた場合は、このループ内での診断ログを間引くなどして 1 フレームの処理時間が
    // 伸びないよう調整することを想定している。
    loop {
        let msg = match rx.try_recv() {
            Ok(m) => m,
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                diag_info_string("oob_finalizer_rx_disconnected=1");
                let (tx, new_rx) = mpsc::channel();
                state.stop_controller.register_finalizer(tx);
                state.finalizer_rx = Some(new_rx);
                return;
            }
        };

        match msg {
            FinalizerMsg::SessionStart { session_id } => {
                state.current_session_core_id = Some(session_id);
                diag_info_string(format!("oob_session_start id={}", session_id));
            }
            FinalizerMsg::Finalize { session_id, reason } => {
                // Accept only if this session is active and we haven't emitted yet
                if !state.searching || state.bestmove_emitted {
                    continue;
                }
                // Suppress OOB finalize during ponder (except UserStop).
                // Note: FinalizeReason::PonderToMove is handled directly by handle_ponderhit
                // in the USI layer, so it is suppressed here to avoid duplicate finalize.
                if state.current_is_ponder && !matches!(reason, FinalizeReason::UserStop) {
                    let root =
                        state.current_root_hash.unwrap_or_else(|| state.position.zobrist_hash());
                    diag_info_string(format!(
                        "oob_finalize_guard suppressed=1 reason={:?} sid={} root={}",
                        reason,
                        session_id,
                        fmt_hash(root)
                    ));
                    continue;
                }
                // Late-bind if SessionStart hasn't arrived yet
                if state.current_session_core_id.is_none() {
                    state.current_session_core_id = Some(session_id);
                    diag_info_string(format!("oob_session_late_bind id={}", session_id));
                }
                if state.current_session_core_id != Some(session_id) {
                    // Stale or mismatched session; ignore with extended diagnostics for debugging
                    let active_session = state
                        .search_session
                        .as_ref()
                        .map(|s| s.session_id())
                        .map(|id| id.to_string())
                        .unwrap_or_else(|| "none".to_string());
                    let stop_flag = state
                        .stop_flag
                        .as_ref()
                        .map(|f| f.load(Ordering::Relaxed))
                        .unwrap_or(false);
                    diag_info_string(format!(
                        "oob_finalize_ignored stale=1 sid={} cur={:?} searching={} bestmove_emitted={} active_session={} stop_flag={} pending_result_rx={}",
                        session_id,
                        state.current_session_core_id,
                        state.searching as u8,
                        state.bestmove_emitted as u8,
                        active_session,
                        stop_flag as u8,
                        state.finalizer_rx.is_some() as u8
                    ));
                    continue;
                }

                let label = match reason {
                    FinalizeReason::Hard => "oob_hard_finalize",
                    FinalizeReason::NearHard => "oob_near_hard_finalize",
                    FinalizeReason::Planned => "oob_planned_finalize",
                    FinalizeReason::PlannedMate { .. } => "oob_planned_mate_finalize",
                    FinalizeReason::PonderToMove => "oob_ponder_to_move_finalize",
                    FinalizeReason::TimeManagerStop => "oob_tm_finalize",
                    FinalizeReason::UserStop => "oob_user_finalize",
                };

                diag_info_string(format!(
                    "oob_finalize_request reason={:?} sid={}",
                    reason, session_id
                ));

                // Step 1: broadcast immediate stop to backend/search threads
                if let Some(session) = &state.search_session {
                    session.request_stop();
                }
                // ここでは StopController には finalize を要求せず、外部 stop フラグだけを立てる。
                // finalize は最終的に fast/normal finalize の経路で一度だけ行う。
                state.stop_controller.request_stop_flag_only();

                // compute wait budget based on time control and StopWaitMs
                // Prefer in-place join with extended waiting
                let (wait_budget_ms, chunk_ms, is_pure_byoyomi) =
                    compute_wait_budget_from_state(state, Some(reason));

                diag_info_string(format!(
                    "oob_finalize_wait_budget budget_ms={} is_pure_byo={} stop_wait_ms={} chunk_ms={}",
                    wait_budget_ms,
                    is_pure_byoyomi as u8,
                    state.opts.stop_wait_ms,
                    chunk_ms
                ));

                // Step 3: try to receive result with bounded waiting using SearchSession
                let mut finalize_candidate: Option<engine_core::search::SearchResult> = None;
                if let Some(session) = &state.search_session {
                    if wait_budget_ms > 0 {
                        let max_rounds = wait_budget_ms.div_ceil(chunk_ms.max(1));
                        diag_info_string(format!(
                            "oob_recv_wait_start budget_ms={} max_rounds={} session_id={}",
                            wait_budget_ms,
                            max_rounds,
                            session.session_id()
                        ));

                        let log_wait = |round: u64, waited_ms: u64| {
                            if round.is_multiple_of(4) || waited_ms >= wait_budget_ms {
                                diag_info_string(format!(
                                    "oob_recv_waiting round={} waited_ms={}",
                                    round, waited_ms
                                ));
                            }
                        };

                        if let Some((result, waited)) = wait_for_result_with_budget(
                            session,
                            wait_budget_ms,
                            chunk_ms.max(1),
                            log_wait,
                        ) {
                            diag_info_string(format!(
                                "oob_recv_result waited_ms={} session_id={}",
                                waited,
                                session.session_id()
                            ));
                            finalize_candidate = Some(result);
                        } else {
                            diag_info_string(format!(
                                "oob_recv_timeout_all budget_ms={} max_rounds={}",
                                wait_budget_ms, max_rounds
                            ));
                        }
                    }
                }

                // Step 4: finalize with result or use fast path
                if let Some(result) = finalize_candidate {
                    diag_info_string(format!("oob_finalize_joined label={}", label));
                    state.searching = false;
                    state.stop_flag = None;
                    state.ponder_hit_flag = None;
                    state.search_session = None;
                    let stale = state
                        .current_root_hash
                        .map(|h| h != state.position.zobrist_hash())
                        .unwrap_or(false);
                    if let Some(tm) = state.active_time_manager.take() {
                        let elapsed_ms = result.stats.elapsed.as_millis() as u64;
                        let time_state = state.time_state_for_update(elapsed_ms);
                        tm.update_after_move(elapsed_ms, time_state);
                    }
                    finalize_and_send(state, label, Some(&result), stale, Some(reason));
                    if !state.bestmove_emitted {
                        let fallback = result
                            .best_move
                            .map(|mv| move_to_usi(&mv))
                            .unwrap_or_else(|| "resign".to_string());
                        let _ = emit_bestmove_once(state, fallback, None);
                    }
                    diag_info_string(format!("oob_finalize_result label={} mode=joined", label));
                    state.current_is_ponder = false;
                    state.current_root_hash = None;
                    state.current_time_control = None;
                    state.notify_idle();
                } else {
                    // Result not received within wait budget → fast finalize
                    // Note: Previously tried to prohibit fast path for pure byoyomi with margin,
                    // but this caused infinite loops because Finalize message is not resent.
                    // Better to send bestmove immediately than to time-loss.
                    diag_info_string(format!(
                        "oob_finalize_timeout no_result=1 sid={} budget_ms={}",
                        session_id, wait_budget_ms
                    ));
                    fast_finalize_no_detach(state, label, Some(reason));
                    diag_info_string(format!("oob_finalize_result label={} mode=fast", label));
                }
            }
        }
    }

    // Put receiver back
    state.finalizer_rx = Some(rx);
}

/// Poll StopController snapshot to detect short-mate (distance <= K) and
/// trigger early finalize. This realizes USI-led instant-mate finalization (A案).
pub fn poll_instant_mate(state: &mut EngineState) {
    if !state.searching || !state.opts.instant_mate_move_enabled {
        return;
    }
    // Do not act if a bestmove has already been emitted (session finished)
    if state.bestmove_emitted {
        return;
    }

    // Check the latest snapshot for a mate score on PV(s)
    if let Some(snapshot) = state.stop_controller.try_read_snapshot() {
        // Gate 1: Snapshot source (Stable required if configured)
        if state.opts.instant_mate_require_stable && snapshot.source != SnapshotSource::Stable {
            return;
        }
        // Gate 2: Minimum reported depth
        if state.opts.instant_mate_min_depth > 0
            && snapshot.depth < state.opts.instant_mate_min_depth
        {
            return;
        }
        // Gate 3: Respect small minimum think time for fast finalize
        if state.opts.instant_mate_respect_min_think_ms
            && snapshot.elapsed_ms < state.opts.instant_mate_min_respect_ms
        {
            return;
        }

        let mut try_lines: smallvec::SmallVec<[&engine_core::search::types::RootLine; 4]> =
            smallvec::SmallVec::new();
        if state.opts.instant_mate_check_all_pv {
            for l in &snapshot.lines {
                try_lines.push(l);
            }
        } else if let Some(first) = snapshot.lines.first() {
            try_lines.push(first);
        }

        for line in try_lines {
            use engine_core::search::constants::mate_distance as md;
            // Gate 4: bound must be Exact（fail-high/lowは不発）
            if !matches!(line.bound, Bound::Exact) {
                continue;
            }
            let dist = line.mate_distance.or_else(|| md(line.score_internal)).unwrap_or(0);
            if !(dist > 0 && dist <= state.opts.instant_mate_move_max_distance as i32) {
                continue;
            }
            // Candidate move (prefer PV[0], fallback to root_move / snapshot.best)
            let cand = line.pv.first().copied().or(Some(line.root_move)).or(snapshot.best);
            let Some(best_mv) = cand else { continue };
            // Position must match snapshot root
            let pos_hash = state.position.zobrist_hash();
            if pos_hash != snapshot.root_key {
                // stale snapshot; wait next tick
                continue;
            }
            // Lightweight verification
            let verify_passed = match state.opts.instant_mate_verify_mode {
                crate::state::InstantMateVerifyMode::Off => true,
                crate::state::InstantMateVerifyMode::CheckOnly => {
                    let mut pos1 = state.position.clone();
                    let _ = pos1.do_move(best_mv);
                    let mg = MoveGenerator::new();
                    match mg.generate_all(&pos1) {
                        Ok(list) => list.as_slice().iter().all(|&m| !pos1.is_legal_move(m)),
                        Err(_) => false,
                    }
                }
                crate::state::InstantMateVerifyMode::QSearch => {
                    // TODO: implement tiny qsearch verification (node-limited). For now fallback to CheckOnly.
                    let mut pos1 = state.position.clone();
                    let _ = pos1.do_move(best_mv);
                    let mg = MoveGenerator::new();
                    match mg.generate_all(&pos1) {
                        Ok(list) => list.as_slice().iter().all(|&m| !pos1.is_legal_move(m)),
                        Err(_) => false,
                    }
                }
            };
            if !verify_passed {
                continue;
            }

            let was_ponder = state.current_is_ponder;
            info_string(format!(
                "instant_mate_triggered=1 distance={} was_ponder={} depth={} elapsed_ms={} bound={:?} source={:?} sid={} root={} verify_passed=1",
                dist,
                was_ponder as u8,
                snapshot.depth,
                snapshot.elapsed_ms,
                line.bound,
                snapshot.source,
                snapshot.search_id,
                fmt_hash(snapshot.root_key)
            ));
            state.stop_controller.request_finalize(FinalizeReason::PlannedMate {
                distance: dist,
                was_ponder,
            });
            if !was_ponder {
                fast_finalize_no_detach(
                    state,
                    "instant_mate_finalize",
                    Some(FinalizeReason::PlannedMate {
                        distance: dist,
                        was_ponder: false,
                    }),
                );
            }
            break;
        }
    }
}

#[cfg(test)]
mod tests_oob_finalize {
    use super::*;
    use crate::finalize::take_last_emitted_bestmove;
    use engine_core::search::types::{Bound, RootLine};
    use engine_core::shogi::Move;

    fn make_mate1_line() -> RootLine {
        use engine_core::search::constants::MATE_SCORE;
        RootLine {
            multipv_index: 1,
            root_move: Move::null(),
            score_internal: MATE_SCORE - 1, // mate in 1 ply
            score_cp: 30_000,               // clipped display (not used by detector)
            bound: Bound::Exact,
            depth: 10,
            seldepth: Some(10),
            pv: smallvec::smallvec![Move::null()],
            nodes: Some(1000),
            time_ms: Some(5),
            nps: Some(200_000),
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: Some(1),
        }
    }

    #[test]
    fn instant_mate_non_ponder_emits_bestmove_once() {
        let mut state = EngineState::new();
        state.opts.instant_mate_move_enabled = true;
        state.opts.instant_mate_move_max_distance = 1;
        state.searching = true;
        state.current_is_ponder = false;

        // Publish a snapshot with mate in 1
        let sid = 4242u64;
        let root_key = state.position.zobrist_hash();
        state.stop_controller.publish_session(None, sid);
        state.stop_controller.publish_root_line(sid, root_key, &make_mate1_line());

        // Before: no bestmove
        assert!(!state.bestmove_emitted);

        // Trigger
        poll_instant_mate(&mut state);

        // After: bestmove must be emitted exactly once
        assert!(state.bestmove_emitted, "bestmove should be emitted on instant mate");
        let bm = take_last_emitted_bestmove();
        assert!(bm.is_some(), "bestmove payload should be recorded in tests");
    }

    #[test]
    fn instant_mate_in_ponder_does_not_emit_immediately() {
        let mut state = EngineState::new();
        state.opts.instant_mate_move_enabled = true;
        state.opts.instant_mate_move_max_distance = 1;
        state.searching = true;
        state.current_is_ponder = true;

        // Publish a snapshot with mate in 1
        let sid = 777u64;
        let root_key = state.position.zobrist_hash();
        state.stop_controller.publish_session(None, sid);
        state.stop_controller.publish_root_line(sid, root_key, &make_mate1_line());

        // Trigger
        poll_instant_mate(&mut state);

        // Ponder中は即送信しない（停止要求のみ）。bestmove未送信のまま。
        assert!(!state.bestmove_emitted, "ponder mode must not emit bestmove immediately");
    }

    #[test]
    fn instant_mate_helper_non_exact_does_not_trigger() {
        use engine_core::search::constants::MATE_SCORE;
        use engine_core::search::types::Bound;

        let mut state = EngineState::new();
        state.opts.instant_mate_move_enabled = true;
        state.opts.instant_mate_move_max_distance = 1;
        state.searching = true;
        state.current_is_ponder = false;

        // Publish a snapshot with mate score but non-Exact bound (LowerBound)
        let sid = 8888u64;
        let root_key = state.position.zobrist_hash();
        state.stop_controller.publish_session(None, sid);
        let mut line = make_mate1_line();
        line.bound = Bound::LowerBound;
        // still keep internal mate score to verify guard works
        line.score_internal = MATE_SCORE - 1;
        state.stop_controller.publish_root_line(sid, root_key, &line);

        poll_instant_mate(&mut state);
        assert!(
            !state.bestmove_emitted,
            "non-Exact helper snapshot must not trigger instant-mate finalize"
        );
    }

    #[test]
    fn instant_mate_pv2_only_triggers_when_check_all_pv_enabled() {
        let mut state = EngineState::new();
        state.opts.instant_mate_move_enabled = true;
        state.opts.instant_mate_move_max_distance = 1;
        state.searching = true;
        state.current_is_ponder = false;

        // Publish a stable snapshot with PV1 non-mate and PV2 mate in 1
        let sid = 20251u64;
        let root_key = state.position.zobrist_hash();
        state.stop_controller.publish_session(None, sid);
        let mut pv1 = make_mate1_line();
        pv1.bound = Bound::Exact;
        pv1.mate_distance = None;
        pv1.score_internal = 120; // not a mate

        let mut pv2 = make_mate1_line(); // Exact mate in 1
        pv2.multipv_index = 2; // PV2

        // SmallVec の inline 容量を明示し型推論エラー (SmallVec<_>) を解消
        let lines: smallvec::SmallVec<[RootLine; 2]> =
            smallvec::smallvec![pv1.clone(), pv2.clone()];
        state
            .stop_controller
            .publish_committed_snapshot(sid, root_key, &lines[..], 1000, 10);

        // Default (CheckAllPV=false): should NOT trigger (PV1 is not mate)
        poll_instant_mate(&mut state);
        assert!(
            !state.bestmove_emitted,
            "should not trigger when only PV2 is mate and CheckAllPV=false"
        );

        // Enable CheckAllPV → now should trigger
        state.opts.instant_mate_check_all_pv = true;
        poll_instant_mate(&mut state);
        assert!(state.bestmove_emitted, "CheckAllPV=true should trigger on PV2 mate");
        let _ = take_last_emitted_bestmove();
    }

    #[test]
    fn instant_mate_prefers_mate_distance_field_when_available() {
        use engine_core::search::types::Bound;

        let mut state = EngineState::new();
        state.opts.instant_mate_move_enabled = true;
        state.opts.instant_mate_move_max_distance = 2;
        state.searching = true;
        state.current_is_ponder = false;

        let sid = 9999u64;
        let root_key = state.position.zobrist_hash();
        state.stop_controller.publish_session(None, sid);
        let mut line = make_mate1_line();
        line.bound = Bound::Exact;
        // publish
        state.stop_controller.publish_root_line(sid, root_key, &line);

        poll_instant_mate(&mut state);
        assert!(state.bestmove_emitted, "Exact helper snapshot with mate must trigger");
        let bm = take_last_emitted_bestmove();
        assert!(bm.is_some());
    }
}

/// Enforce locally computed deadlines (USI層のみで完結するOOB finalize)
///
/// - hard 期限を過ぎたら探索合流を待たずに fast finalize を発火
/// - near-hard は現時点ではログのみ（必要なら hard の前に同様に発火可能）
///
/// Hard 到達時は `request_finalize(Hard)` → `session.request_stop()` →
/// `fast_finalize_no_detach()` の順に呼び出し、StopInfo の `hard_timeout=true`
/// がログ出力に確実に反映されるようにしている。
pub fn enforce_deadline(state: &mut EngineState) {
    if !state.searching || state.bestmove_emitted {
        return;
    }
    if state.current_is_ponder {
        return;
    }

    let now = Instant::now();

    if let Some(nh) = state.deadline_near {
        if now >= nh && !state.deadline_near_notified {
            diag_info_string("oob_deadline_nearhard_reached");
            diag_info_string("oob_finalize_request reason=NearHard");
            state.stop_controller.request_finalize(FinalizeReason::NearHard);
            state.deadline_near_notified = true;
            state.deadline_near = None;
        }
    }

    if let Some(hard) = state.deadline_hard {
        if now >= hard {
            diag_info_string("oob_finalize_request reason=Hard");
            // Mark StopInfo as TimeLimit/Hard for logging consistency and request finalize
            state.stop_controller.request_finalize(FinalizeReason::Hard);
            if let Some(session) = &state.search_session {
                session.request_stop();
            }
            fast_finalize_no_detach(state, "oob_hard_finalize", Some(FinalizeReason::Hard));
            // Clear deadlines
            state.deadline_hard = None;
            state.deadline_near = None;
            state.deadline_near_notified = false;
        }
    }
}

/// Fast finalize without waiting for result (SearchSession will clean up automatically)
fn fast_finalize_no_detach(
    state: &mut EngineState,
    label: &str,
    finalize_reason: Option<FinalizeReason>,
) {
    state.searching = false;
    state.stop_flag = None;
    state.ponder_hit_flag = None;
    state.search_session = None;
    state.finalize_time_manager();
    finalize_and_send_fast(state, label, finalize_reason);
    state.current_is_ponder = false;
    state.current_root_hash = None;
    state.current_time_control = None;
    state.deadline_hard = None;
    state.deadline_near = None;
    state.deadline_near_notified = false;
    state.notify_idle();
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::search::types::StopInfo;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn finalize_before_session_start_late_bind_emits_once() {
        let mut state = EngineState::new();
        let (tx, rx) = mpsc::channel();
        state.finalizer_rx = Some(rx);
        state.stop_controller.register_finalizer(tx.clone());

        state.stop_controller.prime_stop_info(StopInfo::default());
        state.searching = true;
        state.bestmove_emitted = false;
        state.stop_flag = Some(Arc::new(AtomicBool::new(false)));
        state.current_session_core_id = None;

        tx.send(FinalizerMsg::Finalize {
            session_id: 42,
            reason: FinalizeReason::NearHard,
        })
        .unwrap();
        tx.send(FinalizerMsg::SessionStart { session_id: 42 }).unwrap();

        poll_oob_finalize(&mut state);

        assert_eq!(state.current_session_core_id, Some(42));
        assert!(state.bestmove_emitted);
        assert!(!state.searching);

        // Ensure no duplicate emission when draining remaining messages
        poll_oob_finalize(&mut state);
        assert!(state.bestmove_emitted);
    }

    #[test]
    fn finalize_is_suppressed_during_ponder() {
        let mut state = EngineState::new();
        let (tx, rx) = mpsc::channel();
        state.finalizer_rx = Some(rx);
        state.stop_controller.register_finalizer(tx.clone());

        state.searching = true;
        state.current_is_ponder = true;
        state.bestmove_emitted = false;
        state.stop_flag = Some(Arc::new(AtomicBool::new(false)));

        tx.send(FinalizerMsg::Finalize {
            session_id: 99,
            reason: FinalizeReason::Planned,
        })
        .unwrap();

        poll_oob_finalize(&mut state);

        assert!(state.searching, "ponder finalize should not end the search");
        assert!(!state.bestmove_emitted);
        assert!(state.stop_controller.try_claim_finalize());
    }

    #[test]
    fn finalizer_channel_is_reestablished_after_disconnect() {
        let mut state = EngineState::new();
        let (tx, rx) = mpsc::channel();
        drop(tx);
        state.finalizer_rx = Some(rx);

        poll_oob_finalize(&mut state);

        let rx_ref = state.finalizer_rx.as_ref().expect("finalizer receiver should be reattached");
        assert!(matches!(rx_ref.try_recv(), Err(mpsc::TryRecvError::Empty)));
    }
}
