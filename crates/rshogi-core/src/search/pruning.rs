//! 枝刈りヘルパー群
//!
//! - Razoring
//! - Futility Pruning
//! - Null Move Pruning
//! - ProbCut
//! - Step14 pruning (LMP, SEE等)

use crate::nnue::DirtyPiece;
use crate::position::Position;
use crate::types::{Bound, Depth, Move, Value, DEPTH_QS};

use super::alpha_beta::{
    FutilityParams, SearchContext, SearchState, Step14Context, Step14Outcome, TTContext,
};
use super::qsearch::qsearch;
use super::search_helpers::{
    clear_cont_history_for_null, cont_history_tables, nnue_pop, nnue_push,
    set_cont_history_for_move,
};
use super::stats::{inc_stat, inc_stat_by_depth};
use super::types::{value_to_tt, NodeType};
use super::{LimitsType, MovePicker, TimeManagement};

// =============================================================================
// Futility Pruning
// =============================================================================

/// Futility margin の基準係数
const FUTILITY_MARGIN_BASE: i32 = 91;
const FUTILITY_MARGIN_TT_BONUS: i32 = 21;

/// Futility pruning
#[inline]
pub(super) fn try_futility_pruning(params: FutilityParams) -> Option<Value> {
    if !params.pv_node
        && !params.in_check
        && params.depth < 14
        && params.static_eval != Value::NONE
        && params.static_eval >= params.beta
        && !params.beta.is_loss()
        && !params.static_eval.is_win()
        && (!params.tt_move_exists || params.tt_capture)
    {
        let futility_mult =
            FUTILITY_MARGIN_BASE - FUTILITY_MARGIN_TT_BONUS * (!params.tt_hit) as i32;
        let futility_margin = Value::new(
            futility_mult * params.depth
                - (params.improving as i32) * futility_mult * 2094 / 1024
                - (params.opponent_worsening as i32) * futility_mult * 1324 / 4096
                + (params.correction_value.abs() / 158_105),
        );

        if params.static_eval - futility_margin >= params.beta {
            // YaneuraOu: return (2 * beta + eval) / 3
            return Some(Value::new((2 * params.beta.raw() + params.static_eval.raw()) / 3));
        }
    }
    None
}

// =============================================================================
// Small ProbCut
// =============================================================================

/// Small ProbCut
#[inline]
pub(super) fn try_small_probcut(depth: Depth, beta: Value, tt_ctx: &TTContext) -> Option<Value> {
    if depth >= 1 {
        let sp_beta = beta + Value::new(417);
        if tt_ctx.hit
            && tt_ctx.data.bound == Bound::Lower
            && tt_ctx.data.depth >= depth - 4
            && tt_ctx.value != Value::NONE
            && tt_ctx.value >= sp_beta
            && !tt_ctx.value.is_mate_score()
            && !beta.is_mate_score()
        {
            return Some(sp_beta);
        }
    }
    None
}

// =============================================================================
// Step14 Pruning
// =============================================================================

/// Step14 の枝刈り
#[inline]
pub(super) fn step14_pruning(
    ctx: &SearchContext<'_>,
    step_ctx: Step14Context<'_>,
) -> Step14Outcome {
    if step_ctx.mv.is_pass() {
        return Step14Outcome::Continue;
    }

    let lmr_depth = step_ctx.lmr_depth;

    if step_ctx.ply != 0 && !step_ctx.best_value.is_loss() {
        let lmp_denominator = 2 - step_ctx.improving as i32;
        debug_assert!(lmp_denominator > 0, "LMP denominator must be positive");
        let lmp_limit = (3 + step_ctx.depth * step_ctx.depth) / lmp_denominator;
        if step_ctx.move_count >= lmp_limit && !step_ctx.is_capture && !step_ctx.gives_check {
            return Step14Outcome::Skip { best_value: None };
        }

        if step_ctx.is_capture || step_ctx.gives_check {
            let captured = step_ctx.pos.piece_on(step_ctx.mv.to());
            let capt_hist = ctx.history.with_read(|h| {
                h.capture_history.get_with_captured_piece(
                    step_ctx.mv.moved_piece_after(),
                    step_ctx.mv.to(),
                    captured,
                ) as i32
            });

            if !step_ctx.gives_check && lmr_depth < 7 && !step_ctx.in_check {
                // step_ctx doesn't have static_eval, so we skip this check
                // This is a simplification - the full implementation would need static_eval
            }

            let margin = (158 * step_ctx.depth + capt_hist / 31).clamp(0, 283 * step_ctx.depth);
            if !step_ctx.pos.see_ge(step_ctx.mv, Value::new(-margin)) {
                return Step14Outcome::Skip { best_value: None };
            }
        } else {
            // Quiet moves
            let moved_piece = step_ctx.mv.moved_piece_after();
            let to_sq = step_ctx.mv.to();
            let cont_hist_0 = step_ctx.cont_history_1.get(moved_piece, to_sq) as i32;
            let cont_hist_1 = step_ctx.cont_history_2.get(moved_piece, to_sq) as i32;
            let main_hist = ctx
                .history
                .with_read(|h| h.main_history.get(step_ctx.mover, step_ctx.mv) as i32);
            let hist_score = 2 * main_hist + cont_hist_0 + cont_hist_1;

            if lmr_depth < 12 && hist_score < -5000 * step_ctx.depth {
                return Step14Outcome::Skip { best_value: None };
            }

            if !step_ctx.in_check
                && lmr_depth <= 4
                && !step_ctx.pos.see_ge(step_ctx.mv, Value::new(-60 * lmr_depth))
            {
                return Step14Outcome::Skip { best_value: None };
            }
        }
    }

    Step14Outcome::Continue
}

// =============================================================================
// Razoring
// =============================================================================

/// Razoring
///
/// 評価値が非常に低い場合、通常探索をスキップして静止探索の値を返す。
#[allow(clippy::too_many_arguments)]
#[inline]
pub(super) fn try_razoring<const NT: u8, F>(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
    pos: &mut Position,
    depth: Depth,
    alpha: Value,
    beta: Value,
    ply: i32,
    pv_node: bool,
    in_check: bool,
    static_eval: Value,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    _search_node: F, // 未使用、将来の拡張用
) -> Option<Value>
where
    F: Fn(
        &mut SearchState,
        &SearchContext<'_>,
        &mut Position,
        Depth,
        Value,
        Value,
        i32,
        bool,
        &LimitsType,
        &mut TimeManagement,
    ) -> Value,
{
    // depth <= 3 の浅い探索で、静的評価値が alpha より十分低い場合
    if !pv_node && !in_check && depth <= 3 {
        let razoring_threshold = alpha - Value::new(200 * depth);
        if static_eval < razoring_threshold {
            let value = qsearch::<{ NodeType::NonPV as u8 }>(
                st,
                ctx,
                pos,
                DEPTH_QS,
                alpha,
                beta,
                ply,
                limits,
                time_manager,
            );
            if value <= alpha {
                inc_stat!(st, razoring_applied);
                inc_stat_by_depth!(st, razoring_by_depth, depth);
                return Some(value);
            }
        }
    }
    None
}

// =============================================================================
// Null Move Pruning
// =============================================================================

/// Null move pruning
///
/// search_node を呼び出すため、コールバックとして受け取る。
#[allow(clippy::too_many_arguments)]
#[inline]
pub(super) fn try_null_move_pruning<const NT: u8, F>(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
    pos: &mut Position,
    depth: Depth,
    beta: Value,
    ply: i32,
    cut_node: bool,
    in_check: bool,
    static_eval: Value,
    mut improving: bool,
    excluded_move: Move,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    search_node: F,
) -> (Option<Value>, bool)
where
    F: Fn(
        &mut SearchState,
        &SearchContext<'_>,
        &mut Position,
        Depth,
        Value,
        Value,
        i32,
        bool,
        &LimitsType,
        &mut TimeManagement,
    ) -> Value,
{
    // ply >= 1 のガード（防御的プログラミング）
    if ply < 1 {
        return (None, improving);
    }

    let margin = 18 * depth - 390;
    let prev_move = st.stack[(ply - 1) as usize].current_move;
    let prev_is_pass = prev_move.is_pass();

    // NMPスキップ理由の統計収集（search-stats feature 有効時のみ）
    #[cfg(feature = "search-stats")]
    {
        // 候補ノード: excluded_move.is_none() && ply >= nmp_min_ply && !beta.is_loss()
        // これらは基本的な前提条件
        if excluded_move.is_none() && ply >= st.nmp_min_ply && !beta.is_loss() {
            st.stats.nmp_candidate_nodes += 1;
            if !cut_node {
                st.stats.nmp_skip_not_cut_node += 1;
            } else if in_check {
                st.stats.nmp_skip_in_check += 1;
            } else if static_eval < beta - Value::new(margin) {
                st.stats.nmp_skip_eval_low += 1;
            } else if prev_move.is_null() || prev_is_pass {
                st.stats.nmp_skip_prev_null += 1;
            }
        } else if excluded_move.is_some() {
            st.stats.nmp_skip_excluded += 1;
        }
    }

    if excluded_move.is_none()
        && cut_node
        && !in_check
        && static_eval >= beta - Value::new(margin)
        && ply >= st.nmp_min_ply
        && !beta.is_loss()
        && !prev_move.is_null()
        && !prev_is_pass
    {
        inc_stat!(st, nmp_attempted);
        let r = 7 + depth / 3;

        let use_pass = pos.is_pass_rights_enabled() && pos.can_pass();

        if use_pass {
            st.stack[ply as usize].current_move = Move::PASS;
        } else {
            st.stack[ply as usize].current_move = Move::NULL;
        }
        clear_cont_history_for_null(st, ctx, ply);

        if use_pass {
            pos.do_pass_move();
        } else {
            pos.do_null_move_with_prefetch(ctx.tt);
        }
        nnue_push(st, DirtyPiece::new());
        let null_value = -search_node(
            st,
            ctx,
            pos,
            depth - r,
            -beta,
            -beta + Value::new(1),
            ply + 1,
            false,
            limits,
            time_manager,
        );
        nnue_pop(st);
        if use_pass {
            pos.undo_pass_move();
        } else {
            pos.undo_null_move();
        }

        if null_value >= beta && !null_value.is_win() {
            if st.nmp_min_ply != 0 || depth < 16 {
                inc_stat!(st, nmp_cutoff);
                inc_stat_by_depth!(st, nmp_cutoff_by_depth, depth);
                return (Some(null_value), improving);
            }

            st.nmp_min_ply = ply + 3 * (depth - r) / 4;

            let v = search_node(
                st,
                ctx,
                pos,
                depth - r,
                beta - Value::new(1),
                beta,
                ply,
                false,
                limits,
                time_manager,
            );

            st.nmp_min_ply = 0;

            if v >= beta {
                inc_stat!(st, nmp_cutoff);
                inc_stat_by_depth!(st, nmp_cutoff_by_depth, depth);
                return (Some(null_value), improving);
            }
        }
    }

    if !in_check && static_eval != Value::NONE {
        improving |= static_eval >= beta;
    }

    (None, improving)
}

// =============================================================================
// ProbCut
// =============================================================================

/// ProbCut
///
/// search_node と qsearch を呼び出すため、search_node をコールバックとして受け取る。
#[allow(clippy::too_many_arguments)]
#[inline]
pub(super) fn try_probcut<F>(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
    pos: &mut Position,
    depth: Depth,
    beta: Value,
    improving: bool,
    tt_ctx: &TTContext,
    ply: i32,
    static_eval: Value,
    unadjusted_static_eval: Value,
    in_check: bool,
    limits: &LimitsType,
    time_manager: &mut TimeManagement,
    search_node: F,
) -> Option<Value>
where
    F: Fn(
        &mut SearchState,
        &SearchContext<'_>,
        &mut Position,
        Depth,
        Value,
        Value,
        i32,
        bool,
        &LimitsType,
        &mut TimeManagement,
    ) -> Value,
{
    if in_check || depth < 3 || static_eval == Value::NONE {
        return None;
    }

    let prob_beta = beta + Value::new(215 - 60 * improving as i32);
    if beta.is_mate_score()
        || (tt_ctx.hit
            && tt_ctx.value != Value::NONE
            && tt_ctx.value < prob_beta
            && !tt_ctx.value.is_mate_score())
    {
        return None;
    }

    let threshold = prob_beta - static_eval;
    if threshold <= Value::ZERO {
        return None;
    }

    let dynamic_reduction = (static_eval - beta).raw() / 300;
    let probcut_depth = (depth - 5 - dynamic_reduction).max(0);

    inc_stat!(st, probcut_attempted);

    let cont_tables = cont_history_tables(st, ctx, ply);
    let probcut_moves = {
        let mut mp = MovePicker::new_probcut(
            pos,
            tt_ctx.mv,
            threshold,
            ply,
            cont_tables,
            ctx.generate_all_legal_moves,
        );

        let mut buf = [Move::NONE; crate::movegen::MAX_MOVES];
        let mut len = 0;
        loop {
            let mv = ctx.history.with_read(|h| mp.next_move(pos, h));
            if mv == Move::NONE {
                break;
            }
            buf[len] = mv;
            len += 1;
        }
        (buf, len)
    };
    let (buf, len) = probcut_moves;

    for &mv in buf[..len].iter() {
        if !pos.is_legal(mv) {
            continue;
        }

        let gives_check = pos.gives_check(mv);
        let is_capture = pos.is_capture(mv);
        let cont_hist_piece = mv.moved_piece_after();
        let cont_hist_to = mv.to();

        st.stack[ply as usize].current_move = mv;
        let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, ctx.tt);
        nnue_push(st, dirty_piece);
        st.nodes += 1;
        set_cont_history_for_move(
            st,
            ctx,
            ply,
            in_check,
            is_capture,
            cont_hist_piece,
            cont_hist_to,
        );
        let mut value = -qsearch::<{ NodeType::NonPV as u8 }>(
            st,
            ctx,
            pos,
            DEPTH_QS,
            -prob_beta,
            -prob_beta + Value::new(1),
            ply + 1,
            limits,
            time_manager,
        );

        if value >= prob_beta && probcut_depth > 0 {
            value = -search_node(
                st,
                ctx,
                pos,
                probcut_depth,
                -prob_beta,
                -prob_beta + Value::new(1),
                ply + 1,
                true,
                limits,
                time_manager,
            );
        }
        nnue_pop(st);
        pos.undo_move(mv);

        if value >= prob_beta {
            inc_stat!(st, probcut_cutoff);
            let stored_depth = (probcut_depth + 1).max(1);
            tt_ctx.result.write(
                tt_ctx.key,
                value_to_tt(value, ply),
                st.stack[ply as usize].tt_pv,
                Bound::Lower,
                stored_depth,
                mv,
                unadjusted_static_eval,
                ctx.tt.generation(),
            );
            inc_stat_by_depth!(st, tt_write_by_depth, stored_depth);

            if value.raw().abs() < Value::INFINITE.raw() {
                return Some(value - (prob_beta - beta));
            }
            return Some(value);
        }
    }

    None
}
