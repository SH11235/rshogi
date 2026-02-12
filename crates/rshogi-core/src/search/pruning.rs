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
use super::tt_sanity::{helper_tt_write_enabled_for_depth, maybe_trace_tt_write, TtWriteTrace};
use super::types::{value_to_tt, NodeType};
use super::{LimitsType, MovePicker, TimeManagement};

/// Futility pruning
#[inline]
pub(super) fn try_futility_pruning(
    params: FutilityParams,
    tune_params: &super::SearchTuneParams,
) -> Option<Value> {
    if !params.tt_pv
        && !params.in_check
        && params.depth < 14
        && params.static_eval != Value::NONE
        && params.static_eval >= params.beta
        && !params.beta.is_loss()
        && !params.static_eval.is_win()
        && (!params.tt_move_exists || params.tt_capture)
    {
        let futility_mult = tune_params.futility_margin_base
            - tune_params.futility_margin_tt_bonus * (!params.tt_hit) as i32;
        let futility_margin = Value::new(
            futility_mult * params.depth
                - (params.improving as i32) * futility_mult * tune_params.futility_improving_scale
                    / 1024
                - (params.opponent_worsening as i32)
                    * futility_mult
                    * tune_params.futility_opponent_worsening_scale
                    / 4096
                + (params.correction_value.abs() / tune_params.futility_correction_div.max(1)),
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
pub(super) fn try_small_probcut(
    depth: Depth,
    beta: Value,
    tt_ctx: &TTContext,
    tune_params: &super::SearchTuneParams,
) -> Option<Value> {
    if depth >= 1 {
        let sp_beta = beta + Value::new(tune_params.small_probcut_margin);
        if tt_ctx.hit
            && tt_ctx.data.bound.is_lower_or_exact()
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
        // LMPはalpha_beta.rsで処理するため、ここでは行わない

        if step_ctx.is_capture || step_ctx.gives_check {
            let captured = step_ctx.pos.piece_on(step_ctx.mv.to());
            let capt_hist = ctx.history.with_read(|h| {
                h.capture_history.get_with_captured_piece(
                    step_ctx.mv.moved_piece_after(),
                    step_ctx.mv.to(),
                    captured,
                ) as i32
            });

            // Futility pruning for captures (駒取り手に対するfutility枝刈り)
            // YaneuraOu準拠: !in_check/static_eval!=NONEガードなし
            // (VALUE_NONE=32002によりfutilityValueが常にalpha超えのため暗黙的に安全)
            if !step_ctx.gives_check && lmr_depth < 7 {
                use super::movepicker::piece_value;
                let captured_value = piece_value(captured);
                let futility_value = step_ctx.static_eval.raw()
                    + 231
                    + 211 * lmr_depth
                    + captured_value
                    + 130 * capt_hist / 1024;
                if futility_value <= step_ctx.alpha.raw() {
                    return Step14Outcome::Skip { best_value: None };
                }
            }

            // SEE based pruning for captures (157 * depth + captHist / 29)
            // YaneuraOu準拠: alpha >= VALUE_DRAW 条件を追加
            if step_ctx.alpha >= Value::DRAW {
                let margin = (157 * step_ctx.depth + capt_hist / 29).max(0);
                if !step_ctx.pos.see_ge(step_ctx.mv, Value::new(-margin)) {
                    return Step14Outcome::Skip { best_value: None };
                }
            }
        } else {
            // Quiet moves
            let moved_piece = step_ctx.mv.moved_piece_after();
            let to_sq = step_ctx.mv.to();
            let cont_hist_0 = step_ctx.cont_history_1.get(moved_piece, to_sq) as i32;
            let cont_hist_1 = step_ctx.cont_history_2.get(moved_piece, to_sq) as i32;
            let (main_hist, pawn_hist) = ctx.history.with_read(|h| {
                let mh = h.main_history.get(step_ctx.mover, step_ctx.mv) as i32;
                let ph = h.pawn_history.get(step_ctx.pawn_history_index, moved_piece, to_sq) as i32;
                (mh, ph)
            });

            // YaneuraOu準拠: Continuation history（mainHistoryを含まない）
            // yaneuraou-search.cpp:3273-3276
            //
            // 【実装メモ: df8d771d】
            // 旧実装(00c06b7f)では hist_score = 2*main_hist + cont0 + cont1 + pawn_hist で判定していた。
            // YaneuraOu準拠に修正した結果:
            // - ノード数: 1.3-2.2倍増加（枝刈りが緩くなった）
            // - 自己対局: 48.25% vs 00c06b7f（200局, 秒読み2秒）
            // mainHistoryは通常負（悪い手）のため、含めると過剰に枝刈りしていた。
            // YaneuraOu準拠で正しいが、NPS差により時間制限下では不利。
            // NPS改善後に再評価予定。
            // 詳細: docs/step14_implementation_issues.md（localファイルでgit管理外）
            let cont_history = cont_hist_0 + cont_hist_1 + pawn_hist;

            // Continuation history based pruning (YaneuraOu: -4312 * depth)
            if cont_history < -4312 * step_ctx.depth {
                return Step14Outcome::Skip { best_value: None };
            }

            // YaneuraOu準拠: mainHistoryは pruning判定後に追加
            // yaneuraou-search.cpp:3283: history += 76 * mainHistory / 32
            let hist_score = cont_history + 76 * main_hist / 32;

            // lmrDepth調整 (枝刈りされなかった場合のみ実行)
            let lmr_depth = lmr_depth + hist_score / 3220;

            // Futility pruning for quiet moves (親ノードでの枝刈り)
            let no_best_move = step_ctx.best_move.is_none();
            let futility_value = step_ctx.static_eval.raw()
                + 47
                + 171 * no_best_move as i32
                + 134 * lmr_depth
                + 90 * (step_ctx.static_eval > step_ctx.alpha) as i32;

            // YaneuraOu準拠: static_eval!=NONEガードなし（!in_check + VALUE_NONEで暗黙的に安全）
            if !step_ctx.in_check && lmr_depth < 11 && futility_value <= step_ctx.alpha.raw() {
                // YaneuraOu準拠: bestValueをfutilityValueで更新する条件
                // if (bestValue <= futilityValue && !is_decisive(bestValue) && !is_win(futilityValue))
                let futility_val = Value::new(futility_value);
                if step_ctx.best_value <= futility_val
                    && !step_ctx.best_value.is_mate_score()
                    && !futility_val.is_win()
                {
                    return Step14Outcome::Skip {
                        best_value: Some(futility_val),
                    };
                }
                return Step14Outcome::Skip { best_value: None };
            }

            // SEE pruning for quiet moves (YaneuraOu: -27 * lmrDepth * lmrDepth)
            // YaneuraOu準拠: !in_check/lmrDepth>0ガードなし
            // lmrDepth=0時はthreshold=0でSEE<0の手を枝刈り
            let lmr_depth_clamped = lmr_depth.max(0);
            if !step_ctx
                .pos
                .see_ge(step_ctx.mv, Value::new(-27 * lmr_depth_clamped * lmr_depth_clamped))
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
pub(super) fn try_razoring<const NT: u8>(
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
) -> Option<Value> {
    // YaneuraOu準拠: 評価値が非常に低い場合、通常探索をスキップしてqsearch値を返す
    if !pv_node
        && !in_check
        && static_eval
            < alpha
                - Value::new(
                    ctx.tune_params.razoring_margin_base
                        + ctx.tune_params.razoring_margin_depth2_coeff * depth * depth,
                )
    {
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
        inc_stat!(st, razoring_applied);
        inc_stat_by_depth!(st, razoring_by_depth, depth);
        return Some(value);
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

    let margin = ctx.tune_params.nmp_margin_depth_mult * depth + ctx.tune_params.nmp_margin_offset;
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
        let r = ctx.tune_params.nmp_reduction_base
            + depth / ctx.tune_params.nmp_reduction_depth_div.max(1);

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
            if st.nmp_min_ply != 0 || depth < ctx.tune_params.nmp_verification_depth_threshold {
                inc_stat!(st, nmp_cutoff);
                inc_stat_by_depth!(st, nmp_cutoff_by_depth, depth);
                return (Some(null_value), improving);
            }

            st.nmp_min_ply = ply
                + ctx.tune_params.nmp_min_ply_update_num * (depth - r)
                    / ctx.tune_params.nmp_min_ply_update_den.max(1);

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

    // YaneuraOu準拠: Step10直前の improving 再計算は VALUE_NONE を含めて評価する。
    // in-check ノードは YO ではこの経路に入らないため、現実装では !in_check のみ維持する。
    if !in_check {
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
    cut_node: bool,
    excluded_move: Move,
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

    let prob_beta = beta
        + Value::new(
            ctx.tune_params.probcut_beta_margin_base
                - ctx.tune_params.probcut_beta_improving_sub * improving as i32,
        );
    // YaneuraOu準拠: ttData.value が有効で probCutBeta 未満なら probCut を試さない。
    // hit フラグや mate 判定で追加ガードしない。
    if beta.is_mate_score() || (tt_ctx.value != Value::NONE && tt_ctx.value < prob_beta) {
        return None;
    }

    let threshold = prob_beta - static_eval;

    let dynamic_reduction =
        (static_eval - beta).raw() / ctx.tune_params.probcut_dynamic_reduction_div.max(1);
    // YaneuraOu準拠: std::clamp(depth - 5 - ..., 0, depth) で上限もクランプ
    let probcut_depth =
        (depth - ctx.tune_params.probcut_depth_base - dynamic_reduction).clamp(0, depth);

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
        if mv == excluded_move {
            continue;
        }
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
                !cut_node,
                limits,
                time_manager,
            );
        }
        nnue_pop(st);
        pos.undo_move(mv);

        if value >= prob_beta {
            inc_stat!(st, probcut_cutoff);
            let stored_depth = (probcut_depth + 1).max(1);
            if ctx.allow_tt_write
                && helper_tt_write_enabled_for_depth(ctx.thread_id, Bound::Lower, stored_depth)
            {
                maybe_trace_tt_write(TtWriteTrace {
                    stage: "probcut_store",
                    thread_id: ctx.thread_id,
                    ply,
                    key: tt_ctx.key,
                    depth: stored_depth,
                    bound: Bound::Lower,
                    is_pv: st.stack[ply as usize].tt_pv,
                    tt_move: mv,
                    stored_value: value_to_tt(value, ply),
                    eval: unadjusted_static_eval,
                    root_move: if ply >= 1 {
                        st.stack[0].current_move
                    } else {
                        Move::NONE
                    },
                });
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
            }

            if !value.is_mate_score() {
                return Some(value - (prob_beta - beta));
            }
        }
    }

    None
}
