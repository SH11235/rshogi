use engine_core::engine::controller::{FinalBest, FinalBestSource};
use engine_core::search::common::is_mate_score;
use engine_core::search::constants::mate_distance as md;
use engine_core::search::parallel::FinalizeReason;
use engine_core::search::snapshot::SnapshotSource;
use engine_core::search::{
    types::{NodeType, StopInfo},
    SearchResult,
};
use engine_core::usi::{append_usi_score_and_bound, move_to_usi, parse_usi_move};
use std::cmp::Reverse;
// use engine_core::util::search_helpers::quick_search_move; // not used in current impl
use engine_core::{movegen::MoveGenerator, shogi::PieceType};

use crate::io::{diag_info_string, info_string, usi_println};
use crate::state::EngineState;
use crate::util::{emit_bestmove, score_view_with_clamp};

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

// near-draw の判定しきい値（cp換算）。README にも注記あり。
const NEAR_DRAW_CP: i32 = 10;

#[cfg(test)]
thread_local! {
    static LAST_EMITTED_BESTMOVE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_record_bestmove(final_usi: &str, ponder: Option<&str>) {
    let mut payload = format!("bestmove {}", final_usi);
    if let Some(p) = ponder {
        payload.push_str(&format!(" ponder {}", p));
    }
    LAST_EMITTED_BESTMOVE.with(|slot| *slot.borrow_mut() = Some(payload));
}

#[cfg(test)]
pub fn take_last_emitted_bestmove() -> Option<String> {
    LAST_EMITTED_BESTMOVE.with(|slot| slot.borrow_mut().take())
}

// ---------- Test probe for structured finalize outcomes (cfg(test) only)
#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub struct FinalizeOutcome {
    pub mode: &'static str,             // "joined" | "fast"
    pub chosen_source: &'static str,    // see source_to_str
    pub reason_tags: Vec<&'static str>, // ["sanity", "mate_gate", "postverify"] etc.
    pub mate_gate_blocked: bool,
    pub postverify_reject: bool,
    // reserved for future checks (removed to keep tests minimal)
}

#[cfg(test)]
thread_local! {
    static TEST_FINALIZE_OUTCOME: std::cell::RefCell<Option<FinalizeOutcome>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn test_probe_record(outcome: FinalizeOutcome) {
    TEST_FINALIZE_OUTCOME.with(|s| *s.borrow_mut() = Some(outcome));
}

#[cfg(test)]
pub fn test_probe_reset() {
    TEST_FINALIZE_OUTCOME.with(|s| *s.borrow_mut() = None);
}

#[cfg(test)]
pub fn test_probe_take() -> Option<FinalizeOutcome> {
    TEST_FINALIZE_OUTCOME.with(|s| s.borrow_mut().take())
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
    // Optional: if only-one-legal-move at root, emit 1 info line with quick eval
    maybe_emit_forced_eval_info(state, final_move);
    diag_info_string(format!(
        "{}_select source={} move={} soft_ms={} hard_ms={}",
        label,
        source_to_str(source),
        final_move,
        stop_meta.soft_ms,
        stop_meta.hard_ms
    ));
    // Emit micro-log before sending bestmove for easier correlation with finalize_event
    let sid = state.current_session_core_id.unwrap_or(0);
    info_string(format!(
        "finalize_emit=1 sid={} label={} source={}",
        sid,
        label,
        source_to_str(source)
    ));
    let _ = emit_bestmove_once(state, final_move.to_string(), ponder);
}

fn maybe_emit_forced_eval_info(state: &mut EngineState, final_move_usi: &str) {
    if !state.opts.forced_move_emit_eval {
        return;
    }
    if state.opts.forced_move_min_search_ms == 0 {
        return;
    }
    // Count legal moves at root
    let mg = MoveGenerator::new();
    let Ok(list) = mg.generate_all(&state.position) else {
        return;
    };
    let mut legal_count = 0usize;
    for &m in list.as_slice() {
        if state.position.is_legal_move(m) {
            legal_count += 1;
            if legal_count > 1 {
                break;
            }
        }
    }
    if legal_count != 1 {
        return; // not a forced move
    }
    // Parse final move and run a tiny search on the child for a quick score line
    let Ok(mv) = parse_usi_move(final_move_usi) else {
        return;
    };
    if !state.position.is_legal_move(mv) {
        return;
    }
    let mut pos1 = state.position.clone();
    pos1.do_move(mv);
    // Use minimal fixed time and depth=1 for speed
    let ms = state.opts.forced_move_min_search_ms.min(50);
    let limits = engine_core::search::SearchLimits::builder().depth(1).fixed_time_ms(ms).build();
    let start = Instant::now();
    // 極小予算で try-lock。失敗時は info を諦める（bestmove を遅らせない）。
    let Some((mut eng, _spent_ms, _spent_us)) = try_lock_engine_with_budget(&state.engine, 3)
    else {
        diag_info_string("forced_eval_skip=1 reason=lock_unavailable");
        return;
    };
    let res = eng.search(&mut pos1, limits);
    drop(eng);
    let forced_eval_ms = start.elapsed().as_millis() as u64;
    // Emit a single info line with score/time/nodes and PV rooted at the final move
    let nps = if res.stats.elapsed.as_millis() > 0 {
        (res.stats.nodes as u128)
            .saturating_mul(1000)
            .saturating_div(res.stats.elapsed.as_millis())
    } else {
        0
    };
    // ルート視点にスコア/バウンドを正規化
    let (score_for_root, bound_for_root) =
        normalize_for_root_with_bound(state.position.side_to_move, &pos1, res.score, res.node_type);

    let final_best_stub = FinalBest {
        best_move: Some(mv),
        pv: vec![mv],
        source: FinalBestSource::Committed,
    };
    // hashfullは不明なので0を渡す（表示のみ）。score/bound をroot視点で上書き。
    emit_single_pv(&res, &final_best_stub, nps, 0, Some(score_for_root), Some(bound_for_root));
    diag_info_string(format!("forced_eval_ms={} depth=1 nps={}", forced_eval_ms, nps));
}

#[inline]
fn normalize_for_root(
    root_side: engine_core::Color,
    pos_after: &engine_core::shogi::Position,
    score: i32,
) -> i32 {
    if pos_after.side_to_move != root_side {
        // 子局面の手番がルートと反転しているなら符号を反転してルート視点に合わせる
        score.saturating_neg()
    } else {
        score
    }
}

#[inline]
fn normalize_for_root_with_bound(
    root_side: engine_core::Color,
    pos_after: &engine_core::shogi::Position,
    score: i32,
    bound: engine_core::search::types::NodeType,
) -> (i32, engine_core::search::types::NodeType) {
    use engine_core::search::types::NodeType::{Exact, LowerBound, UpperBound};
    if pos_after.side_to_move != root_side {
        let flipped = match bound {
            LowerBound => UpperBound,
            UpperBound => LowerBound,
            Exact => Exact,
        };
        (score.saturating_neg(), flipped)
    } else {
        (score, bound)
    }
}

fn finalize_sanity_check(
    state: &mut EngineState,
    stop_meta: &StopMeta,
    final_best: &FinalBest,
    result: Option<&SearchResult>,
    score_hint: Option<i32>,
    path_label: &str,
) -> Option<engine_core::shogi::Move> {
    if !state.opts.finalize_sanity_enabled {
        info_string("sanity_skipped=1 reason=disabled");
        return None;
    }
    // 時間ゲート: ハード締切までの残りで判断する（ソフト超過でも、ハードに十分余裕があれば最小検証を実施）
    // StopInfo の有無と「期限が未知か」を先に判定
    let limits_unknown = stop_meta
        .info
        .as_ref()
        .map(|si| si.hard_limit_ms == 0 && si.soft_limit_ms == 0)
        .unwrap_or(true);
    if let Some(si) = stop_meta.info.as_ref() {
        if !limits_unknown {
            let remain_hard = si.hard_limit_ms.saturating_sub(si.elapsed_ms);
            if si.hard_timeout || remain_hard <= state.opts.finalize_sanity_min_ms {
                info_string("sanity_skipped=1 reason=tm_hard");
                return None;
            }
        }
    }
    // 方針A: 王手中でもミニ検証を行う（過大評価抑制）。ログのみ付与。
    let in_check_now = state.position.is_in_check();
    if in_check_now {
        diag_info_string("sanity_in_check=1");
    }
    let pv1 = final_best.best_move?;
    // 以前は「PV1が王手ならFinalizeSanityをスキップ」していたが、
    // 対称性のためスキップは行わず、後段で微小ペナルティを適用する。
    // SEE gate (own move) + Opponent capture SEE gate after PV1
    let see = state.position.see(pv1);
    let see_min = state.opts.finalize_sanity_see_min_cp;
    let mut need_verify = see < see_min;

    // If own SEE is fine (non-negative or above threshold), still guard
    // against immediate opponent tactical shots after PV1.
    // Compute max opponent capture SEE in child position and trigger
    // mini verification if it exceeds configured threshold.
    // After PV1: check immediate capture threat and approximate 2-ply threat (quiet->capture)
    let mut opp_cap_see_max = 0;
    let mut opp_threat2_max = 0;
    let opp_gate = state.opts.finalize_sanity_opp_see_min_cp.max(0);
    let threat2_gate = state.opts.finalize_threat2_min_cp.max(0);
    let mut pos1 = state.position.clone();
    pos1.do_move(pv1);
    let mg2 = MoveGenerator::new();
    if let Ok(list2) = mg2.generate_all(&pos1) {
        // immediate captures
        for &mv in list2.as_slice() {
            if mv.is_capture_hint() && pos1.is_legal_move(mv) {
                let g = pos1.see(mv);
                if g > opp_cap_see_max {
                    opp_cap_see_max = g;
                }
            }
        }
        // approximate two-ply threat: opponent quiet (beam-limited) then opponent capture (after our null)
        let beam_k = state.opts.finalize_threat2_beam_k.max(1) as usize;
        // Quiet候補だけを集め、簡易優先度で上位Kを選抜
        let mut quiets: Vec<_> = list2
            .as_slice()
            .iter()
            .copied()
            .filter(|m| !m.is_capture_hint() && pos1.is_legal_move(*m))
            .collect();
        // 簡易優先度: 王手>成り>SEE（軽量）。SEEの寄与を少し持ち上げてビームの安定性を上げる。
        const THREAT2_PROMO_DELTA: i32 = 300; // 小さめの昇格寄与（T8の微補強用）
        quiets.sort_by_key(|m| {
            let mut key = 0i32;
            if pos1.gives_check(*m) {
                key += 2000;
            }
            if m.is_promote() {
                key += 1000 + THREAT2_PROMO_DELTA;
            }
            // 近似：PV1適用後局面でのSEE（正）を強めに持ち上げる
            key += 5 * pos1.see(*m).max(0);
            Reverse(key)
        });
        for &mvq in quiets.iter().take(beam_k) {
            let mut posq = pos1.clone();
            posq.do_move(mvq);
            // quietで王手になっている場合は Threat2 対象外（null moveの意味論と揃える）
            if posq.is_in_check() {
                continue;
            }
            // give move back to opponent by a null move, then evaluate opponent captures
            let undo_null = posq.do_null_move();
            if let Ok(listc) = mg2.generate_all(&posq) {
                for &mvc in listc.as_slice() {
                    if mvc.is_capture_hint() && posq.is_legal_move(mvc) {
                        let g2 = posq.see(mvc);
                        if g2 > opp_threat2_max {
                            opp_threat2_max = g2;
                        }
                    }
                }
            }
            posq.undo_null_move(undo_null);
        }
    }
    // Threat2/opp_cap の両方がゲート超時のみ検証（AND 判定）
    if need_verify_from_risks(opp_cap_see_max, opp_threat2_max, opp_gate, threat2_gate) {
        need_verify = true;
    }
    // 極端なThreat2は例外的にSanityを起動（OR）。ただし勝勢帯では無効化
    if opp_threat2_max >= state.opts.finalize_threat2_extreme_min_cp.max(0) {
        let win_disable_cp = state.opts.finalize_threat2_extreme_win_disable_cp.max(0);
        let win_hint_ok = score_hint.map(|s| s >= win_disable_cp).unwrap_or(false);
        if !win_hint_ok {
            need_verify = true;
        }
    }

    // near-draw は現状ログ可視化のみ（±NEAR_DRAW_CP 近傍を簡易判定）。
    let near_draw = score_hint.map(|s| s.abs() <= NEAR_DRAW_CP).unwrap_or(false);
    // 既存のコメントどおり、将来的に alt/King 判定へ活用する場合はここで扱う。
    let pv1_is_king = is_king_move(&state.position, &pv1);

    // stop_finalize 等で期限が不明(unknown)のときは、低リスクならミニ検証を省略する。
    // 高リスクの定義: need_verify=1（SEE/Threat2）、または PV1 が王手、または PV1 が玉手。
    if limits_unknown && !(need_verify || state.position.gives_check(pv1) || pv1_is_king) {
        info_string(format!("sanity_skipped=1 path={} reason=tm_unknown_low_risk", path_label));
        return None;
    }

    let see_min_dbg = state.opts.finalize_sanity_see_min_cp;
    let opp_gate_dbg = state.opts.finalize_sanity_opp_see_min_cp.max(0);
    let chk_penalty_dbg = state.opts.finalize_sanity_check_penalty_cp;
    let diag_base = format!(
        "sanity_checked=1 path={} see={} see_min={} opp_cap_see_max={} opp_gate={} opp_threat2_max={} threat2_gate={} check_penalty_cp={} need_verify={} near_draw={}",
        path_label,
        see,
        see_min_dbg,
        opp_cap_see_max,
        opp_gate_dbg,
        opp_threat2_max,
        threat2_gate,
        chk_penalty_dbg,
        need_verify as u8,
        near_draw as u8
    );
    // SEE/T2安全でも near-draw/alt 例外のためにこの段階では早期 return しない
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
                l2.pv.first().copied().filter(|m| !m.equals_without_piece_type(&pv1))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };
    // Special-case: PV1が「非成り」かつ同一手の成りが合法なら、まず成り手を代替候補にする（歩/R/B対応）
    if best_alt.is_none() {
        if let Some(from_sq) = pv1.from() {
            if let Some(piece) = state.position.board.piece_on(from_sq) {
                if !pv1.is_promote()
                    && (piece.piece_type == engine_core::shogi::PieceType::Pawn
                        || matches!(
                            piece.piece_type,
                            engine_core::shogi::PieceType::Rook
                                | engine_core::shogi::PieceType::Bishop
                        ))
                {
                    // 探索生成から“同一from/toでis_promote=true”の合法手を探す
                    let mgp = MoveGenerator::new();
                    if let Ok(listp) = mgp.generate_all(&state.position) {
                        if let Some(mv_p) = listp.as_slice().iter().copied().find(|m| {
                            m.is_promote()
                                && m.equals_without_piece_type(&pv1)
                                && state.position.is_legal_move(*m)
                        }) {
                            best_alt = Some(mv_p);
                        }
                    }
                }
            }
        }
    }
    let mut alt_from_pv2 = false;
    // PV2 候補の合法性確認（擬似合法の可能性を排除）
    // Validate PV2 candidate legality to prevent pseudo-legal moves from being selected
    let mut pv2_illegal = false;
    if let Some(mv0) = best_alt {
        if !state.position.is_legal_move(mv0) {
            info_string(format!("sanity_pv2_illegal=1 path={} fallback=see_best", path_label));
            best_alt = None; // Fallback to SEE-best candidate
            pv2_illegal = true;
        } else if mv0.equals_without_piece_type(&pv1) {
            best_alt = None; // 同一手は除外
        } else if let (Some(res), Some(mv0)) = (result, best_alt) {
            if let Some(lines) = &res.lines {
                if let Some(l2) = lines.iter().find(|l| l.multipv_index == 2) {
                    // USI表現一致も併用して防振
                    let usi0 = move_to_usi(&mv0);
                    if l2.pv.first().is_some_and(|m| {
                        m.equals_without_piece_type(&mv0) || move_to_usi(m) == usi0
                    }) {
                        alt_from_pv2 = true;
                    }
                }
            }
        }
    }
    // SEE>=0 を最優先、それが無ければ最大SEEを採用
    let mut best_ge0: Option<(engine_core::shogi::Move, i32)> = None;
    let mut best_any: Option<(engine_core::shogi::Move, i32)> = None;
    let mut best_any_nonking_floor: Option<(engine_core::shogi::Move, i32)> = None;
    for &mv in list.as_slice() {
        if Some(mv) == final_best.best_move {
            continue;
        }
        if !state.position.is_legal_move(mv) {
            continue;
        }
        // PV1 と同一手（駒種無視）は除外（PV2と同じ基準で統一）
        if mv.equals_without_piece_type(&pv1) {
            continue;
        }
        let s = state.position.see(mv);
        if best_alt.is_none() {
            if s >= 0 {
                best_ge0 = match best_ge0 {
                    Some((m, v)) if v >= s => Some((m, v)),
                    _ => Some((mv, s)),
                };
            } else if best_ge0.is_some() {
                // SEE>=0候補が確定していれば負値候補はスキップ
                continue;
            }
            best_any = match best_any {
                Some((m, v)) if v >= s => Some((m, v)),
                _ => Some((mv, s)),
            };
            // 非玉かつ“防御用の小幅負値”を許容するためのフロア（need_verify時のみ）
            if need_verify
                && s >= state.opts.finalize_defense_see_neg_floor_cp
                && !is_king_move(&state.position, &mv)
            {
                best_any_nonking_floor = match best_any_nonking_floor {
                    Some((m, v)) if v >= s => Some((m, v)),
                    _ => Some((mv, s)),
                };
            }
        }
    }
    if best_alt.is_none() {
        // SEE>=0 を優先。許可フラグが false の場合は負値代替を禁止。
        if let Some((m, _)) = best_ge0 {
            best_alt = Some(m);
        } else if need_verify {
            // 高リスク時は“受け”の小幅負値（フロア基準）を非玉優先で許容
            if let Some((m, _)) = best_any_nonking_floor {
                info_string(format!(
                    "sanity_defense_negfloor_used=1 floor={}",
                    state.opts.finalize_defense_see_neg_floor_cp
                ));
                best_alt = Some(m);
            } else if state.opts.finalize_allow_see_lt0_alt {
                best_alt = best_any.map(|(m, _)| m);
            }
        }
    }
    let Some(alt) = best_alt else {
        info_string(format!("{} switched=0 reason=no_alt", diag_base));
        return None;
    };
    // Budget（動的化）: remain/32 を基準に 2..10ms にクランプし、USIオプションの上限で抑制
    let budget_max = state.opts.finalize_sanity_budget_ms;
    let dynamic_budget = if let Some(si) = stop_meta.info.as_ref() {
        let mut limit = u64::MAX;
        if si.soft_limit_ms > 0 {
            limit = limit.min(si.soft_limit_ms);
        }
        if si.hard_limit_ms > 0 {
            limit = limit.min(si.hard_limit_ms);
        }
        if limit == u64::MAX || limit == 0 || si.elapsed_ms >= limit {
            0
        } else {
            let remain = limit - si.elapsed_ms;
            let mut b = remain / 32;
            b = b.clamp(2, 10);
            b
        }
    } else {
        0
    };
    // Ensure minimum budget even when StopInfo has no deadlines (e.g., ponder finalize)
    let fallback_min = state.opts.finalize_sanity_min_ms;
    let desired = if dynamic_budget == 0 {
        fallback_min
    } else {
        dynamic_budget
    };
    let mut total_budget = desired.max(fallback_min);
    if budget_max > 0 {
        total_budget = total_budget.min(budget_max);
    }
    // 低リスク・ゲート: fast経路で、危険なし(need_verify=0) かつ PV1が玉手でない場合、
    // StopInfoが無くMinMsのみの実行(dynamic_budget=0)に限ってミニ検証を省略する。
    // 追加ガード: pv1が王手になるときはスキップせず軽検証を行う（浅い王手バイアス抑制）
    if path_label == "fast"
        && dynamic_budget == 0
        && !need_verify
        && !pv1_is_king
        && !state.position.gives_check(pv1)
    {
        info_string(format!(
            "{} alt={} switched=0 reason=no_need_verify",
            diag_base,
            move_to_usi(&alt)
        ));
        return None;
    }
    if total_budget == 0 {
        info_string(format!(
            "sanity_skipped=1 reason=no_budget opp_cap_see_max={} opp_threat2_max={}",
            opp_cap_see_max, opp_threat2_max
        ));
        return None;
    }
    // Mini search: PV1=玉・非チェック時のみ局所的に深さを強める（MiniDepth>=3）
    let base_mini = state.opts.finalize_sanity_mini_depth.max(1);
    let mini_depth = if pv1_is_king && !in_check_now {
        base_mini.max(3)
    } else {
        base_mini
    };
    let switch_margin = state.opts.finalize_sanity_switch_margin_cp;
    let (s1_temp, s2_raw, pv1_check_flag, alt_check_flag) = {
        let (mv1, mv2) = (pv1, alt);
        if let Some((mut eng, spent_ms, _)) =
            try_lock_engine_with_budget(&state.engine, total_budget)
        {
            // 予算厳守: ロックに消費した時間を差し引く
            let remain_budget = total_budget.saturating_sub(spent_ms);
            if spent_ms > (total_budget / 2) {
                info_string(format!(
                    "sanity_lock_heavy=1 spent_ms={} total_ms={}",
                    spent_ms, total_budget
                ));
            }
            if remain_budget == 0 {
                info_string(format!(
                    "sanity_skipped=1 reason=lock_spent_budget total_ms={} spent_ms={}",
                    total_budget, spent_ms
                ));
                return None;
            }
            // 2回合計がremain_budgetを超えないよう逐次分配
            let per1 = remain_budget / 2;
            let per2 = remain_budget - per1;

            // Evaluate child of pv1
            let mut pos1 = state.position.clone();
            pos1.do_move(mv1);
            let mut s1_local = if per1 > 0 {
                eng.search(
                    &mut pos1,
                    engine_core::search::SearchLimits::builder()
                        .depth(mini_depth)
                        .fixed_time_ms(per1)
                        .build(),
                )
                .score
            } else {
                0
            };
            // PV1 がチェックなら微減点（浅い読みの“王手バイアス”抑制）
            let check_penalty_cp = state.opts.finalize_sanity_check_penalty_cp.max(0);
            let pv1_check = state.position.gives_check(mv1);
            if pv1_check {
                s1_local = s1_local.saturating_sub(check_penalty_cp);
            }

            // Evaluate child of alt
            let mut pos2 = state.position.clone();
            pos2.do_move(mv2);
            let mut s2_local = if per2 > 0 {
                eng.search(
                    &mut pos2,
                    engine_core::search::SearchLimits::builder()
                        .depth(mini_depth)
                        .fixed_time_ms(per2)
                        .build(),
                )
                .score
            } else {
                0
            };
            // alt がチェックなら微減点（浅い読みの過大評価抑制）
            let alt_check = state.position.gives_check(mv2);
            if alt_check {
                s2_local = s2_local.saturating_sub(check_penalty_cp);
            }
            // 子局面スコアをルート視点に正規化
            let root_side = state.position.side_to_move;
            let s1_root = normalize_for_root(root_side, &pos1, s1_local);
            let s2_root = normalize_for_root(root_side, &pos2, s2_local);
            (s1_root, s2_root, pv1_check, alt_check)
        } else {
            info_string("sanity_skipped=1 reason=lock_failed");
            return None;
        }
    };
    let s1 = s1_temp;
    // 相手最大捕獲SEEに応じてPV1側スコアへ軽いペナルティを付加（過大評価抑制）
    const OPP_SEE_PENALTY_NUM: i32 = 1; // λ=0.5（分数表現）
    const OPP_SEE_PENALTY_DEN: i32 = 2;
    let penalty_cap = state.opts.finalize_sanity_opp_see_penalty_cap_cp.max(0);
    let mut opp_penalty = 0i32;
    if opp_cap_see_max >= opp_gate_dbg && penalty_cap > 0 {
        let capped = opp_cap_see_max.min(penalty_cap).max(0) as i64;
        let p_i64 =
            capped.saturating_mul(OPP_SEE_PENALTY_NUM as i64) / (OPP_SEE_PENALTY_DEN as i64);
        let mut p = p_i64.clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        if s1 >= 200 {
            p /= 2; // 優勢帯では半減
        }
        opp_penalty = p;
    }
    let mut s1_adj = s1.saturating_sub(opp_penalty);
    // 非成り抑制: PV1が「歩/飛/角」の非成りで、同一手の成りが合法なら軽いペナルティ
    const NONPROMOTE_PAWN_PENALTY_CP: i32 = 200;
    let nonpromote_major_pen = state.opts.finalize_non_promote_major_penalty_cp.clamp(0, 300);
    if let Some(from_sq) = pv1.from() {
        if let Some(piece) = state.position.board.piece_on(from_sq) {
            if !pv1.is_promote() {
                let exists = list.as_slice().iter().copied().any(|m| {
                    m.is_promote()
                        && m.equals_without_piece_type(&pv1)
                        && state.position.is_legal_move(m)
                });
                if exists {
                    if piece.piece_type == engine_core::shogi::PieceType::Pawn {
                        s1_adj = s1_adj.saturating_sub(NONPROMOTE_PAWN_PENALTY_CP);
                    } else if matches!(
                        piece.piece_type,
                        engine_core::shogi::PieceType::Rook | engine_core::shogi::PieceType::Bishop
                    ) {
                        s1_adj = s1_adj.saturating_sub(nonpromote_major_pen);
                    }
                }
            }
        }
    }
    // 余裕時にだけ相手番の軽いMateProbe（短手）を実行（任意の保険）。
    // 条件: 設定ON かつ s1_adj >= +200 かつ 残ハード >= time_ms
    if state.opts.finalize_mate_probe_enabled && s1_adj >= 200 {
        let remain_hard = stop_meta
            .info
            .as_ref()
            .map(|si| si.hard_limit_ms.saturating_sub(si.elapsed_ms))
            .unwrap_or(0);
        let probe_ms = state.opts.finalize_mate_probe_time_ms.min(20);
        if remain_hard >= probe_ms && probe_ms > 0 {
            if let Some((mut eng, _, _)) = try_lock_engine_with_budget(&state.engine, probe_ms) {
                let mut pos_probe = state.position.clone();
                pos_probe.do_move(pv1);
                let limits = engine_core::search::SearchLimits::builder()
                    .depth(state.opts.finalize_mate_probe_depth.max(1))
                    .fixed_time_ms(probe_ms)
                    .build();
                let resp = eng.search(&mut pos_probe, limits);
                drop(eng);
                let found = is_mate_score(resp.score) && resp.score < 0; // 相手番の有利な詰み傾向
                info_string(format!(
                    "mate_probe=1 depth={} time_ms={} found={} score={}",
                    state.opts.finalize_mate_probe_depth, probe_ms, found as u8, resp.score
                ));
                if found {
                    // 軽い減点でaltに傾ける（SwitchMarginの中で判定）
                    s1_adj = s1_adj.saturating_sub(300);
                }
            }
        }
    }
    // --- King-alt guard (non-check): 非チェック時に玉の手への切替を原則禁止 ---
    let alt_is_king = is_king_move(&state.position, &alt);
    let mut s2_eff = s2_raw;
    // alt 側が非成り（歩/飛/角）で同一成りが合法なら軽ペナルティ
    if let Some(from_sq) = alt.from() {
        if let Some(piece) = state.position.board.piece_on(from_sq) {
            if !alt.is_promote() {
                let mgp = MoveGenerator::new();
                if let Ok(listp) = mgp.generate_all(&state.position) {
                    let exists = listp.as_slice().iter().copied().any(|m| {
                        m.is_promote()
                            && m.equals_without_piece_type(&alt)
                            && state.position.is_legal_move(m)
                    });
                    if exists {
                        if piece.piece_type == engine_core::shogi::PieceType::Pawn {
                            s2_eff = s2_eff.saturating_sub(NONPROMOTE_PAWN_PENALTY_CP);
                        } else if matches!(
                            piece.piece_type,
                            engine_core::shogi::PieceType::Rook
                                | engine_core::shogi::PieceType::Bishop
                        ) {
                            let pen =
                                state.opts.finalize_non_promote_major_penalty_cp.clamp(0, 300);
                            s2_eff = s2_eff.saturating_sub(pen);
                        }
                    }
                }
            }
        }
    }
    if alt_is_king && !in_check_now {
        let kap = state.opts.finalize_sanity_king_alt_penalty_cp.max(0);
        if kap > 0 {
            s2_eff = s2_eff.saturating_sub(kap);
        }
        let min_gain = state.opts.finalize_sanity_king_alt_min_gain_cp.max(0);
        let gain = s2_eff.saturating_sub(s1_adj);
        if gain < min_gain && (opp_cap_see_max >= opp_gate_dbg || opp_threat2_max >= threat2_gate) {
            // near-draw 例外: ここに来た場合は near-draw でも King-alt の判断を実施している。
            info_string(format!(
                "sanity_checked=1 path={} see={} see_min={} opp_cap_see_max={} opp_gate={} opp_threat2_max={} threat2_gate={} check_penalty_cp={}",
                path_label,
                see,
                see_min_dbg,
                opp_cap_see_max,
                opp_gate_dbg,
                opp_threat2_max,
                threat2_gate,
                chk_penalty_dbg
            ));
            info_string(format!(
                "sanity_king_alt_blocked=1 path={} alt={} s1_adj={} s2={} s2_eff={} gain={} min_gain={}",
                path_label,
                move_to_usi(&alt),
                s1_adj,
                s2_raw,
                s2_eff,
                gain,
                min_gain
            ));
            // 非玉の代替を再探索（SEE>=0優先→任意）。見つかれば差し替え。
            let mg = MoveGenerator::new();
            if let Ok(list) = mg.generate_all(&state.position) {
                let pos = &state.position;
                let legal_nonking: Vec<_> = list
                    .as_slice()
                    .iter()
                    .copied()
                    .filter(|&m| pos.is_legal_move(m))
                    .filter(|&m| !is_king_move(pos, &m))
                    .filter(|&m| !m.equals_without_piece_type(&pv1))
                    .collect();
                // AllowSEElt0Alt を遵守
                let allow_neg = state.opts.finalize_allow_see_lt0_alt;
                if let Some(alt2) =
                    choose_legal_fallback_with_see_filtered(pos, &legal_nonking, allow_neg)
                {
                    info_string(format!(
                        "sanity_alt_reselect_nonking=1 path={} new_alt={}",
                        path_label,
                        move_to_usi(&alt2)
                    ));
                    return Some(alt2);
                }
                // 最終安全弁: 高リスクかつ非玉代替が見つからない場合は no_publish=1（安全手クラスへ強制）。
                if opp_cap_see_max >= opp_gate_dbg && opp_threat2_max >= threat2_gate {
                    if let Some(safe_alt) =
                        choose_safe_nonking_fallback(pos, &legal_nonking, allow_neg)
                    {
                        info_string(format!(
                            "no_publish=1 path={} reason=sanity_risk_no_alt alt_class=safe_nonking new_alt={}",
                            path_label,
                            move_to_usi(&safe_alt)
                        ));
                        return Some(safe_alt);
                    }
                }
            }
            return None;
        } else {
            info_string(format!(
                "sanity_king_alt_allowed=1 path={} alt={} s1_adj={} s2={} s2_eff={} gain={} min_gain={}",
                path_label,
                move_to_usi(&alt),
                s1_adj,
                s2_raw,
                s2_eff,
                gain,
                min_gain
            ));
        }
    }

    // PV1=玉手のガード（非チェック時）: PV1が玉手で、切替メリットが小さい場合は非玉へ切替を試みる
    if pv1_is_king && !in_check_now {
        let min_gain = state.opts.finalize_sanity_king_alt_min_gain_cp.max(0);
        let gain_keep = s1_adj.saturating_sub(s2_eff); // keep側の余裕（s2の方が良いなら負になる）
        if gain_keep < min_gain
            && (opp_cap_see_max >= opp_gate_dbg || opp_threat2_max >= threat2_gate)
        {
            info_string(format!(
                "sanity_pv1_is_king=1 s1_adj={} s2={} keep_gain={} min_gain={}",
                s1_adj, s2_eff, gain_keep, min_gain
            ));
            // 既に alt を評価済み。非玉であればこのまま切替、玉なら再選択。
            if !alt_is_king {
                return Some(alt);
            } else {
                let mg = MoveGenerator::new();
                if let Ok(list) = mg.generate_all(&state.position) {
                    let pos = &state.position;
                    let legal_nonking: Vec<_> = list
                        .as_slice()
                        .iter()
                        .copied()
                        .filter(|&m| pos.is_legal_move(m))
                        .filter(|&m| !is_king_move(pos, &m))
                        .filter(|&m| !m.equals_without_piece_type(&pv1))
                        .collect();
                    let allow_neg = state.opts.finalize_allow_see_lt0_alt;
                    // まずは小幅のSEE負（>= -120cp）まで暫定許容して非玉代替を探す
                    const SEE_NEG_FLOOR: i32 = -120;
                    let mut best_any: Option<(engine_core::shogi::Move, i32)> = None;
                    for &m in &legal_nonking {
                        let s = pos.see(m);
                        if s >= SEE_NEG_FLOOR {
                            best_any = match best_any {
                                Some((bm, bs)) if bs >= s => Some((bm, bs)),
                                _ => Some((m, s)),
                            };
                        }
                    }
                    let mut alt2 = best_any.map(|(m, _)| m);
                    // 閾値内になければ従来ポリシー（SEE>=0のみ or 設定ONで負も可）
                    if alt2.is_none() {
                        alt2 =
                            choose_legal_fallback_with_see_filtered(pos, &legal_nonking, allow_neg);
                    }
                    if let Some(alt2) = alt2 {
                        info_string(format!(
                            "sanity_pv1_king_alt_reselect_nonking=1 new_alt={}",
                            move_to_usi(&alt2)
                        ));
                        return Some(alt2);
                    }
                    if opp_cap_see_max >= opp_gate_dbg && opp_threat2_max >= threat2_gate {
                        if let Some(safe_alt) =
                            choose_safe_nonking_fallback(pos, &legal_nonking, allow_neg)
                        {
                            info_string(format!(
                                "no_publish=1 path={} reason=sanity_pv1_king_risk_no_alt alt_class=safe_nonking new_alt={}",
                                path_label,
                                move_to_usi(&safe_alt)
                            ));
                            return Some(safe_alt);
                        }
                    }
                }
            }
        }
    }
    let would_switch = s2_eff > s1_adj + switch_margin;
    // 置換前後のリスク比較（alt採用前チェック）
    let mut switched = false;
    if would_switch {
        let risk_before = opp_cap_see_max.max(opp_threat2_max);
        let (opp_cap_after, opp_t2_after) = compute_risks_after_move(
            &state.position,
            alt,
            state.opts.finalize_threat2_beam_k as usize,
        );
        let risk_after = opp_cap_after.max(opp_t2_after);
        let delta = risk_before.saturating_sub(risk_after);
        let gate_ok = opp_cap_after < opp_gate_dbg && opp_t2_after < threat2_gate;
        let delta_ok = delta >= state.opts.finalize_risk_min_delta_cp.max(0);
        switched = gate_ok || delta_ok;
        if !switched {
            info_string(format!(
                "{} alt={} s1={} s1_adj={} s2={} margin={} switched=0 lambda={}/{} pv1_check={} alt_check={} opp_cap={} opp_t2={} risk_before={} risk_after={} risk_drop={} reason=no_risk_drop",
                diag_base,
                move_to_usi(&alt),
                s1,
                s1_adj,
                s2_eff,
                switch_margin,
                OPP_SEE_PENALTY_NUM,
                OPP_SEE_PENALTY_DEN,
                pv1_check_flag as i32,
                alt_check_flag as i32,
                opp_cap_see_max,
                opp_threat2_max,
                risk_before,
                risk_after,
                delta
            ));
        } else {
            info_string(format!(
                "{} alt={} s1={} s1_adj={} s2={} margin={} switched=1 origin={} total_budget_ms={} lambda={}/{} pv1_check={} alt_check={} opp_cap={} opp_t2={} risk_before={} risk_after={} risk_drop={}",
                diag_base,
                move_to_usi(&alt),
                s1,
                s1_adj,
                s2_eff,
                switch_margin,
                if alt_from_pv2 { "pv2" } else if pv2_illegal { "pv2_illegal->see_best" } else { "see_best" },
                total_budget,
                OPP_SEE_PENALTY_NUM,
                OPP_SEE_PENALTY_DEN,
                pv1_check_flag as i32,
                alt_check_flag as i32,
                opp_cap_see_max,
                opp_threat2_max,
                risk_before,
                risk_after,
                delta
            ));
            // 差し替え確定時は軽い評価行を1本だけ出力してGUI乖離を軽減
            let mut line = String::from("info depth 1 score ");
            append_usi_score_and_bound(
                &mut line,
                engine_core::usi::ScoreView::Cp(s2_eff),
                NodeType::Exact,
            );
            line.push_str(" pv ");
            line.push_str(&move_to_usi(&alt));
            usi_println(&line);
        }
    }
    if !switched {
        info_string(format!(
            "{} alt={} s1={} s1_adj={} s2={} margin={} switched=0 lambda={}/{} pv1_check={} alt_check={} opp_cap={} opp_t2={}",
            diag_base,
            move_to_usi(&alt),
            s1,
            s1_adj,
            s2_eff,
            switch_margin,
            OPP_SEE_PENALTY_NUM,
            OPP_SEE_PENALTY_DEN,
            pv1_check_flag as i32,
            alt_check_flag as i32,
            opp_cap_see_max,
            opp_threat2_max,
        ));
    }
    if switched {
        Some(alt)
    } else {
        None
    }
}

fn compute_risks_after_move(
    pos: &engine_core::shogi::Position,
    mv: engine_core::shogi::Move,
    beam_k: usize,
) -> (i32, i32) {
    let mut pos1 = pos.clone();
    pos1.do_move(mv);
    let mg = MoveGenerator::new();
    let mut opp_cap_see_max = 0;
    let mut opp_threat2_max = 0;
    if let Ok(list2) = mg.generate_all(&pos1) {
        for &m in list2.as_slice() {
            if m.is_capture_hint() && pos1.is_legal_move(m) {
                let g = pos1.see(m);
                if g > opp_cap_see_max {
                    opp_cap_see_max = g;
                }
            }
        }
        // Threat2 approximation: quiet then capture after null
        let mut quiets: Vec<_> = list2
            .as_slice()
            .iter()
            .copied()
            .filter(|m| !m.is_capture_hint() && pos1.is_legal_move(*m))
            .collect();
        const THREAT2_PROMO_DELTA: i32 = 300;
        quiets.sort_by_key(|m| {
            let mut key = 0i32;
            if pos1.gives_check(*m) {
                key += 2000;
            }
            if m.is_promote() {
                key += 1000 + THREAT2_PROMO_DELTA;
            }
            key += 5 * pos1.see(*m).max(0);
            Reverse(key)
        });
        for &mq in quiets.iter().take(beam_k.max(1)) {
            let mut pq = pos1.clone();
            pq.do_move(mq);
            if pq.is_in_check() {
                continue;
            }
            let undo = pq.do_null_move();
            if let Ok(listc) = mg.generate_all(&pq) {
                for &mc in listc.as_slice() {
                    if mc.is_capture_hint() && pq.is_legal_move(mc) {
                        let g2 = pq.see(mc);
                        if g2 > opp_threat2_max {
                            opp_threat2_max = g2;
                        }
                    }
                }
            }
            pq.undo_null_move(undo);
        }
    }
    (opp_cap_see_max, opp_threat2_max)
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
        stop_tag: src.stop_tag.clone(),
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
    if remain <= 3 {
        diag_info_string("computed_tt_budget_ms=0 reason=remain_le_3");
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
        // ログ: スピン0で取得（静かなケース）
        diag_info_string(format!("engine_lock_spins=0 budget_ms={}", budget_ms));
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
            // しきい値を超えるスピン/イールドが発生した場合のみ可視化
            if spins > 0 {
                info_string(format!("engine_lock_spins={} budget_ms={}", spins, budget_ms));
            }
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
            // 予算を保護: 2ms以上残る場合のみ sleep(1ms)。それ未満では yield に留める。
            if remaining >= Duration::from_millis(2) {
                std::thread::sleep(Duration::from_millis(1));
            } else {
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

    let mut pv_head_mismatch_flag = false;
    let committed = if let Some(res) = result {
        let mut committed_pv = res.stats.pv.clone();
        if let Some(bm) = res.best_move {
            // Use equals_without_piece_type to avoid false positives from piece type differences
            // MSRV互換のため map_or(true, ...) を使用
            let has_mismatch =
                committed_pv.first().is_none_or(|pv0| !pv0.equals_without_piece_type(&bm));
            if has_mismatch {
                pv_head_mismatch_flag = true;
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
    // If PV頭をbest_moveへ差し替えた場合に限り、info出力のscoreをStableスナップショットのcpへ上書き
    let info_score_override: Option<i32> = if pv_head_mismatch_flag {
        snapshot_committed.as_ref().map(|ci| ci.score)
    } else {
        None
    };
    let info_bound_override: Option<NodeType> = if pv_head_mismatch_flag {
        snapshot_committed.as_ref().map(|ci| ci.node_type)
    } else {
        None
    };
    let mut final_best = {
        let eng = state.lock_engine();
        if let Some(ci) = snapshot_committed.as_ref() {
            eng.choose_final_bestmove(&state.position, Some(ci))
        } else {
            eng.choose_final_bestmove(&state.position, committed.as_ref())
        }
    };

    // Mate gating (YO流): Lower/Upper の mate距離では確定扱いしない。安定条件不足も抑止。
    let mut mate_rejected = false;
    let mut mate_gate_blocked = false;
    let mut mate_post_reject = false;
    if let Some(res) = result.as_ref() {
        if is_mate_score(res.score) {
            let cfg = MateGateCfg {
                min_stable_depth: state.opts.mate_gate_min_stable_depth,
                fast_ok_min_depth: state.opts.mate_gate_fast_ok_min_depth,
                fast_ok_min_elapsed_ms: state.opts.mate_gate_fast_ok_min_elapsed_ms,
            };
            let block = mate_gate_should_block(
                res.node_type,
                res.stats.stable_depth,
                res.stats.depth,
                res.stats.elapsed.as_millis() as u64,
                &cfg,
            );
            if block {
                let dist = md(res.score).unwrap_or(0);
                info_string(format!(
                    "mate_gate_blocked=1 bound={} depth={} elapsed_ms={} mate_dist={}",
                    match res.node_type {
                        engine_core::search::types::NodeType::LowerBound => "lb",
                        engine_core::search::types::NodeType::UpperBound => "ub",
                        _ => "pv",
                    },
                    res.stats.depth,
                    res.stats.elapsed.as_millis(),
                    dist
                ));
                // 距離1はゲート免除（必ずCheckOnlyのpost-verifyを実施）
                if dist <= 1 {
                    mate_gate_blocked = false;
                    mate_rejected = false;
                } else {
                    mate_gate_blocked = true;
                    mate_rejected = true;
                }
            }
        }
    }

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

        // PostVerify for mate距離: bestを1手進めて“相手が詰み回避できない=応手0”かを軽確認。
        if state.opts.post_verify && is_mate_score(res.score) {
            if let Some(bm) = res.best_move {
                let mut pos1 = state.position.clone();
                let _ = pos1.do_move(bm);
                // 合法回避手が0かを厳密に確認する
                let mg = MoveGenerator::new();
                let mut has_evasion = false;
                if let Ok(list) = mg.generate_all(&pos1) {
                    for &mv in list.as_slice() {
                        if !pos1.is_legal_move(mv) {
                            continue;
                        }
                        let undo = pos1.do_move(mv);
                        let still = pos1.is_in_check();
                        pos1.undo_move(mv, undo);
                        if !still {
                            has_evasion = true;
                            break;
                        }
                    }
                }
                let forced_mate = pos1.is_in_check() && !has_evasion;
                if !forced_mate {
                    let dist = md(res.score).unwrap_or(0);
                    info_string(format!(
                        "mate_postverify_reject=1 evasion_exists={} mate_dist={}",
                        has_evasion as u8, dist
                    ));
                    mate_post_reject = true;
                    mate_rejected = true;
                }
            }
        }

        // 早期finalize連動: Post-VerifyがNGなら残りsoftで短時間延長→再判定し、必要なら最善再取得
        if mate_post_reject && state.opts.post_verify_require_pass {
            let remain_soft = stop_meta
                .info
                .as_ref()
                .map(|si| si.soft_limit_ms.saturating_sub(si.elapsed_ms))
                .unwrap_or(0);
            let ext_ms = remain_soft.min(state.opts.post_verify_extend_ms);
            if ext_ms >= 10 {
                info_string(format!(
                    "early_finalize_blocked=1 reason=postverify_reject extend_ms={} remain_soft_ms={}",
                    ext_ms, remain_soft
                ));
                let mut pos = state.position.clone();
                let limits = engine_core::search::SearchLimits::builder()
                    .depth(1)
                    .fixed_time_ms(ext_ms)
                    .build();
                if let Some((mut eng, _, _)) = try_lock_engine_with_budget(&state.engine, 3) {
                    let res2 = eng.search(&mut pos, limits);
                    drop(eng);
                    let mut reject_again = false;
                    if is_mate_score(res2.score) {
                        if let Some(bm2) = res2.best_move {
                            let mut pos1 = state.position.clone();
                            let _ = pos1.do_move(bm2);
                            // 合法回避が存在するか再確認
                            let mg = MoveGenerator::new();
                            let mut has_evasion2 = false;
                            if let Ok(list2) = mg.generate_all(&pos1) {
                                for &mv in list2.as_slice() {
                                    if !pos1.is_legal_move(mv) {
                                        continue;
                                    }
                                    let undo = pos1.do_move(mv);
                                    let still = pos1.is_in_check();
                                    pos1.undo_move(mv, undo);
                                    if !still {
                                        has_evasion2 = true;
                                        break;
                                    }
                                }
                            }
                            if has_evasion2 || !pos1.is_in_check() {
                                reject_again = true;
                            }
                        }
                    }
                    if !reject_again {
                        info_string("early_finalize_recheck_pass=1");
                        // TT更新後の最善を再取得
                        let eng2 = state.lock_engine();
                        final_best = eng2.choose_final_bestmove(&state.position, None);
                    } else {
                        info_string("early_finalize_recheck_fail=1");
                    }
                }
            }
        }

        // 劣勢/終盤帯のVerify強化（限定ON）: スコアが一定以下なら小延長でTTを温め最善を再取得
        if state.opts.post_verify {
            let disadv = res.score <= state.opts.post_verify_disadvantage_cp;
            if disadv {
                let remain_soft = stop_meta
                    .info
                    .as_ref()
                    .map(|si| si.soft_limit_ms.saturating_sub(si.elapsed_ms))
                    .unwrap_or(0);
                let ext_ms = (state.opts.post_verify_extend_ms / 2).max(10).min(remain_soft);
                if ext_ms >= 10 {
                    diag_info_string(format!(
                        "postverify_strengthen=1 ext_ms={} remain_soft_ms={}",
                        ext_ms, remain_soft
                    ));
                    let mut pos = state.position.clone();
                    let limits = engine_core::search::SearchLimits::builder()
                        .depth(1)
                        .fixed_time_ms(ext_ms)
                        .build();
                    if let Some((mut eng, _, _)) = try_lock_engine_with_budget(&state.engine, 3) {
                        let _ = eng.search(&mut pos, limits);
                        drop(eng);
                        // TT更新後の最善を再取得
                        let eng2 = state.lock_engine();
                        final_best = eng2.choose_final_bestmove(&state.position, None);
                    }
                }
            }
        }

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
                let eng = state.lock_engine();
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
                    let eng = state.lock_engine();
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
                    let eng = state.lock_engine();
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
                        // PV1（index==1）のみ、必要ならscore/boundをStable由来で上書き
                        let score_used = if index == 1 {
                            info_score_override.unwrap_or(line.score_internal)
                        } else {
                            line.score_internal
                        };
                        let bound_used = if index == 1 {
                            info_bound_override.unwrap_or(line.bound)
                        } else {
                            line.bound
                        };
                        let view = score_view_with_clamp(score_used);
                        append_usi_score_and_bound(&mut s, view, bound_used);
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
                    emit_single_pv(
                        res,
                        &final_best,
                        nps_agg,
                        hf_permille,
                        info_score_override,
                        info_bound_override,
                    );
                }
            } else {
                emit_single_pv(
                    res,
                    &final_best,
                    nps_agg,
                    hf_permille,
                    info_score_override,
                    info_bound_override,
                );
            }
        }
    }

    #[cfg(feature = "tt_metrics")]
    {
        let summary_opt = {
            let eng = state.lock_engine();
            eng.tt_metrics_summary()
        };
        if let Some(sum) = summary_opt {
            for line in sum.lines() {
                usi_println(&format!("info string tt_metrics {}", line));
            }
        }
    }

    // Optional finalize sanity check (may switch PV1)
    // joined 経路でも near_draw 可視化を揃えるため、score_hint を渡す
    let score_hint_joined = result.map(|r| r.score);
    let mut maybe_switch =
        finalize_sanity_check(state, &stop_meta, &final_best, result, score_hint_joined, "joined");

    // If mate was rejected, force an alternative: try PV2 head from snapshot, else SEE-best alt
    if mate_rejected {
        let pv1 = final_best.best_move;
        let mut forced_alt: Option<engine_core::shogi::Move> = None;
        if let Some(snap) = snapshot_valid.as_ref() {
            if let Some(line2) = snap.lines.get(1) {
                if let Some(&m2) = line2.pv.first() {
                    if pv1 != Some(m2) && state.position.is_legal_move(m2) {
                        forced_alt = Some(m2);
                    }
                }
            }
        }
        if forced_alt.is_none() {
            let mg = MoveGenerator::new();
            if let Ok(list) = mg.generate_all(&state.position) {
                let mut best_mv: Option<engine_core::shogi::Move> = None;
                let mut best_see = i32::MIN;
                for &mv in list.as_slice() {
                    if Some(mv) == pv1 {
                        continue;
                    }
                    let s = state.position.see(mv);
                    if s > best_see {
                        best_see = s;
                        best_mv = Some(mv);
                    }
                }
                forced_alt = best_mv;
            }
        }
        if let Some(alt) = forced_alt {
            info_string(format!("mate_switch=1 alt={}", move_to_usi(&alt)));
            maybe_switch = Some(alt);
            // no-op for test probe: PV2優先の有無は現状検査対象外
        }
    }
    let (chosen_mv, chosen_src) = if let Some(m) = maybe_switch {
        (Some(m), FinalBestSource::Committed)
    } else {
        (final_best.best_move, final_best.source)
    };

    let final_usi = chosen_mv.map(|m| move_to_usi(&m)).unwrap_or_else(|| "resign".to_string());
    let ponder_mv = if state.opts.ponder {
        if maybe_switch.is_some() {
            // スイッチ時は最終採用手（chosen_mv）基準でTTからのみ取得（PV1の2手目は不一致の恐れがある）
            chosen_mv.and_then(|bm| {
                let eng = state.lock_engine();
                eng.get_ponder_from_tt(&state.position, bm).map(|m| move_to_usi(&m))
            })
        } else {
            // スイッチなしなら従来通りPVの2手目を優先し、無ければTTから
            final_best.pv.get(1).map(move_to_usi).or_else(|| {
                chosen_mv.and_then(|bm| {
                    let eng = state.lock_engine();
                    eng.get_ponder_from_tt(&state.position, bm).map(|m| move_to_usi(&m))
                })
            })
        }
    } else {
        None
    };
    if maybe_switch.is_some() {
        // legacy reason (kept for compatibility)
        let reason = if mate_gate_blocked && mate_post_reject {
            "mate_gate+postverify"
        } else if mate_gate_blocked {
            "mate_gate"
        } else if mate_post_reject {
            "postverify"
        } else {
            "sanity"
        };
        // extensible tags for future coexistence (sanity + mate_gate/postverify)
        let mut tags: Vec<&str> = vec!["sanity"]; // switching implies sanity layer involved
        if mate_gate_blocked {
            tags.push("mate_gate");
        }
        if mate_post_reject {
            tags.push("postverify");
        }
        info_string(format!("final_select_tags tags={}", tags.join("+")));
        let prev = final_best.best_move.map(|m| move_to_usi(&m)).unwrap_or_else(|| "-".to_string());
        info_string("sanity_switch=1");
        info_string(format!(
            "final_select move={} switched_from={} reason={}",
            final_usi, prev, reason
        ));
        #[cfg(test)]
        {
            test_probe_record(FinalizeOutcome {
                mode: "joined",
                chosen_source: source_to_str(chosen_src),
                reason_tags: tags.to_vec(),
                mate_gate_blocked,
                postverify_reject: mate_post_reject,
            });
        }
    }
    log_and_emit_final_selection(state, label, chosen_src, &final_usi, ponder_mv, &stop_meta);
    // Clear pending_ponder_result to prevent stale buffer usage
    state.pending_ponder_result = None;
}

fn emit_single_pv(
    res: &SearchResult,
    final_best: &FinalBest,
    nps_agg: u128,
    hf_permille: u16,
    score_override: Option<i32>,
    bound_override: Option<NodeType>,
) {
    let mut s = String::from("info");
    s.push_str(&format!(" depth {}", res.stats.depth));
    if let Some(sd) = res.stats.seldepth {
        s.push_str(&format!(" seldepth {}", sd));
    }
    s.push_str(&format!(" time {}", res.stats.elapsed.as_millis()));
    s.push_str(&format!(" nodes {}", res.stats.nodes));
    s.push_str(&format!(" nps {}", nps_agg));
    s.push_str(&format!(" hashfull {}", hf_permille));

    let score_to_use = score_override.unwrap_or(res.score);
    let view = score_view_with_clamp(score_to_use);
    let bound_to_use = bound_override.unwrap_or(res.node_type);
    append_usi_score_and_bound(&mut s, view, bound_to_use);

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
                let stop_meta = gather_stop_meta(None, None, Some(FinalizeReason::PonderToMove));
                emit_finalize_event(
                    state,
                    label,
                    "cached",
                    &stop_meta,
                    &FinalizeEventParams {
                        reported_depth: pr.depth,
                        stable_depth: Some(pr.depth),
                        incomplete_depth: None,
                        report_source: SnapshotSource::Stable,
                        snapshot_version: None,
                    },
                );
                let prev_usi = best.clone();
                let mut final_usi = best.clone();
                let mut ponder_mv: Option<String> = pr.pv_second;
                // Run FinalizeSanity (MinMs) before emitting; if switched, compute ponder from TT
                if let Ok(mv) = parse_usi_move(&prev_usi) {
                    let fb = FinalBest {
                        best_move: Some(mv),
                        pv: Vec::new(),
                        source: FinalBestSource::Committed,
                    };
                    // ponderhitのcached経路でも near-draw 判定の一貫化のため score_hint=0 を与える
                    if let Some(alt) =
                        finalize_sanity_check(state, &stop_meta, &fb, None, Some(0), "fast")
                    {
                        let new_usi = move_to_usi(&alt);
                        info_string("final_select_tags tags=sanity");
                        info_string("sanity_switch=1");
                        info_string(format!(
                            "final_select move={} switched_from={} reason=sanity",
                            new_usi, prev_usi
                        ));
                        final_usi = new_usi;
                        if state.opts.ponder {
                            let eng = state.lock_engine();
                            ponder_mv = eng
                                .get_ponder_from_tt(&state.position, alt)
                                .map(|m| move_to_usi(&m));
                            drop(eng);
                        }
                    }
                }
                log_and_emit_final_selection(
                    state,
                    label,
                    FinalBestSource::Committed,
                    &final_usi,
                    ponder_mv,
                    &stop_meta,
                );
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
                // Mate fast gating: ignore shallow/inexact mate lines
                let mut blocked_by_mate_gate = false;
                if let Some(line0) = snap.lines.first() {
                    if engine_core::search::common::is_mate_score(line0.score_internal) && {
                        let cfg = MateGateCfg {
                            min_stable_depth: state.opts.mate_gate_min_stable_depth,
                            fast_ok_min_depth: state.opts.mate_gate_fast_ok_min_depth,
                            fast_ok_min_elapsed_ms: state.opts.mate_gate_fast_ok_min_elapsed_ms,
                        };
                        let stable_opt =
                            (snap.source == SnapshotSource::Stable).then_some(snap.depth);
                        mate_gate_should_block(
                            line0.bound,
                            stable_opt,
                            snap.depth,
                            snap.elapsed_ms,
                            &cfg,
                        )
                    } {
                        let dist = md(line0.score_internal).unwrap_or(0);
                        info_string(format!(
                            "mate_fast_gate_blocked=1 bound={} depth={} elapsed_ms={} mate_dist={}",
                            match line0.bound {
                                engine_core::search::types::NodeType::LowerBound => "lb",
                                engine_core::search::types::NodeType::UpperBound => "ub",
                                _ => "pv",
                            },
                            snap.depth,
                            snap.elapsed_ms,
                            dist
                        ));
                        blocked_by_mate_gate = dist > 1; // 距離1は免除
                    }
                }

                if shallow || blocked_by_mate_gate {
                    let mut tt_choice: Option<(
                        FinalBest,
                        String,
                        Option<String>,
                        FinalBestSource,
                        u64,
                        u64,
                    )> = None;
                    if let Some((eng_guard, spent_ms, spent_us)) =
                        try_lock_engine_with_budget(&state.engine, budget_ms)
                    {
                        let (final_usi, ponder_mv, final_source, final_best) = {
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
                            (final_usi, ponder_mv, final_source, final_best)
                        };
                        drop(eng_guard);
                        tt_choice = Some((
                            final_best,
                            final_usi,
                            ponder_mv,
                            final_source,
                            spent_ms,
                            spent_us,
                        ));
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
                        #[cfg(test)]
                        {
                            let mut tags: Vec<&'static str> = Vec::new();
                            if blocked_by_mate_gate {
                                tags.push("mate_gate");
                            }
                            test_probe_record(FinalizeOutcome {
                                mode: "fast",
                                chosen_source: source_to_str(FinalBestSource::Committed),
                                reason_tags: tags,
                                mate_gate_blocked: blocked_by_mate_gate,
                                postverify_reject: false,
                            });
                        }
                    }
                    if let Some((
                        final_best,
                        mut final_usi,
                        mut ponder_mv,
                        mut final_source,
                        _spent_ms,
                        _spent_us,
                    )) = tt_choice
                    {
                        // fastでもFinalizeSanity（StopInfoが無い場合でもMinMsで実施）。切替時はCommitted扱い。
                        let score_hint = snap.lines.first().map(|l| l.score_internal);
                        if let Some(alt) = finalize_sanity_check(
                            state,
                            &stop_meta,
                            &final_best,
                            None,
                            score_hint,
                            "fast",
                        ) {
                            let prev_usi = final_usi.clone();
                            final_usi = move_to_usi(&alt);
                            if state.opts.ponder {
                                let eng2 = state.lock_engine();
                                ponder_mv = eng2
                                    .get_ponder_from_tt(&state.position, alt)
                                    .map(|m| move_to_usi(&m));
                                drop(eng2);
                            }
                            final_source = FinalBestSource::Committed;
                            info_string("final_select_tags tags=sanity");
                            info_string("sanity_switch=1");
                            info_string(format!(
                                "final_select move={} switched_from={} reason=sanity",
                                final_usi, prev_usi
                            ));
                        }
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
                    // ロック獲得に失敗：mateゲートでブロックされているなら snapshot を使わず合法フォールバックへ
                    if blocked_by_mate_gate {
                        // A案: 残余が2ms以上あるときのみ1ms再トライ→失敗なら手計算SEEフォールバック
                        let retry_budget_ms =
                            compute_tt_probe_budget_ms(stop_meta.info.as_ref(), 0);
                        let do_retry = retry_budget_ms >= 2;
                        let fallback = if do_retry {
                            if let Some((eng_guard, _ms, _us)) =
                                try_lock_engine_with_budget(&state.engine, 1)
                            {
                                let fb = eng_guard.choose_final_bestmove(&state.position, None);
                                drop(eng_guard);
                                fb.best_move
                                    .map(|m| move_to_usi(&m))
                                    .unwrap_or_else(|| "resign".to_string())
                            } else {
                                // 手計算: SEE>=0の戦術手→SEE>=0任意→従来ヒューリスティック（共通ヘルパで選択）
                                let mg = MoveGenerator::new();
                                if let Ok(list) = mg.generate_all(&state.position) {
                                    let pos = &state.position;
                                    let legal: Vec<_> = list
                                        .as_slice()
                                        .iter()
                                        .copied()
                                        .filter(|&m| pos.is_legal_move(m))
                                        .collect();
                                    choose_legal_fallback_with_see(pos, &legal)
                                        .map(|mv| move_to_usi(&mv))
                                        .unwrap_or_else(|| "resign".to_string())
                                } else {
                                    "resign".to_string()
                                }
                            }
                        } else {
                            // 手計算: SEE>=0の戦術手→SEE>=0任意→従来ヒューリスティック（共通ヘルパで選択）
                            let mg = MoveGenerator::new();
                            if let Ok(list) = mg.generate_all(&state.position) {
                                let pos = &state.position;
                                let legal: Vec<_> = list
                                    .as_slice()
                                    .iter()
                                    .copied()
                                    .filter(|&m| pos.is_legal_move(m))
                                    .collect();
                                choose_legal_fallback_with_see(pos, &legal)
                                    .map(|mv| move_to_usi(&mv))
                                    .unwrap_or_else(|| "resign".to_string())
                            } else {
                                "resign".to_string()
                            }
                        };
                        #[cfg(test)]
                        {
                            let tags: Vec<&'static str> = vec!["mate_gate"];
                            test_probe_record(FinalizeOutcome {
                                mode: "fast",
                                chosen_source: source_to_str(FinalBestSource::LegalFallback),
                                reason_tags: tags,
                                mate_gate_blocked: true,
                                postverify_reject: false,
                            });
                        }
                        log_and_emit_final_selection(
                            state,
                            label,
                            FinalBestSource::LegalFallback,
                            &fallback,
                            None,
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
                #[cfg(test)]
                {
                    test_probe_record(FinalizeOutcome {
                        mode: "fast",
                        chosen_source: source_to_str(FinalBestSource::Committed),
                        reason_tags: Vec::new(),
                        mate_gate_blocked: false,
                        postverify_reject: false,
                    });
                }
                // スナップショットのみの場合でもFinalizeSanity（MinMs）を実施
                if state.opts.finalize_sanity_enabled {
                    let fb = FinalBest {
                        best_move: Some(best),
                        pv: Vec::new(),
                        source: FinalBestSource::Committed,
                    };
                    // score_hint: snapshot先頭のscore_internalを使用
                    let score_hint = snap.lines.first().map(|l| l.score_internal);
                    if let Some(alt) =
                        finalize_sanity_check(state, &stop_meta, &fb, None, score_hint, "fast")
                    {
                        let prev_usi = final_usi.clone();
                        let alt_usi = move_to_usi(&alt);
                        let alt_ponder = if state.opts.ponder {
                            let eng2 = state.lock_engine();
                            let p = eng2
                                .get_ponder_from_tt(&state.position, alt)
                                .map(|m| move_to_usi(&m));
                            drop(eng2);
                            p
                        } else {
                            None
                        };
                        info_string("final_select_tags tags=sanity");
                        info_string("sanity_switch=1");
                        info_string(format!(
                            "final_select move={} switched_from={} reason=sanity",
                            alt_usi, prev_usi
                        ));
                        log_and_emit_final_selection(
                            state,
                            label,
                            FinalBestSource::Committed,
                            &alt_usi,
                            alt_ponder,
                            &stop_meta,
                        );
                        return;
                    }
                }
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
    // まずTTでの最終手を取り出し（stateの不変借用スコープ内）
    let mut tt_final_best: Option<FinalBest> = None;
    let mut tt_final_usi: Option<String> = None;
    let mut tt_ponder: Option<String> = None;
    let mut tt_source: Option<FinalBestSource> = None;
    if let Some((eng_guard, spent_ms, spent_us)) =
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
        let fb_src = final_best.source;
        tt_final_best = Some(final_best);
        tt_final_usi = Some(final_usi);
        tt_ponder = ponder_mv;
        tt_source = Some(fb_src);
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
    }
    if let (Some(final_best), Some(mut final_usi)) = (tt_final_best, tt_final_usi) {
        let mut ponder_mv = tt_ponder;
        let mut final_source = tt_source.unwrap_or(FinalBestSource::Committed);
        // TTのみのfast経路では cheap な score_hint を与える（±10 判定用）。
        let score_hint = Some(0);
        if let Some(alt) =
            finalize_sanity_check(state, &stop_meta, &final_best, None, score_hint, "fast")
        {
            let prev_usi = final_usi.clone();
            final_usi = move_to_usi(&alt);
            if state.opts.ponder {
                let eng2 = state.lock_engine();
                ponder_mv = eng2.get_ponder_from_tt(&state.position, alt).map(|m| move_to_usi(&m));
                drop(eng2);
            }
            final_source = FinalBestSource::Committed;
            info_string("final_select_tags tags=sanity");
            info_string("sanity_switch=1");
            info_string(format!(
                "final_select move={} switched_from={} reason=sanity",
                final_usi, prev_usi
            ));
        }
        #[cfg(test)]
        {
            test_probe_record(FinalizeOutcome {
                mode: "fast",
                chosen_source: source_to_str(final_source),
                reason_tags: Vec::new(),
                mate_gate_blocked: false,
                postverify_reject: false,
            });
        }
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
                let legal_moves: Vec<_> =
                    slice.iter().copied().filter(|&m| pos.is_legal_move(m)).collect();

                let chosen = if legal_moves.is_empty() {
                    None
                } else if in_check {
                    legal_moves.first().copied()
                } else {
                    choose_legal_fallback_with_see(pos, &legal_moves)
                };

                if let Some(chosen) = chosen {
                    let final_usi = move_to_usi(&chosen);
                    #[cfg(test)]
                    {
                        test_probe_record(FinalizeOutcome {
                            mode: "fast",
                            chosen_source: source_to_str(FinalBestSource::LegalFallback),
                            reason_tags: Vec::new(),
                            mate_gate_blocked: false,
                            postverify_reject: false,
                        });
                    }
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
struct MateGateCfg {
    min_stable_depth: u8,
    fast_ok_min_depth: u8,
    fast_ok_min_elapsed_ms: u64,
}

/// YO流のmateゲート判定。
/// 非Exactは常にブロック。Exactでも (stable_ok || fast_ok) を満たさなければブロック。
/// - stable_ok: stable_depth >= cfg.min_stable_depth（Stableスナップショット時のみ評価）
/// - fast_ok  : depth >= cfg.fast_ok_min_depth || elapsed_ms >= cfg.fast_ok_min_elapsed_ms
fn mate_gate_should_block(
    bound: NodeType,
    stable_depth: Option<u8>,
    depth: u8,
    elapsed_ms: u64,
    cfg: &MateGateCfg,
) -> bool {
    let exact = matches!(bound, NodeType::Exact);
    let stable_ok = stable_depth.map(|d| d >= cfg.min_stable_depth).unwrap_or(false);
    let fast_ok = depth >= cfg.fast_ok_min_depth || elapsed_ms >= cfg.fast_ok_min_elapsed_ms;
    !exact || !(stable_ok || fast_ok)
}

#[inline]
fn is_king_move(pos: &engine_core::shogi::Position, m: &engine_core::shogi::Move) -> bool {
    // Drop手（打）は from() が None。piece_type() が Some(King) なら王手。
    m.piece_type()
        .or_else(|| m.from().and_then(|sq| pos.board.piece_on(sq).map(|p| p.piece_type)))
        .map(|pt| matches!(pt, PieceType::King))
        .unwrap_or(false)
}

#[inline]
fn is_tactical(m: &engine_core::shogi::Move) -> bool {
    m.is_drop() || m.is_capture_hint() || m.is_promote()
}

/// SEEを用いた最終フォールバック選択（戦術>=0→任意>=0→非王手の戦術→非王手→先頭）
fn choose_legal_fallback_with_see(
    pos: &engine_core::shogi::Position,
    legal_moves: &[engine_core::shogi::Move],
) -> Option<engine_core::shogi::Move> {
    if legal_moves.is_empty() {
        return None;
    }
    // SEE>=0（戦術）
    let mut best_ge0_tac: Option<(engine_core::shogi::Move, i32)> = None;
    for &m in legal_moves.iter() {
        if is_king_move(pos, &m) || !is_tactical(&m) {
            continue;
        }
        let s = pos.see(m);
        if s >= 0 {
            best_ge0_tac = match best_ge0_tac {
                Some((mm, ss)) if ss >= s => Some((mm, ss)),
                _ => Some((m, s)),
            };
        }
    }
    if let Some((m, _)) = best_ge0_tac {
        return Some(m);
    }
    // SEE>=0（任意）
    let mut best_ge0_any: Option<(engine_core::shogi::Move, i32)> = None;
    for &m in legal_moves.iter() {
        if is_king_move(pos, &m) {
            continue;
        }
        let s = pos.see(m);
        if s >= 0 {
            best_ge0_any = match best_ge0_any {
                Some((mm, ss)) if ss >= s => Some((mm, ss)),
                _ => Some((m, s)),
            };
        }
    }
    if let Some((m, _)) = best_ge0_any {
        return Some(m);
    }
    // 非王手の戦術 → 非王手 → 先頭
    legal_moves
        .iter()
        .find(|m| is_tactical(m) && !is_king_move(pos, m))
        .copied()
        .or_else(|| legal_moves.iter().find(|m| !is_king_move(pos, m)).copied())
        .or_else(|| legal_moves.first().copied())
}

/// SEE>=0 の制約をオプションで切り替えできる版（非玉のみ対象で使う想定）
fn choose_legal_fallback_with_see_filtered(
    pos: &engine_core::shogi::Position,
    legal_moves: &[engine_core::shogi::Move],
    allow_see_lt0: bool,
) -> Option<engine_core::shogi::Move> {
    if legal_moves.is_empty() {
        return None;
    }
    // まずは SEE>=0 の候補（戦術優先→任意）
    if let Some(mv) = legal_moves
        .iter()
        .copied()
        .filter(|m| !is_king_move(pos, m))
        .filter(is_tactical)
        .map(|m| (m, pos.see(m)))
        .filter(|&(_, s)| s >= 0)
        .max_by_key(|&(_, s)| s)
        .map(|(m, _)| m)
    {
        return Some(mv);
    }
    if let Some(mv) = legal_moves
        .iter()
        .copied()
        .filter(|m| !is_king_move(pos, m))
        .map(|m| (m, pos.see(m)))
        .filter(|&(_, s)| s >= 0)
        .max_by_key(|&(_, s)| s)
        .map(|(m, _)| m)
    {
        return Some(mv);
    }
    if !allow_see_lt0 {
        // SEE<0 を許容しない場合はここで打ち切り
        return None;
    }
    // 以降は SEE サインを問わずに安全側を優先
    legal_moves
        .iter()
        .find(|m| is_tactical(m) && !is_king_move(pos, m))
        .copied()
        .or_else(|| legal_moves.iter().find(|m| !is_king_move(pos, m)).copied())
        .or_else(|| legal_moves.first().copied())
}

/// "安全手クラス"を選ぶための最終フォールバック（非玉限定）。
/// allow_see_lt0=falseのときは SEE>=0 のみ、trueのときは負も可。
fn choose_safe_nonking_fallback(
    pos: &engine_core::shogi::Position,
    legal_nonking: &[engine_core::shogi::Move],
    allow_see_lt0: bool,
) -> Option<engine_core::shogi::Move> {
    // 非玉かつ SEE>=0 を優先
    if let Some(mv) = legal_nonking
        .iter()
        .copied()
        .map(|m| (m, pos.see(m)))
        .filter(|&(_, s)| s >= 0)
        .max_by_key(|&(_, s)| s)
        .map(|(m, _)| m)
    {
        return Some(mv);
    }
    if allow_see_lt0 {
        // SEE 制約を外して次点
        if let Some(mv) = legal_nonking
            .iter()
            .copied()
            .map(|m| (m, pos.see(m)))
            .max_by_key(|&(_, s)| s)
            .map(|(m, _)| m)
        {
            return Some(mv);
        }
    }
    None
}

#[inline]
fn need_verify_from_risks(
    opp_cap_see_max: i32,
    opp_threat2_max: i32,
    opp_gate: i32,
    threat2_gate: i32,
) -> bool {
    // AND 条件: 両方のゲートを超えたときのみ検証を要求
    opp_cap_see_max >= opp_gate && opp_threat2_max >= threat2_gate
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
    fn need_verify_from_risks_and_logic() {
        // 両側がゲート超過のときのみ true
        assert!(need_verify_from_risks(150, 350, 100, 300));
        // 片側のみ超過は false
        assert!(!need_verify_from_risks(100, 0, 100, 300));
        assert!(!need_verify_from_risks(0, 300, 100, 300));
        // いずれも未達なら false
        assert!(!need_verify_from_risks(99, 299, 100, 300));
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
            stop_tag: None,
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
            let eng = state.lock_engine();
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
    fn probe_fast_partial_snapshot_mate_gate_blocked() {
        use engine_core::search::constants::MATE_SCORE;
        super::test_probe_reset();

        let mut state = EngineState::new();
        let session_id = 7;
        state.current_session_core_id = Some(session_id);
        state.stop_controller.publish_session(None, session_id);
        state.stop_controller.prime_stop_info(StopInfo::default());

        // Construct a mate LB line at shallow depth to trigger fast gate block
        let root_mv = parse_usi_move("7g7f").unwrap();
        let mut pv: SmallVec<[engine_core::shogi::Move; 32]> = SmallVec::new();
        pv.push(root_mv);
        let line = RootLine {
            multipv_index: 1,
            root_move: root_mv,
            score_internal: MATE_SCORE - 3,
            score_cp: 30_000, // display-only; internal matters
            bound: NodeType::LowerBound,
            depth: 3, // shallow
            seldepth: Some(3),
            pv,
            nodes: Some(100),
            time_ms: Some(5),
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: Some(3),
        };
        // Publish partial snapshot
        state
            .stop_controller
            .publish_root_line(session_id, state.position.zobrist_hash(), &line);

        finalize_and_send_fast(&mut state, "probe_fast_mate_gate", Some(FinalizeReason::Hard));

        let out = super::test_probe_take().expect("finalize outcome recorded");
        assert_eq!(out.mode, "fast");
        assert!(out.mate_gate_blocked, "mate gate should be blocked");
        assert!(out.reason_tags.contains(&"mate_gate"));
    }

    #[test]
    fn probe_joined_postverify_reject() {
        use engine_core::search::constants::MATE_SCORE;
        use engine_core::search::SearchStats;
        use std::time::Duration;
        super::test_probe_reset();

        let mut state = EngineState::new();
        // Fabricate a SearchResult with a mate score that passes gate but fails CheckOnly verify
        let stats = SearchStats {
            elapsed: Duration::from_millis(100),
            depth: 10,
            seldepth: Some(10),
            ..Default::default()
        };
        // PV head can be any legal move; leave empty to fall back to best_move
        let res = SearchResult::with_node_type(
            state.lock_engine().choose_final_bestmove(&state.position, None).best_move,
            MATE_SCORE - 3,
            stats,
            NodeType::Exact,
        );

        finalize_and_send(&mut state, "probe_joined_postverify", Some(&res), false, None);
        let out = super::test_probe_take().expect("finalize outcome recorded");
        assert_eq!(out.mode, "joined");
        assert!(out.postverify_reject, "postverify should reject non-mate-in-1");
        assert!(out.reason_tags.contains(&"postverify"));
        assert!(!out.chosen_source.is_empty());
    }

    #[test]
    fn probe_fast_trylock_fail_see_fallback() {
        use engine_core::search::constants::MATE_SCORE;
        super::test_probe_reset();

        let mut state = EngineState::new();
        let session_id = 8;
        state.current_session_core_id = Some(session_id);
        state.stop_controller.publish_session(None, session_id);
        state.stop_controller.prime_stop_info(StopInfo::default());

        // Publish a shallow mate LB snapshot to trigger the blocked path and then force lock failure
        let mv = parse_usi_move("2g2f").unwrap();
        let mut pv: SmallVec<[engine_core::shogi::Move; 32]> = SmallVec::new();
        pv.push(mv);
        let line = RootLine {
            multipv_index: 1,
            root_move: mv,
            score_internal: MATE_SCORE - 3,
            score_cp: 30_000,
            bound: NodeType::LowerBound,
            depth: 3,
            seldepth: Some(3),
            pv,
            nodes: Some(100),
            time_ms: Some(5),
            nps: None,
            exact_exhausted: false,
            exhaust_reason: None,
            mate_distance: Some(3),
        };
        state
            .stop_controller
            .publish_root_line(session_id, state.position.zobrist_hash(), &line);

        // Hold engine lock from another thread to make try_lock_engine_with_budget fail without borrow conflicts
        let eng_arc = state.engine.clone();
        let locker = std::thread::spawn(move || {
            let _g = eng_arc.lock().unwrap();
            // Keep the lock long enough to overlap with try_lock budget comfortably
            std::thread::sleep(std::time::Duration::from_millis(100));
        });
        // Ensure the locking thread acquires the mutex before finalize
        std::thread::sleep(std::time::Duration::from_millis(10));
        finalize_and_send_fast(&mut state, "probe_fast_trylock_fail", Some(FinalizeReason::Hard));
        let _ = locker.join();

        let out = super::test_probe_take().expect("finalize outcome recorded");
        assert_eq!(out.mode, "fast");
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
            let eng = state.lock_engine();
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
            stop_tag: None,
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
