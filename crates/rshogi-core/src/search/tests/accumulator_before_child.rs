//! 親の事前計算と、事前計算しない参照探索との一致。

use std::cell::Cell;

thread_local! {
    // テスト内の参照探索だけ、子へ進む前の事前計算を省く。
    static SKIP_PREPARATION: Cell<bool> = const { Cell::new(false) };
}

pub(in crate::search) fn skip_preparation() -> bool {
    SKIP_PREPARATION.get()
}

#[cfg(any(
    feature = "halfkx-arch",
    all(
        feature = "layerstack-arch",
        feature = "layerstacks-1536x16x32",
        feature = "ft-halfka_hm_merged"
    ),
    feature = "nnue-runtime-dimensions"
))]
mod checks {
    use super::SKIP_PREPARATION;
    use std::cell::Cell;
    use std::sync::{Arc, atomic::AtomicBool};

    use crate::eval::EvalHash;
    use crate::nnue::{self, AccumulatorStackVariant, DirtyPiece};
    use crate::position::{Position, SFEN_HIRATE};
    use crate::search::alpha_beta::{SearchContext, SearchWorker, TTContext};
    use crate::search::engine::{Search, SearchInfo};
    use crate::search::search_helpers::{do_move_and_push, nnue_evaluate, nnue_prepare_parent};
    use crate::search::{LimitsType, NodeType, SearchTuneParams, TimeManagement};
    use crate::tt::TranspositionTable;
    use crate::types::{Bound, DEPTH_QS, Move, Value};

    const CAPTURE: &str = "K8/9/9/4r4/4R4/9/9/9/8k b - 1";
    const IN_CHECK: &str = "K8/9/r8/9/4R4/9/9/9/8k b - 1";

    fn position(sfen: &str) -> Position {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        pos
    }

    fn worker() -> Box<SearchWorker> {
        let mut worker = SearchWorker::new(
            Arc::new(TranspositionTable::new(1)),
            Arc::new(EvalHash::new(1)),
            0,
            0,
            SearchTuneParams::default(),
        );
        worker.prepare_search(&LimitsType {
            depth: 5,
            ..Default::default()
        });
        worker.state.root_delta = 2 * Value::INFINITE.raw();
        worker
    }

    fn context(worker: &SearchWorker) -> SearchContext<'_> {
        worker.create_context()
    }

    fn time_manager() -> TimeManagement {
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)))
    }

    fn seed_eval_hash(worker: &mut SearchWorker, pos: &Position) -> Value {
        let value = nnue_evaluate(&mut worker.state, pos);
        worker.eval_hash.store(pos.key(), value.raw());
        worker.state.nnue_stack.reset();
        assert!(!worker.state.nnue_stack.is_current_computed());
        value
    }

    fn paths() {
        // EvalHash hit で静的評価を省略しても、通常探索・qsearch の親は準備される。
        for sfen in [CAPTURE, IN_CHECK] {
            for depth in [0, 2] {
                let mut worker = worker();
                let mut pos = position(sfen);
                seed_eval_hash(&mut worker, &pos);
                worker.search_node_wrapper::<{ NodeType::PV as u8 }>(
                    &mut pos,
                    depth,
                    -Value::INFINITE,
                    Value::INFINITE,
                    1,
                    false,
                    &LimitsType::default(),
                    &mut time_manager(),
                );
                assert!(worker.state.nodes > 0, "sfen={sfen}, depth={depth}");
                assert!(worker.state.nnue_stack.is_current_computed());
                assert_eq!(pos.to_sfen(), sfen);
            }
        }

        // stand pat で返る葉では EvalHash hit 後も未計算のまま。
        let mut worker = worker();
        let mut pos = position(CAPTURE);
        let value = seed_eval_hash(&mut worker, &pos);
        worker.search_node_wrapper::<{ NodeType::NonPV as u8 }>(
            &mut pos,
            0,
            value - Value::new(1001),
            value - Value::new(1000),
            1,
            false,
            &LimitsType::default(),
            &mut time_manager(),
        );
        assert_eq!(worker.state.nodes, 0);
        assert!(!worker.state.nnue_stack.is_current_computed());

        // null move の callback は子の位置で呼ばれる。pop して親の準備を確認する。
        for use_pass in [false, true] {
            if use_pass && !cfg!(feature = "search-pass-rules") {
                continue;
            }
            let mut worker = self::worker();
            let mut pos = position(SFEN_HIRATE);
            if use_pass {
                pos.enable_pass_rights(1, 1);
            }
            let before = pos.to_sfen();
            worker.state.stack[0].current_move =
                pos.to_move(Move::from_usi("7g7f").unwrap()).unwrap();
            let ctx_worker = self::worker();
            let ctx = context(&ctx_worker);
            let called = Cell::new(false);
            crate::search::pruning::try_null_move_pruning::<{ NodeType::NonPV as u8 }, _>(
                &mut worker.state,
                &ctx,
                &mut pos,
                4,
                Value::ZERO,
                1,
                true,
                false,
                Value::new(10000),
                false,
                Move::NONE,
                &LimitsType::default(),
                &mut time_manager(),
                |state, _, _, _, _, _, _, _, _, _| {
                    called.set(true);
                    assert!(!state.nnue_stack.is_current_computed());
                    state.nnue_stack.pop();
                    assert!(state.nnue_stack.is_current_computed());
                    state.nnue_stack.push(DirtyPiece::new());
                    Value::new(-1)
                },
            );
            assert!(called.get());
            assert_eq!(worker.state.nodes, 0);
            assert_eq!(pos.to_sfen(), before);
        }
    }

    fn incremental_matches_refresh() {
        let mut worker = worker();
        let mut pos = position(SFEN_HIRATE);
        let network = nnue::get_network().unwrap();
        for usi in ["7g7f", "3c3d", "8h2b+", "3a2b", "B*4e", "B*6e", "5i6h"] {
            let mv = pos.to_move(Move::from_usi(usi).unwrap()).unwrap();
            assert!(pos.is_legal(mv));
            let gives_check = pos.gives_check(mv);
            let dirty = pos.clone().do_move(mv, gives_check);
            do_move_and_push(
                &mut worker.state,
                &mut pos,
                mv,
                gives_check,
                &*worker.tt,
                &worker.eval_hash,
            );
            worker.state.nnue_stack.pop();
            assert!(worker.state.nnue_stack.is_current_computed());
            worker.state.nnue_stack.push(dirty);
            nnue_prepare_parent(&mut worker.state, &pos);
            let prepared = accumulation(&worker.state.nnue_stack);
            #[cfg(all(feature = "layerstack-arch", feature = "layerstacks-1536x16x32"))]
            if let AccumulatorStackVariant::LayerStacks(nnue::LayerStacksAccStack::L1536x16x32(s)) =
                &worker.state.nnue_stack
            {
                assert!(!s.current().accumulator.computed_score);
                #[cfg(feature = "nnue-progress-diff")]
                if nnue::get_layer_stack_bucket_mode() == nnue::LayerStackBucketMode::ProgressKPAbs
                {
                    assert!(!s.current().computed_progress);
                }
            }
            let actual = nnue_evaluate(&mut worker.state, &pos);
            let mut fresh = AccumulatorStackVariant::from_network(&network);
            assert_eq!(actual, nnue::evaluate_dispatch(&pos, &mut fresh, &mut None), "{usi}");
            assert_eq!(prepared, accumulation(&fresh), "accumulator: {usi}");
            // 同じ親を再利用しても探索側のカウンタと評価値は変わらない。
            let nodes = worker.state.nodes;
            nnue_prepare_parent(&mut worker.state, &pos);
            assert_eq!(worker.state.nodes, nodes);
            assert_eq!(actual, nnue_evaluate(&mut worker.state, &pos));
        }
    }

    fn probcut_parent() {
        let mut worker = worker();
        let mut pos = position(CAPTURE);
        let capture = pos.to_move(Move::from_usi("5e5d").unwrap()).unwrap();
        let mut child = pos.clone();
        let dirty = child.do_move(capture, child.gives_check(capture));
        let ctx_worker = self::worker();
        let ctx = context(&ctx_worker);
        let prob_beta = ctx.tune_params.probcut_beta_margin_base;
        let value = Value::new(-prob_beta - 100);
        assert!(ctx.tt.probe(child.key(), &child).write(
            child.key(),
            value,
            false,
            Bound::Exact,
            DEPTH_QS,
            Move::NONE,
            value,
            ctx.tt.generation()
        ));
        let probe = ctx.tt.probe(pos.key(), &pos);
        let tt_ctx = TTContext {
            key: pos.key(),
            data: probe.data,
            result: probe,
            hit: false,
            mv: capture,
            value: Value::NONE,
            capture: true,
        };
        let called = Cell::new(false);
        crate::search::pruning::try_probcut(
            &mut worker.state,
            &ctx,
            &mut pos,
            6,
            Value::ZERO,
            false,
            &tt_ctx,
            1,
            Value::ZERO,
            Value::ZERO,
            false,
            true,
            Move::NONE,
            &LimitsType::default(),
            &mut time_manager(),
            |state, _, _, _, _, _, _, _, _, _| {
                called.set(true);
                state.nnue_stack.pop();
                assert!(state.nnue_stack.is_current_computed());
                state.nnue_stack.push(dirty);
                Value::new(-prob_beta - 50)
            },
        );
        assert!(called.get());
        assert_eq!(worker.state.nodes, 1);
        assert_eq!(pos.to_sfen(), CAPTURE);
    }

    fn accumulation(stack: &AccumulatorStackVariant) -> Vec<i16> {
        match stack {
            AccumulatorStackVariant::HalfKP(nnue::halfkp::HalfKPStack::L256(s)) => {
                s.current().accumulator.accumulation.iter().flat_map(|v| v.0).collect()
            }
            #[cfg(all(feature = "layerstack-arch", feature = "layerstacks-1536x16x32"))]
            AccumulatorStackVariant::LayerStacks(nnue::LayerStacksAccStack::L1536x16x32(s)) => {
                s.current().accumulator.accumulation.as_flattened().to_vec()
            }
            #[cfg(feature = "nnue-runtime-dimensions")]
            AccumulatorStackVariant::DynamicHalfKx(s) => s.borrow().current_accumulation().to_vec(),
            #[cfg(feature = "nnue-runtime-dimensions")]
            AccumulatorStackVariant::DynamicLayerStacks(s) => {
                s.borrow().current_accumulation().to_vec()
            }
            _ => panic!("未対応の fixture stack"),
        }
    }

    fn fixed_depth_matches_reference() {
        for sfen in [
            SFEN_HIRATE,
            CAPTURE,
            IN_CHECK,
            "lnsgkgsnl/1r7/p1ppp1bpp/1p3pp2/7P1/2P6/PP1PPPP1P/1B3S1R1/LNSGKG1NL b - 9",
        ] {
            for hash_enabled in [false, true] {
                crate::eval::set_eval_hash_enabled(hash_enabled);
                let run = |skip| {
                    SKIP_PREPARATION.set(skip);
                    let mut search = Search::new(4);
                    let result = search.go(
                        &mut position(sfen),
                        LimitsType {
                            depth: 5,
                            ..Default::default()
                        },
                        None::<fn(&SearchInfo)>,
                    );
                    SKIP_PREPARATION.set(false);
                    assert_eq!(result.depth, 5);
                    (result.nodes, result.score, result.pv, result.best_move)
                };
                assert_eq!(run(true), run(false), "sfen={sfen}, EvalHash={hash_enabled}");
            }
        }
        crate::eval::set_eval_hash_enabled(true);
    }

    fn check_loaded_network() {
        paths();
        probcut_parent();
        incremental_matches_refresh();
        fixed_depth_matches_reference();
    }

    #[test]
    fn parent_accumulator_contract() {
        // NETWORK・評価設定を変更するため、他のテストからプロセスを分離する。
        const CHILD_ENV: &str = "RSHOGI_TEST_PARENT_ACCUMULATOR";
        if std::env::var_os(CHILD_ENV).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "search::tests::accumulator_before_child::checks::parent_accumulator_contract",
                    "--nocapture",
                ])
                .env(CHILD_ENV, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                crate::eval::material::disable_material();
                crate::eval::set_eval_hash_enabled(true);
                #[cfg(feature = "halfkx-arch")]
                {
                    let mut bytes = nnue::network_halfkp::halfkp_loader_fixture(
                        256,
                        "Features=HalfKP(Friend)[125388->256x2],l2=32,l3=32",
                    );
                    // FT に符号と局面依存性を与え、一定評価だけの比較を避ける。
                    let arch_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
                    let start = 12 + arch_len + 4 + 256 * 2;
                    for (i, value) in bytes[start..start + nnue::HALFKP_DIMENSIONS * 256 * 2]
                        .chunks_exact_mut(2)
                        .enumerate()
                    {
                        let weight = if i % 13 == 0 {
                            i16::MAX
                        } else {
                            (i % 17) as i16 - 8
                        };
                        value.copy_from_slice(&weight.to_le_bytes());
                    }
                    nnue::init_nnue_from_bytes(&bytes).unwrap();
                    check_loaded_network();
                }
                #[cfg(any(
                    all(
                        feature = "layerstack-arch",
                        feature = "layerstacks-1536x16x32",
                        feature = "ft-halfka_hm_merged"
                    ),
                    feature = "nnue-runtime-dimensions"
                ))]
                {
                    let fixture = nnue::net_delta::test_utils::build_synthetic_layer_stacks(
                        "HalfKaHmMerged",
                        nnue::HALFKA_HM_DIMENSIONS,
                        1536,
                        16,
                        32,
                        9,
                    );
                    nnue::init_nnue_from_bytes(&fixture.bytes).unwrap();
                    nnue::configure_layer_stack_routing(
                        nnue::LayerStackBucketMode::KingRank9,
                        9,
                        None,
                    )
                    .unwrap();
                    check_loaded_network();
                    #[cfg(feature = "nnue-progress-diff")]
                    {
                        nnue::set_layer_stack_progress_kpabs_weights(
                            (0..nnue::SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS)
                                .map(|i| ((i % 101) as f32 - 50.0) * 0.017)
                                .collect::<Vec<_>>()
                                .into_boxed_slice(),
                        )
                        .unwrap();
                        nnue::configure_layer_stack_routing(
                            nnue::LayerStackBucketMode::ProgressKPAbs,
                            9,
                            Some(9),
                        )
                        .unwrap();
                        check_loaded_network();
                    }
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
