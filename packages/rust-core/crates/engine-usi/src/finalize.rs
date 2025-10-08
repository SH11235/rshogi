use engine_core::engine::controller::{FinalBest, FinalBestSource};
use engine_core::search::parallel::FinalizeReason;
use engine_core::search::snapshot::SnapshotSource;
use engine_core::search::{types::StopInfo, SearchResult};
use engine_core::usi::{append_usi_score_and_bound, move_to_usi};
// use engine_core::util::search_helpers::quick_search_move; // not used in current impl
use engine_core::{movegen::MoveGenerator, shogi::PieceType};

use crate::io::{diag_info_string, info_string, usi_println};
use crate::state::EngineState;
use crate::util::{emit_bestmove, score_view_with_clamp};

#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[cfg(test)]
static LAST_EMITTED_BESTMOVE: OnceLock<Mutex<Option<String>>> = OnceLock::new();

#[cfg(test)]
fn test_record_bestmove(final_usi: &str, ponder: Option<&str>) {
    let entry = LAST_EMITTED_BESTMOVE.get_or_init(|| Mutex::new(None));
    let mut guard = entry.lock().unwrap();
    let mut payload = format!("bestmove {}", final_usi);
    if let Some(p) = ponder {
        payload.push_str(&format!(" ponder {}", p));
    }
    *guard = Some(payload);
}

#[cfg(test)]
pub fn take_last_emitted_bestmove() -> Option<String> {
    LAST_EMITTED_BESTMOVE.get_or_init(|| Mutex::new(None)).lock().unwrap().take()
}

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

fn finalize_sanity_check(
    state: &mut EngineState,
    stop_meta: &StopMeta,
    final_best: &FinalBest,
    result: Option<&SearchResult>,
) -> Option<engine_core::shogi::Move> {
    if !state.opts.finalize_sanity_enabled || state.current_is_ponder {
        return None;
    }
    if state.position.is_in_check() {
        return None;
    }
    let pv1 = final_best.best_move?;
    // Mate/draw guard: if PV1 score is mate帯 or (近似的に)ドロー評価に近いならスイッチ禁止
    if let Some(res) = result {
        use engine_core::search::constants::MATE_SCORE;
        let sc = res.score;
        if sc.abs() >= MATE_SCORE - 100 || sc.abs() <= 10 {
            return None;
        }
    }
    // SEE gate (own move) + Opponent capture SEE gate after PV1
    let see = state.position.see(pv1);
    let see_min = state.opts.finalize_sanity_see_min_cp;
    let mut need_verify = see < see_min;

    // If own SEE is fine (non-negative or above threshold), still guard
    // against immediate opponent tactical shots after PV1.
    // Compute max opponent capture SEE in child position and trigger
    // mini verification if it exceeds configured threshold.
    let mut opp_cap_see_max = 0;
    if !need_verify {
        let mut pos1 = state.position.clone();
        pos1.do_move(pv1);
        let mg2 = MoveGenerator::new();
        if let Ok(list2) = mg2.generate_all(&pos1) {
            let mut best = 0;
            let opp_gate = state.opts.finalize_sanity_opp_see_min_cp.max(0);
            for &mv in list2.as_slice() {
                // 相手手番。捕獲ヒントかつ合法手に限定して SEE を評価
                if mv.is_capture_hint() && pos1.is_legal_move(mv) {
                    let g = pos1.see(mv); // opponent perspective (gain if positive)
                    if g > best {
                        best = g;
                        // 閾値到達で早期打ち切り
                        if best >= opp_gate {
                            break;
                        }
                    }
                }
            }
            opp_cap_see_max = best;
            if best >= opp_gate {
                need_verify = true;
            }
        }
    }

    let diag_base = format!("sanity_checked=1 see={} opp_cap_see_max={}", see, opp_cap_see_max);
    if !need_verify {
        info_string(format!("{} switched=0 reason=see_ok", diag_base));
        return None;
    }
    // Candidate: prefer PV2 if available and legal; fallback to best SEE>=0 (または最高SEE)
    let mg = MoveGenerator::new();
    let Ok(list) = mg.generate_all(&state.position) else {
        info_string("sanity_checked=1 switched=0 reason=no_moves");
        return None;
    };
    // Prefer PV2 (from SearchResult lines, if available)
    let mut best_alt = if let Some(res) = result {
        if let Some(lines) = &res.lines {
            if let Some(l2) = lines.iter().find(|l| l.multipv_index == 2) {
                l2.pv.first().copied().filter(|m| Some(*m) != final_best.best_move)
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    let mut alt_from_pv2 = false;
    // PV2 候補の合法性確認（擬似合法の可能性を排除）
    // Validate PV2 candidate legality to prevent pseudo-legal moves from being selected
    if let Some(mv0) = best_alt {
        if !state.position.is_legal_move(mv0) {
            info_string("sanity_pv2_illegal=1 fallback=see_best");
            best_alt = None; // Fallback to SEE-best candidate
        } else if let (Some(res), Some(mv0)) = (result, best_alt) {
            if let Some(lines) = &res.lines {
                if let Some(l2) = lines.iter().find(|l| l.multipv_index == 2) {
                    if l2.pv.first().is_some_and(|m| *m == mv0) {
                        alt_from_pv2 = true;
                    }
                }
            }
        }
    }
    let mut best_alt_see = i32::MIN;
    for &mv in list.as_slice() {
        if Some(mv) == final_best.best_move {
            continue;
        }
        if !state.position.is_legal_move(mv) {
            continue;
        }
        let s = state.position.see(mv);
        if best_alt.is_none() {
            // if PV2 wasn’t set, pick SEE-best candidate
            if s > best_alt_see {
                best_alt_see = s;
                best_alt = Some(mv);
            }
        }
    }
    let Some(alt) = best_alt else {
        info_string(format!("{} switched=0 reason=no_alt", diag_base));
        return None;
    };
    // Budget
    let budget_max = state.opts.finalize_sanity_budget_ms;
    let total_budget = compute_tt_probe_budget_ms(stop_meta.info.as_ref(), 0).min(budget_max);
    // Split budget across both mini searches（合計上限を厳守）
    let per_budget = if total_budget >= 2 {
        total_budget / 2
    } else {
        total_budget
    };
    if total_budget == 0 {
        return None;
    }
    // Mini search (depth 1-2) for pv1 vs alt
    let mini_depth = state.opts.finalize_sanity_mini_depth;
    let switch_margin = state.opts.finalize_sanity_switch_margin_cp;
    let (s1, s2);
    let (mv1, mv2) = (pv1, alt);
    // Lock engine with small budget
    if let Some((mut eng, _, _)) = try_lock_engine_with_budget(&state.engine, total_budget) {
        // Evaluate child of pv1
        let mut pos1 = state.position.clone();
        pos1.do_move(mv1);
        s1 = eng
            .search(
                &mut pos1,
                engine_core::search::SearchLimits::builder()
                    .depth(mini_depth)
                    .fixed_time_ms(per_budget)
                    .build(),
            )
            .score;

        // Evaluate child of alt
        let mut pos2 = state.position.clone();
        pos2.do_move(mv2);
        s2 = eng
            .search(
                &mut pos2,
                engine_core::search::SearchLimits::builder()
                    .depth(mini_depth)
                    .fixed_time_ms(per_budget)
                    .build(),
            )
            .score;
    } else {
        return None;
    }
    let switched = s2 > s1 + switch_margin;
    if switched {
        info_string(format!(
            "{} alt={} s1={} s2={} margin={} switched=1 origin={} total_budget_ms={} per_ms={}",
            diag_base,
            move_to_usi(&alt),
            s1,
            s2,
            switch_margin,
            if alt_from_pv2 { "pv2" } else { "see_best" },
            total_budget,
            per_budget,
        ));
    } else {
        info_string(format!(
            "{} alt={} s1={} s2={} margin={} switched=0",
            diag_base,
            move_to_usi(&alt),
            s1,
            s2,
            switch_margin,
        ));
    }
    if switched {
        Some(alt)
    } else {
        None
    }
}

fn prepare_stop_meta(
    _label: &str,
    controller_info: Option<StopInfo>,
    result_stop_info: Option<&StopInfo>,
    finalize_reason: Option<FinalizeReason>,
) -> StopMeta {
    gather_stop_meta(controller_info, result_stop_info, finalize_reason)
}

struct FinalizeEventParams {
    reported_depth: u8,
    stable_depth: Option<u8>,
    incomplete_depth: Option<u8>,
    report_source: SnapshotSource,
    snapshot_version: Option<u64>,
}

fn emit_finalize_event(
    state: &EngineState,
    label: &str,
    mode: &str,
    stop_meta: &StopMeta,
    params: &FinalizeEventParams,
) {
    let sid = state.current_session_core_id.unwrap_or(0);
    let stable = params.stable_depth.unwrap_or(0);
    let incomplete = params.incomplete_depth.unwrap_or(0);
    let source_str = match params.report_source {
        SnapshotSource::Stable => "stable",
        SnapshotSource::Partial => "partial",
    };
    let version = params.snapshot_version.unwrap_or(0);
    info_string(format!(
        "finalize_event label={label} mode={mode} reason={} sid={sid} soft_ms={} hard_ms={} reported_depth={} stable_depth={stable} incomplete_depth={incomplete} source={source_str} snapshot_version={version}",
        stop_meta.reason_label,
        stop_meta.soft_ms,
        stop_meta.hard_ms,
        params.reported_depth
    ));
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

    if finalize_reason.is_some_and(|r| matches!(r, FinalizeReason::PonderToMove)) {
        reason_label.push_str("|tm=n/a");
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
    #[cfg(test)]
    test_record_bestmove(&final_usi, ponder.as_deref());
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
    if state.current_is_ponder && !matches!(finalize_reason, Some(FinalizeReason::UserStop)) {
        diag_info_string(format!(
            "{}_ponder_guard suppressed=1 reason={:?}",
            label, finalize_reason
        ));
        return;
    }
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
        if snap.source != SnapshotSource::Stable {
            return None;
        }
        snap.lines.first().map(|line| engine_core::search::CommittedIteration {
            depth: snap.depth,
            seldepth: snap.seldepth,
            score: line.score_cp,
            pv: line.pv.iter().copied().collect(),
            node_type: line.bound,
            nodes: snap.nodes,
            elapsed: Duration::from_millis(snap.elapsed_ms),
        })
    });
    if let Some(snap) = snapshot_valid.as_ref() {
        if snapshot_committed.is_some() {
            diag_info_string(format!(
                "{label}_snapshot_pref sid={} depth={} nodes={} elapsed_ms={} source={:?} version={}",
                snap.search_id,
                snap.depth,
                snap.nodes,
                snap.elapsed_ms,
                snap.source,
                snap.version
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
        result.as_ref().and_then(|r| r.stop_info.as_ref()),
        finalize_reason,
    );

    let (reported_depth, report_source, stable_depth_stat, incomplete_depth_stat, snapshot_version) =
        if let Some(res) = result.as_ref() {
            (
                res.stats.depth,
                res.stats.root_report_source.unwrap_or(SnapshotSource::Partial),
                res.stats.stable_depth,
                res.stats.incomplete_depth,
                res.stats.snapshot_version,
            )
        } else if let Some(snap) = snapshot_valid.as_ref() {
            (
                snap.depth,
                snap.source,
                (snap.source == SnapshotSource::Stable).then_some(snap.depth),
                None,
                Some(snap.version),
            )
        } else {
            (0, SnapshotSource::Partial, None, None, None)
        };

    emit_finalize_event(
        state,
        label,
        "joined",
        &stop_meta,
        &FinalizeEventParams {
            reported_depth,
            stable_depth: stable_depth_stat,
            incomplete_depth: incomplete_depth_stat,
            report_source,
            snapshot_version,
        },
    );

    if let Some(res) = result {
        let best_usi =
            res.best_move.map(|m| move_to_usi(&m)).unwrap_or_else(|| "resign".to_string());
        let pv0_usi = res.stats.pv.first().map(move_to_usi).unwrap_or_else(|| "-".to_string());
        let source_label = res
            .stats
            .root_report_source
            .map(|src| match src {
                SnapshotSource::Stable => "stable",
                SnapshotSource::Partial => "partial",
            })
            .unwrap_or("partial");
        let snap_version = res.stats.snapshot_version.unwrap_or(0);
        diag_info_string(format!(
            "finalize_snapshot best={} pv0={} depth={} nodes={} elapsed_ms={} stop_reason={} source={} snapshot_version={}",
            best_usi,
            pv0_usi,
            res.stats.depth,
            res.stats.nodes,
            res.stats.elapsed.as_millis(),
            stop_meta.reason_label,
            source_label,
            snap_version
        ));

        // 極小Byoyomi対策の可視化: ハード/ソフト上限と停止理由
        diag_info_string(format!(
            "time_caps hard_ms={} soft_ms={} reason={}",
            stop_meta.hard_ms, stop_meta.soft_ms, stop_meta.reason_label
        ));

        if let Some(helper_share) = res.stats.helper_share_pct {
            info_string(format!("helper_share_pct={helper_share:.2}"));
        }
        if let Some(heur) = res.stats.heuristics.as_ref() {
            let summary = heur.summary();
            let lmr_trials = res.stats.lmr_trials.unwrap_or(summary.lmr_trials);
            info_string(format!(
                "heuristics quiet_max={} cont_max={} capture_max={} counter_filled={} lmr_trials={}",
                summary.quiet_max,
                summary.continuation_max,
                summary.capture_max,
                summary.counter_filled,
                lmr_trials
            ));
        }

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
                .helper_share_pct
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

    // Optional finalize sanity check (may switch PV1)
    let maybe_switch = finalize_sanity_check(state, &stop_meta, &final_best, result);
    let (chosen_mv, chosen_src) = if let Some(m) = maybe_switch {
        (Some(m), FinalBestSource::Committed)
    } else {
        (final_best.best_move, final_best.source)
    };

    let final_usi = chosen_mv.map(|m| move_to_usi(&m)).unwrap_or_else(|| "resign".to_string());
    let ponder_mv = if state.opts.ponder {
        final_best.pv.get(1).map(move_to_usi).or_else(|| {
            chosen_mv.and_then(|bm| {
                let eng = state.engine.lock().unwrap();
                eng.get_ponder_from_tt(&state.position, bm).map(|m| move_to_usi(&m))
            })
        })
    } else {
        None
    };
    log_and_emit_final_selection(state, label, chosen_src, &final_usi, ponder_mv, &stop_meta);
    // Clear pending_ponder_result to prevent stale buffer usage
    state.pending_ponder_result = None;
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
    if state.current_is_ponder && !matches!(finalize_reason, Some(FinalizeReason::UserStop)) {
        diag_info_string(format!(
            "{}_ponder_guard suppressed=1 reason={:?}",
            label, finalize_reason
        ));
        return;
    }
    if state.bestmove_emitted {
        diag_info_string(format!("{label}_fast_skip already_emitted=1"));
        return;
    }
    if !state.stop_controller.try_claim_finalize() {
        diag_info_string(format!("{label}_fast_skip claimed_by_other=1"));
        return;
    }
    diag_info_string(format!("{label}_fast_claim_success=1"));

    // Prioritize pending_ponder_result if available (ponderhit-instant-finalize)
    if let Some(pr) = state.pending_ponder_result.take() {
        // Verify session and position match to prevent stale buffer usage
        // Relaxed session_id check: allow None on receiver side (late-bind)
        let sid_match = match (pr.session_id, state.current_session_core_id) {
            (Some(a), Some(b)) => a == b,
            (_, None) => true, // Receiver side not yet initialized -> allow
            _ => false,
        };
        let position_match = pr.root_hash == state.position.zobrist_hash()
            || state.current_root_hash.map(|h| h == pr.root_hash).unwrap_or(false);

        // Prioritize position_match; late-bind session_id if needed
        if position_match && !state.searching {
            if !sid_match && pr.session_id.is_some() {
                state.current_session_core_id = pr.session_id;
                info_string("ponderhit_cached_sid_late_bind=1");
            }
            if let Some(best) = pr.best_move {
                info_string(format!(
                    "ponderhit_cached=1 depth={} nodes={} elapsed_ms={} sid_match={} pos_match={}",
                    pr.depth, pr.nodes, pr.elapsed_ms, sid_match, position_match
                ));
                // Emit finalize_event for consistent logging
                emit_finalize_event(
                    state,
                    label,
                    "cached",
                    &gather_stop_meta(None, None, Some(FinalizeReason::PonderToMove)),
                    &FinalizeEventParams {
                        reported_depth: pr.depth,
                        stable_depth: Some(pr.depth),
                        incomplete_depth: None,
                        report_source: SnapshotSource::Stable,
                        snapshot_version: None,
                    },
                );
                let ponder_hint = pr.pv_second;
                let _ = emit_bestmove_once(state, best, ponder_hint);
                // Clear buffer after successful emission
                state.pending_ponder_result = None;
                return;
            }
        } else {
            info_string(format!(
                "ponderhit_cached_stale=1 sid_match={} pos_match={} searching={}",
                sid_match, position_match, state.searching
            ));
        }
    }
    // Defense-in-depth: clear stale buffer to prevent unintended reuse
    state.pending_ponder_result = None;

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

    let snapshot_any = state.stop_controller.try_read_snapshot();

    let stop_meta = prepare_stop_meta(label, controller_stop_info, None, finalize_reason);
    let (reported_depth, report_source, stable_depth_stat, snapshot_version) =
        if let Some(snap) = snapshot_any.as_ref() {
            (
                snap.depth,
                snap.source,
                (snap.source == SnapshotSource::Stable).then_some(snap.depth),
                Some(snap.version),
            )
        } else {
            (0, SnapshotSource::Partial, None, None)
        };
    emit_finalize_event(
        state,
        label,
        "fast",
        &stop_meta,
        &FinalizeEventParams {
            reported_depth,
            stable_depth: stable_depth_stat,
            incomplete_depth: None,
            report_source,
            snapshot_version,
        },
    );
    diag_info_string(format!("{}_fast_reason reason={}", label, stop_meta.reason_label));

    let root_key_hex = fmt_hash(state.position.zobrist_hash());

    // Try snapshot first to avoid engine lock when possible
    if let Some(snap) = snapshot_any.clone().or_else(|| state.stop_controller.try_read_snapshot()) {
        // SessionStart より先に Finalize が届く場合は root_key 側で裏取りする。
        let sid_ok = state.current_session_core_id.map(|sid| sid == snap.search_id).unwrap_or(true);
        let rk_ok = snap.root_key == state.position.zobrist_hash();
        if sid_ok && rk_ok {
            if let Some(best) = snap.best {
                let shallow = snap.depth
                    < engine_core::search::constants::HELPER_SNAPSHOT_MIN_DEPTH as u8
                    || snap.pv.is_empty();
                let elapsed_ms_u32 = snap.elapsed_ms.min(u32::MAX as u64) as u32;
                let budget_ms = compute_tt_probe_budget_ms(stop_meta.info.as_ref(), elapsed_ms_u32);
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
                    "{}_fast_snapshot sid={} root_key={} depth={} nodes={} elapsed={} pv_len={} source={:?} snapshot_version={} tt_probe=1 tt_probe_src=snapshot tt_probe_budget_ms={} tt_probe_spent_ms={}",
                    label,
                    snap.search_id,
                    fmt_hash(snap.root_key),
                    snap.depth,
                    snap.nodes,
                    snap.elapsed_ms,
                    snap.pv.len(),
                    snap.source,
                    snap.version,
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
                let note = if shallow {
                    "shallow_tt_probe_missed"
                } else {
                    "depth_sufficient"
                };
                diag_info_string(format!(
                    "{}_fast_snapshot sid={} root_key={} depth={} nodes={} elapsed={} pv_len={} source={:?} snapshot_version={} tt_probe=0 tt_probe_src=snapshot tt_probe_budget_ms={} note={}",
                    label,
                    snap.search_id,
                    fmt_hash(snap.root_key),
                    snap.depth,
                    snap.nodes,
                    snap.elapsed_ms,
                    snap.pv.len(),
                    snap.source,
                    snap.version,
                    budget_ms,
                    note
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::EngineState;
    use engine_core::engine::controller::{Engine, EngineType};
    use engine_core::movegen::MoveGenerator;
    use engine_core::search::parallel::FinalizeReason;
    use engine_core::search::types::{NodeType, RootLine};
    use engine_core::usi::parse_usi_move;
    use smallvec::SmallVec;

    fn build_root_line(
        root_move: engine_core::shogi::Move,
        depth: u32,
        nodes: u64,
        time_ms: u64,
        score_cp: i32,
    ) -> RootLine {
        let mut pv: SmallVec<[engine_core::shogi::Move; 32]> = SmallVec::new();
        pv.push(root_move);
        RootLine {
            multipv_index: 1,
            root_move,
            score_internal: score_cp,
            score_cp,
            bound: NodeType::Exact,
            depth,
            seldepth: Some(depth.min(u8::MAX as u32) as u8),
            pv,
            nodes: Some(nodes),
            time_ms: Some(time_ms),
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        }
    }

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
    fn finalize_guard_skips_bestmove_during_ponder() {
        let mut state = EngineState::new();
        state.current_is_ponder = true;
        finalize_and_send(&mut state, "test_guard", None, false, Some(FinalizeReason::Planned));
        assert!(!state.bestmove_emitted);
        assert!(
            state.stop_controller.try_claim_finalize(),
            "guarded finalize should not consume the claim"
        );
    }

    #[test]
    fn finalize_fast_guard_skips_bestmove_during_ponder() {
        let mut state = EngineState::new();
        state.current_is_ponder = true;
        finalize_and_send_fast(&mut state, "test_guard_fast", Some(FinalizeReason::Hard));
        assert!(!state.bestmove_emitted);
        assert!(state.stop_controller.try_claim_finalize());
    }

    #[test]
    fn finalize_fast_uses_tt_probe_for_shallow_snapshot() {
        let mut state = EngineState::new();
        let session_id = 99;
        state.current_session_core_id = Some(session_id);
        state.stop_controller.publish_session(None, session_id);
        state.stop_controller.prime_stop_info(StopInfo::default());

        let best_move = parse_usi_move("5g5f").unwrap();
        let mut pv: SmallVec<[engine_core::shogi::Move; 32]> = SmallVec::new();
        pv.push(best_move);
        let shallow_line = RootLine {
            multipv_index: 1,
            root_move: best_move,
            score_internal: 120,
            score_cp: 120,
            bound: NodeType::Exact,
            depth: 2,
            seldepth: Some(2),
            pv,
            nodes: Some(128),
            time_ms: Some(5),
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: None,
        };

        state.stop_controller.publish_committed_snapshot(
            session_id,
            state.position.zobrist_hash(),
            std::slice::from_ref(&shallow_line),
            128,
            5,
        );

        let expected = {
            let eng = state.engine.lock().unwrap();
            let fb = eng.choose_final_bestmove(&state.position, None);
            fb.best_move.expect("legal fallback expected")
        };

        super::take_last_emitted_bestmove();

        finalize_and_send_fast(&mut state, "fast_snapshot_unit", Some(FinalizeReason::Hard));
        assert!(state.bestmove_emitted, "bestmove must be emitted");

        let emitted = super::take_last_emitted_bestmove()
            .expect("captured bestmove output")
            .replace("bestmove ", "");
        assert_eq!(emitted, move_to_usi(&expected));
    }

    #[test]
    fn finalize_fast_emits_partial_snapshot_bestmove() {
        let mut state = EngineState::new();
        let session_id = 1;
        state.current_session_core_id = Some(session_id);
        let root_key = state.position.zobrist_hash();
        state.stop_controller.publish_session(None, session_id);
        state.stop_controller.prime_stop_info(StopInfo::default());

        let best_move = parse_usi_move("7g7f").unwrap();
        let line = build_root_line(best_move, 4, 256, 10, 80);
        super::take_last_emitted_bestmove();
        state.stop_controller.publish_root_line(session_id, root_key, &line);

        finalize_and_send_fast(&mut state, "partial_fast_unit", Some(FinalizeReason::Hard));
        assert!(state.bestmove_emitted);
        let emitted = super::take_last_emitted_bestmove().expect("bestmove emitted");
        assert_eq!(emitted, format!("bestmove {}", move_to_usi(&best_move)));

        // Subsequent fast finalize should be a no-op.
        super::take_last_emitted_bestmove();
        finalize_and_send_fast(&mut state, "partial_fast_unit_repeat", Some(FinalizeReason::Hard));
        assert!(super::take_last_emitted_bestmove().is_none());
    }

    #[test]
    fn finalize_fast_prefers_latest_partial_snapshot() {
        let mut state = EngineState::new();
        let session_id = 2;
        state.current_session_core_id = Some(session_id);
        let root_key = state.position.zobrist_hash();
        state.stop_controller.publish_session(None, session_id);
        state.stop_controller.prime_stop_info(StopInfo::default());

        let first_move = parse_usi_move("7g7f").unwrap();
        let second_move = parse_usi_move("2g2f").unwrap();

        let shallow = build_root_line(first_move, 3, 200, 12, 60);
        let deeper = build_root_line(second_move, 5, 400, 20, 90);

        super::take_last_emitted_bestmove();
        state.stop_controller.publish_root_line(session_id, root_key, &shallow);
        state.stop_controller.publish_root_line(session_id, root_key, &deeper);

        finalize_and_send_fast(&mut state, "partial_fast_latest", Some(FinalizeReason::Hard));
        let emitted = super::take_last_emitted_bestmove().expect("bestmove emitted");
        assert_eq!(emitted, format!("bestmove {}", move_to_usi(&second_move)));
    }

    #[test]
    fn finalize_fast_ignores_partial_snapshot_from_other_session() {
        let mut state = EngineState::new();
        let session_id = 3;
        state.current_session_core_id = Some(session_id);
        state.stop_controller.publish_session(None, session_id);
        state.stop_controller.prime_stop_info(StopInfo::default());

        let fallback = {
            let eng = state.engine.lock().unwrap();
            let final_best = eng.choose_final_bestmove(&state.position, None);
            final_best.best_move.expect("legal fallback expected")
        };
        let fallback_usi = move_to_usi(&fallback);

        let mg = MoveGenerator::new();
        let legal_moves = mg.generate_all(&state.position).expect("legal moves");
        let alternate = legal_moves
            .as_slice()
            .iter()
            .copied()
            .find(|mv| move_to_usi(mv) != fallback_usi)
            .expect("alternate legal move exists");
        let line = build_root_line(alternate, 4, 256, 15, 100);

        super::take_last_emitted_bestmove();
        state.stop_controller.publish_root_line(
            session_id + 1,
            state.position.zobrist_hash(),
            &line,
        );

        finalize_and_send_fast(&mut state, "partial_fast_ignore", Some(FinalizeReason::Hard));
        let emitted = super::take_last_emitted_bestmove().expect("bestmove emitted");
        assert_eq!(emitted, format!("bestmove {}", fallback_usi));
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
