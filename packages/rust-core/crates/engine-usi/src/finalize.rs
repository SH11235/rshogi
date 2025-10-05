use engine_core::engine::controller::{FinalBest, FinalBestSource};
use engine_core::search::parallel::FinalizeReason;
use engine_core::search::{
    types::{NodeType, StopInfo},
    SearchResult,
};
use engine_core::usi::{append_usi_score_and_bound, move_to_usi};
use engine_core::{movegen::MoveGenerator, shogi::PieceType};

use crate::io::{diag_info_string, usi_println};
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

fn log_and_emit_final_selection(
    state: &mut EngineState,
    label: &str,
    source: FinalBestSource,
    final_move: &str,
    ponder: Option<String>,
    stop_meta: &StopMeta,
) {
    diag_info_string(format!(
        "{}_select source={} move={} soft_ms={} hard_ms={}",
        label,
        source_to_str(source),
        final_move,
        stop_meta.soft_ms,
        stop_meta.hard_ms
    ));
    let _ = emit_bestmove_once(state, final_move.to_string(), ponder);
}

fn prepare_stop_meta(
    _label: &str,
    controller_info: Option<StopInfo>,
    result_stop_info: Option<&StopInfo>,
    finalize_reason: Option<FinalizeReason>,
) -> StopMeta {
    gather_stop_meta(controller_info, result_stop_info, finalize_reason)
}

#[derive(Debug, Clone)]
struct StopMeta {
    info: Option<StopInfo>,
    reason_label: String,
    soft_ms: u64,
    hard_ms: u64,
}

fn copy_stop_info(src: &StopInfo) -> StopInfo {
    StopInfo {
        reason: src.reason,
        elapsed_ms: src.elapsed_ms,
        nodes: src.nodes,
        depth_reached: src.depth_reached,
        hard_timeout: src.hard_timeout,
        soft_limit_ms: src.soft_limit_ms,
        hard_limit_ms: src.hard_limit_ms,
    }
}

/// Build `StopMeta`, prioritizing metadata in the order
/// `FinalizeReason` > `result.stop_info` > controller-derived `StopInfo`.
fn gather_stop_meta(
    mut controller_info: Option<StopInfo>,
    result_info: Option<&StopInfo>,
    finalize_reason: Option<FinalizeReason>,
) -> StopMeta {
    let controller_reason = controller_info.as_ref().map(|si| format!("{:?}", si.reason));
    let result_info_for_reason = result_info;
    let result_reason = result_info_for_reason.map(|si| format!("{:?}", si.reason));

    let mut reason_label = finalize_reason
        .map(|r| format!("{:?}", r))
        .or(result_reason.clone())
        .or(controller_reason.clone())
        .unwrap_or_else(|| "Unknown".to_string());

    if finalize_reason.is_some_and(|r| matches!(r, FinalizeReason::TimeManagerStop)) {
        let result_info_for_tm = result_info;
        let tm_tag_source =
            result_info_for_tm.or(controller_info.as_ref().map(|si| si as &StopInfo));
        let tm_tag = tm_tag_source
            .map(|si| {
                if si.hard_timeout {
                    "tm=hard"
                } else {
                    "tm=soft"
                }
            })
            .unwrap_or("tm=unknown");
        reason_label.push('|');
        reason_label.push_str(tm_tag);
    }

    let chosen_info = result_info.map(copy_stop_info).or(controller_info.take());

    let (soft_ms, hard_ms) = chosen_info
        .as_ref()
        .map(|si| (si.soft_limit_ms, si.hard_limit_ms))
        .unwrap_or((0, 0));

    StopMeta {
        info: chosen_info,
        reason_label,
        soft_ms,
        hard_ms,
    }
}

fn sanitize_ponder_for_bestmove(final_usi: &str, ponder: Option<String>) -> Option<String> {
    if matches!(final_usi, "resign" | "win") {
        None
    } else {
        ponder
    }
}

/// Emit bestmove exactly once per go-session and update common state.
///
/// Returns true when the move was emitted in this call. If the bestmove was
/// already sent earlier, the function leaves the state untouched and returns
/// false so callers can decide whetherフェールセーフを走らせるか判断できる。
#[must_use]
pub fn emit_bestmove_once<S: Into<String>>(
    state: &mut EngineState,
    final_move: S,
    ponder: Option<String>,
) -> bool {
    if state.bestmove_emitted {
        return false;
    }

    let final_usi = final_move.into();
    let ponder = sanitize_ponder_for_bestmove(&final_usi, ponder);
    emit_bestmove(&final_usi, ponder);

    state.bestmove_emitted = true;
    state.current_root_hash = None;
    state.deadline_hard = None;
    state.deadline_near = None;
    state.deadline_near_notified = false;

    true
}

const TT_LOCK_MAX_SPINS: usize = 16;

/// StopInfo から "残り" 時間を見積もり、TT ロックに使ってよい猶予を ms で返す。
/// 現状は soft/hard の最小値のみを参照する。将来 planned limit も StopInfo に
/// 反映された場合には、ここで同様に最小値へ折り込む。
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
    if remain <= 1 {
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
) -> Option<(MutexGuard<'a, engine_core::engine::controller::Engine>, u64, u64)> {
    let start = Instant::now();
    if let Ok(guard) = engine.try_lock() {
        let elapsed = start.elapsed();
        return Some((guard, elapsed.as_millis() as u64, elapsed.as_micros() as u64));
    }
    if budget_ms == 0 {
        return None;
    }
    let deadline = start + Duration::from_millis(budget_ms);
    let mut spins = 0usize;
    while Instant::now() < deadline {
        if let Ok(guard) = engine.try_lock() {
            let elapsed = start.elapsed();
            return Some((guard, elapsed.as_millis() as u64, elapsed.as_micros() as u64));
        }

        if spins < TT_LOCK_MAX_SPINS {
            std::hint::spin_loop();
        } else if spins < TT_LOCK_MAX_SPINS * 2 {
            std::thread::yield_now();
        } else {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline - now;
            if remaining >= Duration::from_millis(1) {
                std::thread::sleep(Duration::from_millis(1));
            } else {
                // Windows では 1ms 未満の sleep が丸め込まれるため、残り予算が細い場合は yield で粘る。
                std::thread::yield_now();
            }
        }
        spins += 1;
    }
    None
}

/// 中央集約された finalize 処理。
pub fn finalize_and_send(
    state: &mut EngineState,
    label: &str,
    result: Option<&SearchResult>,
    stale: bool,
    finalize_reason: Option<FinalizeReason>,
) {
    if stale {
        diag_info_string(format!("{label}_stale=1 fallback=fast"));
        finalize_and_send_fast(state, label, finalize_reason);
        return;
    }

    if state.bestmove_emitted {
        diag_info_string(format!("{label}_skip already_emitted=1"));
        return;
    }
    if !state.stop_controller.try_claim_finalize() {
        diag_info_string(format!("{label}_skip claimed_by_other=1"));
        return;
    }
    diag_info_string(format!("{label}_claim_success=1"));

    let committed = if let Some(res) = result {
        let mut committed_pv = res.stats.pv.clone();
        if let Some(bm) = res.best_move {
            // Use equals_without_piece_type to avoid false positives from piece type differences
            let has_mismatch =
                committed_pv.first().is_none_or(|pv0| !pv0.equals_without_piece_type(&bm));
            if has_mismatch {
                diag_info_string(format!(
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

    let snapshot_valid = state.stop_controller.try_read_snapshot().filter(|snap| {
        let sid_ok = state.current_session_core_id.map(|sid| sid == snap.search_id).unwrap_or(true);
        let root_ok = snap.root_key == state.position.zobrist_hash();
        sid_ok && root_ok
    });
    let snapshot_committed = snapshot_valid.as_ref().and_then(|snap| {
        snap.best.map(|best| {
            let mut pv: Vec<_> = snap.pv.iter().copied().collect();
            if pv.first().is_none_or(|mv| !mv.equals_without_piece_type(&best)) {
                pv.insert(0, best);
            }
            engine_core::search::CommittedIteration {
                depth: snap.depth,
                seldepth: None,
                score: snap.score_cp,
                pv,
                node_type: NodeType::Exact,
                nodes: snap.nodes,
                elapsed: Duration::from_millis(snap.elapsed_ms as u64),
            }
        })
    });
    if let Some(snap) = snapshot_valid.as_ref() {
        if snapshot_committed.is_some() {
            diag_info_string(format!(
                "{label}_snapshot_pref sid={} depth={} nodes={} elapsed_ms={}",
                snap.search_id, snap.depth, snap.nodes, snap.elapsed_ms
            ));
        }
    }
    let final_best = {
        let eng = state.engine.lock().unwrap();
        if let Some(ci) = snapshot_committed.as_ref() {
            eng.choose_final_bestmove(&state.position, Some(ci))
        } else {
            eng.choose_final_bestmove(&state.position, committed.as_ref())
        }
    };

    let controller_stop_info = state.stop_controller.try_read_stop_info();

    let stop_meta = prepare_stop_meta(
        label,
        controller_stop_info,
        result.and_then(|r| r.stop_info.as_ref()),
        finalize_reason,
    );

    if let Some(res) = result {
        let best_usi =
            res.best_move.map(|m| move_to_usi(&m)).unwrap_or_else(|| "resign".to_string());
        let pv0_usi = res.stats.pv.first().map(move_to_usi).unwrap_or_else(|| "-".to_string());
        diag_info_string(format!(
            "finalize_snapshot best={} pv0={} depth={} nodes={} elapsed_ms={} stop_reason={}",
            best_usi,
            pv0_usi,
            res.stats.depth,
            res.stats.nodes,
            res.stats.elapsed.as_millis(),
            stop_meta.reason_label
        ));

        // 極小Byoyomi対策の可視化: ハード/ソフト上限と停止理由
        diag_info_string(format!(
            "time_caps hard_ms={} soft_ms={} reason={}",
            stop_meta.hard_ms, stop_meta.soft_ms, stop_meta.reason_label
        ));

        if let Some(tt_hits) = res.stats.tt_hits {
            let nodes = res.stats.nodes;
            let hit_pct = if nodes > 0 {
                (tt_hits as f64 * 100.0) / (nodes as f64)
            } else {
                0.0
            };
            diag_info_string(format!(
                "tt_summary nodes={} hits={} hit_pct={:.2}",
                nodes, tt_hits, hit_pct
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
            let sel_raw =
                res.stats.raw_seldepth.map(|v| v.to_string()).unwrap_or_else(|| "-".to_string());
            let dup = res
                .stats
                .duplication_percentage
                .map(|d| format!("{:.1}", d))
                .unwrap_or_else(|| "-".to_string());
            let rfhi = res.stats.root_fail_high_count.unwrap_or(0);
            let lmr_count = res.stats.lmr_count.unwrap_or(0);
            let lmr_trials = res.stats.lmr_trials.unwrap_or(lmr_count);
            let root_hint_exist = res.stats.root_tt_hint_exists.unwrap_or(0);
            let root_hint_used = res.stats.root_tt_hint_used.unwrap_or(0);

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

            diag_info_string(format!(
                "finalize_diag seldepth={} seldepth_raw={} qratio={:.3} ab_nodes={} tt_hit_rate={:.3} tt_hits={} asp_fail={} asp_hit={} re_searches={} pv_changed={} dup_pct={} root_fail_high={} lmr={} lmr_trials={} root_hint_exist={} root_hint_used={} root_in_check={} root_legal_count={} root_evasion_count={} root_scoring=static checks_in_q_allowed={}",
                sel,
                sel_raw,
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
                lmr_count,
                lmr_trials,
                root_hint_exist,
                root_hint_used,
                root_in_check as i32,
                root_legal_count,
                root_evasion_count,
                checks_in_q_allowed
            ));
        }
    }

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
            let nps_agg_u64 = nps_agg.min(u64::MAX as u128) as u64;

            // Emit TT diagnostics snapshot (address/size/hf/attempts)
            {
                let dbg = {
                    let eng = state.engine.lock().unwrap();
                    eng.tt_debug_info()
                };
                diag_info_string(format!(
                    "tt_debug addr={:#x} size_mb={} hf_permille={} hf_phys_permille={} store_attempts={}",
                    dbg.addr,
                    dbg.size_mb,
                    dbg.hf_permille,
                    dbg.hf_physical_permille,
                    dbg.store_attempts
                ));
            }

            // Optional: TT roundtrip smoke test at current root hash
            #[cfg(all(feature = "diagnostics", feature = "tt_metrics"))]
            {
                let root_hash = state.position.zobrist_hash();
                let ok = {
                    let eng = state.engine.lock().unwrap();
                    eng.tt_roundtrip_test(root_hash)
                };
                diag_info_string(format!("tt_roundtrip root={}", ok));
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
                        let line_nodes = line.nodes.unwrap_or(res.stats.nodes);
                        let line_time_ms =
                            line.time_ms.unwrap_or(res.stats.elapsed.as_millis() as u64);
                        let line_nps = match (line.nodes, line.time_ms) {
                            (Some(n), Some(t)) if t > 0 => n.saturating_mul(1000).saturating_div(t),
                            _ => nps_agg_u64,
                        };
                        s.push_str(&format!(" time {}", line_time_ms));
                        s.push_str(&format!(" nodes {}", line_nodes));
                        s.push_str(&format!(" nps {}", line_nps));
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
    log_and_emit_final_selection(
        state,
        label,
        final_best.source,
        &final_usi,
        ponder_mv,
        &stop_meta,
    );
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

pub fn finalize_and_send_fast(
    state: &mut EngineState,
    label: &str,
    finalize_reason: Option<FinalizeReason>,
) {
    if state.bestmove_emitted {
        diag_info_string(format!("{label}_fast_skip already_emitted=1"));
        return;
    }
    if !state.stop_controller.try_claim_finalize() {
        diag_info_string(format!("{label}_fast_skip claimed_by_other=1"));
        return;
    }
    diag_info_string(format!("{label}_fast_claim_success=1"));

    let controller_stop_info = state.stop_controller.try_read_stop_info();

    if let Some(ref si) = controller_stop_info {
        diag_info_string(format!(
            "{label}_oob_stop_info sid={} reason={:?} elapsed_ms={} soft_ms={} hard_ms={}",
            state.current_session_core_id.unwrap_or(0),
            si.reason,
            si.elapsed_ms,
            si.soft_limit_ms,
            si.hard_limit_ms
        ));
    }

    let stop_meta = prepare_stop_meta(label, controller_stop_info, None, finalize_reason);
    diag_info_string(format!("{}_fast_reason reason={}", label, stop_meta.reason_label));

    let root_key_hex = fmt_hash(state.position.zobrist_hash());

    // Try snapshot first to avoid engine lock when possible
    if let Some(snap) = state.stop_controller.try_read_snapshot() {
        // SessionStart より先に Finalize が届く場合は root_key 側で裏取りする。
        let sid_ok = state.current_session_core_id.map(|sid| sid == snap.search_id).unwrap_or(true);
        let rk_ok = snap.root_key == state.position.zobrist_hash();
        if sid_ok && rk_ok {
            if let Some(best) = snap.best {
                let shallow =
                    snap.depth < FAST_SNAPSHOT_MIN_DEPTH_FOR_DIRECT_EMIT || snap.pv.is_empty();
                let budget_ms =
                    compute_tt_probe_budget_ms(stop_meta.info.as_ref(), snap.elapsed_ms);
                if shallow {
                    let snapshot_emit = if let Some((eng_guard, spent_ms, spent_us)) =
                        try_lock_engine_with_budget(&state.engine, budget_ms)
                    {
                        let (final_usi, ponder_mv, final_source) = {
                            let final_best = eng_guard.choose_final_bestmove(&state.position, None);
                            let used_snapshot_move = final_best.best_move.is_none();
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
                            let final_source = if used_snapshot_move {
                                FinalBestSource::Committed
                            } else {
                                final_best.source
                            };
                            (final_usi, ponder_mv, final_source)
                        };
                        drop(eng_guard);
                        Some((final_usi, ponder_mv, final_source, spent_ms, spent_us))
                    } else {
                        None
                    };

                    if let Some((final_usi, ponder_mv, final_source, spent_ms, spent_us)) =
                        snapshot_emit
                    {
                        diag_info_string(format!(
                            "{}_fast_snapshot sid={} root_key={} depth={} nodes={} elapsed={} pv_len={} tt_probe=1 tt_probe_src=snapshot tt_probe_budget_ms={} tt_probe_spent_ms={}",
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
                        diag_info_string(format!(
                            "{}_fast_snapshot_tt sid={} root_key={} tt_probe_spent_us={}",
                            label,
                            snap.search_id,
                            fmt_hash(snap.root_key),
                            spent_us
                        ));
                        log_and_emit_final_selection(
                            state,
                            label,
                            final_source,
                            &final_usi,
                            ponder_mv,
                            &stop_meta,
                        );
                        return;
                    }
                }

                let final_usi = move_to_usi(&best);
                let ponder_mv = if state.opts.ponder {
                    snap.pv.get(1).map(move_to_usi)
                } else {
                    None
                };
                diag_info_string(format!(
                    "{}_fast_snapshot sid={} root_key={} depth={} nodes={} elapsed={} pv_len={} tt_probe=0 tt_probe_src=snapshot tt_probe_budget_ms={} note=depth_sufficient",
                    label,
                    snap.search_id,
                    fmt_hash(snap.root_key),
                    snap.depth,
                    snap.nodes,
                    snap.elapsed_ms,
                    snap.pv.len(),
                    budget_ms
                ));
                log_and_emit_final_selection(
                    state,
                    label,
                    FinalBestSource::Committed,
                    &final_usi,
                    ponder_mv,
                    &stop_meta,
                );
                return;
            }
        }
    }

    let fallback_budget_ms = compute_tt_probe_budget_ms(stop_meta.info.as_ref(), 0);
    let fallback_emit = if let Some((eng_guard, spent_ms, spent_us)) =
        try_lock_engine_with_budget(&state.engine, fallback_budget_ms)
    {
        let dbg = eng_guard.tt_debug_info();
        let (final_usi, ponder_mv, final_source) = {
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
            (final_usi, ponder_mv, final_best.source)
        };
        drop(eng_guard);
        diag_info_string(format!(
            "{}_fast_tt_debug sid={} root_key={} addr={:#x} size_mb={} hf_permille={} hf_phys_permille={} store_attempts={} tt_probe_budget_ms={} tt_probe_spent_ms={} tt_probe_spent_us={}",
            label,
            state.current_session_core_id.unwrap_or(0),
            root_key_hex,
            dbg.addr,
            dbg.size_mb,
            dbg.hf_permille,
            dbg.hf_physical_permille,
            dbg.store_attempts,
            fallback_budget_ms,
            spent_ms,
            spent_us
        ));
        Some((final_usi, ponder_mv, final_source, spent_ms, spent_us, dbg))
    } else {
        None
    };

    if let Some((final_usi, ponder_mv, final_source, spent_ms, spent_us, dbg)) = fallback_emit {
        diag_info_string(format!(
            "{}_fast_tt_probe sid={} root_key={} tt_probe=1 tt_probe_src=tt tt_probe_budget_ms={} tt_probe_spent_ms={} tt_probe_spent_us={}",
            label,
            state.current_session_core_id.unwrap_or(0),
            root_key_hex,
            fallback_budget_ms,
            spent_ms,
            spent_us
        ));
        diag_info_string(format!(
            "{}_fast_tt_meta sid={} root_key={} addr={:#x} size_mb={} hf_permille={} hf_phys_permille={} store_attempts={}",
            label,
            state.current_session_core_id.unwrap_or(0),
            root_key_hex,
            dbg.addr,
            dbg.size_mb,
            dbg.hf_permille,
            dbg.hf_physical_permille,
            dbg.store_attempts
        ));
        log_and_emit_final_selection(state, label, final_source, &final_usi, ponder_mv, &stop_meta);
        return;
    }

    diag_info_string(format!(
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
                diag_info_string(format!(
                    "{}_fast_select_resign sid={} root_key={}",
                    label,
                    state.current_session_core_id.unwrap_or(0),
                    root_key_hex
                ));
                log_and_emit_final_selection(
                    state,
                    label,
                    FinalBestSource::Resign,
                    "resign",
                    None,
                    &stop_meta,
                );
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
                    None
                } else if in_check {
                    legal_moves.first().copied()
                } else if let Some(mv) =
                    legal_moves.iter().find(|m| is_tactical(m) && !is_king_move(m)).copied()
                {
                    Some(mv)
                } else if let Some(mv) = legal_moves.iter().find(|m| !is_king_move(m)).copied() {
                    Some(mv)
                } else {
                    legal_moves.first().copied()
                };

                if let Some(chosen) = chosen {
                    let final_usi = move_to_usi(&chosen);
                    log_and_emit_final_selection(
                        state,
                        label,
                        FinalBestSource::LegalFallback,
                        &final_usi,
                        None,
                        &stop_meta,
                    );
                } else {
                    diag_info_string(format!(
                        "{}_fast_select_resign sid={} root_key={} no_legal_moves=1",
                        label,
                        state.current_session_core_id.unwrap_or(0),
                        root_key_hex
                    ));
                    log_and_emit_final_selection(
                        state,
                        label,
                        FinalBestSource::Resign,
                        "resign",
                        None,
                        &stop_meta,
                    );
                }
            }
        }
        Err(e) => {
            diag_info_string(format!(
                "{}_fast_select_error sid={} root_key={} resign_fallback=1 err={}",
                label,
                state.current_session_core_id.unwrap_or(0),
                root_key_hex,
                e
            ));
            log_and_emit_final_selection(
                state,
                label,
                FinalBestSource::Resign,
                "resign",
                None,
                &stop_meta,
            );
        }
    }
}
const FAST_SNAPSHOT_MIN_DEPTH_FOR_DIRECT_EMIT: u8 = 2;

#[cfg(test)]
mod tests {
    use super::*;
    use engine_core::engine::controller::{Engine, EngineType};

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
        // Remaining時間が 2ms 未満ならロックを諦める
        let si_close = StopInfo {
            elapsed_ms: 1_999,
            ..si
        };
        assert_eq!(compute_tt_probe_budget_ms(Some(&si_close), 0), 0);
        // Missing StopInfo yields zero budget
        assert_eq!(compute_tt_probe_budget_ms(None, 0), 0);
    }

    #[test]
    fn emit_bestmove_once_sets_flags_and_is_idempotent() {
        use crate::state::EngineState;
        use std::time::Instant;

        let mut state = EngineState::new();
        state.current_root_hash = Some(0x1234);
        state.deadline_hard = Some(Instant::now());
        state.deadline_near = Some(Instant::now());
        state.deadline_near_notified = true;

        assert!(emit_bestmove_once(&mut state, "resign", None));
        assert!(state.bestmove_emitted);
        assert!(state.current_root_hash.is_none());
        assert!(state.deadline_hard.is_none());
        assert!(state.deadline_near.is_none());
        assert!(!state.deadline_near_notified);

        // Second call should be a no-op
        assert!(!emit_bestmove_once(&mut state, "resign", None));
    }

    #[test]
    fn sanitize_ponder_drops_for_resign() {
        assert!(sanitize_ponder_for_bestmove("resign", Some("7g7f".to_string())).is_none());
        let kept = sanitize_ponder_for_bestmove("7g7f", Some("7g7f".to_string()));
        assert_eq!(kept.as_deref(), Some("7g7f"));
        assert!(sanitize_ponder_for_bestmove("win", Some("7g7f".to_string())).is_none());
    }

    #[test]
    fn try_lock_engine_with_budget_succeeds_when_unlocked() {
        let engine = Arc::new(Mutex::new(Engine::new(EngineType::Material)));
        let result =
            super::try_lock_engine_with_budget(&engine, 1).expect("lock should succeed when free");
        drop(result.0);
    }

    #[test]
    fn try_lock_engine_with_budget_respects_deadline_when_locked() {
        let engine = Arc::new(Mutex::new(Engine::new(EngineType::Material)));
        let guard = engine.lock().unwrap();
        let result = super::try_lock_engine_with_budget(&engine, 1);
        drop(guard);
        assert!(result.is_none(), "lock attempt must time out when mutex is held");
    }

    #[test]
    fn gather_stop_meta_appends_tm_kind_tag() {
        use engine_core::search::types::TerminationReason;

        let mut base = StopInfo {
            reason: TerminationReason::TimeLimit,
            elapsed_ms: 1_000,
            nodes: 42,
            depth_reached: 12,
            hard_timeout: false,
            soft_limit_ms: 1_500,
            hard_limit_ms: 2_000,
        };

        let soft_meta =
            gather_stop_meta(Some(base.clone()), None, Some(FinalizeReason::TimeManagerStop));
        assert_eq!(soft_meta.reason_label, "TimeManagerStop|tm=soft");

        base.hard_timeout = true;
        let hard_meta =
            gather_stop_meta(Some(base.clone()), None, Some(FinalizeReason::TimeManagerStop));
        assert_eq!(hard_meta.reason_label, "TimeManagerStop|tm=hard");

        let unknown_meta = gather_stop_meta(None, None, Some(FinalizeReason::TimeManagerStop));
        assert_eq!(unknown_meta.reason_label, "TimeManagerStop|tm=unknown");
    }
}
