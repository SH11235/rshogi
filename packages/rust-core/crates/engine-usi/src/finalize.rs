use engine_core::engine::controller::{FinalBest, FinalBestSource};
use engine_core::search::{types::StopInfo, SearchResult};
use engine_core::usi::{append_usi_score_and_bound, move_to_usi};
use engine_core::{movegen::MoveGenerator, shogi::PieceType};

use crate::io::{info_string, usi_println};
use crate::state::EngineState;
use crate::util::{emit_bestmove, score_view_with_clamp};

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[inline]
pub fn fmt_hash(h: u64) -> String {
    format!("{h:016x}")
}

#[inline]
fn source_to_str(src: FinalBestSource) -> &'static str {
    match src {
        FinalBestSource::Book => "book",
        FinalBestSource::Committed => "committed",
        FinalBestSource::TT => "tt",
        FinalBestSource::LegalFallback => "legal",
        FinalBestSource::Resign => "resign",
    }
}

const TT_LOCK_BACKOFF_US: u64 = 100;

fn compute_tt_probe_budget_ms(stop_info: Option<&StopInfo>, snapshot_elapsed_ms: u32) -> u64 {
    let stop_info = match stop_info {
        Some(si) => si,
        None => return 0,
    };

    let mut limit = u64::MAX;
    if stop_info.soft_limit_ms > 0 {
        limit = limit.min(stop_info.soft_limit_ms);
    }
    if stop_info.hard_limit_ms > 0 {
        limit = limit.min(stop_info.hard_limit_ms);
    }
    if limit == u64::MAX || limit == 0 {
        return 0;
    }

    let elapsed = if snapshot_elapsed_ms > 0 {
        snapshot_elapsed_ms as u64
    } else {
        stop_info.elapsed_ms
    };
    if elapsed >= limit {
        return 0;
    }

    let remain = limit - elapsed;
    if remain == 0 {
        return 0;
    }

    let mut budget = (remain / 10).min(2);
    if budget == 0 && remain > 0 {
        budget = 1;
    }
    budget
}

fn try_lock_engine_with_budget<'a>(
    engine: &'a Arc<Mutex<engine_core::engine::controller::Engine>>,
    budget_ms: u64,
) -> Option<(MutexGuard<'a, engine_core::engine::controller::Engine>, u64)> {
    let start = Instant::now();
    if let Ok(guard) = engine.try_lock() {
        return Some((guard, start.elapsed().as_millis() as u64));
    }
    if budget_ms == 0 {
        return None;
    }
    let deadline = start + Duration::from_millis(budget_ms);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_micros(TT_LOCK_BACKOFF_US));
        if let Ok(guard) = engine.try_lock() {
            let spent = start.elapsed().as_millis() as u64;
            return Some((guard, spent));
        }
    }
    None
}

/// 中央集約された finalize 処理。
pub fn finalize_and_send(
    state: &mut EngineState,
    label: &str,
    result: Option<&SearchResult>,
    stale: bool,
) {
    if stale {
        info_string(format!("{label}_stale=1 fallback=fast"));
        finalize_and_send_fast(state, label);
        return;
    }

    if state.bestmove_emitted {
        info_string(format!("{label}_skip already_emitted=1"));
        return;
    }
    if !state.stop_bridge.try_claim_finalize() {
        info_string(format!("{label}_skip claimed_by_other=1"));
        return;
    }

    let committed = if let Some(res) = result {
        let mut committed_pv = res.stats.pv.clone();
        if let Some(bm) = res.best_move {
            if committed_pv.first().copied() != Some(bm) {
                info_string(format!(
                    "pv_head_mismatch=1 pv0={} best={}",
                    committed_pv.first().map(move_to_usi).unwrap_or_else(|| "-".to_string()),
                    move_to_usi(&bm)
                ));
                committed_pv.clear();
                committed_pv.push(bm);
            }
        }
        Some(engine_core::search::CommittedIteration {
            depth: res.stats.depth,
            seldepth: res.stats.seldepth,
            score: res.score,
            pv: committed_pv,
            node_type: res.node_type,
            nodes: res.stats.nodes,
            elapsed: res.stats.elapsed,
        })
    } else {
        None
    };

    let final_best = {
        let eng = state.engine.lock().unwrap();
        eng.choose_final_bestmove(&state.position, committed.as_ref())
    };

    let (soft_ms, hard_ms) = result
        .and_then(|r| r.stop_info.as_ref())
        .map(|si| (si.soft_limit_ms, si.hard_limit_ms))
        .unwrap_or((0, 0));

    if let Some(res) = result {
        let best_usi =
            res.best_move.map(|m| move_to_usi(&m)).unwrap_or_else(|| "resign".to_string());
        let pv0_usi = res.stats.pv.first().map(move_to_usi).unwrap_or_else(|| "-".to_string());
        let stop_reason = res
            .stop_info
            .as_ref()
            .map(|si| format!("{:?}", si.reason))
            .unwrap_or_else(|| "Unknown".to_string());
        info_string(format!(
            "finalize_snapshot best={} pv0={} depth={} nodes={} elapsed_ms={} stop_reason={}",
            best_usi,
            pv0_usi,
            res.stats.depth,
            res.stats.nodes,
            res.stats.elapsed.as_millis(),
            stop_reason
        ));

        // 極小Byoyomi対策の可視化: ハード/ソフト上限と停止理由
        if let Some(si) = res.stop_info.as_ref() {
            info_string(format!(
                "time_caps hard_ms={} soft_ms={} reason={:?}",
                si.hard_limit_ms, si.soft_limit_ms, si.reason
            ));
        }

        #[cfg(feature = "diagnostics")]
        {
            let nodes = res.stats.nodes.max(1);
            let qnodes = res.stats.qnodes;
            let qratio = (qnodes as f64) / (nodes as f64);
            let tt_hits = res.stats.tt_hits.unwrap_or(0);
            let tt_hit_rate = (tt_hits as f64) / (nodes as f64);
            let asp_fail = res.stats.aspiration_failures.unwrap_or(0);
            let asp_hit = res.stats.aspiration_hits.unwrap_or(0);
            let rese = res.stats.re_searches.unwrap_or(0);
            let pvchg = res.stats.pv_changed.unwrap_or(0);
            let sel = res.stats.seldepth.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
            let dup = res
                .stats
                .duplication_percentage
                .map(|d| format!("{:.1}", d))
                .unwrap_or_else(|| "-".to_string());
            let rfhi = res.stats.root_fail_high_count.unwrap_or(0);

            // Additional root snapshot (diagnostics)
            let (root_in_check, root_legal_count, root_evasion_count) = {
                // Work on a clone to avoid mutably borrowing shared state
                let mut pos = state.position.clone();
                let mg = MoveGenerator::new();
                let in_check = pos.is_in_check();
                let mut legal_count = 0usize;
                let mut evasion_count = 0usize;
                if let Ok(mvlist) = mg.generate_all(&pos) {
                    legal_count = mvlist.len();
                    if in_check {
                        for &mv in mvlist.as_slice().iter() {
                            let undo = pos.do_move(mv);
                            let still = pos.is_in_check();
                            pos.undo_move(mv, undo);
                            if !still {
                                evasion_count += 1;
                            }
                        }
                    }
                }
                (in_check, legal_count, evasion_count)
            };

            // Report whether quiescence allows checking moves, honoring compile-time overrides first
            let checks_in_q_allowed = {
                #[cfg(feature = "qs_checks_force_off")]
                {
                    "Off"
                }
                #[cfg(all(not(feature = "qs_checks_force_off"), feature = "qs_checks_force_on"))]
                {
                    "On"
                }
                #[cfg(all(
                    not(feature = "qs_checks_force_off"),
                    not(feature = "qs_checks_force_on")
                ))]
                {
                    if std::env::var("SHOGI_QS_DISABLE_CHECKS").map(|v| v == "1").unwrap_or(false) {
                        "Off"
                    } else {
                        "On"
                    }
                }
            };

            info_string(format!(
                "finalize_diag seldepth={} qratio={:.3} ab_nodes={} tt_hit_rate={:.3} tt_hits={} asp_fail={} asp_hit={} re_searches={} pv_changed={} dup_pct={} root_fail_high={} root_in_check={} root_legal_count={} root_evasion_count={} root_scoring=static checks_in_q_allowed={}",
                sel,
                qratio,
                nodes.saturating_sub(qnodes),
                tt_hit_rate,
                tt_hits,
                asp_fail,
                asp_hit,
                rese,
                pvchg,
                dup,
                rfhi,
                root_in_check as i32,
                root_legal_count,
                root_evasion_count,
                checks_in_q_allowed
            ));
        }
    }

    info_string(format!(
        "{}_select source={} move={} soft_ms={} hard_ms={}",
        label,
        source_to_str(final_best.source),
        final_best
            .best_move
            .map(|m| move_to_usi(&m))
            .unwrap_or_else(|| "resign".to_string()),
        soft_ms,
        hard_ms
    ));

    if let Some(res) = result {
        if !stale {
            let hf_permille = {
                let eng = state.engine.lock().unwrap();
                eng.tt_hashfull_permille()
            };
            let nps_agg: u128 = if res.stats.elapsed.as_millis() > 0 {
                (res.stats.nodes as u128).saturating_mul(1000) / res.stats.elapsed.as_millis()
            } else {
                0
            };

            // Emit TT diagnostics snapshot (address/size/hf/attempts)
            {
                let dbg = {
                    let eng = state.engine.lock().unwrap();
                    eng.tt_debug_info()
                };
                info_string(format!(
                    "tt_debug addr={:#x} size_mb={} hf={} store_attempts={}",
                    dbg.addr, dbg.size_mb, dbg.hf_permille, dbg.store_attempts
                ));
            }

            // Optional: TT roundtrip smoke test at current root hash
            #[cfg(all(feature = "diagnostics", feature = "tt_metrics"))]
            {
                let root_hash = state.position.zobrist_hash;
                let ok = {
                    let eng = state.engine.lock().unwrap();
                    eng.tt_roundtrip_test(root_hash)
                };
                info_string(format!("tt_roundtrip root={}", ok));
            }

            if state.opts.multipv > 1 {
                if let Some(lines) = &res.lines {
                    for line in lines.iter() {
                        let mut s = String::from("info");
                        let index = line.multipv_index.max(1);
                        s.push_str(&format!(" multipv {}", index));
                        s.push_str(&format!(" depth {}", line.depth));
                        if let Some(sd) = line.seldepth.or(res.stats.seldepth) {
                            s.push_str(&format!(" seldepth {}", sd));
                        }
                        s.push_str(&format!(" time {}", res.stats.elapsed.as_millis()));
                        let nodes = line.nodes.unwrap_or(res.stats.nodes);
                        s.push_str(&format!(" nodes {}", nodes));
                        s.push_str(&format!(" nps {}", nps_agg));
                        s.push_str(&format!(" hashfull {}", hf_permille));
                        let view = score_view_with_clamp(line.score_internal);
                        append_usi_score_and_bound(&mut s, view, line.bound);
                        if !line.pv.is_empty() {
                            s.push_str(" pv");
                            for m in line.pv.iter() {
                                s.push(' ');
                                s.push_str(&move_to_usi(m));
                            }
                        }
                        usi_println(&s);
                    }
                } else {
                    emit_single_pv(res, &final_best, nps_agg, hf_permille);
                }
            } else {
                emit_single_pv(res, &final_best, nps_agg, hf_permille);
            }
        }
    }

    #[cfg(feature = "tt_metrics")]
    {
        let summary_opt = {
            let eng = state.engine.lock().unwrap();
            eng.tt_metrics_summary()
        };
        if let Some(sum) = summary_opt {
            for line in sum.lines() {
                usi_println(&format!("info string tt_metrics {}", line));
            }
        }
    }

    let final_usi = final_best
        .best_move
        .map(|m| move_to_usi(&m))
        .unwrap_or_else(|| "resign".to_string());
    let ponder_mv = if state.opts.ponder {
        final_best.pv.get(1).map(move_to_usi).or_else(|| {
            final_best.best_move.and_then(|bm| {
                let eng = state.engine.lock().unwrap();
                eng.get_ponder_from_tt(&state.position, bm).map(|m| move_to_usi(&m))
            })
        })
    } else {
        None
    };
    emit_bestmove(&final_usi, ponder_mv);
    state.bestmove_emitted = true;
    state.current_root_hash = None;
}

fn emit_single_pv(res: &SearchResult, final_best: &FinalBest, nps_agg: u128, hf_permille: u16) {
    let mut s = String::from("info");
    s.push_str(&format!(" depth {}", res.stats.depth));
    if let Some(sd) = res.stats.seldepth {
        s.push_str(&format!(" seldepth {}", sd));
    }
    s.push_str(&format!(" time {}", res.stats.elapsed.as_millis()));
    s.push_str(&format!(" nodes {}", res.stats.nodes));
    s.push_str(&format!(" nps {}", nps_agg));
    s.push_str(&format!(" hashfull {}", hf_permille));

    let view = score_view_with_clamp(res.score);
    append_usi_score_and_bound(&mut s, view, res.node_type);

    let pv_ref: &[_] = if !final_best.pv.is_empty() {
        &final_best.pv
    } else {
        &res.stats.pv
    };
    if !pv_ref.is_empty() {
        s.push_str(" pv");
        for m in pv_ref.iter() {
            s.push(' ');
            s.push_str(&move_to_usi(m));
        }
    }
    usi_println(&s);
}

pub fn finalize_and_send_fast(state: &mut EngineState, label: &str) {
    if state.bestmove_emitted {
        info_string(format!("{label}_fast_skip already_emitted=1"));
        return;
    }
    if !state.stop_bridge.try_claim_finalize() {
        info_string(format!("{label}_fast_skip claimed_by_other=1"));
        return;
    }

    let stop_info_snapshot = state.stop_bridge.try_read_stop_info();

    if let Some(si) = stop_info_snapshot.as_ref() {
        info_string(format!(
            "{label}_oob_stop_info sid={} reason={:?} elapsed_ms={} soft_ms={} hard_ms={}",
            state.current_session_core_id.unwrap_or(0),
            si.reason,
            si.elapsed_ms,
            si.soft_limit_ms,
            si.hard_limit_ms
        ));
    }

    let root_key_hex = fmt_hash(state.position.zobrist_hash());

    // Try snapshot first to avoid engine lock when possible
    if let Some(snap) = state.stop_bridge.try_read_snapshot() {
        let sid_ok = state.current_session_core_id.map(|sid| sid == snap.search_id).unwrap_or(true);
        let rk_ok = snap.root_key == state.position.zobrist_hash();
        if sid_ok && rk_ok {
            if let Some(best) = snap.best {
                let shallow =
                    snap.depth < FAST_SNAPSHOT_MIN_DEPTH_FOR_DIRECT_EMIT || snap.pv.is_empty();
                let budget_ms =
                    compute_tt_probe_budget_ms(stop_info_snapshot.as_ref(), snap.elapsed_ms);
                if shallow {
                    if let Some((eng_guard, spent_ms)) =
                        try_lock_engine_with_budget(&state.engine, budget_ms)
                    {
                        let final_best = eng_guard.choose_final_bestmove(&state.position, None);
                        let final_usi = final_best
                            .best_move
                            .map(|m| move_to_usi(&m))
                            .unwrap_or_else(|| move_to_usi(&best));
                        let ponder_mv = if state.opts.ponder {
                            final_best
                                .pv
                                .get(1)
                                .map(move_to_usi)
                                .or_else(|| snap.pv.get(1).map(move_to_usi))
                        } else {
                            None
                        };
                        drop(eng_guard);
                        info_string(format!(
                            "{}_fast_snapshot sid={} root_key={} depth={} nodes={} elapsed={} pv_len={} tt_probe=1 tt_probe_budget_ms={} tt_probe_spent_ms={}",
                            label,
                            snap.search_id,
                            fmt_hash(snap.root_key),
                            snap.depth,
                            snap.nodes,
                            snap.elapsed_ms,
                            snap.pv.len(),
                            budget_ms,
                            spent_ms
                        ));
                        emit_bestmove(&final_usi, ponder_mv);
                        state.bestmove_emitted = true;
                        state.current_root_hash = None;
                        return;
                    }
                }

                let final_usi = move_to_usi(&best);
                let ponder_mv = if state.opts.ponder {
                    snap.pv.get(1).map(move_to_usi)
                } else {
                    None
                };
                info_string(format!(
                    "{}_fast_snapshot sid={} root_key={} depth={} nodes={} elapsed={} pv_len={} tt_probe=0 tt_probe_budget_ms={}",
                    label,
                    snap.search_id,
                    fmt_hash(snap.root_key),
                    snap.depth,
                    snap.nodes,
                    snap.elapsed_ms,
                    snap.pv.len(),
                    budget_ms
                ));
                emit_bestmove(&final_usi, ponder_mv);
                state.bestmove_emitted = true;
                state.current_root_hash = None;
                return;
            }
        }
    }

    let fallback_budget_ms = compute_tt_probe_budget_ms(stop_info_snapshot.as_ref(), 0);
    if let Some((eng_guard, spent_ms)) =
        try_lock_engine_with_budget(&state.engine, fallback_budget_ms)
    {
        let dbg = eng_guard.tt_debug_info();
        let final_best = eng_guard.choose_final_bestmove(&state.position, None);
        let final_usi = final_best
            .best_move
            .map(|m| move_to_usi(&m))
            .unwrap_or_else(|| "resign".to_string());
        let ponder_mv = if state.opts.ponder {
            final_best.pv.get(1).map(move_to_usi).or_else(|| {
                final_best.best_move.and_then(|bm| {
                    eng_guard.get_ponder_from_tt(&state.position, bm).map(|m| move_to_usi(&m))
                })
            })
        } else {
            None
        };
        drop(eng_guard);
        info_string(format!(
            "{}_fast_tt_debug addr={:#x} size_mb={} hf={} store_attempts={} tt_probe_budget_ms={} tt_probe_spent_ms={}",
            label,
            dbg.addr,
            dbg.size_mb,
            dbg.hf_permille,
            dbg.store_attempts,
            fallback_budget_ms,
            spent_ms
        ));
        info_string(format!(
            "{}_fast_select sid={} source={} move={} ponder={:?}",
            label,
            state.current_session_core_id.unwrap_or(0),
            source_to_str(final_best.source),
            final_usi,
            ponder_mv
        ));
        emit_bestmove(&final_usi, ponder_mv);
        state.bestmove_emitted = true;
        state.current_root_hash = None;
        return;
    }

    info_string(format!(
        "{}_fast_path=legal_fallback sid={} root_key={} tt_probe_budget_ms={}",
        label,
        state.current_session_core_id.unwrap_or(0),
        root_key_hex,
        fallback_budget_ms
    ));
    let mg = MoveGenerator::new();
    match mg.generate_all(&state.position) {
        Ok(list) => {
            let slice = list.as_slice();
            if slice.is_empty() {
                info_string(format!("{}_fast_select_resign", label));
                emit_bestmove("resign", None);
            } else {
                let pos = &state.position;
                let in_check = pos.is_in_check();
                let is_king_move = |m: &engine_core::shogi::Move| {
                    m.piece_type()
                        .or_else(|| {
                            m.from().and_then(|sq| pos.board.piece_on(sq).map(|p| p.piece_type))
                        })
                        .map(|pt| matches!(pt, PieceType::King))
                        .unwrap_or(false)
                };
                let is_tactical = |m: &engine_core::shogi::Move| {
                    m.is_drop() || m.is_capture_hint() || m.is_promote()
                };

                let legal_moves: Vec<_> =
                    slice.iter().copied().filter(|&m| pos.is_legal_move(m)).collect();

                let chosen = if legal_moves.is_empty() {
                    slice[0]
                } else if in_check {
                    legal_moves[0]
                } else if let Some(mv) =
                    legal_moves.iter().find(|m| is_tactical(m) && !is_king_move(m)).copied()
                {
                    mv
                } else if let Some(mv) = legal_moves.iter().find(|m| !is_king_move(m)).copied() {
                    mv
                } else {
                    legal_moves[0]
                };

                let final_usi = move_to_usi(&chosen);
                info_string(format!("{}_fast_select_legal move={}", label, final_usi));
                emit_bestmove(&final_usi, None);
            }
        }
        Err(e) => {
            info_string(format!("{}_fast_select_error resign_fallback=1 err={}", label, e));
            emit_bestmove("resign", None);
        }
    }
    state.bestmove_emitted = true;
    state.current_root_hash = None;
}
const FAST_SNAPSHOT_MIN_DEPTH_FOR_DIRECT_EMIT: u8 = 2;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tt_probe_budget_respects_stop_info_and_snapshot_elapsed() {
        let si = StopInfo {
            reason: engine_core::search::types::TerminationReason::TimeLimit,
            elapsed_ms: 1_500,
            nodes: 0,
            depth_reached: 0,
            hard_timeout: false,
            soft_limit_ms: 2_000,
            hard_limit_ms: 2_500,
        };

        assert_eq!(compute_tt_probe_budget_ms(Some(&si), 0), 2);
        // Snapshot elapsed overrides StopInfo elapsed when provided
        assert_eq!(compute_tt_probe_budget_ms(Some(&si), 1_995), 1);
        // Remaining time less than 10ms still yields minimum 1ms budget
        let si_close = StopInfo {
            elapsed_ms: 1_999,
            ..si
        };
        assert_eq!(compute_tt_probe_budget_ms(Some(&si_close), 0), 1);
        // Missing StopInfo yields zero budget
        assert_eq!(compute_tt_probe_budget_ms(None, 0), 0);
    }
}
