//! 実際の mate1 専用 writer と warm TT probe の距離契約。
use std::sync::{Arc, atomic::AtomicBool};

use crate::eval::EvalHash;
use crate::position::Position;
use crate::search::alpha_beta::{ProbeOutcome, SearchContext, SearchWorker};
use crate::search::types::{NodeType, value_from_tt, value_to_tt};
use crate::search::{LimitsType, SearchTuneParams, TimeManagement};
use crate::tt::TranspositionTable;
use crate::types::{Bound, Move, Value};

fn fixture() -> Position {
    let mut pos = Position::new();
    pos.set_sfen("7Pk/6R2/9/9/9/9/9/9/4K4 b G 1").unwrap();
    assert!(!pos.in_check());
    assert!(pos.mate_1ply().is_some());
    pos
}

fn run_writer(qsearch_writer: bool, write_ply: i32) {
    let tt = Arc::new(TranspositionTable::new(1));
    let mut worker = SearchWorker::new(
        Arc::clone(&tt),
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
    let mut pos = fixture();
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
    let mut tm =
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    assert!(!tt.probe(pos.key(), &pos).found);
    let cold = if qsearch_writer {
        super::super::qsearch::qsearch::<{ NodeType::NonPV as u8 }>(
            &mut worker.state,
            &ctx,
            &mut pos,
            Value::new(-1),
            Value::ZERO,
            write_ply,
            &limits,
            &mut tm,
        )
    } else {
        match super::super::eval_helpers::probe_transposition::<{ NodeType::NonPV as u8 }>(
            &mut worker.state,
            &ctx,
            &mut pos,
            2,
            Value::ZERO,
            write_ply,
            false,
            false,
            Move::NONE,
            true,
        ) {
            ProbeOutcome::Cutoff { value, tt_move, .. } => {
                // cold miss の専用 mate1 branch は探索返値を返し、tt_move は NONE。
                assert_eq!(tt_move, Move::NONE);
                value
            }
            ProbeOutcome::Continue(_) => panic!("mate1 dedicated branch must cut off"),
        }
    };
    assert!(!worker.state.stack[write_ply as usize].tt_hit);
    assert_eq!(cold, Value::mate_in(write_ply + 1));
    let hit = tt.probe(pos.key(), &pos);
    assert!(hit.found);
    assert_eq!(hit.data.bound, Bound::Exact);
    assert_eq!(hit.data.value, Value::mate_in(1));
    assert!(hit.data.mv.is_some());
    for read_ply in [0, write_ply, 9] {
        let expected = Value::mate_in(read_ply + 1);
        assert_eq!(value_from_tt(hit.data.value, read_ply), expected);
        let warm = if qsearch_writer {
            super::super::qsearch::qsearch::<{ NodeType::NonPV as u8 }>(
                &mut worker.state,
                &ctx,
                &mut pos,
                Value::new(-1),
                Value::ZERO,
                read_ply,
                &limits,
                &mut tm,
            )
        } else {
            match super::super::eval_helpers::probe_transposition::<{ NodeType::NonPV as u8 }>(
                &mut worker.state,
                &ctx,
                &mut pos,
                1,
                Value::ZERO,
                read_ply,
                false,
                false,
                Move::NONE,
                true,
            ) {
                ProbeOutcome::Cutoff { value, .. } => value,
                ProbeOutcome::Continue(_) => panic!("warm exact TT must cut off"),
            }
        };
        assert!(worker.state.stack[read_ply as usize].tt_hit);
        assert_eq!(warm, expected);
        assert_eq!(tt.probe(pos.key(), &pos).data.value, Value::mate_in(1));
    }
}

#[test]
fn mate1_tt_qsearch_cold_warm_and_different_ply() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            for ply in [0, 5] {
                run_writer(true, ply);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn mate1_tt_alpha_beta_dedicated_nonroot_writer() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            for ply in [1, 5] {
                run_writer(false, ply);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn mate1_tt_normal_and_none_stores_remain_unchanged() {
    let tt = TranspositionTable::new(1);
    let pos = fixture();
    for (index, value) in [Value::new(123), Value::new(-456), Value::NONE].into_iter().enumerate() {
        let key = pos.key().wrapping_add(index as u64);
        assert!(tt.probe(key, &pos).write(
            key,
            value_to_tt(value, 5),
            false,
            Bound::Exact,
            10,
            Move::NONE,
            Value::NONE,
            tt.generation(),
        ));
        let hit = tt.probe(key, &pos);
        assert!(hit.found);
        assert_eq!(hit.data.value, value);
        for ply in [0, 5, 9] {
            assert_eq!(value_from_tt(hit.data.value, ply), value);
        }
    }
}
