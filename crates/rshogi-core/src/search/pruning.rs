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

use super::alpha_beta::{FutilityParams, SearchWorker, Step14Context, Step14Outcome, TTContext};
use super::movepicker::piece_value;
use super::stats::{inc_stat, inc_stat_by_depth};
use super::types::{value_to_tt, NodeType};
use super::{LimitsType, MovePicker, TimeManagement};

/// Futility margin の基準係数
const FUTILITY_MARGIN_BASE: i32 = 90;

impl SearchWorker {
    /// Razoring
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub(super) fn try_razoring<const NT: u8>(
        &mut self,
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
        if !pv_node && !in_check && depth <= 3 {
            let razoring_threshold = alpha - Value::new(200 * depth);
            if static_eval < razoring_threshold {
                let value = self.qsearch::<{ NodeType::NonPV as u8 }>(
                    pos,
                    DEPTH_QS,
                    alpha,
                    beta,
                    ply,
                    limits,
                    time_manager,
                );
                if value <= alpha {
                    inc_stat!(self, razoring_applied);
                    inc_stat_by_depth!(self, razoring_by_depth, depth);
                    return Some(value);
                }
            }
        }
        None
    }

    /// Futility pruning
    // 2026-02-02: 深さ制限をYaneuraOu準拠に調整（8→14、探索深度改善のため）
    #[inline]
    pub(super) fn try_futility_pruning(&self, params: FutilityParams) -> Option<Value> {
        if !params.pv_node
            && !params.in_check
            && params.depth < 14
            && params.static_eval != Value::NONE
        {
            let futility_mult =
                FUTILITY_MARGIN_BASE - 20 * (params.cut_node && !params.tt_hit) as i32;
            let futility_margin = Value::new(
                futility_mult * params.depth
                    - (params.improving as i32) * futility_mult * 2
                    - (params.opponent_worsening as i32) * futility_mult / 3
                    + (params.correction_value.abs() / 171_290),
            );

            if params.static_eval - futility_margin >= params.beta {
                return Some(params.static_eval);
            }
        }
        None
    }

    /// Null move pruning with Verification Search（Stockfish/YaneuraOu準拠）
    ///
    /// NMPは、自分の手番で「何もしない（パス）」という仮想的な手を打ち、
    /// それでも優勢なら探索を打ち切る枝刈り技術。
    ///
    /// Verification Search（深度 >= 16 の場合）:
    /// - NMPの結果が正しいかを検証するため、NMPを無効化して再探索
    /// - Zugzwang（動かなければならないこと自体が不利な局面）対策
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub(super) fn try_null_move_pruning<const NT: u8>(
        &mut self,
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
    ) -> (Option<Value>, bool) {
        // ply >= 1 のガード（防御的プログラミング）
        if ply < 1 {
            return (None, improving);
        }

        let margin = 18 * depth - 390;
        let prev_move = self.stack[(ply - 1) as usize].current_move;
        let prev_is_pass = prev_move.is_pass();
        if excluded_move.is_none()
            && cut_node
            && !in_check
            && static_eval >= beta - Value::new(margin)
            && ply >= self.nmp_min_ply
            && !beta.is_loss()
            && !prev_move.is_null()
            && !prev_is_pass
        {
            inc_stat!(self, nmp_attempted);
            let r = 7 + depth / 3;

            let use_pass = pos.is_pass_rights_enabled() && pos.can_pass();

            if use_pass {
                self.stack[ply as usize].current_move = Move::PASS;
            } else {
                self.stack[ply as usize].current_move = Move::NULL;
            }
            self.clear_cont_history_for_null(ply);

            if use_pass {
                pos.do_pass_move();
            } else {
                pos.do_null_move_with_prefetch(self.tt.as_ref());
            }
            self.nnue_push(DirtyPiece::new());
            let null_value = -self.search_node::<{ NodeType::NonPV as u8 }>(
                pos,
                depth - r,
                -beta,
                -beta + Value::new(1),
                ply + 1,
                false,
                limits,
                time_manager,
            );
            self.nnue_pop();
            if use_pass {
                pos.undo_pass_move();
            } else {
                pos.undo_null_move();
            }

            if null_value >= beta && !null_value.is_win() {
                if self.nmp_min_ply != 0 || depth < 16 {
                    inc_stat!(self, nmp_cutoff);
                    inc_stat_by_depth!(self, nmp_cutoff_by_depth, depth);
                    return (Some(null_value), improving);
                }

                self.nmp_min_ply = ply + 3 * (depth - r) / 4;

                let v = self.search_node::<{ NodeType::NonPV as u8 }>(
                    pos,
                    depth - r,
                    beta - Value::new(1),
                    beta,
                    ply,
                    false,
                    limits,
                    time_manager,
                );

                self.nmp_min_ply = 0;

                if v >= beta {
                    inc_stat!(self, nmp_cutoff);
                    inc_stat_by_depth!(self, nmp_cutoff_by_depth, depth);
                    return (Some(null_value), improving);
                }
            }
        }

        if !in_check && static_eval != Value::NONE {
            improving |= static_eval >= beta;
        }

        (None, improving)
    }

    /// ProbCut（YaneuraOu準拠）
    #[allow(clippy::too_many_arguments)]
    #[inline]
    pub(super) fn try_probcut(
        &mut self,
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
    ) -> Option<Value> {
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

        inc_stat!(self, probcut_attempted);

        let probcut_moves = {
            let cont_tables = self.cont_history_tables(ply);
            let mut mp = MovePicker::new_probcut(
                pos,
                tt_ctx.mv,
                threshold,
                ply,
                cont_tables,
                self.generate_all_legal_moves,
            );

            let mut buf = [Move::NONE; crate::movegen::MAX_MOVES];
            let mut len = 0;
            loop {
                let mv = mp.next_move(pos, &self.history);
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

            self.stack[ply as usize].current_move = mv;
            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_push(dirty_piece);
            self.nodes += 1;
            self.set_cont_history_for_move(
                ply,
                in_check,
                is_capture,
                cont_hist_piece,
                cont_hist_to,
            );
            let mut value = -self.qsearch::<{ NodeType::NonPV as u8 }>(
                pos,
                DEPTH_QS,
                -prob_beta,
                -prob_beta + Value::new(1),
                ply + 1,
                limits,
                time_manager,
            );

            if value >= prob_beta && probcut_depth > 0 {
                value = -self.search_node::<{ NodeType::NonPV as u8 }>(
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
            self.nnue_pop();
            pos.undo_move(mv);

            if value >= prob_beta {
                inc_stat!(self, probcut_cutoff);
                let stored_depth = (probcut_depth + 1).max(1);
                tt_ctx.result.write(
                    tt_ctx.key,
                    value_to_tt(value, ply),
                    self.stack[ply as usize].tt_pv,
                    Bound::Lower,
                    stored_depth,
                    mv,
                    unadjusted_static_eval,
                    self.tt.generation(),
                );
                inc_stat_by_depth!(self, tt_write_by_depth, stored_depth);

                if value.raw().abs() < Value::INFINITE.raw() {
                    return Some(value - (prob_beta - beta));
                }
                return Some(value);
            }
        }

        None
    }

    /// Small ProbCut
    #[inline]
    pub(super) fn try_small_probcut(
        &self,
        depth: Depth,
        beta: Value,
        tt_ctx: &TTContext,
    ) -> Option<Value> {
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

    /// Step14 の枝刈り（進行可否を返す）
    #[inline]
    pub(super) fn step14_pruning(&self, ctx: Step14Context<'_>) -> Step14Outcome {
        if ctx.mv.is_pass() {
            return Step14Outcome::Continue;
        }

        let mut lmr_depth = ctx.lmr_depth;

        if ctx.ply != 0 && !ctx.best_value.is_loss() {
            let lmp_denominator = 2 - ctx.improving as i32;
            debug_assert!(lmp_denominator > 0, "LMP denominator must be positive");
            let lmp_limit = (3 + ctx.depth * ctx.depth) / lmp_denominator;
            if ctx.move_count >= lmp_limit && !ctx.is_capture && !ctx.gives_check {
                return Step14Outcome::Skip { best_value: None };
            }

            if ctx.is_capture || ctx.gives_check {
                let captured = ctx.pos.piece_on(ctx.mv.to());
                let capt_hist = self.history.capture_history.get_with_captured_piece(
                    ctx.mv.moved_piece_after(),
                    ctx.mv.to(),
                    captured,
                ) as i32;

                if !ctx.gives_check && lmr_depth < 7 && !ctx.in_check {
                    let futility_value = self.stack[ctx.ply as usize].static_eval
                        + Value::new(232 + 224 * lmr_depth)
                        + Value::new(piece_value(captured))
                        + Value::new(131 * capt_hist / 1024);
                    if futility_value <= ctx.alpha {
                        return Step14Outcome::Skip { best_value: None };
                    }
                }

                let margin = (158 * ctx.depth + capt_hist / 31).clamp(0, 283 * ctx.depth);
                if !ctx.pos.see_ge(ctx.mv, Value::new(-margin)) {
                    return Step14Outcome::Skip { best_value: None };
                }
            } else {
                let mut history = 0;
                history += ctx.cont_history_1.get(ctx.mv.moved_piece_after(), ctx.mv.to()) as i32;
                history += ctx.cont_history_2.get(ctx.mv.moved_piece_after(), ctx.mv.to()) as i32;
                history += self.history.pawn_history.get(
                    ctx.pos.pawn_history_index(),
                    ctx.mv.moved_piece_after(),
                    ctx.mv.to(),
                ) as i32;

                if history < -4361 * ctx.depth {
                    return Step14Outcome::Skip { best_value: None };
                }

                history += 71 * self.history.main_history.get(ctx.mover, ctx.mv) as i32 / 32;
                lmr_depth += history / 3233;

                let base_futility = if ctx.best_move.is_some() { 46 } else { 230 };
                let futility_value = self.stack[ctx.ply as usize].static_eval
                    + Value::new(base_futility + 131 * lmr_depth)
                    + Value::new(
                        91 * (self.stack[ctx.ply as usize].static_eval > ctx.alpha) as i32,
                    );

                if !ctx.in_check && lmr_depth < 11 && futility_value <= ctx.alpha {
                    if ctx.best_value <= futility_value
                        && !ctx.best_value.is_mate_score()
                        && !futility_value.is_win()
                    {
                        return Step14Outcome::Skip {
                            best_value: Some(futility_value),
                        };
                    }
                    return Step14Outcome::Skip { best_value: None };
                }

                lmr_depth = lmr_depth.max(0);
                if !ctx.pos.see_ge(ctx.mv, Value::new(-26 * lmr_depth * lmr_depth)) {
                    return Step14Outcome::Skip { best_value: None };
                }
            }
        }

        Step14Outcome::Continue
    }
}
