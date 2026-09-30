//! ProbCut の実 qsearch 停止と通常子探索 callback 中断の復元・TT 契約。
use std::cell::Cell;
use std::sync::{Arc, atomic::AtomicBool};

use crate::eval::EvalHash;
use crate::position::Position;
use crate::search::alpha_beta::{SearchContext, SearchWorker, TTContext};
use crate::search::pruning::try_probcut;
use crate::search::{LimitsType, SearchTuneParams, TimeManagement};
use crate::tt::TranspositionTable;
use crate::types::{Bound, DEPTH_QS, Move, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildOutcome {
    QsearchStop,
    SearchAbort,
    Complete,
}

#[cfg(all(
    feature = "halfkx-arch",
    any(not(feature = "mode-specific"), feature = "ft-halfkp")
))]
fn install_stack(worker: &mut SearchWorker) {
    use crate::nnue::halfkp::{HalfKPL256, HalfKPNetwork};
    use crate::nnue::network_halfkp::{HalfKP256CReLU, halfkp_loader_fixture};
    use crate::nnue::search_evaluator::SearchEvaluator;
    static NET: std::sync::OnceLock<Arc<HalfKPNetwork>> = std::sync::OnceLock::new();
    let net = NET.get_or_init(|| {
        let bytes = halfkp_loader_fixture(256, "Features=HalfKP[125388->256x2],l2=32,l3=32");
        let net = HalfKP256CReLU::read(&mut std::io::Cursor::new(bytes)).unwrap();
        Arc::new(HalfKPNetwork::L256(HalfKPL256::CReLU32x32(Box::new(net))))
    });
    worker.state.evaluator =
        SearchEvaluator::from_network(&crate::nnue::NNUENetwork::HalfKP(Arc::clone(net)));
}

#[cfg(all(
    feature = "halfkx-arch",
    any(not(feature = "mode-specific"), feature = "ft-halfkp")
))]
fn stack_index(state: &crate::search::alpha_beta::SearchState) -> usize {
    match &state.evaluator {
        crate::nnue::search_evaluator::SearchEvaluator::HalfKP { stack, .. } => {
            stack.current_index()
        }
        _ => panic!("fixture uses a typed HalfKP evaluator"),
    }
}

fn run_case(outcome: ChildOutcome, beta: i32, existing_parent: bool) {
    let tt = Arc::new(TranspositionTable::new(1));
    let mut worker =
        SearchWorker::new(tt, Arc::new(EvalHash::new(1)), 0, 0, SearchTuneParams::default());
    let limits = LimitsType {
        depth: 6,
        ..Default::default()
    };
    worker.prepare_search(&limits);
    #[cfg(all(
        feature = "halfkx-arch",
        any(not(feature = "mode-specific"), feature = "ft-halfkp")
    ))]
    install_stack(&mut worker);
    // qsearch は TT hit または入口の stop で返るため NNUE forward は実行しない。
    worker.state.calls_cnt = 1;
    let mut pos = Position::new();
    pos.set_sfen("K8/9/9/4r4/4R4/9/9/9/8k b - 1").unwrap();
    let capture = pos.to_move(Move::from_usi("5e5d").unwrap()).unwrap();
    assert!(pos.is_legal(capture) && pos.is_capture(capture) && !pos.in_check());
    let before_sfen = pos.to_sfen();
    let before_key = pos.key();
    let prob_beta = beta + worker.search_tune_params.probcut_beta_margin_base;
    assert_eq!(prob_beta > 0, beta == 0);

    let mut child = pos.clone();
    let gives_check = child.gives_check(capture);
    child.do_move(capture, gives_check);
    // 通常子探索を呼ぶため、最初の qsearch を確定 TT 値で制御する。
    let qs_value = Value::new(-(prob_beta + 100));
    assert!(worker.tt.probe(child.key(), &child).write(
        child.key(),
        qs_value,
        false,
        Bound::Exact,
        DEPTH_QS,
        Move::NONE,
        qs_value,
        worker.tt.generation(),
    ));
    if existing_parent {
        assert!(worker.tt.probe(pos.key(), &pos).write(
            pos.key(),
            Value::NONE,
            false,
            Bound::None,
            0,
            Move::NONE,
            Value::new(17),
            worker.tt.generation(),
        ));
    }
    let parent_probe = worker.tt.probe(pos.key(), &pos);
    assert_eq!(parent_probe.found, existing_parent);
    let before_data = parent_probe.data;
    let tt_ctx = TTContext {
        key: pos.key(),
        data: before_data,
        result: parent_probe,
        hit: existing_parent,
        mv: capture,
        value: Value::NONE,
        capture: true,
    };
    let ctx = SearchContext {
        tt: &worker.tt,
        eval_hash: &worker.eval_hash,
        eh_probe_on_tt_miss: worker.eh_probe_on_tt_miss,
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
    let stop = Arc::new(AtomicBool::new(outcome == ChildOutcome::QsearchStop));
    let mut tm = TimeManagement::new(stop, Arc::new(AtomicBool::new(false)));
    let calls = Cell::new(0);
    assert!(!worker.state.abort);
    let result = try_probcut(
        &mut worker.state,
        &ctx,
        &mut pos,
        6,
        Value::new(beta),
        false,
        &tt_ctx,
        1,
        Value::new(beta),
        Value::new(beta),
        false,
        true,
        Move::NONE,
        &limits,
        &mut tm,
        |state, _ctx, child_pos, depth, _alpha, _beta, ply, _cut, _limits, _tm| {
            calls.set(calls.get() + 1);
            assert!(depth > 0);
            assert_eq!(ply, 2);
            assert_ne!(child_pos.key(), before_key);
            #[cfg(all(
                feature = "halfkx-arch",
                any(not(feature = "mode-specific"), feature = "ft-halfkp")
            ))]
            assert_eq!(stack_index(state), 1);
            if outcome == ChildOutcome::SearchAbort {
                state.abort = true;
                Value::ZERO
            } else {
                Value::new(-(prob_beta + 50))
            }
        },
    );
    assert_eq!(pos.to_sfen(), before_sfen);
    assert_eq!(pos.key(), before_key);
    #[cfg(all(
        feature = "halfkx-arch",
        any(not(feature = "mode-specific"), feature = "ft-halfkp")
    ))]
    assert_eq!(stack_index(&worker.state), 0);
    assert_eq!(worker.state.nodes, 1, "actual child was entered exactly once");
    assert_eq!(calls.get(), usize::from(outcome != ChildOutcome::QsearchStop));
    let after = worker.tt.probe(pos.key(), &pos);
    if outcome == ChildOutcome::Complete {
        assert!(!worker.state.abort);
        assert_eq!(result, Some(Value::new(beta + 50)));
        assert!(after.found);
        assert_eq!(after.data.bound, Bound::Lower);
        assert_eq!(after.data.value, Value::new(prob_beta + 50));
    } else {
        assert!(worker.state.abort);
        assert_eq!(result, Some(Value::ZERO));
        assert_eq!(after.found, existing_parent);
        if existing_parent {
            assert_eq!(
                (
                    after.data.value,
                    after.data.bound,
                    after.data.depth,
                    after.data.eval,
                    after.data.mv
                ),
                (
                    before_data.value,
                    before_data.bound,
                    before_data.depth,
                    before_data.eval,
                    before_data.mv
                )
            );
        }
    }
}

#[test]
fn probcut_abort_restores_position_and_does_not_publish_tt() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            for outcome in [
                ChildOutcome::QsearchStop,
                ChildOutcome::SearchAbort,
                ChildOutcome::Complete,
            ] {
                // 番兵値 ZERO がちょうど cutoff 条件を満たす境界も含める。
                for beta in [
                    -1000,
                    -SearchTuneParams::default().probcut_beta_margin_base,
                    0,
                ] {
                    for existing_parent in [false, true] {
                        run_case(outcome, beta, existing_parent);
                    }
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
