//! グローバル設定を別プロセスに隔離した PASS ボーナスの実探索テスト。
use crate::eval::{EvalHash, set_pass_move_bonus};
use crate::nnue::{
    AccumulatorStackVariant, halfka_split::HalfKaSplitStack,
    network_halfka_split::AccumulatorStackHalfKaSplit,
};
use crate::position::Position;
use crate::search::{
    LimitsType, RootMove, RootMoves, SearchTuneParams, SearchWorker, TimeManagement,
};
use crate::tt::TranspositionTable;
use crate::types::{Move, Value};
use std::sync::{Arc, atomic::AtomicBool};

fn run_root(
    sfen: &str,
    bonus: i32,
    mode: u8,
    window: (Value, Value),
    stop: bool,
) -> (Value, Value) {
    set_pass_move_bonus(bonus);
    let mut pos = Position::new();
    pos.set_sfen(sfen).unwrap();
    pos.enable_pass_rights(1, 0);
    let before = (pos.to_sfen(), pos.key());
    let limits = LimitsType {
        depth: 1,
        ..Default::default()
    };
    let mut worker = SearchWorker::new(
        Arc::new(TranspositionTable::new(1)),
        Arc::new(EvalHash::new(1)),
        0,
        0,
        SearchTuneParams::default(),
    );
    worker.prepare_search(&limits);
    // 未初期化 HalfKP push の別課題に依存しない有効な storage。
    worker.state.nnue_stack = AccumulatorStackVariant::HalfKaSplit(HalfKaSplitStack::L256(
        AccumulatorStackHalfKaSplit::new(),
    ));
    worker.state.calls_cnt = 2;
    worker.state.root_depth = 1;
    worker.state.root_moves = if mode == 1 || mode == 2 || mode == 4 {
        let normal = RootMoves::from_legal_moves(&pos, &[])
            .iter()
            .find(|rm| !rm.pv[0].is_pass())
            .unwrap()
            .pv[0];
        if mode == 1 {
            RootMoves::from_vec(vec![RootMove::new(normal), RootMove::new(Move::PASS)])
        } else {
            RootMoves::from_vec(vec![RootMove::new(Move::PASS), RootMove::new(normal)])
        }
    } else {
        RootMoves::from_legal_moves(&pos, &[Move::PASS])
    };
    assert!(pos.is_legal(Move::PASS));
    let mut tm =
        TimeManagement::new(Arc::new(AtomicBool::new(stop)), Arc::new(AtomicBool::new(false)));
    let value = if mode == 3 {
        worker.tt.probe(pos.key(), &pos).write(
            pos.key(),
            Value::NONE,
            false,
            crate::types::Bound::None,
            0,
            Move::PASS,
            Value::NONE,
            worker.tt.generation(),
        );
        worker.search_node_wrapper::<{ crate::search::NodeType::PV as u8 }>(
            &mut pos, 1, window.0, window.1, 1, false, &limits, &mut tm,
        )
    } else if mode == 1 || mode == 4 {
        worker.search_root_for_pv(&mut pos, 1, window.0, window.1, 1, &limits, &mut tm)
    } else {
        worker.search_root(&mut pos, 1, window.0, window.1, &limits, &mut tm)
    };
    assert_eq!((pos.to_sfen(), pos.key()), before);
    match &worker.state.nnue_stack {
        AccumulatorStackVariant::HalfKaSplit(stack) => assert_eq!(stack.current_index(), 0),
        _ => unreachable!(),
    }
    assert_eq!(worker.state.abort, stop);
    let rm = worker.state.root_moves.iter().find(|rm| rm.pv[0].is_pass()).unwrap();
    (value, rm.score)
}

#[test]
fn pass_bonus_isolated_regression() {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "search::tests::pass_bonus::pass_bonus_child",
            "--ignored",
            "--nocapture",
        ])
        .env("RSHOGI_PASS_BONUS_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "グローバル設定を隔離する親テスト経由でのみ実行"]
fn pass_bonus_child() {
    assert_eq!(std::env::var("RSHOGI_PASS_BONUS_CHILD").as_deref(), Ok("1"));
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
            let full = (-Value::INFINITE, Value::INFINITE);
            let only_pass = "Kp7/1r7/9/2b6/9/9/9/9/8k b - 1";
            let mut fixture = Position::new();
            fixture.set_sfen(only_pass).unwrap();
            fixture.enable_pass_rights(1, 0);
            let legal = RootMoves::from_legal_moves(&fixture, &[]);
            assert_eq!(legal.len(), 1);
            assert_eq!(legal[0].pv[0], Move::PASS);
            let base = run_root(only_pass, 0, 0, full, false).0;
            assert!(!base.is_mate_score());
            for bonus in [-100, 0, 100] {
                let expected = base + Value::new(bonus);
                assert_eq!(run_root(only_pass, bonus, 0, full, false), (expected, expected));
                if bonus != 0 {
                    // 真値が fail-low になる窓。未補正の子の fail-high bound を加算してはいけない。
                    let narrow = (expected + Value::new(1), expected + Value::new(2));
                    assert_eq!(run_root(only_pass, bonus, 0, narrow, false).0, expected);
                }
                assert_eq!(run_root(only_pass, bonus, 0, full, true).0, Value::ZERO);
            }
            let mate_sfen = "K8/8r/9/9/9/9/9/9/1r6k b - 1";
            let mate = run_root(mate_sfen, 0, 0, full, false).0;
            assert!(mate.is_mate_score());
            for bonus in [-100, 100] {
                assert_eq!(run_root(mate_sfen, bonus, 0, full, false).0, mate);
            }
            let internal_base = run_root(only_pass, 0, 3, full, false).0;
            for bonus in [-100, 100] {
                let expected = internal_base + Value::new(bonus);
                assert_eq!(run_root(only_pass, bonus, 3, full, false).0, expected);
                assert_eq!(
                    run_root(
                        only_pass,
                        bonus,
                        3,
                        (expected + Value::new(1), expected + Value::new(2)),
                        false
                    )
                    .0,
                    expected
                );
                assert_eq!(run_root(only_pass, bonus, 3, full, true).0, Value::ZERO);
            }
            let mixed = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
            let base = run_root(mixed, 0, 1, full, false).0;
            let normal = run_root(mixed, 0, 4, full, false).0;
            for bonus in [-100, 0, 100] {
                let expected = base + Value::new(bonus);
                assert_eq!(run_root(mixed, bonus, 4, full, false).0, normal);
                assert_eq!(run_root(mixed, bonus, 2, full, false).0, expected.max(normal));
                assert_eq!(run_root(mixed, bonus, 1, full, false), (expected, expected));
                assert_eq!(run_root(mixed, bonus, 0, full, false), (expected, expected));
            }
            use crate::search::alpha_beta::apply_pass_move_bonus;
            for value in [Value::mate_in(1), Value::mated_in(7), Value::NONE] {
                for bonus in [-100, 100] {
                    assert_eq!(apply_pass_move_bonus(value, bonus), value);
                }
            }
            for bonus in [i32::MIN, i32::MAX] {
                let value = apply_pass_move_bonus(Value::new(50), bonus);
                assert!(!value.is_mate_score());
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
