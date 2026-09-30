//! TT と EvalHash の評価値を区別し、probe 条件と store の維持を検証する。
use std::sync::{Arc, atomic::AtomicBool};

use crate::eval::{EvalHash, eval_hash_enabled, set_eval_hash_enabled};
use crate::position::Position;
use crate::search::alpha_beta::{SearchContext, SearchWorker, TTContext};
use crate::search::{LimitsType, SearchTuneParams, TimeManagement};
use crate::tt::TranspositionTable;
use crate::types::{Bound, Move, Value};

struct RestoreOptions {
    use_hash: bool,
}

impl Drop for RestoreOptions {
    fn drop(&mut self) {
        set_eval_hash_enabled(self.use_hash);
    }
}

#[test]
fn eval_hash_probe_policy_preserves_stores_and_tt_eval_paths() {
    let _guard = crate::eval::material::test_support::lock_material();
    let _restore = RestoreOptions {
        use_hash: eval_hash_enabled(),
    };
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let mut worker = SearchWorker::new(
                Arc::new(TranspositionTable::new(1)),
                Arc::new(EvalHash::new(1)),
                0,
                0,
                SearchTuneParams::default(),
            );
            let mut pos = Position::new();
            pos.set_sfen("8k/9/9/9/4P4/9/9/9/K8 b - 1").unwrap();
            let fresh = crate::eval::material::evaluate_material(&pos);
            assert!(fresh > Value::new(-1000));
            assert!(pos.mate_1ply().is_none());
            for use_hash in [false, true] {
                set_eval_hash_enabled(use_hash);
                for qs in [false, true] {
                    for pv in [false, true] {
                        for tt_eval in [None, Some(Value::NONE), Some(Value::new(2345))] {
                            for warm in [false, true] {
                                check_case(&mut worker, &mut pos, qs, pv, tt_eval, warm);
                            }
                        }
                    }
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

fn check_case(
    worker: &mut SearchWorker,
    pos: &mut Position,
    qs: bool,
    pv: bool,
    tt_eval: Option<Value>,
    warm: bool,
) {
    let limits = LimitsType {
        depth: 1,
        ..Default::default()
    };
    worker.prepare_search(&limits);
    Arc::get_mut(&mut worker.tt).unwrap().clear();
    worker.eval_hash.clear();
    let fresh = crate::eval::material::evaluate_material(pos);
    let cached = Value::new(1234);
    assert_ne!(cached, fresh);
    let key = pos.key();
    if warm {
        worker.eval_hash.store(key, cached.raw());
    }
    if let Some(eval) = tt_eval {
        assert!(worker.tt.probe(key, pos).write(
            key,
            Value::NONE,
            pv,
            Bound::None,
            0,
            Move::NONE,
            eval,
            worker.tt.generation(),
        ));
    }
    let result = worker.tt.probe(key, pos);
    assert_eq!(result.found, tt_eval.is_some());
    let tt_ctx = TTContext {
        key,
        data: result.data,
        hit: result.found,
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
        entering_king_rule: worker.entering_king_rule,
    };
    let value = if qs {
        let mut tm =
            TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        if pv {
            super::super::qsearch::qsearch::<{ crate::search::NodeType::PV as u8 }>(
                &mut worker.state,
                &ctx,
                pos,
                Value::new(-1001),
                Value::new(-1000),
                2,
                &limits,
                &mut tm,
            );
        } else {
            super::super::qsearch::qsearch::<{ crate::search::NodeType::NonPV as u8 }>(
                &mut worker.state,
                &ctx,
                pos,
                Value::new(-1001),
                Value::new(-1000),
                2,
                &limits,
                &mut tm,
            );
        }
        worker.state.stack[2].static_eval
    } else {
        super::super::eval_helpers::compute_eval_context(
            &mut worker.state,
            &ctx,
            pos,
            2,
            false,
            pv,
            &tt_ctx,
            Move::NONE,
        )
        .unadjusted_static_eval
    };
    let use_tt = tt_eval.is_some_and(|v| v != Value::NONE)
        && (qs || (cfg!(feature = "use-lazy-evaluate") && !pv));
    let probe = tt_ctx.hit;
    let expected = if use_tt {
        tt_eval.unwrap()
    } else if eval_hash_enabled() && probe && warm {
        cached
    } else {
        fresh
    };
    assert_eq!(value, expected, "qs={qs} pv={pv} tt_eval={tt_eval:?} warm={warm}");
    let stored = if eval_hash_enabled() && !use_tt {
        Some(expected.raw())
    } else {
        warm.then_some(cached.raw())
    };
    assert_eq!(worker.eval_hash.probe(key), stored);
    if !tt_ctx.hit {
        assert_eq!(worker.tt.probe(key, pos).data.eval, expected);
    }
    #[cfg(feature = "search-stats")]
    {
        let mut expected_stats = [[super::super::stats::EvalHashProbeStats::default(); 2]; 2];
        if eval_hash_enabled() && !use_tt {
            expected_stats[usize::from(qs)][usize::from(tt_ctx.hit)] =
                super::super::stats::EvalHashProbeStats {
                    probes: u64::from(probe),
                    hits: u64::from(probe && warm),
                    skipped: u64::from(!probe),
                };
        }
        assert_eq!(worker.state.stats.eval_hash, expected_stats);
    }
}
