//! 静止探索 (Quiescence Search)
//!
//! 王手や駒取りなど、局面が安定するまで探索を続ける。

use crate::eval::evaluate_pass_rights;
use crate::position::Position;
use crate::types::{Bound, Depth, Move, Value, DEPTH_QS, DEPTH_UNSEARCHED, MAX_PLY};

use super::alpha_beta::{draw_jitter, to_corrected_static_eval, SearchContext, SearchState};
use super::eval_helpers::correction_value;
use super::movepicker::piece_value;
use super::search_helpers::{
    check_abort, clear_cont_history_for_null, cont_history_ref, cont_history_tables, nnue_evaluate,
    nnue_pop, nnue_push, set_cont_history_for_move,
};
use super::stats::{inc_stat, inc_stat_by_depth};
use super::tt_sanity::{
    helper_tt_write_enabled_for_depth, is_valid_tt_eval, is_valid_tt_stored_value,
    maybe_log_invalid_tt_data, maybe_trace_tt_cutoff, maybe_trace_tt_probe, maybe_trace_tt_write,
    InvalidTtLog, TtCutoffTrace, TtProbeTrace, TtWriteTrace,
};
use super::types::{draw_value, value_from_tt, value_to_tt, NodeType, OrderedMovesBuffer};
use super::{LimitsType, MovePicker, TimeManagement};

/// 静止探索
#[allow(clippy::too_many_arguments)]
pub(super) fn qsearch<const NT: u8>(
    st: &mut SearchState,
    ctx: &SearchContext<'_>,
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

    // 静止探索統計
    inc_stat!(st, qs_nodes);
    #[cfg(feature = "search-stats")]
    {
        // depth を 0, -1, -2, ... から 0, 1, 2, ... にマップ
        let depth_idx = (-depth).max(0) as usize;
        if depth_idx < super::stats::STATS_MAX_DEPTH {
            st.stats.qs_nodes_by_depth[depth_idx] += 1;
        }
        if in_check {
            st.stats.qs_in_check_nodes += 1;
        }
    }

    if ply >= MAX_PLY {
        return if in_check {
            Value::ZERO
        } else {
            nnue_evaluate(st, pos)
        };
    }

    if pv_node && st.sel_depth < ply + 1 {
        st.sel_depth = ply + 1;
    }

    if check_abort(st, ctx, limits, time_manager) {
        return Value::ZERO;
    }

    let rep_state = pos.repetition_state(ply);
    if rep_state.is_repetition() || rep_state.is_superior_inferior() {
        let v = draw_value(rep_state, pos.side_to_move());
        if v != Value::NONE {
            if v == Value::DRAW {
                let jittered = Value::new(v.raw() + draw_jitter(st.nodes, &ctx.tune_params));
                return value_from_tt(jittered, ply);
            }
            return value_from_tt(v, ply);
        }
    }

    // 引き分け手数ルール（YaneuraOu準拠、MaxMovesToDrawオプション）
    if ctx.max_moves_to_draw > 0 && pos.game_ply() > ctx.max_moves_to_draw {
        return Value::new(Value::DRAW.raw() + draw_jitter(st.nodes, &ctx.tune_params));
    }

    let key = pos.key();
    let tt_result = ctx.tt.probe(key, pos);
    let tt_hit = tt_result.found;
    let mut tt_data = tt_result.data;
    let pv_hit = tt_hit && tt_data.is_pv;
    st.stack[ply as usize].tt_hit = tt_hit;
    // YaneuraOu準拠: qsearch では ss->ttPv に上書きしない (yaneuraou-search.cpp:4289)
    // pv_hit はローカル変数のみで使用
    let mut tt_move = if tt_hit { tt_data.mv } else { Move::NONE };
    if tt_move.is_some() && !pos.pseudo_legal_with_all(tt_move, ctx.generate_all_legal_moves) {
        maybe_log_invalid_tt_data(InvalidTtLog {
            reason: "invalid_move",
            stage: "qsearch_probe",
            thread_id: ctx.thread_id,
            ply,
            key,
            depth: tt_data.depth,
            bound: tt_data.bound,
            tt_move,
            stored_value: tt_data.value,
            converted_value: Value::NONE,
            eval: tt_data.eval,
        });
        tt_move = Move::NONE;
    }
    let mut tt_value = if tt_hit {
        value_from_tt(tt_data.value, ply)
    } else {
        Value::NONE
    };
    if tt_hit && !is_valid_tt_stored_value(tt_data.value) {
        maybe_log_invalid_tt_data(InvalidTtLog {
            reason: "invalid_value",
            stage: "qsearch_probe",
            thread_id: ctx.thread_id,
            ply,
            key,
            depth: tt_data.depth,
            bound: tt_data.bound,
            tt_move,
            stored_value: tt_data.value,
            converted_value: tt_value,
            eval: tt_data.eval,
        });
        tt_value = Value::NONE;
    }
    if tt_hit && !is_valid_tt_eval(tt_data.eval) {
        maybe_log_invalid_tt_data(InvalidTtLog {
            reason: "invalid_eval",
            stage: "qsearch_probe",
            thread_id: ctx.thread_id,
            ply,
            key,
            depth: tt_data.depth,
            bound: tt_data.bound,
            tt_move,
            stored_value: tt_data.value,
            converted_value: tt_value,
            eval: tt_data.eval,
        });
        tt_data.eval = Value::NONE;
    }
    maybe_trace_tt_probe(TtProbeTrace {
        stage: "qsearch_probe",
        thread_id: ctx.thread_id,
        ply,
        key,
        hit: tt_hit,
        depth: tt_data.depth,
        bound: tt_data.bound,
        tt_move,
        stored_value: tt_data.value,
        converted_value: tt_value,
        eval: tt_data.eval,
        root_move: if ply >= 1 {
            st.stack[0].current_move
        } else {
            Move::NONE
        },
    });

    // TT ヒット統計
    if tt_hit {
        inc_stat!(st, qs_tt_hit);
    }

    if !pv_node
        && tt_hit
        && tt_data.depth >= DEPTH_QS
        && tt_value != Value::NONE
        && tt_data.bound.can_cutoff(tt_value, beta)
    {
        maybe_trace_tt_cutoff(TtCutoffTrace {
            stage: "qsearch_cutoff",
            thread_id: ctx.thread_id,
            ply,
            key,
            search_depth: depth,
            depth: tt_data.depth,
            bound: tt_data.bound,
            value: tt_value,
            beta,
            root_move: if ply >= 1 {
                st.stack[0].current_move
            } else {
                Move::NONE
            },
        });
        inc_stat!(st, qs_tt_cutoff);
        return tt_value;
    }

    let mut best_move = Move::NONE;

    let corr_value = correction_value(st, ctx, pos, ply);
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
                let mate_value = Value::mate_in(ply + 1);
                if ctx.allow_tt_write
                    && helper_tt_write_enabled_for_depth(ctx.thread_id, Bound::Exact, DEPTH_QS)
                {
                    maybe_trace_tt_write(TtWriteTrace {
                        stage: "qsearch_mate1_store",
                        thread_id: ctx.thread_id,
                        ply,
                        key,
                        depth: DEPTH_QS,
                        bound: Bound::Exact,
                        is_pv: pv_hit,
                        tt_move: mate_move,
                        stored_value: mate_value,
                        eval: unadjusted_static_eval,
                        root_move: if ply >= 1 {
                            st.stack[0].current_move
                        } else {
                            Move::NONE
                        },
                    });
                    tt_result.write(
                        key,
                        mate_value,
                        pv_hit,
                        Bound::Exact,
                        DEPTH_QS,
                        mate_move,
                        unadjusted_static_eval,
                        ctx.tt.generation(),
                    );
                    inc_stat_by_depth!(st, tt_write_by_depth, 0);
                }
                return mate_value;
            }
        }
        unadjusted_static_eval = nnue_evaluate(st, pos);
        unadjusted_static_eval
    };

    if !in_check && unadjusted_static_eval != Value::NONE {
        static_eval = to_corrected_static_eval(unadjusted_static_eval, corr_value);
        // パス権評価を動的に追加（TTには保存されないので手数依存でもOK）
        static_eval += evaluate_pass_rights(pos, pos.game_ply() as u16);
    }

    st.stack[ply as usize].static_eval = static_eval;

    let mut alpha = alpha;
    let mut best_value = if in_check {
        Value::mated_in(ply)
    } else {
        static_eval
    };

    if !in_check && tt_hit && tt_value != Value::NONE && !tt_value.is_mate_score() {
        // YO準拠: ttValue で補正するのは bestValue のみ。ss->staticEval は維持する。
        let bound_matches = if tt_value > best_value {
            tt_data.bound.is_lower_or_exact()
        } else {
            matches!(tt_data.bound, Bound::Upper | Bound::Exact)
        };
        if bound_matches {
            best_value = tt_value;
        }
    }

    if !in_check && best_value >= beta {
        inc_stat!(st, qs_stand_pat_cutoff);
        let mut v = best_value;
        if !v.is_mate_score() {
            v = Value::new((v.raw() + beta.raw()) / 2);
        }
        if !tt_hit {
            // YaneuraOu準拠: stand pat cutoff 時は ttPv=false で保存
            // (yaneuraou-search.cpp:4454)
            if ctx.allow_tt_write
                && helper_tt_write_enabled_for_depth(ctx.thread_id, Bound::Lower, DEPTH_UNSEARCHED)
            {
                maybe_trace_tt_write(TtWriteTrace {
                    stage: "qsearch_stand_pat_store",
                    thread_id: ctx.thread_id,
                    ply,
                    key,
                    depth: DEPTH_UNSEARCHED,
                    bound: Bound::Lower,
                    is_pv: false,
                    tt_move: Move::NONE,
                    stored_value: value_to_tt(v, ply),
                    eval: unadjusted_static_eval,
                    root_move: if ply >= 1 {
                        st.stack[0].current_move
                    } else {
                        Move::NONE
                    },
                });
                tt_result.write(
                    key,
                    value_to_tt(v, ply),
                    false,
                    Bound::Lower,
                    DEPTH_UNSEARCHED,
                    Move::NONE,
                    unadjusted_static_eval,
                    ctx.tt.generation(),
                );
                inc_stat_by_depth!(st, tt_write_by_depth, 0);
            }
        }
        return v;
    }

    if !in_check && best_value > alpha {
        alpha = best_value;
    }

    let futility_base = if in_check {
        Value::NONE
    } else {
        static_eval + Value::new(ctx.tune_params.qsearch_futility_base)
    };

    if depth <= DEPTH_QS
        && tt_move.is_some()
        && ((!pos.capture_stage(tt_move) && !pos.gives_check(tt_move)) || depth < -16)
    {
        tt_move = Move::NONE;
    }

    let prev_move = if ply >= 1 {
        st.stack[(ply - 1) as usize].current_move
    } else {
        Move::NONE
    };

    let ordered_moves = {
        let cont_tables = cont_history_tables(st, ctx, ply);
        let mut buf_moves = OrderedMovesBuffer::new();

        {
            let mut mp = if in_check {
                MovePicker::new_evasions(
                    pos,
                    tt_move,
                    ply,
                    cont_tables,
                    ctx.generate_all_legal_moves,
                )
            } else {
                MovePicker::new(
                    pos,
                    tt_move,
                    DEPTH_QS,
                    ply,
                    cont_tables,
                    ctx.generate_all_legal_moves,
                )
            };

            loop {
                let mv = ctx.history.with_read(|h| mp.next_move(pos, h));
                if mv == Move::NONE {
                    break;
                }
                buf_moves.push(mv);
            }
        }

        if !in_check && depth == DEPTH_QS {
            let mut buf = crate::movegen::ExtMoveBuffer::new();
            let gen_type = if ctx.generate_all_legal_moves {
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
            let gen_type = if ctx.generate_all_legal_moves {
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

    // 生成された手の数を記録
    #[cfg(feature = "search-stats")]
    {
        st.stats.qs_moves_generated += ordered_moves.len() as u64;
    }

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
            inc_stat!(st, qs_see_pruned);
            continue;
        }

        move_count += 1;

        if !best_value.is_loss() {
            if !gives_check
                && (!prev_move.is_normal() || mv.to() != prev_move.to())
                && futility_base != Value::NONE
            {
                if move_count > 2 {
                    inc_stat!(st, qs_futility_pruned);
                    continue;
                }

                let futility_value = futility_base + Value::new(piece_value(pos.piece_on(mv.to())));

                if futility_value <= alpha {
                    inc_stat!(st, qs_futility_pruned);
                    best_value = best_value.max(futility_value);
                    continue;
                }

                if !pos.see_ge(mv, alpha - futility_base) {
                    inc_stat!(st, qs_futility_pruned);
                    best_value = best_value.min(alpha.min(futility_base));
                    continue;
                }
            }
            if !capture {
                let cont_score =
                    cont_history_ref(st, ctx, ply, 1).get(mv.moved_piece_after(), mv.to()) as i32;

                let pawn_idx = pos.pawn_history_index();
                let pawn_score = ctx.history.with_read(|h| {
                    h.pawn_history.get(pawn_idx, pos.moved_piece(mv), mv.to()) as i32
                });
                if cont_score + pawn_score <= 5868 {
                    inc_stat!(st, qs_history_pruned);
                    continue;
                }
            }

            if !pos.see_ge(mv, Value::new(-74)) {
                inc_stat!(st, qs_see_margin_pruned);
                continue;
            }
        }

        st.stack[ply as usize].current_move = mv;

        // 実際に探索された手をカウント
        inc_stat!(st, qs_moves_searched);

        let dirty_piece = pos.do_move_with_prefetch(mv, gives_check, ctx.tt);
        nnue_push(st, dirty_piece);
        st.nodes += 1;

        // PASS は to()/moved_piece_after() が未定義のため、null move と同様に扱う
        if mv.is_pass() {
            clear_cont_history_for_null(st, ctx, ply);
        } else {
            let cont_hist_pc = mv.moved_piece_after();
            let cont_hist_to = mv.to();
            set_cont_history_for_move(st, ctx, ply, in_check, capture, cont_hist_pc, cont_hist_to);
        }

        let value =
            -qsearch::<NT>(st, ctx, pos, depth - 1, -beta, -alpha, ply + 1, limits, time_manager);

        nnue_pop(st);
        pos.undo_move(mv);

        if st.abort {
            return Value::ZERO;
        }

        if value > best_value {
            best_value = value;
            best_move = mv;

            if value > alpha {
                if value >= beta {
                    break;
                }
                // YaneuraOu準拠: fail-high しなかった場合のみ alpha を更新する。
                alpha = value;
            }
        }
    }

    if in_check && move_count == 0 {
        return Value::mated_in(ply);
    }

    if !best_value.is_mate_score() && best_value > beta {
        best_value = Value::new((best_value.raw() + beta.raw()) / 2);
    }

    // YaneuraOu準拠: qsearchの結果は Exact としては保存しない。
    let bound = if best_value >= beta {
        Bound::Lower
    } else {
        Bound::Upper
    };

    // YaneuraOu: pvHitを使用
    if ctx.allow_tt_write && helper_tt_write_enabled_for_depth(ctx.thread_id, bound, DEPTH_QS) {
        maybe_trace_tt_write(TtWriteTrace {
            stage: "qsearch_store",
            thread_id: ctx.thread_id,
            ply,
            key,
            depth: DEPTH_QS,
            bound,
            is_pv: pv_hit,
            tt_move: best_move,
            stored_value: value_to_tt(best_value, ply),
            eval: unadjusted_static_eval,
            root_move: if ply >= 1 {
                st.stack[0].current_move
            } else {
                Move::NONE
            },
        });
        tt_result.write(
            key,
            value_to_tt(best_value, ply),
            pv_hit,
            bound,
            DEPTH_QS,
            best_move,
            unadjusted_static_eval,
            ctx.tt.generation(),
        );
        inc_stat_by_depth!(st, tt_write_by_depth, 0);
    }

    best_value
}
