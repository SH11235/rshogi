//! 静止探索 (Quiescence Search)
//!
//! 王手や駒取りなど、局面が安定するまで探索を続ける。

use crate::eval::evaluate_pass_rights;
use crate::position::Position;
use crate::types::{Bound, Depth, Move, Value, DEPTH_QS, DEPTH_UNSEARCHED, MAX_PLY};

use super::alpha_beta::{draw_jitter, to_corrected_static_eval, SearchWorker};
use super::movepicker::piece_value;
use super::stats::inc_stat_by_depth;
use super::types::{draw_value, value_from_tt, value_to_tt, NodeType, OrderedMovesBuffer};
use super::{LimitsType, MovePicker, TimeManagement};

impl SearchWorker {
    /// 静止探索
    #[inline]
    pub(super) fn qsearch<const NT: u8>(
        &mut self,
        pos: &mut Position,
        depth: Depth,
        alpha: Value,
        beta: Value,
        ply: i32,
        limits: &LimitsType,
        time_manager: &mut TimeManagement,
    ) -> Value {
        let pv_node = NT == NodeType::PV as u8;
        let in_check = pos.in_check();

        if ply >= MAX_PLY {
            return if in_check {
                Value::ZERO
            } else {
                self.nnue_evaluate(pos)
            };
        }

        if pv_node && self.state.sel_depth < ply + 1 {
            self.state.sel_depth = ply + 1;
        }

        if self.check_abort(limits, time_manager) {
            return Value::ZERO;
        }

        let rep_state = pos.repetition_state(ply);
        if rep_state.is_repetition() || rep_state.is_superior_inferior() {
            let v = draw_value(rep_state, pos.side_to_move());
            if v != Value::NONE {
                if v == Value::DRAW {
                    let jittered = Value::new(v.raw() + draw_jitter(self.state.nodes));
                    return value_from_tt(jittered, ply);
                }
                return value_from_tt(v, ply);
            }
        }

        // 引き分け手数ルール（YaneuraOu準拠、MaxMovesToDrawオプション）
        if self.max_moves_to_draw > 0 && pos.game_ply() > self.max_moves_to_draw {
            return Value::new(Value::DRAW.raw() + draw_jitter(self.state.nodes));
        }

        let key = pos.key();
        let tt_result = self.tt.probe(key, pos);
        let tt_hit = tt_result.found;
        let tt_data = tt_result.data;
        let pv_hit = tt_hit && tt_data.is_pv;
        self.state.stack[ply as usize].tt_hit = tt_hit;
        self.state.stack[ply as usize].tt_pv = pv_hit;
        let mut tt_move = if tt_hit { tt_data.mv } else { Move::NONE };
        let tt_value = if tt_hit {
            value_from_tt(tt_data.value, ply)
        } else {
            Value::NONE
        };

        if !pv_node
            && tt_hit
            && tt_data.depth >= DEPTH_QS
            && tt_value != Value::NONE
            && tt_data.bound.can_cutoff(tt_value, beta)
        {
            return tt_value;
        }

        let mut best_move = Move::NONE;

        let correction_value = self.correction_value(pos, ply);
        let mut unadjusted_static_eval = Value::NONE;
        let mut static_eval = if in_check {
            Value::NONE
        } else if tt_hit && tt_data.eval != Value::NONE {
            unadjusted_static_eval = tt_data.eval;
            unadjusted_static_eval
        } else {
            // 置換表に無いときだけ簡易1手詰め判定を行う
            if !tt_hit {
                let mate_move = pos.mate_1ply();
                if mate_move.is_some() {
                    return Value::mate_in(ply + 1);
                }
            }
            unadjusted_static_eval = self.nnue_evaluate(pos);
            unadjusted_static_eval
        };

        if !in_check && unadjusted_static_eval != Value::NONE {
            static_eval = to_corrected_static_eval(unadjusted_static_eval, correction_value);
            // パス権評価を動的に追加（TTには保存されないので手数依存でもOK）
            static_eval += evaluate_pass_rights(pos, pos.game_ply() as u16);
        }

        self.state.stack[ply as usize].static_eval = static_eval;

        let mut alpha = alpha;
        let mut best_value = if in_check {
            Value::mated_in(ply)
        } else {
            static_eval
        };

        if !in_check && tt_hit && tt_value != Value::NONE && !tt_value.is_mate_score() {
            let improves = (tt_value > best_value && tt_data.bound == Bound::Lower)
                || (tt_value < best_value && tt_data.bound == Bound::Upper);
            if improves {
                best_value = tt_value;
                static_eval = tt_value;
                self.state.stack[ply as usize].static_eval = static_eval;
            }
        }

        if !in_check && best_value >= beta {
            let mut v = best_value;
            if !v.is_mate_score() {
                v = Value::new((v.raw() + beta.raw()) / 2);
            }
            if !tt_hit {
                // YaneuraOu: pvHitを使用
                tt_result.write(
                    key,
                    value_to_tt(v, ply),
                    pv_hit,
                    Bound::Lower,
                    DEPTH_UNSEARCHED,
                    Move::NONE,
                    unadjusted_static_eval,
                    self.tt.generation(),
                );
                inc_stat_by_depth!(self, tt_write_by_depth, 0);
            }
            return v;
        }

        if !in_check && best_value > alpha {
            alpha = best_value;
        }

        let futility_base = if in_check {
            Value::NONE
        } else {
            static_eval + Value::new(352)
        };

        if depth <= DEPTH_QS
            && tt_move.is_some()
            && ((!pos.capture_stage(tt_move) && !pos.gives_check(tt_move)) || depth < -16)
        {
            tt_move = Move::NONE;
        }

        let prev_move = if ply >= 1 {
            self.state.stack[(ply - 1) as usize].current_move
        } else {
            Move::NONE
        };

        let ordered_moves = {
            let cont_tables = self.cont_history_tables(ply);
            let mut buf_moves = OrderedMovesBuffer::new();

            {
                let mut mp = if in_check {
                    MovePicker::new_evasions(
                        pos,
                        tt_move,
                        ply,
                        cont_tables,
                        self.generate_all_legal_moves,
                    )
                } else {
                    MovePicker::new(
                        pos,
                        tt_move,
                        DEPTH_QS,
                        ply,
                        cont_tables,
                        self.generate_all_legal_moves,
                    )
                };

                loop {
                    let mv = mp.next_move(pos, &self.history);
                    if mv == Move::NONE {
                        break;
                    }
                    buf_moves.push(mv);
                }
            }

            if !in_check && depth == DEPTH_QS {
                let mut buf = crate::movegen::ExtMoveBuffer::new();
                let gen_type = if self.generate_all_legal_moves {
                    crate::movegen::GenType::QuietChecksAll
                } else {
                    crate::movegen::GenType::QuietChecks
                };
                crate::movegen::generate_with_type(pos, gen_type, &mut buf, None);
                for ext in buf.iter() {
                    if buf_moves.contains(&ext.mv) {
                        continue;
                    }
                    buf_moves.push(ext.mv);
                }
            }

            if !in_check && depth <= -5 && ply >= 1 && prev_move.is_normal() {
                let mut buf = crate::movegen::ExtMoveBuffer::new();
                let rec_sq = prev_move.to();
                let gen_type = if self.generate_all_legal_moves {
                    crate::movegen::GenType::RecapturesAll
                } else {
                    crate::movegen::GenType::Recaptures
                };
                crate::movegen::generate_with_type(pos, gen_type, &mut buf, Some(rec_sq));
                buf_moves.clear();
                for ext in buf.iter() {
                    buf_moves.push(ext.mv);
                }
            }

            buf_moves
        };

        let mut move_count = 0;

        for mv in ordered_moves.iter() {
            // 静止探索では PASS は対象外（TT手として来る可能性があるため明示的にスキップ）
            if mv.is_pass() {
                continue;
            }

            if !pos.is_legal(mv) {
                continue;
            }

            let gives_check = pos.gives_check(mv);
            let capture = pos.capture_stage(mv);

            if !in_check && depth <= DEPTH_QS && !capture && !gives_check {
                continue;
            }

            if !in_check && capture && !pos.see_ge(mv, Value::ZERO) {
                continue;
            }

            move_count += 1;

            if !best_value.is_loss() {
                if !gives_check
                    && (!prev_move.is_normal() || mv.to() != prev_move.to())
                    && futility_base != Value::NONE
                {
                    if move_count > 2 {
                        continue;
                    }

                    let futility_value =
                        futility_base + Value::new(piece_value(pos.piece_on(mv.to())));

                    if futility_value <= alpha {
                        best_value = best_value.max(futility_value);
                        continue;
                    }

                    if !pos.see_ge(mv, alpha - futility_base) {
                        best_value = best_value.min(alpha.min(futility_base));
                        continue;
                    }
                }
                if !capture {
                    let mut cont_score = 0;

                    // ss-1の参照（ContinuationHistory直結）
                    cont_score +=
                        self.cont_history_ref(ply, 1).get(mv.moved_piece_after(), mv.to()) as i32;

                    let pawn_idx = pos.pawn_history_index();
                    cont_score +=
                        self.history.pawn_history.get(pawn_idx, pos.moved_piece(mv), mv.to())
                            as i32;
                    if cont_score <= 5868 {
                        continue;
                    }
                }

                if !pos.see_ge(mv, Value::new(-74)) {
                    continue;
                }
            }

            self.state.stack[ply as usize].current_move = mv;

            let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, self.tt.as_ref());
            self.nnue_push(dirty_piece);
            self.state.nodes += 1;

            // PASS は to()/moved_piece_after() が未定義のため、null move と同様に扱う
            if mv.is_pass() {
                self.clear_cont_history_for_null(ply);
            } else {
                let cont_hist_pc = mv.moved_piece_after();
                let cont_hist_to = mv.to();
                self.set_cont_history_for_move(ply, in_check, capture, cont_hist_pc, cont_hist_to);
            }

            let value =
                -self.qsearch::<NT>(pos, depth - 1, -beta, -alpha, ply + 1, limits, time_manager);

            self.nnue_pop();
            pos.undo_move(mv);

            if self.state.abort {
                return Value::ZERO;
            }

            if value > best_value {
                best_value = value;
                best_move = mv;

                if value > alpha {
                    alpha = value;

                    if value >= beta {
                        break;
                    }
                }
            }
        }

        if in_check && move_count == 0 {
            return Value::mated_in(ply);
        }

        if !best_value.is_mate_score() && best_value > beta {
            best_value = Value::new((best_value.raw() + beta.raw()) / 2);
        }

        let bound = if best_value >= beta {
            Bound::Lower
        } else if pv_node && best_move.is_some() {
            Bound::Exact
        } else {
            Bound::Upper
        };

        // YaneuraOu: pvHitを使用
        tt_result.write(
            key,
            value_to_tt(best_value, ply),
            pv_hit,
            bound,
            DEPTH_QS,
            best_move,
            unadjusted_static_eval,
            self.tt.generation(),
        );
        inc_stat_by_depth!(self, tt_write_by_depth, 0);

        best_value
    }
}
