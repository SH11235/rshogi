use super::driver::{
    return_stack_cache, root_see_gate_should_skip, take_stack_cache, ClassicBackend,
};
use super::ordering::{EvalMoveGuard, Heuristics, MovePicker};
use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::api::{InfoEvent, InfoEventCallback};
use crate::search::config;
use crate::search::constants::{mate_distance, MIN_QNODES_LIMIT, SEARCH_INF};
use crate::search::mate1ply;
use crate::search::root_threat::{self, RootThreat};
use crate::search::tt::TTProbe;
use crate::search::types::{
    normalize_root_pv, InfoStringCallback, RootLine, RootVerifyFailKind, SearchResult,
};
use crate::search::SearchLimits;
use crate::shogi::{Color, Move, Piece, PieceType, Position, Square};
use smallvec::SmallVec;
use std::collections::HashSet;
use std::time::Instant;

pub(crate) const WIN_PROTECT_MIN_THINK_MS: u64 = 20;
pub(crate) const WIN_PROTECT_DEPTH_LIMIT: u8 = 13;

#[derive(Clone, Copy)]
pub(crate) struct WinProtectConfig {
    pub enabled: bool,
    pub threshold_cp: i32,
}

impl WinProtectConfig {
    pub(crate) fn load() -> Self {
        Self {
            enabled: config::win_protect_enabled(),
            threshold_cp: config::win_protect_threshold_cp(),
        }
    }
}

#[derive(Clone, Copy)]
struct RootVerifySettings {
    enabled: bool,
    max_ms: u64,
    max_nodes: u64,
    check_depth: i32,
    opp_see_min_cp: i32,
    major_loss_penalty_cp: i32,
    require_pass: bool,
    max_candidates: usize,
    max_candidates_threat: usize,
    max_defense_seeds: usize,
    max_defense_seeds_threat: usize,
}

impl RootVerifySettings {
    fn load() -> Self {
        Self {
            enabled: config::root_verify_enabled(),
            max_ms: config::root_verify_max_ms(),
            max_nodes: config::root_verify_max_nodes(),
            check_depth: config::root_verify_check_depth() as i32,
            opp_see_min_cp: config::root_verify_opp_see_min_cp(),
            major_loss_penalty_cp: config::root_verify_major_loss_penalty_cp(),
            require_pass: config::root_verify_require_pass(),
            max_candidates: config::root_verify_max_candidates() as usize,
            max_candidates_threat: config::root_verify_max_candidates_threat() as usize,
            max_defense_seeds: config::root_verify_max_defense_seeds() as usize,
            max_defense_seeds_threat: config::root_verify_max_defense_seeds_threat() as usize,
        }
    }

    fn max_candidates(&self, threat: bool) -> usize {
        if threat {
            self.max_candidates_threat.max(1)
        } else {
            self.max_candidates.max(1)
        }
    }

    fn max_defense_seeds(&self, threat: bool) -> usize {
        if threat {
            self.max_defense_seeds_threat
        } else {
            self.max_defense_seeds
        }
    }
}

#[derive(Default)]
struct VerifySummary {
    checked: u64,
    fail_count: u64,
    total_elapsed_ms: u64,
}

#[derive(Clone, Copy)]
struct Candidate {
    mv: Move,
    line_index: Option<usize>,
}

struct ProbeReport {
    eval: i32,
    elapsed_ms: u64,
}

enum VerifyFailReason {
    SelfSee(i32),
    OppXsee { piece: PieceType, score: i32 },
    PawnDropHead { piece: PieceType },
    EvalDrop(i32),
    MateInOne { mv: Move },
}

impl VerifyFailReason {
    fn as_str(&self) -> (&'static str, i32, Option<PieceType>, Option<Move>) {
        match *self {
            VerifyFailReason::SelfSee(see) => ("self_see", see, None, None),
            VerifyFailReason::OppXsee { piece, score } => {
                ("opp_xsee_neg", score, Some(piece), None)
            }
            VerifyFailReason::PawnDropHead { piece } => ("opp_drop_head", 0, Some(piece), None),
            VerifyFailReason::EvalDrop(delta) => ("eval_drop", delta, None, None),
            VerifyFailReason::MateInOne { mv } => ("opp_mate_in_one", -32_000, None, Some(mv)),
        }
    }
}

pub(super) fn apply_root_post_verify<E: Evaluator + Send + Sync + 'static>(
    backend: &ClassicBackend<E>,
    root: &Position,
    limits: &SearchLimits,
    info: Option<&InfoEventCallback>,
    result: &mut SearchResult,
    best_score: i32,
    root_move_order: &[Move],
) {
    let settings = RootVerifySettings::load();
    if !settings.enabled {
        return;
    }
    if result.best_move.is_none() {
        return;
    }
    let mut summary = VerifySummary::default();
    let stable_depth = result.stats.stable_depth.unwrap_or(result.stats.depth);
    let win_cfg = WinProtectConfig::load();
    let win_protect_active = win_cfg.enabled
        && best_score >= win_cfg.threshold_cp
        && stable_depth < WIN_PROTECT_DEPTH_LIMIT;
    let required = if win_protect_active { 2 } else { 1 };

    let threat_mode = mate_distance(best_score).map(|dist| dist < 0).unwrap_or(false);
    let fallback_moves = if root_move_order.is_empty() {
        generate_move_order(backend, root)
    } else {
        root_move_order.to_vec()
    };
    let defense_seed_cap = settings.max_defense_seeds(threat_mode);
    let defense_seeds = if defense_seed_cap > 0 {
        defense_seed_moves(root, defense_seed_cap)
    } else {
        Vec::new()
    };
    let max_candidates = settings.max_candidates(threat_mode);
    let mut candidates =
        collect_candidates(result, &fallback_moves, &defense_seeds, required, max_candidates);
    if candidates.is_empty() {
        candidates.push(Candidate {
            mv: result.best_move.unwrap(),
            line_index: None,
        });
    }
    let mut accepted: Option<(Candidate, ProbeReport)> = None;
    let mut fallback: Option<(Candidate, ProbeReport)> = None;
    let verify_cap = max_candidates.max(required).max(1);
    for cand in candidates.iter().take(verify_cap) {
        summary.checked += 1;
        let res = verify_candidate(backend, root, best_score, cand.mv, &settings);
        summary.total_elapsed_ms = summary.total_elapsed_ms.saturating_add(res.report.elapsed_ms);
        if let Some(reason) = res.fail_reason {
            summary.fail_count = summary.fail_count.saturating_add(1);
            record_root_verify_fail(result, cand.mv, &reason);
            if let VerifyFailReason::MateInOne { mv: mate_mv } = &reason {
                result.stats.root_verify_rejected_move = Some(cand.mv);
                result.stats.root_verify_mate_move = Some(*mate_mv);
                crate::search::types::SearchStats::bump(
                    &mut result.stats.root_verify_opp_mate_hits,
                    1,
                );
            }
            emit_fail_log(info, limits.info_string_callback.as_ref(), cand.mv, &reason);
            if fallback.as_ref().map(|(_, rep)| rep.eval < res.report.eval).unwrap_or(true) {
                fallback = Some((*cand, res.report));
            }
        } else {
            emit_pass_log(
                info,
                limits.info_string_callback.as_ref(),
                cand.mv,
                res.report.eval,
                best_score,
                win_protect_active,
            );
            accepted = Some((*cand, res.report));
            break;
        }
    }

    let mut require_pass_failed = false;
    if accepted.is_none() && settings.require_pass {
        require_pass_failed = true;
        if let Some(cb) = info {
            cb(InfoEvent::String(String::from("root_verify require_pass fallback=safe")));
        }
        if let Some(cb) = limits.info_string_callback.as_ref() {
            cb("root_verify require_pass fallback=safe");
        }
    }

    if let Some((cand, report)) = accepted.or(fallback) {
        if result.best_move != Some(cand.mv) {
            apply_move_selection(backend, root, result, limits, cand, report.eval);
        } else if let Some(line_idx) = cand.line_index {
            if let Some(lines) = result.lines.as_mut() {
                promote_line(lines, line_idx);
            }
        }
        result.sync_from_primary_line();
    }
    if require_pass_failed {
        result.stats.root_verify_require_pass_failed = Some(1);
    } else {
        result.stats.root_verify_require_pass_failed = None;
    }
    if let Some(cb) = info {
        cb(InfoEvent::String(format!(
            "root_verify summary checked={} fail={} total_ms={} enabled={}",
            summary.checked, summary.fail_count, summary.total_elapsed_ms, settings.enabled as u8
        )));
    }
    result.stats.root_verify_fail_count = Some(summary.fail_count);
    result.stats.root_verify_checked_moves = Some(summary.checked);
    result.stats.root_verify_total_ms = Some(summary.total_elapsed_ms);
}

fn verify_candidate<E: Evaluator + Send + Sync + 'static>(
    backend: &ClassicBackend<E>,
    root: &Position,
    original_score: i32,
    mv: Move,
    settings: &RootVerifySettings,
) -> VerifyResult {
    let mut child = root.clone();
    if let Some(mate_mv) = mate1ply::enemy_mate_in_one_after(&mut child, mv) {
        return VerifyResult {
            report: ProbeReport {
                eval: original_score,
                elapsed_ms: 0,
            },
            fail_reason: Some(VerifyFailReason::MateInOne { mv: mate_mv }),
        };
    }
    if !mv.is_drop() {
        let xsee = if mv.is_capture_hint() {
            root.see(mv)
        } else {
            root.see_landing_after_move(mv, 0)
        };
        if xsee < 0 {
            return VerifyResult {
                report: ProbeReport {
                    eval: original_score,
                    elapsed_ms: 0,
                },
                fail_reason: Some(VerifyFailReason::SelfSee(xsee)),
            };
        }
    }
    let eval_guard = EvalMoveGuard::new(backend.evaluator.as_ref(), root, mv);
    child.do_move(mv);
    if let Some(threat) =
        root_threat::detect_major_threat(&child, root.side_to_move, settings.opp_see_min_cp)
    {
        let reason = match threat {
            RootThreat::OppXsee { piece, loss } => VerifyFailReason::OppXsee { piece, score: loss },
            RootThreat::PawnDropHead { piece } => VerifyFailReason::PawnDropHead { piece },
        };
        drop(eval_guard);
        return VerifyResult {
            report: ProbeReport {
                eval: original_score,
                elapsed_ms: 0,
            },
            fail_reason: Some(reason),
        };
    }
    let probe = run_probe(backend, &child, settings);
    drop(eval_guard);
    let delta = probe.eval - original_score;
    if delta <= -settings.major_loss_penalty_cp {
        return VerifyResult {
            report: probe,
            fail_reason: Some(VerifyFailReason::EvalDrop(delta)),
        };
    }
    VerifyResult {
        report: probe,
        fail_reason: None,
    }
}

fn record_root_verify_fail(result: &mut SearchResult, mv: Move, reason: &VerifyFailReason) {
    result.stats.root_verify_last_fail_move = Some(mv);
    let (kind, detail) = match reason {
        VerifyFailReason::MateInOne { .. } => (RootVerifyFailKind::MateInOne, None),
        VerifyFailReason::SelfSee(see) => (RootVerifyFailKind::SelfSee, Some(*see)),
        VerifyFailReason::OppXsee { score, .. } => (RootVerifyFailKind::OppXsee, Some(*score)),
        VerifyFailReason::PawnDropHead { .. } => (RootVerifyFailKind::PawnDrop, None),
        VerifyFailReason::EvalDrop(delta) => (RootVerifyFailKind::EvalDrop, Some(*delta)),
    };
    result.stats.root_verify_last_fail_kind = Some(kind);
    result.stats.root_verify_last_fail_detail = detail;
}

fn run_probe<E: Evaluator + Send + Sync + 'static>(
    backend: &ClassicBackend<E>,
    pos: &Position,
    settings: &RootVerifySettings,
) -> ProbeReport {
    use crate::search::limits::SearchLimitsBuilder;
    let mut builder = SearchLimitsBuilder::default().depth(settings.check_depth as u8).multipv(1);
    if settings.max_ms > 0 {
        builder = builder.fixed_time_ms(settings.max_ms);
    }
    if settings.max_nodes > 0 {
        builder = builder
            .nodes(settings.max_nodes)
            .qnodes_limit(settings.max_nodes.clamp(MIN_QNODES_LIMIT, u64::MAX / 2));
    } else {
        builder = builder.qnodes_limit(MIN_QNODES_LIMIT * 2);
    }
    let mut verify_limits = builder.build();
    let verify_start = Instant::now();
    verify_limits.start_time = verify_start;
    let mut stack = take_stack_cache();
    for (idx, entry) in stack.iter_mut().enumerate() {
        entry.ply = idx as u16;
    }
    let mut heur = Heuristics::default();
    let mut nodes = 0_u64;
    let mut seldepth = 0_u32;
    let mut qnodes = 0_u64;
    #[cfg(feature = "diagnostics")]
    let mut abdada_busy_detected = 0_u64;
    #[cfg(feature = "diagnostics")]
    let mut abdada_busy_set = 0_u64;
    let mut ctx = super::pvs::SearchContext {
        limits: &verify_limits,
        start_time: &verify_start,
        nodes: &mut nodes,
        seldepth: &mut seldepth,
        qnodes: &mut qnodes,
        qnodes_limit: verify_limits.qnodes_limit.unwrap_or(MIN_QNODES_LIMIT * 2),
        #[cfg(feature = "diagnostics")]
        abdada_busy_detected: &mut abdada_busy_detected,
        #[cfg(feature = "diagnostics")]
        abdada_busy_set: &mut abdada_busy_set,
    };
    let mut tt_hits = 0_u64;
    let mut beta_cuts = 0_u64;
    let mut lmr_counter = 0_u64;
    let (score_child, _) = backend.alphabeta(
        super::pvs::ABArgs {
            pos,
            depth: settings.check_depth,
            alpha: -SEARCH_INF / 2,
            beta: SEARCH_INF / 2,
            ply: 0,
            is_pv: true,
            stack: &mut stack,
            heur: &mut heur,
            tt_hits: &mut tt_hits,
            beta_cuts: &mut beta_cuts,
            lmr_counter: &mut lmr_counter,
            lmr_blocked_in_check: None,
            lmr_blocked_recapture: None,
            evasion_sparsity_ext: None,
        },
        &mut ctx,
    );
    return_stack_cache(stack);
    ProbeReport {
        eval: -score_child,
        elapsed_ms: verify_start.elapsed().as_millis() as u64,
    }
}

#[allow(dead_code)]
fn promotion_options(piece: Piece, color: Color, from: Square, to: Square) -> SmallVec<[bool; 2]> {
    let mut opts = SmallVec::<[bool; 2]>::new();
    if piece.promoted || !piece.piece_type.can_promote() {
        opts.push(false);
        return opts;
    }
    if must_promote(color, piece.piece_type, to) {
        opts.push(true);
        return opts;
    }
    if can_promote(color, from, to) {
        opts.push(true);
    }
    opts.push(false);
    opts
}

#[allow(dead_code)]
fn must_promote(color: Color, piece_type: PieceType, to: Square) -> bool {
    match (color, piece_type) {
        (Color::Black, PieceType::Pawn | PieceType::Lance) => to.rank() == 0,
        (Color::White, PieceType::Pawn | PieceType::Lance) => to.rank() == 8,
        (Color::Black, PieceType::Knight) => to.rank() <= 1,
        (Color::White, PieceType::Knight) => to.rank() >= 7,
        _ => false,
    }
}

#[allow(dead_code)]
fn head_square(sq: Square, owner: Color) -> Option<Square> {
    match owner {
        Color::Black => {
            if sq.rank() == 0 {
                None
            } else {
                Some(Square::new(sq.file(), sq.rank() - 1))
            }
        }
        Color::White => {
            if sq.rank() >= 8 {
                None
            } else {
                Some(Square::new(sq.file(), sq.rank() + 1))
            }
        }
    }
}

fn can_promote(color: Color, from: Square, to: Square) -> bool {
    match color {
        Color::Black => from.rank() <= 2 || to.rank() <= 2,
        Color::White => from.rank() >= 6 || to.rank() >= 6,
    }
}

fn collect_candidates(
    result: &SearchResult,
    fallback: &[Move],
    defense_seeds: &[Move],
    required: usize,
    max_candidates: usize,
) -> Vec<Candidate> {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut list: Vec<Candidate> = Vec::new();
    let limit = max_candidates.max(required).max(1);
    if let Some(lines) = result.lines.as_ref() {
        for (idx, line) in lines.iter().enumerate() {
            let mv = line.root_move;
            if seen.insert(mv.to_u32()) {
                list.push(Candidate {
                    mv,
                    line_index: Some(idx),
                });
            }
        }
    } else if let Some(mv) = result.best_move {
        list.push(Candidate {
            mv,
            line_index: None,
        });
        seen.insert(mv.to_u32());
    }
    for mv in defense_seeds {
        if list.len() >= limit {
            break;
        }
        if seen.insert(mv.to_u32()) {
            list.push(Candidate {
                mv: *mv,
                line_index: None,
            });
        }
    }
    for mv in fallback {
        if list.len() >= limit {
            break;
        }
        if seen.insert(mv.to_u32()) {
            list.push(Candidate {
                mv: *mv,
                line_index: None,
            });
        }
    }
    if list.len() > limit {
        list.truncate(limit);
    }
    if list.len() < required {
        list.resize_with(required, || Candidate {
            mv: result.best_move.unwrap(),
            line_index: None,
        });
    }
    list
}

fn generate_move_order<E: Evaluator + Send + Sync + 'static>(
    backend: &ClassicBackend<E>,
    root: &Position,
) -> Vec<Move> {
    let mg = MoveGenerator::new();
    if mg.generate_all(root).is_err() {
        return Vec::new();
    }
    let mut hint = None;
    if let Some(tt) = &backend.tt {
        if let Some(entry) = tt.probe(root.zobrist_hash(), root.side_to_move) {
            hint = entry.get_move();
        }
    }
    let mut mp = MovePicker::new_normal(root, hint, None, [None, None], None, None);
    let heur = Heuristics::default();
    let mut moves = Vec::new();
    while let Some(mv) = mp.next(&heur) {
        moves.push(mv);
    }
    if config::root_see_gate_enabled() {
        let xsee = config::root_see_gate_xsee_cp();
        if xsee > 0 {
            moves.retain(|&mv| !root_see_gate_should_skip(root, mv, xsee));
        }
    }
    moves
}

fn chebyshev_distance(a: Square, b: Square) -> i32 {
    let df = a.file() as i32 - b.file() as i32;
    let dr = a.rank() as i32 - b.rank() as i32;
    df.abs().max(dr.abs())
}

fn defense_seed_score(king_sq: Square, mv: Move) -> Option<i32> {
    let to = mv.to();
    let dist = chebyshev_distance(king_sq, to);
    let mut score = 0i32;
    if mv.is_drop() {
        let pt = mv.drop_piece_type();
        if dist <= 1 {
            score += 100 - dist * 10;
        } else if dist == 2 {
            score += 40;
        }
        score += match pt {
            PieceType::Pawn => 25,
            PieceType::Gold => 45,
            PieceType::Silver => 35,
            PieceType::Knight => 30,
            PieceType::Lance => 15,
            PieceType::Rook | PieceType::Bishop => 20,
            PieceType::King => 0,
        };
    } else {
        if let Some(pt) = mv.piece_type() {
            if pt == PieceType::King {
                score += 80;
            } else if matches!(pt, PieceType::Gold | PieceType::Silver) {
                score += 20;
            }
        }
        if let Some(from) = mv.from() {
            let from_dist = chebyshev_distance(king_sq, from);
            if from_dist <= 1 && dist > from_dist {
                score += 35;
            }
        }
        if dist <= 1 {
            score += 30;
        } else if dist == 2 {
            score += 15;
        }
    }
    if score > 0 {
        Some(score - dist * 5)
    } else {
        None
    }
}

fn defense_seed_moves(root: &Position, limit: usize) -> Vec<Move> {
    if limit == 0 {
        return Vec::new();
    }
    let Some(king_sq) = root.king_square(root.side_to_move) else {
        return Vec::new();
    };
    let mg = MoveGenerator::new();
    let Ok(moves) = mg.generate_all(root) else {
        return Vec::new();
    };
    let mut scored: Vec<(i32, Move)> = Vec::new();
    for &mv in moves.as_slice() {
        if !root.is_legal_move(mv) {
            continue;
        }
        if let Some(score) = defense_seed_score(king_sq, mv) {
            scored.push((score, mv));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_u32().cmp(&b.1.to_u32())));
    scored.into_iter().take(limit).map(|(_, mv)| mv).collect()
}

fn apply_move_selection<E: Evaluator + Send + Sync + 'static>(
    backend: &ClassicBackend<E>,
    root: &Position,
    result: &mut SearchResult,
    limits: &SearchLimits,
    cand: Candidate,
    verified_score: i32,
) {
    if let Some(lines) = result.lines.as_mut() {
        if let Some(idx) = cand.line_index {
            promote_line(lines, idx);
            return;
        }
    }
    let mut new_lines = result.lines.clone().unwrap_or_default();
    let pv = backend.extract_pv(root, 2, cand.mv, limits, &mut 0_u64);
    let sel = result.stats.seldepth.unwrap_or(result.stats.depth);
    let mut line = RootLine {
        multipv_index: 1,
        root_move: cand.mv,
        score_internal: verified_score,
        score_cp: crate::search::types::clamp_score_cp(verified_score),
        bound: crate::search::types::NodeType::Exact,
        depth: u32::from(result.stats.depth),
        seldepth: Some(sel),
        pv,
        nodes: None,
        time_ms: None,
        nps: None,
        exact_exhausted: false,
        exhaust_reason: None,
        mate_distance: mate_distance(verified_score),
    };
    normalize_line_pv(&mut line);
    new_lines.retain(|l| l.root_move != cand.mv);
    new_lines.insert(0, line);
    renumber_lines(&mut new_lines);
    result.lines = Some(new_lines);
}

fn promote_line(lines: &mut SmallVec<[RootLine; 4]>, idx: usize) {
    if idx == 0 {
        renumber_lines(lines);
        return;
    }
    if idx >= lines.len() {
        return;
    }
    let line = lines.remove(idx);
    lines.insert(0, line);
    renumber_lines(lines);
}

fn renumber_lines(lines: &mut SmallVec<[RootLine; 4]>) {
    for (i, line) in lines.iter_mut().enumerate() {
        line.multipv_index = (i + 1) as u8;
    }
}

fn normalize_line_pv(line: &mut RootLine) {
    normalize_root_pv(&mut line.pv, line.root_move);
}

fn emit_fail_log(
    info: Option<&InfoEventCallback>,
    info_str: Option<&InfoStringCallback>,
    mv: Move,
    reason: &VerifyFailReason,
) {
    let (tag, value, piece, mate_mv) = reason.as_str();
    let mut extra = String::new();
    if let Some(pt) = piece {
        extra = format!(" piece={:?}", pt);
    }
    if let Some(mate) = mate_mv {
        extra = format!("{} mate_move={}", extra, crate::usi::move_to_usi(&mate));
    }
    let msg = format!(
        "root_verify fail after={} reason={} delta={}{}",
        crate::usi::move_to_usi(&mv),
        tag,
        value,
        extra
    );
    if let Some(cb) = info {
        cb(InfoEvent::String(msg.clone()));
    }
    if let Some(cb) = info_str {
        cb(&msg);
    }
}

fn emit_pass_log(
    info: Option<&InfoEventCallback>,
    info_str: Option<&InfoStringCallback>,
    mv: Move,
    eval: i32,
    base: i32,
    win_mode: bool,
) {
    let msg = format!(
        "root_verify pass after={} score={} delta={} win_protect={}",
        crate::usi::move_to_usi(&mv),
        eval,
        eval - base,
        win_mode as u8
    );
    if let Some(cb) = info {
        cb(InfoEvent::String(msg.clone()));
    }
    if let Some(cb) = info_str {
        cb(&msg);
    }
}

struct VerifyResult {
    report: ProbeReport,
    fail_reason: Option<VerifyFailReason>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usi::{move_to_usi, parse_usi_move};

    #[test]
    fn detects_enemy_mate_in_one_after_bad_bishop_drop() {
        let mut root = Position::from_sfen(
            "+R5bnl/5b3/pl1+P1gkp1/s3pps2/7gp/P1P1R4/1KN1P1PPP/5S3/L1S1GG1NL b N6P 46",
        )
        .expect("valid sfen");
        let mv = parse_usi_move("8g9h").expect("valid move");
        let mut pos = root.clone();
        pos.do_move(mv);
        let mate_mv = parse_usi_move("4b9g+").expect("valid mate move");
        let mut after = pos.clone();
        after.do_move(mate_mv);
        let checker = MoveGenerator::new();
        assert!(
            !checker.has_legal_moves(&after).expect("movegen ok"),
            "should be a terminal position"
        );
        let threat =
            mate1ply::enemy_mate_in_one_after(&mut root, mv).expect("mate threat expected");
        assert_eq!(move_to_usi(&threat), "4b9g+");
    }

    #[test]
    fn defense_seed_picker_generates_adjacent_drop() {
        let pos = Position::from_sfen(
            "lnsgkgsnl/1r5b1/p1ppppppp/9/9/9/P1PPPPPPP/1B5R1/LNSGKGSNL b P2p 1",
        )
        .expect("valid sfen");
        let seeds = defense_seed_moves(&pos, 4);
        assert!(!seeds.is_empty(), "expected defense seeds to generate moves for king shelter");
    }
}
