//! `use-lazy-evaluate` 有効時の静的評価の取得元を観測する。
//!
//! 非 PV ノードの TT hit で eval が有効なら TT の eval を再利用し、
//! PV ノードまたは TT の eval が無効なら評価関数を呼び直すことを確認する。
use std::sync::Arc;

use crate::eval::EvalHash;
use crate::position::Position;
use crate::search::alpha_beta::{SearchContext, SearchWorker, TTContext};
use crate::search::{LimitsType, SearchTuneParams};
use crate::tt::TranspositionTable;
use crate::types::{Bound, Move, Value};

/// 評価関数が返す値と区別できる TT eval
const TT_EVAL: Value = Value::new(1234);

/// `compute_eval_context` が返す未補正の静的評価を取得する
fn unadjusted_static_eval(pv_node: bool, tt_eval: Value) -> (Value, Value) {
    let mut pos = Position::new();
    pos.set_sfen("8k/9/9/9/4P4/9/9/9/K8 b - 1").unwrap();
    assert!(!pos.in_check());
    let fresh = crate::eval::material::evaluate_material(&pos);
    assert_ne!(fresh, TT_EVAL);

    let mut worker = SearchWorker::new(
        Arc::new(TranspositionTable::new(1)),
        Arc::new(EvalHash::new(1)),
        0,
        0,
        SearchTuneParams::default(),
    );
    let limits = LimitsType {
        depth: 1,
        ..Default::default()
    };
    worker.prepare_search(&limits);

    let key = pos.key();
    let _ = worker.tt.probe(key, &pos).write(
        key,
        Value::NONE,
        pv_node,
        Bound::None,
        0,
        Move::NONE,
        tt_eval,
        worker.tt.generation(),
    );
    let result = worker.tt.probe(key, &pos);
    assert!(result.found);
    assert_eq!(result.data.eval, tt_eval);
    let tt_ctx = TTContext {
        key,
        data: result.data,
        hit: true,
        mv: Move::NONE,
        value: Value::NONE,
        capture: false,
        result,
    };

    let ctx = SearchContext {
        tt: &worker.tt,
        eval_hash: &worker.eval_hash,
        history: &worker.history,
        cont_history_sentinel: worker.cont_history_sentinel,
        generate_all_legal_moves: worker.generate_all_legal_moves,
        max_moves_to_draw: worker.max_moves_to_draw,
        thread_id: worker.thread_id,
        allow_tt_write: worker.allow_tt_write,
        tune_params: &worker.search_tune_params,
        reductions: &worker.reductions,
        draw_value_table: worker.draw_value_table,
    };
    let eval_ctx = super::super::eval_helpers::compute_eval_context(
        &mut worker.state,
        &ctx,
        &mut pos,
        2,
        false,
        pv_node,
        &tt_ctx,
        Move::NONE,
    );
    (eval_ctx.unadjusted_static_eval, fresh)
}

#[test]
fn lazy_evaluate_reuses_tt_eval_only_at_non_pv_hit() {
    let guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            // 非 PV + TT hit + 有効な eval: TT の eval をそのまま使う。
            let (value, _) = unadjusted_static_eval(false, TT_EVAL);
            assert_eq!(value, TT_EVAL);
            // PV ノード: TT の eval があっても評価関数を呼び直す。
            let (value, fresh) = unadjusted_static_eval(true, TT_EVAL);
            assert_eq!(value, fresh);
            // TT の eval が無効: 評価関数を呼び直す。
            let (value, fresh) = unadjusted_static_eval(false, Value::NONE);
            assert_eq!(value, fresh);
        })
        .unwrap()
        .join()
        .unwrap();
    drop(guard);
}
