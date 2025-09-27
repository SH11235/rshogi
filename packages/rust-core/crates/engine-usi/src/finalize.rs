use engine_core::engine::controller::{FinalBest, FinalBestSource};
use engine_core::search::SearchResult;
use engine_core::usi::{append_usi_score_and_bound, move_to_usi, score_view_from_internal};

use crate::io::{info_string, usi_println};
use crate::state::EngineState;

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

/// 中央集約された finalize 処理。
pub fn finalize_and_send(
    state: &mut EngineState,
    label: &str,
    result: Option<&SearchResult>,
    stale: bool,
) {
    if stale {
        info_string(format!("{label}_stale resign=1"));
        usi_println("bestmove resign");
        state.bestmove_emitted = true;
        state.current_root_hash = None;
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

            info_string(format!(
                "finalize_diag seldepth={} qratio={:.3} tt_hit_rate={:.3} tt_hits={} asp_fail={} asp_hit={} re_searches={} pv_changed={} dup_pct={} root_fail_high={}",
                sel,
                qratio,
                tt_hit_rate,
                tt_hits,
                asp_fail,
                asp_hit,
                rese,
                pvchg,
                dup,
                rfhi
            ));
        }
    }

    info_string(format!(
        "{}_select source={} move={} stale={} soft_ms={} hard_ms={}",
        label,
        source_to_str(final_best.source),
        final_best
            .best_move
            .map(|m| move_to_usi(&m))
            .unwrap_or_else(|| "resign".to_string()),
        if stale { 1 } else { 0 },
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
                        let mut view = score_view_from_internal(line.score_internal);
                        if let engine_core::usi::ScoreView::Cp(cp) = view {
                            if cp <= -(engine_core::search::constants::SEARCH_INF - 1) {
                                view = engine_core::usi::ScoreView::Cp(-29_999);
                            }
                        }
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

    #[cfg(feature = "tt-metrics")]
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
    if let Some(p) = ponder_mv {
        usi_println(&format!("bestmove {} ponder {}", final_usi, p));
    } else {
        usi_println(&format!("bestmove {}", final_usi));
    }
    state.bestmove_emitted = true;
    state.current_root_hash = None;
}

fn emit_single_pv(res: &SearchResult, final_best: &FinalBest, nps_agg: u128, hf_permille: u16) {
    let mut s = String::from("info");
    s.push_str(" multipv 1");
    s.push_str(&format!(" depth {}", res.stats.depth));
    if let Some(sd) = res.stats.seldepth {
        s.push_str(&format!(" seldepth {}", sd));
    }
    s.push_str(&format!(" time {}", res.stats.elapsed.as_millis()));
    s.push_str(&format!(" nodes {}", res.stats.nodes));
    s.push_str(&format!(" nps {}", nps_agg));
    s.push_str(&format!(" hashfull {}", hf_permille));
    let mut view = score_view_from_internal(res.score);
    if let engine_core::usi::ScoreView::Cp(cp) = view {
        if cp <= -(engine_core::search::constants::SEARCH_INF - 1) {
            view = engine_core::usi::ScoreView::Cp(-29_999);
        }
    }
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
    if let Ok(eng) = state.engine.try_lock() {
        let final_best = eng.choose_final_bestmove(&state.position, None);
        let final_usi = final_best
            .best_move
            .map(|m| move_to_usi(&m))
            .unwrap_or_else(|| "resign".to_string());
        let ponder_mv = if state.opts.ponder {
            final_best.pv.get(1).map(move_to_usi).or_else(|| {
                final_best.best_move.and_then(|bm| {
                    eng.get_ponder_from_tt(&state.position, bm).map(|m| move_to_usi(&m))
                })
            })
        } else {
            None
        };
        info_string(format!(
            "{}_fast_select source={} move={} ponder={:?}",
            label,
            source_to_str(final_best.source),
            final_usi,
            ponder_mv
        ));
        if let Some(p) = ponder_mv {
            usi_println(&format!("bestmove {} ponder {}", final_usi, p));
        } else {
            usi_println(&format!("bestmove {}", final_usi));
        }
        state.bestmove_emitted = true;
        state.current_root_hash = None;
        return;
    }

    info_string(format!("{}_fast_path=legal_fallback", label));
    let mg = engine_core::movegen::MoveGenerator::new();
    match mg.generate_all(&state.position) {
        Ok(list) => {
            let slice = list.as_slice();
            if let Some(mv) = slice.first() {
                let final_usi = move_to_usi(mv);
                info_string(format!("{}_fast_select_legal move={}", label, final_usi));
                usi_println(&format!("bestmove {}", final_usi));
            } else {
                info_string(format!("{}_fast_select_resign", label));
                usi_println("bestmove resign");
            }
        }
        Err(e) => {
            info_string(format!("{}_fast_select_error resign_fallback=1 err={}", label, e));
            usi_println("bestmove resign");
        }
    }
    state.bestmove_emitted = true;
    state.current_root_hash = None;
}
