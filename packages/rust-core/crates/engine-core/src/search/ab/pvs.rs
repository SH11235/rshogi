use crate::evaluation::evaluate::Evaluator;
use crate::search::constants::{TIME_CHECK_MASK_BYOYOMI, TIME_CHECK_MASK_NORMAL};
use crate::search::params as dynp;
use crate::search::tt::TTProbe;
use crate::search::types::SearchStack;
use crate::search::SearchLimits;
use crate::Position;
use smallvec::SmallVec;

use super::driver::ClassicBackend;
use super::ordering::{self, EvalMoveGuard, Heuristics, LateMoveReductionParams, MovePicker};
use super::pruning::{MaybeIidParams, NullMovePruneParams, ProbcutParams};
use crate::movegen::MoveGenerator;
use crate::search::policy::{
    abdada_enabled, capture_futility_enabled, capture_futility_scale_pct, quiet_see_guard_enabled,
    tt_suppress_below_depth,
};
use crate::search::types::NodeType;
use crate::shogi::piece_constants::SEE_PIECE_VALUES;
use std::sync::OnceLock;

#[cfg(feature = "diagnostics")]
use super::qsearch::record_qnodes_peak;

#[cfg(any(debug_assertions, feature = "diagnostics"))]
use super::diagnostics;

const QUIET_SEE_GUARD_CP_SCALE: i32 = 26;
const CAPTURE_FUT_BASE_CP: i32 = 232;
const CAPTURE_FUT_SLOPE_CP: i32 = 224;
const CAPTURE_FUT_MAX_DEPTH: i32 = 6;
const CAPTURE_SEE_BASE_CP: i32 = 96;
const CAPTURE_SEE_SLOPE_CP: i32 = 12;
const CAPTURE_SEE_GUARD_BASE_CP: i32 = 160;
const CAPTURE_SEE_GUARD_SLOPE_CP: i32 = 20;
const CAPTURE_SEE_GUARD_MAX_CP: i32 = 320;
const DROP_BAD_HISTORY_PENALTY_CP: i32 = 400;

/// Quiet SEE gate（YO Step14相当）
/// lmr_depth: LMR適用後の残り深さ（newDepth - r）。
pub(crate) fn quiet_see_guard_should_skip(
    pos: &Position,
    mv: crate::shogi::Move,
    lmr_depth: i32,
    is_pv: bool,
    is_quiet: bool,
    gives_check: bool,
) -> bool {
    if is_pv || !is_quiet || gives_check || !quiet_see_guard_enabled() {
        return false;
    }
    let d = lmr_depth.max(0);
    if d == 0 {
        return false;
    }
    let margin = QUIET_SEE_GUARD_CP_SCALE * d * d;
    !pos.see_ge(mv, -margin)
}

fn capture_fut_margin(depth: i32) -> i32 {
    let scale = capture_futility_scale_pct();
    let base = CAPTURE_FUT_BASE_CP * scale / 100;
    let slope = CAPTURE_FUT_SLOPE_CP * scale / 100;
    base + slope * depth.clamp(1, CAPTURE_FUT_MAX_DEPTH)
}

fn capture_see_margin(depth: i32) -> i32 {
    let scale = capture_futility_scale_pct();
    let base = CAPTURE_SEE_BASE_CP * scale / 100;
    let slope = CAPTURE_SEE_SLOPE_CP * scale / 100;
    base + slope * depth.clamp(1, CAPTURE_FUT_MAX_DEPTH + 2)
}

fn capture_victim_bonus(mv: crate::shogi::Move) -> i32 {
    mv.captured_piece_type().map(|pt| SEE_PIECE_VALUES[0][pt as usize]).unwrap_or(0)
}

fn capture_see_guard_margin(depth: i32) -> i32 {
    let d = depth.max(1);
    let base = CAPTURE_SEE_GUARD_BASE_CP + CAPTURE_SEE_GUARD_SLOPE_CP * d;
    // Cap the margin to a fixed maximum to avoid overly aggressive pruning at large depths.
    let cap = CAPTURE_SEE_GUARD_MAX_CP;
    base.min(cap)
}

pub(crate) fn capture_see_guard_should_skip(
    pos: &Position,
    mv: crate::shogi::Move,
    depth: i32,
    is_capture: bool,
    gives_check: bool,
) -> bool {
    if !(is_capture || gives_check) {
        return false;
    }
    let margin = capture_see_guard_margin(depth);
    !pos.see_ge(mv, -margin)
}

pub(crate) struct CaptureFutilityArgs {
    pub(crate) depth: i32,
    pub(crate) alpha: i32,
    pub(crate) static_eval: i32,
    pub(crate) is_capture: bool,
    pub(crate) gives_check: bool,
    pub(crate) prev_risky: bool,
}

pub(crate) fn capture_futility_should_skip(
    pos: &Position,
    mv: crate::shogi::Move,
    args: &CaptureFutilityArgs,
) -> bool {
    if !capture_futility_enabled()
        || args.depth > CAPTURE_FUT_MAX_DEPTH
        || pos.is_in_check()
        || args.prev_risky
    {
        return false;
    }
    use crate::search::constants::MATE_SCORE;
    if args.alpha.abs() >= MATE_SCORE - 100 {
        return false;
    }
    let mut futility_score = args.static_eval + capture_fut_margin(args.depth);
    if args.is_capture {
        futility_score += capture_victim_bonus(mv);
    }
    if futility_score <= args.alpha && !args.gives_check {
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        super::diagnostics::record_tag(
            pos,
            "cap_fut_skip",
            Some(format!(
                "depth={depth} eval={eval} fut_score={fut_score} alpha={alpha}",
                depth = args.depth,
                eval = args.static_eval,
                fut_score = futility_score,
                alpha = args.alpha
            )),
        );
        return true;
    }
    let see_margin = capture_see_margin(args.depth);
    if !pos.see_ge(mv, -see_margin) {
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        super::diagnostics::record_tag(
            pos,
            "cap_fut_see_skip",
            Some(format!("depth={depth} margin={see_margin}", depth = args.depth)),
        );
        return true;
    }
    false
}

pub(crate) struct SearchContext<'a> {
    pub(crate) limits: &'a SearchLimits,
    pub(crate) start_time: &'a std::time::Instant,
    pub(crate) nodes: &'a mut u64,
    pub(crate) seldepth: &'a mut u32,
    pub(crate) qnodes: &'a mut u64,
    pub(crate) qnodes_limit: u64,
    #[cfg(feature = "diagnostics")]
    pub(crate) abdada_busy_detected: &'a mut u64,
    #[cfg(feature = "diagnostics")]
    pub(crate) abdada_busy_set: &'a mut u64,
}

impl<'a> SearchContext<'a> {
    #[inline]
    pub(crate) fn tick(&mut self, ply: u32) {
        *self.nodes += 1;
        if ply > *self.seldepth {
            *self.seldepth = ply;
        }
    }

    #[inline]
    pub(crate) fn register_qnode(&mut self) -> bool {
        *self.qnodes += 1;
        #[cfg(feature = "diagnostics")]
        record_qnodes_peak(*self.qnodes, self.qnodes_limit);
        *self.qnodes >= self.qnodes_limit
    }

    #[inline]
    pub(crate) fn qnodes_limit_reached(&self) -> bool {
        *self.qnodes >= self.qnodes_limit
    }

    #[inline]
    pub(crate) fn time_up(&self) -> bool {
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        if diagnostics::should_abort_now() {
            return true;
        }
        let should_poll = |mask: u64| (*self.nodes & mask) == 0;
        let time_limit_expired = || {
            if let Some(limit) = self.limits.time_limit() {
                if self.start_time.elapsed() >= limit {
                    return true;
                }
            }
            false
        };

        if let Some(tm) = self.limits.time_manager.as_ref() {
            let mask = if tm.is_in_byoyomi() {
                TIME_CHECK_MASK_BYOYOMI
            } else {
                TIME_CHECK_MASK_NORMAL
            };

            if !should_poll(mask) {
                return false;
            }

            if let Some(flag) = self.limits.stop_flag.as_ref() {
                if flag.load(std::sync::atomic::Ordering::Acquire) {
                    return true;
                }
            }

            if tm.should_stop(*self.nodes) {
                return true;
            }

            return time_limit_expired();
        }

        if !should_poll(TIME_CHECK_MASK_NORMAL) {
            return false;
        }

        time_limit_expired()
    }

    #[inline]
    pub(crate) fn time_up_fast(&self) -> bool {
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        if diagnostics::should_abort_now() {
            return true;
        }
        if let Some(tm) = self.limits.time_manager.as_ref() {
            if let Some(flag) = self.limits.stop_flag.as_ref() {
                if flag.load(std::sync::atomic::Ordering::Acquire) {
                    return true;
                }
            }
            if tm.should_stop(*self.nodes) {
                return true;
            }
        }

        if let Some(limit) = self.limits.time_limit() {
            if self.start_time.elapsed() >= limit {
                return true;
            }
        }

        false
    }
}

pub(crate) struct ABArgs<'a> {
    pub(crate) pos: &'a Position,
    pub(crate) depth: i32,
    pub(crate) alpha: i32,
    pub(crate) beta: i32,
    pub(crate) ply: u32,
    pub(crate) is_pv: bool,
    pub(crate) stack: &'a mut [SearchStack],
    pub(crate) heur: &'a mut Heuristics,
    pub(crate) tt_hits: &'a mut u64,
    pub(crate) beta_cuts: &'a mut u64,
    pub(crate) lmr_counter: &'a mut u64,
    // instrumentation (optional): LMR gating + evasion extension counters
    pub(crate) lmr_blocked_in_check: Option<&'a std::cell::Cell<u64>>,
    pub(crate) lmr_blocked_recapture: Option<&'a std::cell::Cell<u64>>,
    pub(crate) evasion_sparsity_ext: Option<&'a std::cell::Cell<u64>>,
}

impl<E: Evaluator + Send + Sync + 'static> ClassicBackend<E> {
    #[inline]
    fn gating_enabled() -> bool {
        static FLAG: OnceLock<bool> = OnceLock::new();
        *FLAG.get_or_init(|| match crate::util::env_var("SEARCH_GATING_ENABLE") {
            Some(v) => {
                let v = v.trim().to_ascii_lowercase();
                !(v == "0" || v == "false" || v == "off")
            }
            None => true,
        })
    }
    pub(crate) fn alphabeta(
        &self,
        args: ABArgs,
        ctx: &mut SearchContext,
    ) -> (i32, Option<crate::shogi::Move>) {
        let ABArgs {
            pos,
            depth,
            mut alpha,
            beta,
            ply,
            is_pv,
            stack,
            heur,
            tt_hits,
            beta_cuts,
            lmr_counter,
            lmr_blocked_in_check,
            lmr_blocked_recapture,
            evasion_sparsity_ext,
        } = args;
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::record_ab_enter(pos, depth, alpha, beta, is_pv, "ab_enter");
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_enter");
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        if diagnostics::should_abort_now() {
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (self.evaluator.evaluate(pos), None);
        }
        if (ply as usize) >= crate::search::constants::MAX_PLY {
            let eval = self.evaluator.evaluate(pos);
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (eval, None);
        }
        if ctx.time_up() || Self::should_stop(ctx.limits) {
            let eval = self.evaluator.evaluate(pos);
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (eval, None);
        }
        ctx.tick(ply);
        if depth <= 0 {
            let mut qbudget = super::qsearch::initial_quiet_check_budget(ctx);
            let prev_move = if ply > 0 {
                stack[(ply - 1) as usize].current_move
            } else {
                None
            };
            // qsearch の静かチェック生成は“侵入直後のみ”に制限するため、
            // 入口では qdepth=0 を与え、再帰で -1 ずつ減らす設計。
            // これにより将棋特有の手駒を用いた連続王手の組合せ爆発を抑制する。
            let qs = self.qsearch(
                pos,
                super::qsearch::SearchWindow { alpha, beta },
                ctx,
                super::qsearch::QSearchFrame {
                    ply,
                    qdepth: 0,
                    prev_move,
                },
                &*heur,
                &mut qbudget,
            );
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (qs, None);
        }

        let _orig_alpha = alpha;
        let _orig_beta = beta;
        let static_eval = self.evaluator.evaluate(pos);
        stack[ply as usize].static_eval = Some(static_eval);

        let mut used_alpha = alpha;
        let mut used_beta = beta;
        if crate::search::mate_distance_pruning(&mut used_alpha, &mut used_beta, ply as u8) {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (used_alpha, None);
        }
        alpha = used_alpha;
        let beta = used_beta;

        if self.should_static_beta_prune(super::pruning::StaticBetaPruneParams {
            toggles: &self.profile.prune,
            depth,
            pos,
            beta,
            static_eval,
            is_pv,
            ply,
            stack: &*stack,
        }) {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (static_eval, None);
        }

        if let Some(r) = self.razor_prune(super::pruning::RazorPruneParams {
            toggles: &self.profile.prune,
            depth,
            pos,
            alpha,
            static_eval,
            ctx,
            ply,
            is_pv,
            stack: &*stack,
            heur: &*heur,
        }) {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (r, None);
        }

        if let Some(score) = self.null_move_prune(NullMovePruneParams {
            toggles: &self.profile.prune,
            depth,
            pos,
            beta,
            static_eval,
            ply,
            stack: &mut *stack,
            heur: &mut *heur,
            tt_hits: &mut *tt_hits,
            beta_cuts: &mut *beta_cuts,
            lmr_counter: &mut *lmr_counter,
            ctx,
            is_pv,
            #[cfg(test)]
            verify_min_depth_override: None,
        }) {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (score, None);
        }

        let mut tt_hint: Option<crate::shogi::Move> = None;
        let mut tt_depth_ok = false;
        let pos_hash = pos.zobrist_hash();
        // --- ABDADA (in-progress) 簡易版：重複探索の緩和（Non-PV/非王手/十分深い）
        let use_abdada = abdada_enabled();
        // ABDADA: TT 側の busy bit（"exact cut in-progress"）を set/clear するためのスコープガード。
        // - set_exact_cut()/clear_exact_cut() は lock-free/atomic 前提（TT 実装に依存）
        // - busy 検知側（後着）は軽い減深で合流し、先着はフル深さを維持
        // - Drop により確実に busy bit を解放し、false positive/negative を避ける
        struct AbdadaGuard {
            tt: Option<std::sync::Arc<crate::search::TranspositionTable>>,
            hash: u64,
            side: crate::Color,
            active: bool,
        }
        impl Drop for AbdadaGuard {
            fn drop(&mut self) {
                if self.active {
                    if let Some(tt) = &self.tt {
                        tt.clear_exact_cut(self.hash, self.side);
                    }
                }
            }
        }
        let mut _abdada_guard = AbdadaGuard {
            tt: None,
            hash: pos_hash,
            side: pos.side_to_move,
            active: false,
        };
        // ABDADA: busy検知側にのみ減深を適用するためのフラグ
        let mut abdada_reduce = false;
        const ABDADA_MIN_DEPTH: i32 = 6;
        if use_abdada && !is_pv && depth >= ABDADA_MIN_DEPTH && !pos.is_in_check() {
            if let Some(tt_arc) = &self.tt {
                // すでに busy なら“後着側”として軽い減深で合流（同深重複を避ける）
                if tt_arc.has_exact_cut(pos_hash, pos.side_to_move) {
                    abdada_reduce = true;
                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    if let Some(cb) = ctx.limits.info_string_callback.as_ref() {
                        cb(&format!("abdada_busy_detected=1 depth={}", depth));
                    }
                    #[cfg(feature = "diagnostics")]
                    {
                        *ctx.abdada_busy_detected = ctx.abdada_busy_detected.saturating_add(1);
                    }
                } else {
                    // busy 設定（Dropでクリア）
                    tt_arc.set_exact_cut(pos_hash, pos.side_to_move);
                    _abdada_guard = AbdadaGuard {
                        tt: Some(std::sync::Arc::clone(tt_arc)),
                        hash: pos_hash,
                        side: pos.side_to_move,
                        active: true,
                    };
                    #[cfg(feature = "diagnostics")]
                    {
                        *ctx.abdada_busy_set = ctx.abdada_busy_set.saturating_add(1);
                    }
                }
            }
        }
        if let Some(tt) = &self.tt {
            if depth >= 3 && dynp::tt_prefetch_enabled() {
                tt.prefetch_l2(pos_hash, pos.side_to_move);
            }
            if let Some(entry) = tt.probe(pos_hash, pos.side_to_move) {
                *tt_hits += 1;
                let stored = entry.score() as i32;
                let score = crate::search::common::adjust_mate_score_from_tt(stored, ply as u8);
                let sufficient = entry.depth() as i32 >= depth;
                tt_depth_ok = entry.depth() as i32 >= depth - 2;
                match entry.node_type() {
                    NodeType::LowerBound if sufficient && score >= beta => {
                        #[cfg(any(debug_assertions, feature = "diagnostics"))]
                        diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
                        return (score, entry.get_move());
                    }
                    NodeType::UpperBound if sufficient && score <= alpha => {
                        #[cfg(any(debug_assertions, feature = "diagnostics"))]
                        diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
                        return (score, entry.get_move());
                    }
                    NodeType::Exact if sufficient => {
                        #[cfg(any(debug_assertions, feature = "diagnostics"))]
                        diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
                        return (score, entry.get_move());
                    }
                    _ => {
                        tt_hint = entry.get_move();
                    }
                }
            }
        }

        self.maybe_iid(MaybeIidParams {
            toggles: &self.profile.prune,
            depth,
            pos,
            alpha,
            beta,
            ply,
            stack: &mut *stack,
            heur: &mut *heur,
            tt_hits: &mut *tt_hits,
            beta_cuts: &mut *beta_cuts,
            lmr_counter: &mut *lmr_counter,
            ctx,
            tt_hint: &mut tt_hint,
            tt_depth_ok,
        });

        if let Some((score, mv)) = self.probcut(ProbcutParams {
            toggles: &self.profile.prune,
            depth,
            pos,
            beta,
            static_eval,
            ply,
            is_pv,
            stack: &mut *stack,
            heur: &mut *heur,
            tt_hits: &mut *tt_hits,
            beta_cuts: &mut *beta_cuts,
            lmr_counter: &mut *lmr_counter,
            ctx,
        }) {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            return (score, Some(mv));
        }

        let prev_move = if ply > 0 {
            stack[(ply - 1) as usize].current_move
        } else {
            None
        };
        let prev_prev_move = if ply > 1 {
            stack[(ply - 2) as usize].current_move
        } else {
            None
        };
        let counter_mv = prev_move.and_then(|mv| heur.counter.get(pos.side_to_move, mv));
        let killers = stack[ply as usize].killers;
        let excluded_move = stack[ply as usize].excluded_move;
        let mut picker =
            MovePicker::new_normal(pos, tt_hint, excluded_move, killers, counter_mv, prev_move);

        stack[ply as usize].clear_for_new_node();
        stack[ply as usize].in_check = pos.is_in_check();
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_cleared");
        let mut best_mv = None;
        let mut best = i32::MIN / 2;
        let mut best_drop_bad = false;
        let mut moveno: usize = 0;
        let mut first_move_done = false;
        let mut tried_captures: SmallVec<[crate::shogi::Move; 16]> = SmallVec::new();
        let mut aborted = false;
        // Pre-compute evasion count once per node (only when in_check)
        let mut evasion_count: Option<usize> = None;
        if Self::gating_enabled() && stack[ply as usize].in_check {
            let mg = MoveGenerator::new();
            if let Ok(list) = mg.generate_evasions(pos) {
                evasion_count = Some(list.len());
            }
        }
        while let Some(mv) = picker.next(&*heur) {
            if ctx.time_up() || Self::should_stop(ctx.limits) {
                aborted = true;
                break;
            }
            moveno += 1;
            stack[ply as usize].current_move = Some(mv);
            let gives_check = pos.gives_check(mv);
            let is_capture = mv.is_capture_hint();
            let is_quiet = !is_capture && !gives_check;
            let need_see = is_capture || mv.is_drop() || is_quiet;
            let see = if need_see { pos.see(mv) } else { 0 };
            let is_good_capture = is_capture && see >= 0;
            let drop_bad = mv.is_drop() && see < 0;
            let quiet_bad = is_quiet && see < 0;
            let same_to = prev_move.is_some_and(|pm| pm.to() == mv.to());
            let recap = is_capture
                && prev_move.is_some_and(|pm| pm.is_capture_hint() && pm.to() == mv.to());
            let mut stat_score =
                ordering::stat_score(heur, pos, mv, prev_move, prev_prev_move, is_capture);
            if drop_bad {
                stat_score -= DROP_BAD_HISTORY_PENALTY_CP;
            }

            if depth < 14 && is_quiet {
                let mut h = heur.history.get(pos.side_to_move, mv);
                // 明示的に i16 範囲へクランプ（将来の係数変更でも安全）
                h = h.clamp(i16::MIN as i32, i16::MAX as i32);
                let is_counter = counter_mv.is_some_and(|cm| cm.equals_without_piece_type(&mv));
                // しきい値も i16 範囲にクランプして型域を整合（depth≥8 での無効化を防ぐ）
                let mut hp_thresh = dynp::hp_threshold_for_depth(depth);
                hp_thresh = hp_thresh.clamp(i16::MIN as i32, i16::MAX as i32);
                // 遅手のみHP対象（move_no>3）。TT手/カウンター/キラー/チェック静止は除外。
                let is_late = moveno > 3;
                if is_late
                    && !gives_check
                    && h < hp_thresh
                    && !stack[ply as usize].is_killer(mv)
                    && !is_counter
                {
                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    super::diagnostics::record_tag(
                        pos,
                        match depth {
                            1 => "hp_skip_d1",
                            2..=3 => "hp_skip_d2",
                            _ => "hp_skip_d3",
                        },
                        Some(format!("moveno={}", moveno)),
                    );
                    continue;
                }
            }

            if depth <= 3 && is_quiet && !stack[ply as usize].prev_risky {
                let limit = dynp::lmp_limit_for_depth(depth);
                if moveno > limit {
                    continue;
                }
            }
            // Futility（alpha側）: 静止のみ・チェック静止/良捕獲/昇は除外、depth<=8
            if dynp::pruning_safe_mode()
                && dynp::fut_dynamic_enabled()
                && depth <= 8
                && is_quiet
                && !pos.is_in_check()
                && !stack[ply as usize].prev_risky
            {
                use crate::search::constants::MATE_SCORE;
                if alpha.abs() >= MATE_SCORE - 100 { /* mate帯近傍では futility 無効 */ }
                let improving = if ply >= 2 {
                    let idx = (ply - 2) as usize;
                    stack
                        .get(idx)
                        .and_then(|st| st.static_eval)
                        .is_some_and(|prev2| static_eval >= prev2 - 10)
                } else {
                    false
                };
                let d = depth.clamp(1, 8);
                let mut margin = dynp::fut_margin_base() + dynp::fut_margin_slope() * d;
                if improving {
                    margin -= 30;
                }
                let fut_stat_den = dynp::fut_stat_den().max(1);
                margin += stat_score / fut_stat_den;
                if alpha.abs() < MATE_SCORE - 100 && static_eval + margin <= alpha {
                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    {
                        super::diagnostics::record_tag(
                            pos,
                            "fut_skip",
                            Some(format!("d={} marg={}", d, margin)),
                        );
                        super::diagnostics::record_tag(
                            pos,
                            match d {
                                1..=2 => "fut_skip_d1_2",
                                3..=5 => "fut_skip_d3_5",
                                _ => "fut_skip_d6_8",
                            },
                            None,
                        );
                    }
                    continue;
                }
            }
            let mut next_depth = depth - 1;
            let mut reduction = ordering::late_move_reduction(LateMoveReductionParams {
                lmr_trials: &mut heur.lmr_trials,
                depth,
                moveno,
                is_quiet,
                is_good_capture,
                is_pv,
                gives_check,
                static_eval,
                ply,
                stack: &*stack,
            });
            // LMR gating: disable reductions in tactical/sensitive contexts
            if reduction > 0 && Self::gating_enabled() {
                // 1) Current node is in check (evasion node)
                if stack[ply as usize].in_check {
                    if let Some(cell) = lmr_blocked_in_check {
                        cell.set(cell.get().saturating_add(1));
                    }
                    reduction = 0;
                } else {
                    // 2) Recapture: previous move was a capture and we capture back on the same square
                    if recap {
                        if let Some(cell) = lmr_blocked_recapture {
                            cell.set(cell.get().saturating_add(1));
                        }
                        reduction = 0;
                    } else if same_to && is_quiet {
                        // 3) same-to（同一地点応手）: 静止の即応は1段だけ減衰を緩める
                        reduction = (reduction - 1).max(0);
                    }
                }
            }
            // 特例ガード: 直前が回避直後の静止 or TT強ヒント → 減深を1段弱める
            if reduction > 0 && is_quiet {
                let prev_in_check = if ply > 0 {
                    stack[(ply - 1) as usize].in_check
                } else {
                    false
                };
                if prev_in_check || tt_depth_ok {
                    reduction = (reduction - 1).max(0);
                }
                // 直前手が drop / quiet xSEE<0 のときはさらに1段弱める
                if stack[ply as usize].prev_risky {
                    reduction = (reduction - 1).max(0);
                }
            }
            // ABDADA軽減: busy中は追加で1段だけ減深（静止手のみ）
            if reduction > 0 && is_quiet {
                let denom = dynp::lmr_stat_den(depth).max(1);
                let numer = dynp::lmr_stat_num();
                if numer != 0 {
                    reduction -= (stat_score * numer) / denom;
                    if reduction < 0 {
                        reduction = 0;
                    }
                }
            }
            if reduction > 0 {
                next_depth -= reduction;
                *lmr_counter += 1;
            }
            // Lightweight extension: evasion sparsity (+1) when few legal evasions
            if Self::gating_enabled() && stack[ply as usize].in_check {
                if let Some(k) = evasion_count {
                    if k <= 2 {
                        // Cap to original depth
                        next_depth = (next_depth + 1).min(depth);
                        if let Some(cell) = evasion_sparsity_ext {
                            cell.set(cell.get().saturating_add(1));
                        }
                    }
                }
            }
            // 後着（busy検知）時のみ、静止手に限って追加で −1ply 合流
            if use_abdada && abdada_reduce && is_quiet && next_depth > 0 {
                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                if let Some(cb) = ctx.limits.info_string_callback.as_ref() {
                    cb(&format!(
                        "abdada_cut_reduction=1 next_depth={next_depth} -> {reduced}",
                        reduced = next_depth - 1
                    ));
                }
                next_depth -= 1;
            }
            // Recapture / same-to の軽量拡張（+1ply）。深さ上限は親のdepthまで。
            if Self::gating_enabled()
                && !stack[ply as usize].in_check
                && next_depth > 0
                && (recap || (same_to && dynp::same_to_extension_enabled()))
            {
                next_depth = (next_depth + 1).min(depth);
            }
            if !stack[ply as usize].in_check
                && quiet_see_guard_should_skip(pos, mv, next_depth, is_pv, is_quiet, gives_check)
            {
                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                super::diagnostics::record_tag(
                    pos,
                    "quiet_see_skip",
                    Some(format!(
                        "see={see} lmr_depth={d} scale={scale}",
                        see = pos.see(mv),
                        d = next_depth.max(0),
                        scale = QUIET_SEE_GUARD_CP_SCALE,
                    )),
                );
                continue;
            }
            if capture_see_guard_should_skip(pos, mv, depth, is_capture, gives_check) {
                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                super::diagnostics::record_tag(
                    pos,
                    "cap_see_guard_skip",
                    Some(format!(
                        "see={see} depth={depth} margin={margin}",
                        see = pos.see(mv),
                        margin = capture_see_guard_margin(depth)
                    )),
                );
                continue;
            }
            if (is_capture || gives_check)
                && capture_futility_should_skip(
                    pos,
                    mv,
                    &CaptureFutilityArgs {
                        depth,
                        alpha,
                        static_eval,
                        is_capture,
                        gives_check,
                        prev_risky: stack[ply as usize].prev_risky,
                    },
                )
            {
                continue;
            }
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_move_pick(diagnostics::MovePickContext {
                pos,
                depth,
                alpha,
                beta,
                is_pv,
                moveno,
                mv,
                gives_check,
                is_capture,
                reduction,
            });
            let pv_move = !first_move_done;
            let mut did_fullwin_research = false;
            stack[ply as usize].history_score = stat_score;
            let score = {
                let _guard = EvalMoveGuard::new(self.evaluator.as_ref(), pos, mv);
                let mut child = pos.clone();
                child.do_move(mv);
                // Propagate prev_risky to child
                let child_ply = (ply + 1) as usize;
                let save_prev_risky = stack.get(child_ply).map(|st| st.prev_risky).unwrap_or(false);
                if let Some(st) = stack.get_mut(child_ply) {
                    st.prev_risky = drop_bad || quiet_bad;
                }
                let val = if pv_move {
                    let (sc, _) = self.alphabeta(
                        ABArgs {
                            pos: &child,
                            depth: next_depth,
                            alpha: -beta,
                            beta: -alpha,
                            ply: ply + 1,
                            is_pv: true,
                            stack,
                            heur,
                            tt_hits,
                            beta_cuts,
                            lmr_counter,
                            lmr_blocked_in_check,
                            lmr_blocked_recapture,
                            evasion_sparsity_ext,
                        },
                        ctx,
                    );
                    -sc
                } else {
                    let (sc_nw, _) = self.alphabeta(
                        ABArgs {
                            pos: &child,
                            depth: next_depth,
                            alpha: -(alpha + 1),
                            beta: -alpha,
                            ply: ply + 1,
                            is_pv: false,
                            stack,
                            heur,
                            tt_hits,
                            beta_cuts,
                            lmr_counter,
                            lmr_blocked_in_check,
                            lmr_blocked_recapture,
                            evasion_sparsity_ext,
                        },
                        ctx,
                    );
                    let mut s = -sc_nw;
                    if s > alpha {
                        // 再探索条件:
                        //  - β未到達の上振れ (s < beta)
                        //  - 減深が入っており、かつ s >= α+δ（δ=80cp）
                        const REDELTA_CP: i32 = 80;
                        let need_re = (s < beta) || (reduction > 0 && s >= alpha + REDELTA_CP);
                        if need_re && !std::mem::replace(&mut did_fullwin_research, true) {
                            #[cfg(any(debug_assertions, feature = "diagnostics"))]
                            super::diagnostics::record_tag(pos, "lmr_fullwin_re", None);
                            let (sc_fw, _) = self.alphabeta(
                                ABArgs {
                                    pos: &child,
                                    depth: next_depth,
                                    alpha: -beta,
                                    beta: -alpha,
                                    ply: ply + 1,
                                    is_pv: true,
                                    stack,
                                    heur,
                                    tt_hits,
                                    beta_cuts,
                                    lmr_counter,
                                    lmr_blocked_in_check,
                                    lmr_blocked_recapture,
                                    evasion_sparsity_ext,
                                },
                                ctx,
                            );
                            s = -sc_fw;
                        }
                    }
                    s
                };
                if let Some(st) = stack.get_mut(child_ply) {
                    st.prev_risky = save_prev_risky;
                }
                val
            };
            if pv_move {
                first_move_done = true;
            }
            if score > best {
                best = score;
                best_mv = Some(mv);
                best_drop_bad = drop_bad;
            }
            if score > alpha {
                alpha = score;
            }
            if alpha >= beta {
                *beta_cuts += 1;
                if is_quiet {
                    if !drop_bad {
                        stack[ply as usize].update_killers(mv);
                        heur.history.update_good(pos.side_to_move, mv, depth);
                        if let Some(curr_piece) = mv.piece_type() {
                            heur.pawn_history.update_good(
                                pos.side_to_move,
                                curr_piece,
                                mv.to(),
                                depth,
                            );
                        }
                        if ply > 0 {
                            if let Some(prev_mv) = stack[(ply - 1) as usize].current_move {
                                heur.counter.update(pos.side_to_move, prev_mv, mv);
                                if let (Some(prev_piece), Some(curr_piece)) =
                                    (prev_mv.piece_type(), mv.piece_type())
                                {
                                    let key = crate::search::history::ContinuationKey::new(
                                        pos.side_to_move,
                                        prev_piece as usize,
                                        prev_mv.to(),
                                        prev_mv.is_drop(),
                                        curr_piece as usize,
                                        mv.to(),
                                        mv.is_drop(),
                                    );
                                    heur.continuation.update_good(key, depth);
                                }
                            }
                        }
                    }
                } else if is_capture {
                    if let (Some(attacker), Some(victim)) =
                        (mv.piece_type(), mv.captured_piece_type())
                    {
                        heur.capture.update_good(
                            pos.side_to_move,
                            attacker,
                            victim,
                            mv.to(),
                            depth,
                        );
                    }
                }
                break;
            }
            if is_capture {
                tried_captures.push(mv);
            }
            if is_quiet {
                stack[ply as usize].quiet_moves.push(mv);
            }
        }
        if aborted {
            // 中断時は現時点のベスト値（非PV手は探索済み）か静的評価をそのまま返す。
            // 上位では stop 判定と組み合わせて結果を採用/破棄するため、TT へは書き込まない。
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
            if first_move_done {
                return (best, best_mv);
            } else {
                return (static_eval, None);
            }
        }

        if best_drop_bad {
            if let Some(bmv) = best_mv {
                heur.history.update_bad(pos.side_to_move, bmv, depth.max(1));
                if let Some(curr_piece) = bmv.piece_type() {
                    heur.pawn_history.update_bad(
                        pos.side_to_move,
                        curr_piece,
                        bmv.to(),
                        depth.max(1),
                    );
                }
            }
        }
        let result = if best == i32::MIN / 2 {
            let mut qbudget = super::qsearch::initial_quiet_check_budget(ctx);
            let prev_move = if ply > 0 {
                stack[(ply - 1) as usize].current_move
            } else {
                None
            };
            let qs = self.qsearch(
                pos,
                super::qsearch::SearchWindow { alpha, beta },
                ctx,
                super::qsearch::QSearchFrame {
                    ply,
                    qdepth: 0,
                    prev_move,
                },
                &*heur,
                &mut qbudget,
            );
            (qs, None)
        } else {
            if let Some(tt) = &self.tt {
                let node_type = if best <= used_alpha {
                    NodeType::UpperBound
                } else if best >= used_beta {
                    NodeType::LowerBound
                } else {
                    NodeType::Exact
                };
                let store_score = crate::search::common::adjust_mate_score_for_tt(best, ply as u8)
                    .clamp(i16::MIN as i32, i16::MAX as i32);
                let static_eval_i16 = static_eval.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                // A/B1: Helper の根近傍（ply<=2）の非Exact/非PVの保存抑制に加え、
                // 環境で D を指定した場合は（深さ < D）も抑制対象にする。
                let extra_suppr_depth = tt_suppress_below_depth().unwrap_or(-1);
                let suppress_helper_near_root = ctx.limits.helper_role
                    && !is_pv
                    && !matches!(node_type, NodeType::Exact)
                    && ((ply <= 2) || (extra_suppr_depth >= 0 && depth < extra_suppr_depth));
                if !suppress_helper_near_root {
                    // YaneuraOu-style TT depth degradation: reduce depth by 1 for non-Exact entries
                    // to prevent overvalued bounds from dominating move ordering in subsequent iterations
                    let depth_to_store = if matches!(node_type, NodeType::Exact) {
                        depth as u8
                    } else {
                        (depth - 1).max(1) as u8
                    };
                    let mut args = crate::search::tt::TTStoreArgs::new(
                        pos_hash,
                        best_mv,
                        store_score as i16,
                        static_eval_i16,
                        depth_to_store,
                        node_type,
                        pos.side_to_move,
                    );
                    args.is_pv = is_pv;
                    tt.store(args);
                } else {
                    // Diagnostics via info_string_callback (root-scope): suppress helper near root
                    #[cfg(any(debug_assertions, feature = "diagnostics"))]
                    if let Some(cb) = ctx.limits.info_string_callback.as_ref() {
                        cb(&format!(
                            "tt_store_suppressed_helper_near_root=1 ply={} node_type={:?} depth={}",
                            ply, node_type, depth
                        ));
                    }
                }
            }
            for &cmv in &tried_captures {
                if Some(cmv) != best_mv {
                    if let (Some(attacker), Some(victim)) =
                        (cmv.piece_type(), cmv.captured_piece_type())
                    {
                        heur.capture.update_bad(
                            pos.side_to_move,
                            attacker,
                            victim,
                            cmv.to(),
                            depth,
                        );
                    }
                }
            }
            for &qmv in &stack[ply as usize].quiet_moves {
                if Some(qmv) != best_mv {
                    heur.history.update_bad(pos.side_to_move, qmv, depth);
                    if let Some(curr_piece) = qmv.piece_type() {
                        heur.pawn_history.update_bad(pos.side_to_move, curr_piece, qmv.to(), depth);
                    }
                    if qmv.is_drop() && pos.see(qmv) < 0 {
                        heur.history.update_bad(pos.side_to_move, qmv, depth);
                        if let Some(curr_piece) = qmv.piece_type() {
                            heur.pawn_history.update_bad(
                                pos.side_to_move,
                                curr_piece,
                                qmv.to(),
                                depth,
                            );
                        }
                    }
                    if ply > 0 {
                        if let Some(prev_mv) = stack[(ply - 1) as usize].current_move {
                            if let (Some(prev_piece), Some(curr_piece)) =
                                (prev_mv.piece_type(), qmv.piece_type())
                            {
                                let key = crate::search::history::ContinuationKey::new(
                                    pos.side_to_move,
                                    prev_piece as usize,
                                    prev_mv.to(),
                                    prev_mv.is_drop(),
                                    curr_piece as usize,
                                    qmv.to(),
                                    qmv.is_drop(),
                                );
                                heur.continuation.update_bad(key, depth);
                            }
                        }
                    }
                }
            }
            (best, best_mv)
        };
        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::record_stack_state(pos, &stack[ply as usize], "stack_exit");
        result
    }
}
// abdada_enabled(): see crate::search::policy
