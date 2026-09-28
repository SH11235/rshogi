//! HalfKX の親準備、および全方式を準備する参照探索・遅延評価との一致。

use std::cell::Cell;

thread_local! {
    // テスト内の参照探索だけ、子へ進む前の事前計算を省く。
    static SKIP_PREPARATION: Cell<bool> = const { Cell::new(false) };
    static PREPARE_ALL: Cell<bool> = const { Cell::new(false) };
    static SKIP_HALFKX_CACHE: Cell<bool> = const { Cell::new(false) };
}

pub(in crate::search) fn preparation_override(
    st: &mut super::super::alpha_beta::SearchState,
    pos: &crate::position::Position,
) -> bool {
    if SKIP_PREPARATION.get() {
        return true;
    }
    if PREPARE_ALL.get() {
        prepare_all(st, pos);
        return true;
    }
    false
}

// 全方式で親を準備する参照処理。progress と bucket は評価時だけ更新する。
fn prepare_all(st: &mut super::super::alpha_beta::SearchState, pos: &crate::position::Position) {
    if st.nnue_stack.is_current_computed() || crate::eval::material::is_material_enabled() {
        return;
    }
    #[cfg(feature = "layerstack-arch")]
    {
        let ptr = st.network_ptr;
        if !ptr.is_null()
            && let crate::nnue::AccumulatorStackVariant::LayerStacks(ref mut stack) = st.nnue_stack
        {
            // SAFETY: prepare_search() が NETWORK 内の Arc と stack の型を対応付ける。
            // 探索中は network が解放されず、ポインタと stack の対応が維持される。
            let network = unsafe { &*ptr };
            network.as_layer_stacks().update_accumulator(pos, stack, &mut st.acc_cache);
            return;
        }
    }
    #[cfg(feature = "layerstack-arch")]
    let acc_cache = &mut st.acc_cache;
    #[cfg(not(feature = "layerstack-arch"))]
    let acc_cache = &mut None;
    crate::nnue::ensure_accumulator_computed_with_caches(
        pos,
        &mut st.nnue_stack,
        acc_cache,
        #[cfg(feature = "halfkx-arch")]
        &mut st.halfkx_cache,
    );
}

#[cfg(feature = "halfkx-arch")]
pub(in crate::search) fn skip_halfkx_cache() -> bool {
    SKIP_HALFKX_CACHE.get()
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
    #[cfg(all(
        feature = "nnue-progress-diff",
        feature = "layerstack-arch",
        feature = "layerstacks-1536x16x32"
    ))]
    use super::prepare_all;
    use super::{PREPARE_ALL, SKIP_HALFKX_CACHE, SKIP_PREPARATION};
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

    fn paths(prepare_parent: bool) {
        // EvalHash hit で静的評価を省略した通常探索・qsearch の親準備を確認する。
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
                assert_eq!(worker.state.nnue_stack.is_current_computed(), prepare_parent);
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
                    assert_eq!(state.nnue_stack.is_current_computed(), prepare_parent);
                    state.nnue_stack.push(DirtyPiece::new());
                    Value::new(-1)
                },
            );
            assert!(called.get());
            assert_eq!(worker.state.nodes, 0);
            assert_eq!(pos.to_sfen(), before);
        }
    }

    fn incremental_matches_refresh(prepare_parent: bool) {
        let mut worker = worker();
        let mut pos = position(SFEN_HIRATE);
        let network = nnue::get_network().unwrap();
        for (ply, usi) in ["7g7f", "3c3d", "8h2b+", "3a2b", "B*4e", "B*6e", "5i6h"]
            .into_iter()
            .enumerate()
        {
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
            assert_eq!(worker.state.nnue_stack.is_current_computed(), prepare_parent || ply > 0);
            worker.state.nnue_stack.push(dirty);
            let before = accumulation(&worker.state.nnue_stack);
            nnue_prepare_parent(&mut worker.state, &pos);
            assert_eq!(worker.state.nnue_stack.is_current_computed(), prepare_parent);
            let prepared = accumulation(&worker.state.nnue_stack);
            if !prepare_parent {
                assert_eq!(prepared, before, "準備対象外の accumulator は変更しない: {usi}");
            }
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
            assert_eq!(accumulation(&worker.state.nnue_stack), accumulation(&fresh), "{usi}");
            if prepare_parent {
                assert_eq!(prepared, accumulation(&fresh), "accumulator: {usi}");
            }
            // 同じ親を再利用しても探索側のカウンタと評価値は変わらない。
            let nodes = worker.state.nodes;
            nnue_prepare_parent(&mut worker.state, &pos);
            assert_eq!(worker.state.nodes, nodes);
            assert_eq!(actual, nnue_evaluate(&mut worker.state, &pos));
        }
    }

    #[cfg(all(feature = "halfkx-arch", not(feature = "nnue-runtime-dimensions")))]
    fn preparation_uses_finny_cache() {
        use crate::types::Color;
        let mut worker = worker();
        let mut pos = position(SFEN_HIRATE);
        // 根の準備だけで、両視点の cache entry が accumulator と一致する。
        nnue_prepare_parent(&mut worker.state, &pos);
        let expected = accumulation(&worker.state.nnue_stack);
        for color in [Color::Black, Color::White] {
            assert_eq!(
                worker
                    .state
                    .halfkx_cache
                    .as_ref()
                    .unwrap()
                    .cached_accumulation(pos.king_square(color), color)
                    .unwrap(),
                &expected[color.index() * 256..(color.index() + 1) * 256]
            );
        }
        // 玉移動でも、評価なしで新しい玉升の cache が埋まる。
        let mv = pos.to_move(Move::from_usi("5i6h").unwrap()).unwrap();
        let dirty = pos.do_move(mv, pos.gives_check(mv));
        worker.state.nnue_stack.push(dirty);
        nnue_prepare_parent(&mut worker.state, &pos);
        let actual = accumulation(&worker.state.nnue_stack);
        assert_ne!(actual, expected);
        assert_eq!(
            worker
                .state
                .halfkx_cache
                .as_ref()
                .unwrap()
                .cached_accumulation(pos.king_square(Color::Black), Color::Black)
                .unwrap(),
            &actual[..256]
        );
        let network = nnue::get_network().unwrap();
        let mut fresh = AccumulatorStackVariant::from_network(&network);
        nnue::ensure_accumulator_computed(&pos, &mut fresh, &mut None);
        assert_eq!(actual, accumulation(&fresh));
    }

    #[cfg(all(
        feature = "nnue-progress-diff",
        feature = "layerstack-arch",
        feature = "layerstacks-1536x16x32"
    ))]
    fn progress_without_parent_evaluation() {
        fn progress(worker: &SearchWorker) -> (u32, usize) {
            let AccumulatorStackVariant::LayerStacks(nnue::LayerStacksAccStack::L1536x16x32(s)) =
                &worker.state.nnue_stack
            else {
                panic!("LayerStacks fixture")
            };
            assert!(s.current().computed_progress);
            let sum = s.current().progress_sum;
            (sum.to_bits(), nnue::progress_sum_to_bucket(sum, 9))
        }
        // root の評価有無と子の評価間隔を変えて、全計算と差分更新の両方を通す。
        for evaluate_root in [false, true] {
            for interval in [2, 3] {
                let mut prepared = worker();
                let mut lazy = worker();
                let mut pos = position(SFEN_HIRATE);
                if evaluate_root {
                    assert_eq!(
                        nnue_evaluate(&mut prepared.state, &pos),
                        nnue_evaluate(&mut lazy.state, &pos)
                    );
                }
                let mut uncomputed_parents = 0;
                for (ply, usi) in [
                    "7g7f", "3c3d", "8h2b+", "3a2b", "B*4e", "B*6e", "5i6h", "5a6b",
                ]
                .into_iter()
                .enumerate()
                {
                    prepare_all(&mut prepared.state, &pos);
                    let lazy_computed = lazy.state.nnue_stack.is_current_computed();
                    let lazy_before = accumulation(&lazy.state.nnue_stack);
                    nnue_prepare_parent(&mut lazy.state, &pos);
                    assert_eq!(lazy.state.nnue_stack.is_current_computed(), lazy_computed);
                    assert_eq!(accumulation(&lazy.state.nnue_stack), lazy_before);
                    let AccumulatorStackVariant::LayerStacks(
                        nnue::LayerStacksAccStack::L1536x16x32(s),
                    ) = &prepared.state.nnue_stack
                    else {
                        panic!("LayerStacks fixture")
                    };
                    assert!(s.current().accumulator.computed_accumulation);
                    if (ply == 0 && !evaluate_root) || (ply > 0 && ply % interval != 0) {
                        assert!(!s.current().computed_progress);
                        assert!(!s.current().accumulator.computed_score);
                        uncomputed_parents += 1;
                    }
                    let before = accumulation(&prepared.state.nnue_stack);
                    let mv = pos.to_move(Move::from_usi(usi).unwrap()).unwrap();
                    assert!(pos.is_legal(mv));
                    let dirty = pos.do_move(mv, pos.gives_check(mv));
                    prepared.state.nnue_stack.push(dirty);
                    lazy.state.nnue_stack.push(dirty);
                    if (ply + 1) % interval == 0 {
                        assert_eq!(
                            nnue_evaluate(&mut prepared.state, &pos),
                            nnue_evaluate(&mut lazy.state, &pos)
                        );
                        assert_eq!(
                            progress(&prepared),
                            progress(&lazy),
                            "{usi}, interval={interval}"
                        );
                        assert_eq!(
                            accumulation(&prepared.state.nnue_stack),
                            accumulation(&lazy.state.nnue_stack)
                        );
                        assert_ne!(
                            before,
                            accumulation(&prepared.state.nnue_stack),
                            "着手で FT が変化する fixture"
                        );
                    }
                }
                assert!(uncomputed_parents > 0);
            }
        }
    }

    fn probcut_parent(prepare_parent: bool) {
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
                assert_eq!(state.nnue_stack.is_current_computed(), prepare_parent);
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
            AccumulatorStackVariant::HalfKaSplit(nnue::halfka_split::HalfKaSplitStack::L256(s)) => {
                s.current().accumulator.accumulation.iter().flat_map(|v| v.0).collect()
            }
            AccumulatorStackVariant::HalfKaMerged(
                nnue::halfka_merged::HalfKaMergedStack::L256(s),
            ) => s.current().accumulator.accumulation.iter().flat_map(|v| v.0).collect(),
            AccumulatorStackVariant::HalfKaHmSplit(
                nnue::halfka_hm_split::HalfKaHmSplitStack::L256(s),
            ) => s.current().accumulator.accumulation.iter().flat_map(|v| v.0).collect(),
            AccumulatorStackVariant::HalfKaHmMerged(
                nnue::halfka_hm_merged::HalfKaHmMergedStack::L256(s),
            ) => s.current().accumulator.accumulation.iter().flat_map(|v| v.0).collect(),
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
                let run = |skip, skip_cache, prepare_all| {
                    SKIP_PREPARATION.set(skip);
                    SKIP_HALFKX_CACHE.set(skip_cache);
                    PREPARE_ALL.set(prepare_all);
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
                    SKIP_HALFKX_CACHE.set(false);
                    PREPARE_ALL.set(false);
                    assert_eq!(result.depth, 5);
                    (result.nodes, result.score, result.pv, result.best_move)
                };
                let reference = run(true, true, false);
                let actual = run(false, false, false);
                assert_eq!(reference, actual, "sfen={sfen}, EvalHash={hash_enabled}");
                assert_eq!(run(true, false, false), actual, "sfen={sfen}, EvalHash={hash_enabled}");
                assert_eq!(
                    run(false, false, true),
                    actual,
                    "全方式の親準備: {sfen}, EvalHash={hash_enabled}"
                );
                println!(
                    "SEARCH {} {:?} {hash_enabled} {sfen} => {actual:?}",
                    nnue::get_network().unwrap().architecture_name(),
                    nnue::get_layer_stack_bucket_mode()
                );
            }
        }
        crate::eval::set_eval_hash_enabled(true);
    }

    fn check_loaded_network(prepare_parent: bool) {
        let mut worker = worker();
        let pos = position(SFEN_HIRATE);
        assert!(!worker.state.nnue_stack.is_current_computed());
        let before = accumulation(&worker.state.nnue_stack);
        nnue_prepare_parent(&mut worker.state, &pos);
        assert_eq!(worker.state.nnue_stack.is_current_computed(), prepare_parent);
        if !prepare_parent {
            assert_eq!(accumulation(&worker.state.nnue_stack), before);
        }
        paths(prepare_parent);
        probcut_parent(prepare_parent);
        incremental_matches_refresh(prepare_parent);
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
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if line.starts_with("SEARCH ") {
                    println!("{line}");
                }
            }
            return;
        }
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(|| {
                crate::eval::material::disable_material();
                crate::eval::set_eval_hash_enabled(true);
                #[cfg(feature = "halfkx-arch")]
                {
                    let fixtures: &[(&str, usize)] = &[
                        #[cfg(any(feature = "ft-halfkp", feature = "nnue-runtime-dimensions"))]
                        ("HalfKP(Friend)", nnue::HALFKP_DIMENSIONS),
                        #[cfg(any(
                            feature = "ft-halfka_split",
                            feature = "nnue-runtime-dimensions"
                        ))]
                        ("HalfKaSplit", nnue::HALFKA_DIMENSIONS),
                        #[cfg(any(
                            feature = "ft-halfka_merged",
                            feature = "nnue-runtime-dimensions"
                        ))]
                        ("HalfKaMerged", nnue::HALFKA_MERGED_DIMENSIONS),
                        #[cfg(any(
                            feature = "ft-halfka_hm_split",
                            feature = "nnue-runtime-dimensions"
                        ))]
                        ("HalfKaHmSplit", nnue::HALFKA_HM_SPLIT_DIMENSIONS),
                        #[cfg(any(
                            feature = "ft-halfka_hm_merged",
                            feature = "nnue-runtime-dimensions"
                        ))]
                        ("HalfKaHmMerged", nnue::HALFKA_HM_DIMENSIONS),
                    ];
                    for &(feature, dimensions) in fixtures {
                        let arch = format!("Features={feature}[{dimensions}->256x2],l2=32,l3=32");
                        let mut bytes = nnue::network_halfkp::halfkp_loader_fixture(256, &arch);
                        let start = 12 + arch.len() + 4 + 256 * 2;
                        let dense = bytes.split_off(start + nnue::HALFKP_DIMENSIONS * 256 * 2);
                        bytes.resize(start + dimensions * 256 * 2, 0);
                        // FT に符号と局面依存性を与え、i16 の周回も検証する。
                        for (i, value) in bytes[start..].chunks_exact_mut(2).enumerate() {
                            let weight = if i % 13 == 0 {
                                i16::MAX
                            } else {
                                (i % 17) as i16 - 8
                            };
                            value.copy_from_slice(&weight.to_le_bytes());
                        }
                        bytes.extend_from_slice(&dense);
                        nnue::init_nnue_from_bytes(&bytes).unwrap();
                        check_loaded_network(!cfg!(feature = "nnue-runtime-dimensions"));
                        #[cfg(not(feature = "nnue-runtime-dimensions"))]
                        preparation_uses_finny_cache();
                    }
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
                    let mut fixture = nnue::net_delta::test_utils::build_synthetic_layer_stacks(
                        "HalfKaHmMerged",
                        nnue::HALFKA_HM_DIMENSIONS,
                        1536,
                        16,
                        32,
                        9,
                    );
                    // 1 byte signed LEB128 のまま、feature ごとに異なる符号付き重みにする。
                    let start = fixture.ft_biases + 1536;
                    for (i, byte) in fixture.bytes[start..start + nnue::HALFKA_HM_DIMENSIONS * 1536]
                        .iter_mut()
                        .enumerate()
                    {
                        let feature = i / 1536;
                        let lane = i % 1536;
                        let weight = ((feature * 7 + lane * 11) % 31) as i8 - 15;
                        *byte = (weight as u8) & 0x7f;
                    }
                    nnue::init_nnue_from_bytes(&fixture.bytes).unwrap();
                    nnue::configure_layer_stack_routing(
                        nnue::LayerStackBucketMode::KingRank9,
                        9,
                        None,
                    )
                    .unwrap();
                    check_loaded_network(false);
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
                        check_loaded_network(false);
                        #[cfg(all(
                            feature = "layerstack-arch",
                            feature = "layerstacks-1536x16x32"
                        ))]
                        progress_without_parent_evaluation();
                    }
                }
            })
            .unwrap()
            .join()
            .unwrap();
    }
}
