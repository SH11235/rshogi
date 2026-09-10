//! 合法手なしの敗北と、中断時の完了結果の整合性。
use std::sync::{Arc, atomic::AtomicBool};

use crate::eval::EvalHash;
use crate::position::Position;
use crate::search::alpha_beta::SearchWorker;
use crate::search::types::NodeType;
use crate::search::{LimitsType, RootMoves, Search, SearchInfo, SearchTuneParams, TimeManagement};
use crate::tt::TranspositionTable;
use crate::types::{Move, Value};

fn on_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn terminal_without_check_is_loss() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        let mut pos = Position::new();
        pos.set_sfen("K8/8r/9/9/9/9/9/9/1r6k b - 1").unwrap();
        assert!(!pos.in_check());
        assert!(RootMoves::from_legal_moves(&pos, &[]).is_empty());
        let limits = LimitsType {
            depth: 1,
            ..Default::default()
        };
        let mut search = Search::new(1);
        let result = search.go(&mut pos, limits.clone(), None::<fn(&SearchInfo)>);
        assert_eq!(result.best_move, Move::NONE);
        assert_eq!(result.score, Value::mated_in(0));

        let mut worker = SearchWorker::new(
            Arc::new(TranspositionTable::new(1)),
            Arc::new(EvalHash::new(1)),
            0,
            0,
            SearchTuneParams::default(),
        );
        worker.prepare_search(&limits);
        let mut tm =
            TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let score = worker.search_node_wrapper::<{ NodeType::PV as u8 }>(
            &mut pos,
            1,
            -Value::INFINITE,
            Value::INFINITE,
            1,
            false,
            &limits,
            &mut tm,
        );
        assert_eq!(score, Value::mated_in(1));
    });
}

#[test]
fn terminal_searchmoves_empty_is_not_loss() {
    on_large_stack(|| {
        let mut pos = Position::new();
        pos.set_hirate();
        let mut search = Search::new(1);
        let result = search.go(
            &mut pos,
            LimitsType {
                depth: 1,
                search_moves: vec![Move::WIN],
                ..Default::default()
            },
            None::<fn(&SearchInfo)>,
        );
        assert_eq!(result.best_move, Move::NONE);
        assert_eq!(result.score, Value::ZERO);
    });
}

#[test]
fn aspiration_abort_returns_completed_result() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        let mut pos = Position::new();
        pos.set_hirate();
        for usi in ["7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e"] {
            let mv = pos.to_move(Move::from_usi(usi).unwrap()).unwrap();
            assert!(RootMoves::from_legal_moves(&pos, &[]).find(mv).is_some());
            let check = pos.gives_check(mv);
            pos.do_move(mv, check);
        }
        let mut search = Search::new(1);
        let mut infos = Vec::new();
        let result = search.go(
            &mut pos,
            LimitsType {
                nodes: 13_678,
                ..Default::default()
            },
            Some(|info: &SearchInfo| infos.push(info.clone())),
        );
        // 最終infoは返値から再生成されるため、その直前の完了infoと比較する。
        let completed = &infos[infos.len() - 2];
        assert_eq!(result.depth, 10);
        assert_eq!(result.best_move.to_usi(), "2h6h");
        assert_eq!(result.score, Value::ZERO);
        assert_eq!(result.depth, completed.depth);
        assert_eq!(result.best_move, completed.pv[0]);
        assert_eq!(result.score, completed.score);
        assert_eq!(infos.last().unwrap().pv, completed.pv);
        assert_eq!(result.ponder_move, completed.pv.get(1).copied().unwrap_or(Move::NONE));
    });
}

#[test]
fn aspiration_abort_multipv_and_helpers_keep_valid_results() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        for threads in [1, 2] {
            for multi_pv in [1, 4] {
                let mut pos = Position::new();
                pos.set_hirate();
                let mut search = Search::new(1);
                search.set_num_threads(threads);
                let mut infos = Vec::new();
                let result = search.go(
                    &mut pos,
                    LimitsType {
                        nodes: 8_000,
                        multi_pv,
                        ..Default::default()
                    },
                    Some(|info: &SearchInfo| infos.push(info.clone())),
                );
                assert!(result.depth > 0);
                assert!(result.score.raw().abs() < Value::INFINITE.raw());
                let final_info = infos.last().unwrap();
                assert_eq!(result.best_move, final_info.pv[0]);
                assert_eq!(result.score, final_info.score);
                assert_eq!(result.depth, final_info.depth);
                if threads == 1 {
                    let completed = infos[..infos.len() - 1]
                        .iter()
                        .rev()
                        .find(|info| info.multi_pv == 1)
                        .unwrap();
                    assert_eq!(result.depth, completed.depth);
                    assert_eq!(result.score, completed.score);
                    assert_eq!(final_info.pv, completed.pv);
                }
                for &mv in &final_info.pv {
                    assert!(RootMoves::from_legal_moves(&pos, &[]).find(mv).is_some());
                    let check = pos.gives_check(mv);
                    pos.do_move(mv, check);
                }
            }
        }
    });
}
