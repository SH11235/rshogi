use super::driver::{
    return_stack_cache, root_see_gate_should_skip, take_stack_cache, ClassicBackend,
};
use super::ordering::{EvalMoveGuard, Heuristics, MovePicker};
use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::api::{InfoEvent, InfoEventCallback};
use crate::search::config;
use crate::search::constants::{mate_distance, MIN_QNODES_LIMIT, SEARCH_INF};
use crate::search::tt::TTProbe;
use crate::search::types::{normalize_root_pv, RootLine, SearchResult};
use crate::search::SearchLimits;
use crate::shogi::{Color, Move, Piece, PieceType, Position, Square};
use smallvec::SmallVec;
use std::collections::HashSet;
use std::time::Instant;

const MAX_VERIFY_CANDIDATES: usize = 4;
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
}

impl VerifyFailReason {
    fn as_str(&self) -> (&'static str, i32, Option<PieceType>) {
        match *self {
            VerifyFailReason::SelfSee(see) => ("self_see", see, None),
            VerifyFailReason::OppXsee { piece, score } => ("opp_xsee_neg", score, Some(piece)),
            VerifyFailReason::PawnDropHead { piece } => ("opp_drop_head", 0, Some(piece)),
            VerifyFailReason::EvalDrop(delta) => ("eval_drop", delta, None),
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

    let fallback_moves = if root_move_order.is_empty() {
        generate_move_order(backend, root)
    } else {
        root_move_order.to_vec()
    };
    let mut candidates = collect_candidates(result, &fallback_moves, required);
    if candidates.is_empty() {
        candidates.push(Candidate {
            mv: result.best_move.unwrap(),
            line_index: None,
        });
    }
    let mut accepted: Option<(Candidate, ProbeReport)> = None;
    let mut fallback: Option<(Candidate, ProbeReport)> = None;
    for cand in candidates.iter().take(MAX_VERIFY_CANDIDATES) {
        summary.checked += 1;
        let res = verify_candidate(backend, root, best_score, cand.mv, &settings);
        summary.total_elapsed_ms = summary.total_elapsed_ms.saturating_add(res.report.elapsed_ms);
        if let Some(reason) = res.fail_reason {
            summary.fail_count = summary.fail_count.saturating_add(1);
            emit_fail_log(info, cand.mv, &reason);
            if fallback.as_ref().map(|(_, rep)| rep.eval < res.report.eval).unwrap_or(true) {
                fallback = Some((*cand, res.report));
            }
        } else {
            emit_pass_log(info, cand.mv, res.report.eval, best_score, win_protect_active);
            accepted = Some((*cand, res.report));
            break;
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
    let mut child = root.clone();
    let eval_guard = EvalMoveGuard::new(backend.evaluator.as_ref(), root, mv);
    child.do_move(mv);
    if let Some(reason) = detect_major_threat(&child, root.side_to_move, settings.opp_see_min_cp) {
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

fn detect_major_threat(pos: &Position, us: Color, threshold: i32) -> Option<VerifyFailReason> {
    let mut friendly = pos.board.occupied_bb[us as usize];
    while let Some(sq) = friendly.pop_lsb() {
        let Some(piece) = pos.board.piece_on(sq) else {
            continue;
        };
        if !is_major(piece) {
            continue;
        }
        if let Some(loss) = worst_capture_loss(pos, sq, us) {
            if loss <= -threshold {
                return Some(VerifyFailReason::OppXsee {
                    piece: piece.piece_type,
                    score: loss,
                });
            }
        }
        if pawn_drop_head_threat(pos, sq, us) {
            return Some(VerifyFailReason::PawnDropHead {
                piece: piece.piece_type,
            });
        }
    }
    None
}

fn is_major(piece: Piece) -> bool {
    matches!(piece.piece_type, PieceType::Rook | PieceType::Bishop | PieceType::Gold)
        || (piece.piece_type == PieceType::Pawn && piece.promoted)
}

fn worst_capture_loss(pos: &Position, target: Square, us: Color) -> Option<i32> {
    let enemy = us.opposite();
    let mut attackers = pos.get_attackers_to(target, enemy);
    if attackers.is_empty() {
        return None;
    }
    let mut worst: Option<i32> = None;
    while let Some(from) = attackers.pop_lsb() {
        if let Some(piece) = pos.board.piece_on(from) {
            let mut consider = Vec::with_capacity(2);
            consider.push(Move::normal(from, target, false));
            if piece.piece_type.can_promote() && can_promote(piece.color, from, target) {
                consider.push(Move::normal(from, target, true));
            }
            for mv in consider {
                if !pos.is_legal_move(mv) {
                    continue;
                }
                let gain = pos.see(mv);
                let loss = -gain;
                if loss < worst.unwrap_or(0) {
                    worst = Some(loss);
                }
            }
        }
    }
    worst
}

fn pawn_drop_head_threat(pos: &Position, target: Square, us: Color) -> bool {
    let enemy = us.opposite();
    let Some(head) = head_square(target, us) else {
        return false;
    };
    if pos.board.piece_on(head).is_some() {
        return false;
    }
    let Some(hand_idx) = PieceType::Pawn.hand_index() else {
        return false;
    };
    if pos.hands[enemy as usize][hand_idx] == 0 {
        return false;
    }
    let mv = Move::drop(PieceType::Pawn, head);
    pos.is_legal_move(mv)
}

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

fn collect_candidates(result: &SearchResult, fallback: &[Move], required: usize) -> Vec<Candidate> {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut list: Vec<Candidate> = Vec::new();
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
    for mv in fallback {
        if list.len() >= MAX_VERIFY_CANDIDATES {
            break;
        }
        if seen.insert(mv.to_u32()) {
            list.push(Candidate {
                mv: *mv,
                line_index: None,
            });
        }
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

fn emit_fail_log(info: Option<&InfoEventCallback>, mv: Move, reason: &VerifyFailReason) {
    if let Some(cb) = info {
        let (tag, value, piece) = reason.as_str();
        let mut extra = String::new();
        if let Some(pt) = piece {
            extra = format!(" piece={:?}", pt);
        }
        cb(InfoEvent::String(format!(
            "root_verify fail after={} reason={} delta={}{}",
            crate::usi::move_to_usi(&mv),
            tag,
            value,
            extra
        )));
    }
}

fn emit_pass_log(info: Option<&InfoEventCallback>, mv: Move, eval: i32, base: i32, win_mode: bool) {
    if let Some(cb) = info {
        cb(InfoEvent::String(format!(
            "root_verify pass after={} score={} delta={} win_protect={}",
            crate::usi::move_to_usi(&mv),
            eval,
            eval - base,
            win_mode as u8
        )));
    }
}

struct VerifyResult {
    report: ProbeReport,
    fail_reason: Option<VerifyFailReason>,
}
