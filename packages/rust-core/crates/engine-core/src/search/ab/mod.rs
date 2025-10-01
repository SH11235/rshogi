use std::sync::mpsc;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;

use crate::evaluation::evaluate::Evaluator;
use crate::movegen::MoveGenerator;
use crate::search::api::{BackendSearchTask, InfoEvent, InfoEventCallback, SearcherBackend};
use crate::search::history::{ButterflyHistory, CounterMoveHistory};
use crate::search::params as dynp;
use crate::search::params::{
    NMP_BASE_R, NMP_BONUS_DELTA_BETA, NMP_HAND_SUM_DISABLE, NMP_MIN_DEPTH, QS_MARGIN_CAPTURE,
    QS_MAX_QUIET_CHECKS, QS_PROMOTE_BONUS,
};
use crate::search::tt::TTProbe;
use crate::search::types::SearchStack;
use crate::search::types::{NodeType, RootLine};
use crate::search::TranspositionTable;
use crate::search::{SearchLimits, SearchResult, SearchStats};
use crate::Position;
use smallvec::SmallVec;
use std::time::{Duration, Instant};

// 引数過多関数 (qsearch / alphabeta) の Clippy 警告回避用に集約構造体をモジュールスコープで定義
struct QSearchArgs<'a> {
    pos: &'a Position,
    alpha: i32,
    beta: i32,
    limits: &'a SearchLimits,
    start_time: &'a Instant,
    nodes: &'a mut u64,
    seldepth: &'a mut u32,
    ply: u32,
}

struct ABArgs<'a> {
    pos: &'a Position,
    depth: i32,
    alpha: i32,
    beta: i32,
    limits: &'a SearchLimits,
    start_time: &'a Instant,
    nodes: &'a mut u64,
    seldepth: &'a mut u32,
    ply: u32,
    // フル窓で探索されたPVノードかどうか（TT保存の優遇に使用）
    is_pv: bool,
    stack: &'a mut [SearchStack],
    heur: &'a mut Heuristics,
    tt_hits: &'a mut u64,
    beta_cuts: &'a mut u64,
    lmr_counter: &'a mut u64,
}

#[derive(Clone)]
pub struct ClassicBackend<E: Evaluator + Send + Sync + 'static> {
    evaluator: Arc<E>,
    tt: Option<Arc<TranspositionTable>>, // 共有TT（Hashfull出力用、将来はprobe/storeでも使用）
    toggles: PruneToggles,
}

#[derive(Default)]
struct Heuristics {
    history: ButterflyHistory,
    counter: CounterMoveHistory,
    // diagnostics counters
    lmr_trials: u64,
    lmr_applied: u64,
}

#[derive(Clone, Copy)]
pub struct PruneToggles {
    pub enable_iid: bool,
    pub enable_nmp: bool,
}

impl Default for PruneToggles {
    fn default() -> Self {
        Self {
            enable_iid: true,
            enable_nmp: true,
        }
    }
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    pub fn new(evaluator: Arc<E>) -> Self {
        Self {
            evaluator,
            tt: None,
            toggles: PruneToggles::default(),
        }
    }

    pub fn with_tt(evaluator: Arc<E>, tt: Arc<TranspositionTable>) -> Self {
        Self {
            evaluator,
            tt: Some(tt),
            toggles: PruneToggles::default(),
        }
    }

    pub fn with_tt_and_toggles(
        evaluator: Arc<E>,
        tt: Arc<TranspositionTable>,
        toggles: PruneToggles,
    ) -> Self {
        Self {
            evaluator,
            tt: Some(tt),
            toggles,
        }
    }

    fn should_stop(limits: &SearchLimits) -> bool {
        if let Some(flag) = &limits.stop_flag {
            return flag.load(Ordering::Relaxed);
        }
        false
    }

    /// Quiescence search (captures + checks + promising promotions only)
    fn qsearch(&self, args: QSearchArgs) -> i32 {
        let QSearchArgs {
            pos,
            mut alpha,
            beta,
            limits,
            start_time,
            nodes,
            seldepth,
            ply,
        } = args;
        // Hard recursion guard to prevent stack overflow in extreme positions
        if (ply as u16) >= crate::search::constants::MAX_QUIESCE_DEPTH {
            return alpha;
        }
        // Basic time check
        if let Some(limit) = limits.time_limit() {
            if start_time.elapsed() >= limit {
                return alpha;
            }
        }
        if Self::should_stop(limits) {
            return alpha;
        }

        *nodes += 1;
        if ply > *seldepth {
            *seldepth = ply;
        }

        // If in check, generate full legal moves (evasion qsearch)
        if pos.is_in_check() {
            let mg = MoveGenerator::new();
            let Ok(list) = mg.generate_all(pos) else {
                return self.evaluator.evaluate(pos);
            };
            let mut has_legal = false;
            for mv in list.as_slice().iter().copied() {
                if !pos.is_legal_move(mv) {
                    continue;
                }
                has_legal = true;
                let mut child = pos.clone();
                child.do_move(mv);
                let sc = -self.qsearch(QSearchArgs {
                    pos: &child,
                    alpha: -beta,
                    beta: -alpha,
                    limits,
                    start_time,
                    nodes,
                    seldepth,
                    ply: ply + 1,
                });
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
            }
            if !has_legal {
                return crate::search::mate_score(ply as u8, false);
            }
            return alpha;
        }

        // Stand pat
        let stand_pat = self.evaluator.evaluate(pos);
        if stand_pat >= beta {
            return stand_pat;
        }
        if stand_pat > alpha {
            alpha = stand_pat;
        }

        // Generate capture moves
        let mg = MoveGenerator::new();
        let Ok(captures) = mg.generate_captures(pos) else {
            return alpha;
        };

        // Basic delta pruning for captures（params管理）
        // QS_MARGIN_CAPTURE / QS_PROMOTE_BONUS を利用

        // Score captures by SEE (descending)
        let mut caps: Vec<(crate::shogi::Move, i32)> =
            captures.as_slice().iter().copied().map(|m| (m, pos.see(m))).collect();
        caps.sort_unstable_by(|a, b| b.1.cmp(&a.1));

        // First search good captures (SEE >= 0)
        for (mv, _see) in caps.iter().copied().filter(|&(_, s)| s >= 0) {
            // Delta pruning: if even best-case can't raise alpha, skip
            let captured_val = mv
                .captured_piece_type()
                .map(|pt| crate::shogi::piece_constants::SEE_PIECE_VALUES[0][pt as usize])
                .unwrap_or(0);
            let best_gain = stand_pat + captured_val + QS_PROMOTE_BONUS + QS_MARGIN_CAPTURE;
            if best_gain <= alpha {
                continue;
            }

            let mut child = pos.clone();
            child.do_move(mv);
            let sc = -self.qsearch(QSearchArgs {
                pos: &child,
                alpha: -beta,
                beta: -alpha,
                limits,
                start_time,
                nodes,
                seldepth,
                ply: ply + 1,
            });
            if sc >= beta {
                return sc;
            }
            if sc > alpha {
                alpha = sc;
            }
        }

        // Then try quiet checking moves (limit count to keep qsearch bounded)
        let Ok(quiet) = mg.generate_quiet(pos) else {
            return alpha;
        };
        let mut tried_checks = 0usize;
        for mv in quiet.as_slice().iter().copied() {
            if tried_checks >= QS_MAX_QUIET_CHECKS {
                break;
            }
            if pos.gives_check(mv) {
                tried_checks += 1;
                let mut child = pos.clone();
                child.do_move(mv);
                let sc = -self.qsearch(QSearchArgs {
                    pos: &child,
                    alpha: -beta,
                    beta: -alpha,
                    limits,
                    start_time,
                    nodes,
                    seldepth,
                    ply: ply + 1,
                });
                if sc >= beta {
                    return sc;
                }
                if sc > alpha {
                    alpha = sc;
                }
            }
        }

        // Finally, (optionally) try bad captures if they might still raise alpha
        for (mv, _see) in caps.into_iter().filter(|&(_, s)| s < 0) {
            // Only consider if it gives check or captures a high-value piece
            let captured_val = mv
                .captured_piece_type()
                .map(|pt| crate::shogi::piece_constants::SEE_PIECE_VALUES[0][pt as usize])
                .unwrap_or(0);
            if captured_val < 500 && !pos.gives_check(mv) {
                continue;
            }
            let mut child = pos.clone();
            child.do_move(mv);
            let sc = -self.qsearch(QSearchArgs {
                pos: &child,
                alpha: -beta,
                beta: -alpha,
                limits,
                start_time,
                nodes,
                seldepth,
                ply: ply + 1,
            });
            if sc >= beta {
                return sc;
            }
            if sc > alpha {
                alpha = sc;
            }
        }

        alpha
    }

    /// PVSなしの基本Negamax αβ。内部ノードの順序付けを簡易実装。
    fn alphabeta(&self, args: ABArgs) -> (i32, Option<crate::shogi::Move>) {
        let ABArgs {
            pos,
            depth,
            mut alpha,
            beta,
            limits,
            start_time,
            nodes,
            seldepth,
            ply,
            is_pv,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
        } = args;
        // Safety: guard against pathological recursion beyond MAX_PLY
        if (ply as usize) >= crate::search::constants::MAX_PLY {
            let eval = self.evaluator.evaluate(pos);
            return (eval, None);
        }
        // Basic time check
        if let Some(limit) = limits.time_limit() {
            if start_time.elapsed() >= limit {
                let eval = self.evaluator.evaluate(pos);
                return (eval, None);
            }
        }
        if Self::should_stop(limits) {
            return (0, None);
        }
        *nodes += 1;
        if ply > *seldepth {
            *seldepth = ply;
        }
        if depth <= 0 {
            let qs = self.qsearch(QSearchArgs {
                pos,
                alpha,
                beta,
                limits,
                start_time,
                nodes,
                seldepth,
                ply,
            });
            return (qs, None);
        }

        let orig_alpha = alpha;
        let orig_beta = beta;
        // Optional: static eval for pruning/TT store
        let static_eval = self.evaluator.evaluate(pos);
        // Keep static eval on stack for improving判定などに利用
        stack[ply as usize].static_eval = Some(static_eval);

        // Mate Distance Pruning (conservative)
        let mut a_md = alpha;
        let mut b_md = beta;
        if crate::search::mate_distance_pruning(&mut a_md, &mut b_md, ply as u8) {
            return (a_md, None);
        }
        alpha = a_md;

        // Static Beta Pruning (non-PV shallow)
        if depth <= 2 && !pos.is_in_check() {
            let margin = if depth == 1 {
                dynp::sbp_margin_d1()
            } else {
                dynp::sbp_margin_d2()
            };
            if static_eval - margin >= beta {
                return (static_eval, None);
            }
        }

        // Razor: depth==1 quick qsearch probe
        if dynp::razor_enabled() && depth == 1 && !pos.is_in_check() {
            let r = self.qsearch(QSearchArgs {
                pos,
                alpha,
                beta: alpha + 1,
                limits,
                start_time,
                nodes,
                seldepth,
                ply,
            });
            if r <= alpha {
                return (r, None);
            }
        }

        // Null Move Pruning (NMP): 非PV相当、チェックなし、prev_nullでない、深さ≥3、手駒多すぎない
        // R = 2 + depth/4 (+1 if static_eval - beta > 150)
        if self.toggles.enable_nmp {
            // prev null guard
            let prev_null = if ply > 0 {
                stack[(ply - 1) as usize].null_move
            } else {
                false
            };
            if depth >= NMP_MIN_DEPTH && !pos.is_in_check() && !prev_null {
                // 手駒合計が閾値以上なら無効化（打ち駒で評価変動が大きいため）
                let side = pos.side_to_move as usize;
                let hand_sum: i32 = pos.hands[side].iter().map(|&c| c as i32).sum();
                if hand_sum < NMP_HAND_SUM_DISABLE {
                    let bonus = if static_eval - beta > NMP_BONUS_DELTA_BETA {
                        1
                    } else {
                        0
                    };
                    let r = NMP_BASE_R + (depth / 4) + bonus;
                    let r = r.min(depth - 1); // 下限確保
                                              // do null move
                    let mut child = pos.clone();
                    // Evaluator hook（必要なら）
                    self.evaluator.on_do_null_move(&child);
                    let undo_null = child.do_null_move();
                    stack[ply as usize].null_move = true;
                    let (sc, _) = self.alphabeta(ABArgs {
                        pos: &child,
                        depth: depth - 1 - r,
                        alpha: -(beta),
                        beta: -(beta - 1),
                        limits,
                        start_time,
                        nodes,
                        seldepth,
                        ply: ply + 1,
                        is_pv: false,
                        stack,
                        heur,
                        tt_hits,
                        beta_cuts,
                        lmr_counter,
                    });
                    // undo
                    let mut child2 = child; // move child
                    child2.undo_null_move(undo_null);
                    stack[ply as usize].null_move = false;
                    let score = -sc;
                    if score >= beta {
                        return (score, None);
                    }
                }
            }
        }

        // TT probe (cut or move hint)
        let mut tt_hint: Option<crate::shogi::Move> = None;
        let mut tt_depth_ok = false;
        if let Some(tt) = &self.tt {
            // Lightweight prefetch before probe
            if depth >= 3 {
                tt.prefetch_l2(pos.zobrist_hash, pos.side_to_move);
            }
            if let Some(entry) = tt.probe(pos.zobrist_hash, pos.side_to_move) {
                *tt_hits += 1;
                // Adjust mate score from root-relative to current ply
                let stored = entry.score() as i32;
                let score = crate::search::common::adjust_mate_score_from_tt(stored, ply as u8);
                let sufficient = entry.depth() as i32 >= depth;
                tt_depth_ok = entry.depth() as i32 >= depth - 2;
                match entry.node_type() {
                    NodeType::LowerBound if sufficient && score >= beta => {
                        return (score, entry.get_move());
                    }
                    NodeType::UpperBound if sufficient && score <= alpha => {
                        return (score, entry.get_move());
                    }
                    NodeType::Exact if sufficient => {
                        return (score, entry.get_move());
                    }
                    _ => {
                        tt_hint = entry.get_move();
                    }
                }
            }
        }

        // Internal Iterative Deepening (IID): depth≥6・非王手・TT手不在 or TTが浅い
        if self.toggles.enable_iid
            && depth >= dynp::iid_min_depth()
            && !pos.is_in_check()
            && (!tt_depth_ok || tt_hint.is_none())
        {
            let iid_depth = depth - 2;
            let (_s, _mv) = self.alphabeta(ABArgs {
                pos,
                depth: iid_depth,
                alpha,
                beta,
                limits,
                start_time,
                nodes,
                seldepth,
                ply,
                is_pv: false,
                stack,
                heur,
                tt_hits,
                beta_cuts,
                lmr_counter,
            });
            // re-probe TT for hint
            if let Some(tt) = &self.tt {
                if let Some(entry) = tt.probe(pos.zobrist_hash, pos.side_to_move) {
                    tt_hint = entry.get_move();
                }
            }
        }

        // ProbCut: try shallow cut above beta with promising captures (negamax window)
        if depth >= 5 && !pos.is_in_check() {
            let threshold = beta + dynp::probcut_margin(depth);
            let mgp = MoveGenerator::new();
            if let Ok(caps) = mgp.generate_captures(pos) {
                for mv in caps.as_slice().iter().copied() {
                    if pos.see(mv) < 0 {
                        continue;
                    }
                    let mut child = pos.clone();
                    child.do_move(mv);
                    // 子は [-T, -T+1] のナロウ窓で叩く
                    let (sc, _) = self.alphabeta(ABArgs {
                        pos: &child,
                        depth: depth - 2,
                        alpha: -threshold,
                        beta: -threshold + 1,
                        limits,
                        start_time,
                        nodes,
                        seldepth,
                        ply: ply + 1,
                        is_pv: false,
                        stack,
                        heur,
                        tt_hits,
                        beta_cuts,
                        lmr_counter,
                    });
                    let parent_sc = -sc;
                    if parent_sc >= threshold {
                        return (parent_sc, Some(mv));
                    }
                }
            }
        }

        let mg = MoveGenerator::new();
        let Ok(list) = mg.generate_all(pos) else {
            let qs = self.qsearch(QSearchArgs {
                pos,
                alpha,
                beta,
                limits,
                start_time,
                nodes,
                seldepth,
                ply,
            });
            return (qs, None);
        };
        let mut moves: Vec<(crate::shogi::Move, i32)> = list
            .as_slice()
            .iter()
            .copied()
            .map(|m| {
                // Stage風: TT(後で加点) > 王手(3) > 良捕獲(2) > Quiet(1) > 悪捕獲(0)
                let is_check = pos.gives_check(m) as i32;
                let is_capture = m.is_capture_hint();
                let see = if is_capture { pos.see(m) } else { 0 };
                let promo = m.is_promote() as i32;
                let stage = if is_check == 1 {
                    3
                } else if is_capture && see >= 0 {
                    2
                } else if is_capture && see < 0 {
                    0
                } else {
                    1
                };
                let mut key = stage * 100_000 + is_check * 10_000 + see * 10 + promo;
                // Killer boost
                let ss = &stack[ply as usize];
                if ss.is_killer(m) {
                    key += 50_000;
                }
                // Counter move boost
                if ply > 0 {
                    if let Some(prev_mv) = stack[(ply - 1) as usize].current_move {
                        if let Some(cm) = heur.counter.get(pos.side_to_move, prev_mv) {
                            if cm.equals_without_piece_type(&m) {
                                key += 60_000;
                            }
                        }
                    }
                }
                // History bonus
                key += heur.history.get(pos.side_to_move, m);
                (m, key)
            })
            .collect();
        // TT手を最優先に（存在すれば巨大ボーナス）
        if let Some(ttm) = tt_hint {
            for (m, key) in &mut moves {
                if m.equals_without_piece_type(&ttm) {
                    *key += 1_000_000; // 十分大きなボーナス
                }
            }
        }
        moves.sort_unstable_by(|a, b| b.1.cmp(&a.1));

        // Clear per-ply state
        stack[ply as usize].clear_for_new_node();
        let mut best_mv = None;
        let mut best = i32::MIN / 2;
        let mut moveno: usize = 0;
        let mut first_move_done = false;
        for (mv, _key) in moves.into_iter() {
            if !pos.is_legal_move(mv) {
                continue;
            }
            moveno += 1;
            stack[ply as usize].current_move = Some(mv);
            // LMP: 浅い静止手の遅手スキップ（非PV前提）
            let gives_check = pos.gives_check(mv);
            let is_capture = mv.is_capture_hint();
            let see = if is_capture { pos.see(mv) } else { 0 };
            let is_good_capture = is_capture && see >= 0;
            let is_quiet = !is_capture && !gives_check;

            // History Pruning (HP): 低historyの静止手を浅層でスキップ（killer/counterは残す）
            if depth <= 3 && is_quiet {
                let h = heur.history.get(pos.side_to_move, mv);
                let mut is_counter = false;
                if ply > 0 {
                    if let Some(prev_mv) = stack[(ply - 1) as usize].current_move {
                        if let Some(cm) = heur.counter.get(pos.side_to_move, prev_mv) {
                            if cm.equals_without_piece_type(&mv) {
                                is_counter = true;
                            }
                        }
                    }
                }
                if h < dynp::hp_threshold() && !stack[ply as usize].is_killer(mv) && !is_counter {
                    continue;
                }
            }

            if depth <= 3 && is_quiet {
                let limit = dynp::lmp_limit_for_depth(depth);
                if moveno > limit {
                    continue;
                }
            }
            let mut child = pos.clone();
            child.do_move(mv);
            // LMR: 非王手・非良捕獲・moveno>=3 の静止手を減深（PV/王手/improvingで調整）
            let mut next_depth = depth - 1;
            if depth >= 3 && moveno >= 3 && is_quiet && !is_good_capture {
                heur.lmr_trials = heur.lmr_trials.saturating_add(1);
                let rd = ((depth as f32).ln() * (moveno as f32).ln() / dynp::lmr_k_coeff()).floor()
                    as i32;
                let mut r = rd.max(1);
                if is_pv {
                    r -= 1;
                } // PVは控えめ
                if gives_check {
                    r = 0;
                } // 王手は減深しない
                  // improving: ply>=2 かつ current_eval >= prev2_eval - 10cp
                let improving = if ply >= 2 {
                    if let Some(prev2) = stack[ply as usize - 2].static_eval {
                        static_eval >= prev2 - 10
                    } else {
                        false
                    }
                } else {
                    false
                };
                if improving {
                    r -= 1;
                }
                r = r.clamp(0, depth - 1);
                if r > 0 {
                    next_depth -= r;
                    *lmr_counter += 1;
                    heur.lmr_applied = heur.lmr_applied.saturating_add(1);
                }
            }
            // 内部PVS: 先頭手フル、以降はnull-window→必要時フル再探索
            let score = if !first_move_done {
                let (sc, _) = self.alphabeta(ABArgs {
                    pos: &child,
                    depth: next_depth,
                    alpha: -beta,
                    beta: -alpha,
                    limits,
                    start_time,
                    nodes,
                    seldepth,
                    ply: ply + 1,
                    is_pv: true,
                    stack,
                    heur,
                    tt_hits,
                    beta_cuts,
                    lmr_counter,
                });
                first_move_done = true;
                -sc
            } else {
                let (sc_nw, _) = self.alphabeta(ABArgs {
                    pos: &child,
                    depth: next_depth,
                    alpha: -(alpha + 1),
                    beta: -alpha,
                    limits,
                    start_time,
                    nodes,
                    seldepth,
                    ply: ply + 1,
                    is_pv: false,
                    stack,
                    heur,
                    tt_hits,
                    beta_cuts,
                    lmr_counter,
                });
                let mut s = -sc_nw;
                if s > alpha && s < beta {
                    let (sc_fw, _) = self.alphabeta(ABArgs {
                        pos: &child,
                        depth: next_depth,
                        alpha: -beta,
                        beta: -alpha,
                        limits,
                        start_time,
                        nodes,
                        seldepth,
                        ply: ply + 1,
                        is_pv: true,
                        stack,
                        heur,
                        tt_hits,
                        beta_cuts,
                        lmr_counter,
                    });
                    s = -sc_fw;
                }
                s
            };
            if score > best {
                best = score;
                best_mv = Some(mv);
            }
            if score > alpha {
                alpha = score;
            }
            if alpha >= beta {
                *beta_cuts += 1;
                if is_quiet {
                    stack[ply as usize].update_killers(mv);
                    heur.history.update_good(pos.side_to_move, mv, depth);
                    if ply > 0 {
                        if let Some(prev_mv) = stack[(ply - 1) as usize].current_move {
                            heur.counter.update(pos.side_to_move, prev_mv, mv);
                        }
                    }
                }
                break;
            }
            if is_quiet {
                stack[ply as usize].quiet_moves.push(mv);
            }
        }
        if best == i32::MIN / 2 {
            let qs = self.qsearch(QSearchArgs {
                pos,
                alpha,
                beta,
                limits,
                start_time,
                nodes,
                seldepth,
                ply,
            });
            // 深さ0扱いのためTT保存は行わない
            (qs, None)
        } else {
            // TT保存（PV/非PV情報を付与）
            if let Some(tt) = &self.tt {
                let node_type = if best <= orig_alpha {
                    NodeType::UpperBound
                } else if best >= orig_beta {
                    NodeType::LowerBound
                } else {
                    NodeType::Exact
                };
                let store_score = crate::search::common::adjust_mate_score_for_tt(best, ply as u8);
                let mut args = crate::search::tt::TTStoreArgs::new(
                    pos.zobrist_hash,
                    best_mv,
                    store_score as i16,
                    static_eval as i16,
                    depth as u8,
                    node_type,
                    pos.side_to_move,
                );
                args.is_pv = is_pv;
                tt.store(args);
            }
            // Apply history penalties for quiets that didn't cut
            for &qmv in &stack[ply as usize].quiet_moves {
                if Some(qmv) != best_mv {
                    heur.history.update_bad(pos.side_to_move, qmv, depth);
                }
            }
            (best, best_mv)
        }
    }

    /// TTに格納されたEXACTエントリからPVを復元できる場合は優先的に返す。
    /// 先頭手が `first` と一致しない場合や鎖が途切れた場合は `None` を返す。
    fn reconstruct_root_pv_from_tt(
        &self,
        root: &Position,
        depth: i32,
        first: crate::shogi::Move,
    ) -> Option<SmallVec<[crate::shogi::Move; 32]>> {
        let tt = self.tt.as_ref()?;
        if depth <= 0 {
            return None;
        }

        let mut pos = root.clone();
        let max_depth = depth.clamp(0, crate::search::constants::MAX_PLY as i32) as u8;
        let mut pv_vec = tt.reconstruct_pv_from_tt(&mut pos, max_depth);
        if pv_vec.is_empty() {
            return None;
        }

        if !pv_vec[0].equals_without_piece_type(&first) {
            return None;
        }

        pv_vec.truncate(32);
        Some(SmallVec::from_vec(pv_vec))
    }

    /// 既知のbest手からPVを構築するための軽量再探索（fail-soft）。
    /// 速度優先のため、各手でフル窓で1回だけ探索してbest手を辿る。
    fn extract_pv(
        &self,
        root: &Position,
        depth: i32,
        first: crate::shogi::Move,
        limits: &SearchLimits,
        nodes: &mut u64,
    ) -> SmallVec<[crate::shogi::Move; 32]> {
        let mut pv: SmallVec<[crate::shogi::Move; 32]> = SmallVec::new();
        let mut pos = root.clone();
        let mut d = depth;
        let mut seldepth_dummy = 0u32; // PV抽出ではseldepthを更新しない
        let mut stack = vec![SearchStack::default(); crate::search::constants::MAX_PLY + 1];
        let mut heur = Heuristics::default();
        let mut _tt_hits: u64 = 0;
        let mut _beta_cuts: u64 = 0;
        let mut _lmr_counter: u64 = 0;

        let mut first_used = false;
        let t0 = Instant::now();
        while d > 0 {
            let mv = if !first_used {
                first
            } else {
                let (_sc, mv_opt) = self.alphabeta(ABArgs {
                    pos: &pos,
                    depth: d,
                    alpha: i32::MIN / 2,
                    beta: i32::MAX / 2,
                    limits,
                    start_time: &t0,
                    nodes,
                    seldepth: &mut seldepth_dummy,
                    ply: 0,
                    is_pv: true,
                    stack: &mut stack,
                    heur: &mut heur,
                    tt_hits: &mut _tt_hits,
                    beta_cuts: &mut _beta_cuts,
                    lmr_counter: &mut _lmr_counter,
                });
                match mv_opt {
                    Some(m) => m,
                    None => break,
                }
            };
            first_used = true;
            pv.push(mv);
            pos.do_move(mv);
            d -= 1;
        }
        pv
    }

    fn iterative(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<&InfoEventCallback>,
    ) -> SearchResult {
        let max_depth = limits.depth_limit_u8() as i32;
        let mut best: Option<crate::shogi::Move> = None;
        let mut best_score = 0;
        let mut nodes: u64 = 0;
        let t0 = Instant::now();
        let _last_hashfull_emit_ms = 0u64;
        let mut prev_score = 0;
        // Aspiration initial params
        const ASP_DELTA0: i32 = 30;
        const ASP_DELTA_MAX: i32 = 350;

        // Cumulative counters for diagnostics
        let mut cum_tt_hits: u64 = 0;
        let mut cum_beta_cuts: u64 = 0;
        let mut cum_lmr_counter: u64 = 0;
        let mut cum_lmr_trials: u64 = 0;
        let mut stats_hint_exists: u64 = 0;
        let mut stats_hint_used: u64 = 0;

        let mut final_lines: Option<SmallVec<[RootLine; 4]>> = None;
        for d in 1..=max_depth {
            if Self::should_stop(limits) {
                break;
            }
            let mut seldepth: u32 = 0;
            // Build root move list for CurrMove events and basic ordering
            let mg = MoveGenerator::new();
            let Ok(list) = mg.generate_all(root) else {
                break;
            };
            let mut root_moves: Vec<(crate::shogi::Move, i32)> = list
                .as_slice()
                .iter()
                .copied()
                .map(|m| {
                    let is_check = root.gives_check(m) as i32;
                    let see = if m.is_capture_hint() { root.see(m) } else { 0 };
                    let promo = m.is_promote() as i32;
                    let key = is_check * 10_000 + see * 10 + promo;
                    (m, key)
                })
                .collect();
            // Root TT hint boost（存在すれば大ボーナス）
            let mut root_tt_hint_mv: Option<crate::shogi::Move> = None;
            if let Some(tt) = &self.tt {
                tt.prefetch_l2(root.zobrist_hash, root.side_to_move);
                if let Some(entry) = tt.probe(root.zobrist_hash, root.side_to_move) {
                    if let Some(ttm) = entry.get_move() {
                        root_tt_hint_mv = Some(ttm);
                        for (m, key) in &mut root_moves {
                            if m.equals_without_piece_type(&ttm) {
                                *key += 1_000_000;
                            }
                        }
                    }
                }
            }
            root_moves.sort_unstable_by(|a, b| b.1.cmp(&a.1));
            let root_rank: Vec<crate::shogi::Move> = root_moves.iter().map(|(m, _)| *m).collect();

            // MultiPV（逐次選抜）
            let k = limits.multipv.max(1) as usize;
            let mut excluded: SmallVec<[crate::shogi::Move; 32]> = SmallVec::new();
            let mut depth_lines: SmallVec<[RootLine; 4]> = SmallVec::new();

            // Counters aggregate across PVs at this depth
            let mut depth_tt_hits: u64 = 0;
            let mut depth_beta_cuts: u64 = 0;
            let mut depth_lmr_counter: u64 = 0;
            let mut depth_lmr_trials: u64 = 0;
            let mut _local_best_for_next_iter: Option<(crate::shogi::Move, i32)> = None;
            let mut depth_hint_exists: u64 = 0;
            let mut depth_hint_used: u64 = 0;

            for pv_idx in 1..=k {
                // Aspiration window per PV head
                let mut alpha = if d == 1 {
                    i32::MIN / 2
                } else {
                    prev_score - ASP_DELTA0
                };
                let mut beta = if d == 1 {
                    i32::MAX / 2
                } else {
                    prev_score + ASP_DELTA0
                };
                let mut delta = ASP_DELTA0;

                // 検索用stack/heuristicsを初期化
                let mut stack = vec![SearchStack::default(); crate::search::constants::MAX_PLY + 1];
                let mut heur = Heuristics::default();
                let mut tt_hits: u64 = 0;
                let mut beta_cuts: u64 = 0;
                let mut lmr_counter: u64 = 0;
                let mut root_tt_hint_exists: u64 = 0;
                let mut root_tt_hint_used: u64 = 0;

                // 作業用root move配列（excludedを除外）
                let active_moves: Vec<(crate::shogi::Move, i32)> = root_moves
                    .iter()
                    .copied()
                    .filter(|(m, _)| !excluded.iter().any(|e| m.equals_without_piece_type(e)))
                    .collect();

                // 探索ループ（Aspiration）
                let mut local_best_mv = None;
                let mut local_best = i32::MIN / 2;
                loop {
                    // Root move loop with CurrMove events
                    for (idx, (mv, _)) in active_moves.iter().copied().enumerate() {
                        if let Some(limit) = limits.time_limit() {
                            if t0.elapsed() >= limit {
                                break;
                            }
                        }
                        if let Some(cb) = info {
                            let number = root_rank
                                .iter()
                                .position(|x| x.equals_without_piece_type(&mv))
                                .map(|pos| (pos as u32) + 1)
                                .unwrap_or((idx as u32) + 1);
                            cb(InfoEvent::CurrMove { mv, number });
                        }
                        let mut child = root.clone();
                        child.do_move(mv);
                        // Root-level PVS: first move full window, others null-window then re-search if needed
                        let score = if idx == 0 {
                            let (sc, _) = self.alphabeta(ABArgs {
                                pos: &child,
                                depth: d - 1,
                                alpha: -beta,
                                beta: -alpha,
                                limits,
                                start_time: &t0,
                                nodes: &mut nodes,
                                seldepth: &mut seldepth,
                                ply: 1,
                                is_pv: true,
                                stack: &mut stack,
                                heur: &mut heur,
                                tt_hits: &mut tt_hits,
                                beta_cuts: &mut beta_cuts,
                                lmr_counter: &mut lmr_counter,
                            });
                            -sc
                        } else {
                            let (sc_nw, _) = self.alphabeta(ABArgs {
                                pos: &child,
                                depth: d - 1,
                                alpha: -(alpha + 1),
                                beta: -alpha,
                                limits,
                                start_time: &t0,
                                nodes: &mut nodes,
                                seldepth: &mut seldepth,
                                ply: 1,
                                is_pv: false,
                                stack: &mut stack,
                                heur: &mut heur,
                                tt_hits: &mut tt_hits,
                                beta_cuts: &mut beta_cuts,
                                lmr_counter: &mut lmr_counter,
                            });
                            let mut s = -sc_nw;
                            if s > alpha && s < beta {
                                let (sc_fw, _) = self.alphabeta(ABArgs {
                                    pos: &child,
                                    depth: d - 1,
                                    alpha: -beta,
                                    beta: -alpha,
                                    limits,
                                    start_time: &t0,
                                    nodes: &mut nodes,
                                    seldepth: &mut seldepth,
                                    ply: 1,
                                    is_pv: true,
                                    stack: &mut stack,
                                    heur: &mut heur,
                                    tt_hits: &mut tt_hits,
                                    beta_cuts: &mut beta_cuts,
                                    lmr_counter: &mut lmr_counter,
                                });
                                s = -sc_fw;
                            }
                            s
                        };
                        if score > local_best {
                            local_best = score;
                            local_best_mv = Some(mv);
                        }
                        if score > alpha {
                            alpha = score;
                        }
                        if alpha >= beta {
                            break; // fail-high
                        }
                    }

                    if alpha < beta {
                        break; // success
                    }
                    // Aspiration failed → widen window and emit event
                    if let Some(cb) = info {
                        let outcome = if prev_score >= beta {
                            crate::search::api::AspirationOutcome::FailHigh
                        } else {
                            crate::search::api::AspirationOutcome::FailLow
                        };
                        cb(InfoEvent::Aspiration {
                            outcome,
                            old_alpha: alpha,
                            old_beta: beta,
                            new_alpha: alpha.saturating_sub(2 * delta),
                            new_beta: beta.saturating_add(2 * delta),
                        });
                    }
                    let new_alpha = alpha.saturating_sub(2 * delta);
                    let new_beta = beta.saturating_add(2 * delta);
                    alpha = new_alpha.max(i32::MIN / 2);
                    beta = new_beta.min(i32::MAX / 2);
                    delta = (delta * 2).min(ASP_DELTA_MAX);
                }

                // Counters aggregate
                depth_tt_hits = depth_tt_hits.saturating_add(tt_hits);
                depth_beta_cuts = depth_beta_cuts.saturating_add(beta_cuts);
                depth_lmr_counter = depth_lmr_counter.saturating_add(lmr_counter);
                depth_lmr_trials = depth_lmr_trials.saturating_add(heur.lmr_trials);

                // 発火: Depth / Hashfull（深さ1回の発火で十分）
                if pv_idx == 1 {
                    if let Some(cb) = info {
                        cb(InfoEvent::Depth {
                            depth: d as u32,
                            seldepth,
                        });
                        if let Some(tt) = &self.tt {
                            let hf = tt.hashfull_permille() as u32;
                            cb(InfoEvent::Hashfull(hf));
                        }
                    }
                }

                // PV 行の生成と発火
                if let Some(m) = local_best_mv {
                    // 次反復のAspiration用に pv_idx==1 を採用
                    if pv_idx == 1 {
                        best = Some(m);
                        best_score = local_best;
                        prev_score = local_best;
                        if let Some(hint) = root_tt_hint_mv {
                            root_tt_hint_exists = 1;
                            if m.equals_without_piece_type(&hint) {
                                root_tt_hint_used = 1;
                            }
                        }
                        depth_hint_exists = root_tt_hint_exists;
                        depth_hint_used = root_tt_hint_used;
                        _local_best_for_next_iter = Some((m, local_best));
                    }
                    // 可能ならTTからPVを復元し、だめなら軽量再探索へフォールバック
                    let mut pv = self.reconstruct_root_pv_from_tt(root, d, m).unwrap_or_default();
                    if pv.is_empty() {
                        let pv_ex = self.extract_pv(root, d, m, limits, &mut nodes);
                        if pv_ex.is_empty() {
                            pv.push(m);
                        } else {
                            pv = pv_ex;
                        }
                    }
                    let elapsed_ms = t0.elapsed().as_millis() as u64;
                    let nps = if elapsed_ms == 0 {
                        0
                    } else {
                        nodes.saturating_mul(1000).saturating_div(elapsed_ms.max(1))
                    };
                    let line = RootLine {
                        multipv_index: pv_idx as u8,
                        root_move: m,
                        score_internal: local_best,
                        score_cp: local_best,
                        bound: NodeType::Exact,
                        depth: d as u32,
                        seldepth: Some(seldepth as u8),
                        pv,
                        nodes: Some(nodes),
                        time_ms: Some(elapsed_ms),
                        nps: Some(nps),
                        exact_exhausted: false,
                        exhaust_reason: None,
                        mate_distance: None,
                    };
                    if let Some(cb) = info {
                        cb(InfoEvent::PV { line: line.clone() });
                    }
                    depth_lines.push(line);
                    // TT保存は 1行目のみ（Exact, PV=true）
                    if pv_idx == 1 {
                        if let (Some(tt), Some(best_mv_root)) = (&self.tt, best) {
                            let node_type = NodeType::Exact;
                            let store_score =
                                crate::search::common::adjust_mate_score_for_tt(best_score, 0);
                            let mut args = crate::search::tt::TTStoreArgs::new(
                                root.zobrist_hash,
                                Some(best_mv_root),
                                store_score as i16,
                                self.evaluator.evaluate(root) as i16,
                                d as u8,
                                node_type,
                                root.side_to_move,
                            );
                            args.is_pv = true;
                            tt.store(args);
                        }
                    }
                    // 除外へ追加
                    excluded.push(m);
                } else {
                    // 局面が詰み/手なし等でPVが取れない → 打ち切り
                    break;
                }
            }

            // 深さ集計を累積
            cum_tt_hits = cum_tt_hits.saturating_add(depth_tt_hits);
            cum_beta_cuts = cum_beta_cuts.saturating_add(depth_beta_cuts);
            cum_lmr_counter = cum_lmr_counter.saturating_add(depth_lmr_counter);
            cum_lmr_trials = cum_lmr_trials.saturating_add(depth_lmr_trials);

            // 反復ごとのrootヒント統計（最終反復で掲載）
            stats_hint_exists = depth_hint_exists;
            stats_hint_used = depth_hint_used;
            // この深さのMultiPV行を最終結果候補として保持
            final_lines = Some(depth_lines);

            let mut lead_ms = 10u64;
            if let Some(deadlines) = limits.fallback_deadlines {
                if deadlines.hard_limit_ms > 0 {
                    if deadlines.soft_limit_ms > 0
                        && deadlines.soft_limit_ms < deadlines.hard_limit_ms
                    {
                        let diff = deadlines.hard_limit_ms - deadlines.soft_limit_ms;
                        if diff > 0 {
                            lead_ms = lead_ms.max(diff);
                        }
                    }

                    let hard = Duration::from_millis(deadlines.hard_limit_ms);
                    if t0.elapsed() + Duration::from_millis(lead_ms) >= hard {
                        break;
                    }

                    continue;
                }
            }

            if let Some(limit) = limits.time_limit() {
                if t0.elapsed() + Duration::from_millis(lead_ms) >= limit {
                    break;
                }
            }
        }
        // stats は最終反復の集計値を使う
        let mut stats = SearchStats {
            nodes,
            ..Default::default()
        };
        stats.elapsed = t0.elapsed();
        stats.tt_hits = Some(cum_tt_hits);
        stats.lmr_count = Some(cum_lmr_counter);
        stats.lmr_trials = Some(cum_lmr_trials);
        stats.root_fail_high_count = Some(cum_beta_cuts);
        stats.root_tt_hint_exists = Some(stats_hint_exists);
        stats.root_tt_hint_used = Some(stats_hint_used);
        let mut result = SearchResult::new(best, best_score, stats);
        if let Some(lines) = final_lines {
            result.lines = Some(lines);
        }
        result.refresh_summary();
        if let Some(tt) = &self.tt {
            result.hashfull = tt.hashfull_permille() as u32;
        }
        result
    }
}

impl<E: Evaluator + Send + Sync + 'static> SearcherBackend for ClassicBackend<E> {
    fn start_async(
        self: Arc<Self>,
        root: Position,
        mut limits: SearchLimits,
        info: Option<InfoEventCallback>,
        active_counter: Arc<AtomicUsize>,
    ) -> BackendSearchTask {
        let stop_flag =
            limits.stop_flag.get_or_insert_with(|| Arc::new(AtomicBool::new(false))).clone();
        active_counter.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        let backend = self;
        let info_cb = info;
        let handle = thread::Builder::new()
            .name("classic-backend-search".into())
            .spawn({
                let counter = Arc::clone(&active_counter);
                move || {
                    struct Guard(Arc<AtomicUsize>);
                    impl Drop for Guard {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    let _guard = Guard(counter);
                    let result = backend.think_blocking(&root, &limits, info_cb.clone());
                    let _ = tx.send(result);
                }
            })
            .expect("spawn classic backend search thread");
        BackendSearchTask::new(stop_flag, rx, handle)
    }

    fn think_blocking(
        &self,
        root: &Position,
        limits: &SearchLimits,
        info: Option<InfoEventCallback>,
    ) -> SearchResult {
        self.iterative(root, limits, info.as_ref())
    }

    fn update_threads(&self, _n: usize) {}
    fn update_hash(&self, _mb: usize) {
        // Engine側でshared_tt再生成＋Backend再バインド方針のため未使用
    }
}
