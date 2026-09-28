//! 型付き評価器と従来 dispatch・全再計算の一致。
use super::*;
use crate as core;
#[path = "../../tests/common/search_models.rs"]
mod search_models;
use crate::nnue::{self, AccumulatorStackVariant};
use crate::position::SFEN_HIRATE;

fn prepared() -> SearchEvaluator {
    let mut evaluator = SearchEvaluator::Uninitialized;
    evaluator.prepare();
    evaluator
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Snapshot {
    accumulation: Vec<i16>,
    psqt: Vec<i32>,
    threat: Vec<i16>,
    progress: Option<u32>,
}

#[cfg(feature = "layerstack-arch")]
fn layer_snapshot<const N: usize>(
    entry: &nnue::accumulator_layer_stacks::StackEntryLayerStacks<N>,
) -> Snapshot {
    let accumulator = &entry.accumulator;
    Snapshot {
        accumulation: accumulator.accumulation.iter().flatten().copied().collect(),
        #[cfg(feature = "nnue-psqt")]
        psqt: accumulator.psqt_accumulation.iter().flatten().copied().collect(),
        #[cfg(not(feature = "nnue-psqt"))]
        psqt: Vec::new(),
        #[cfg(feature = "nnue-threat")]
        threat: accumulator.threat_accumulation.iter().flatten().copied().collect(),
        #[cfg(not(feature = "nnue-threat"))]
        threat: Vec::new(),
        #[cfg(feature = "nnue-progress-diff")]
        progress: entry.computed_progress.then(|| entry.progress_sum.to_bits()),
        #[cfg(not(feature = "nnue-progress-diff"))]
        progress: None,
    }
}

#[cfg(feature = "nnue-runtime-dimensions")]
fn dynamic_layer_snapshot(stack: &DynamicLayerStacksStack) -> Snapshot {
    Snapshot {
        accumulation: stack.test_accumulation().to_vec(),
        psqt: stack.test_psqt().to_vec(),
        threat: stack.test_threat().to_vec(),
        progress: None,
    }
}

// 各次元の stack から同じ観測を取り、型付き経路と従来経路を比較する。
#[cfg(feature = "halfkx-arch")]
macro_rules! halfkx_observe {
    ($stack:expr, $kind:ident, $entry:ident, $body:expr) => {
        match $stack {
            $kind::L256($entry) => $body,
            $kind::L512($entry) => $body,
            $kind::L768($entry) => $body,
            $kind::L1024($entry) => $body,
        }
    };
}

#[cfg(feature = "layerstack-arch")]
macro_rules! layer_observe {
    ($stack:expr, $entry:ident, $body:expr) => {
        match $stack {
            #[cfg(feature = "layerstacks-1536x16x32")]
            LayerStacksAccStack::L1536x16x32($entry) => $body,
            #[cfg(feature = "layerstacks-1536x32x32")]
            LayerStacksAccStack::L1536x32x32($entry) => $body,
            #[cfg(feature = "layerstacks-768x16x32")]
            LayerStacksAccStack::L768x16x32($entry) => $body,
            #[cfg(feature = "layerstacks-768x8x32")]
            LayerStacksAccStack::L768x8x32($entry) => $body,
            #[cfg(feature = "layerstacks-512x16x32")]
            LayerStacksAccStack::L512x16x32($entry) => $body,
            #[cfg(feature = "layerstacks-1024x16x32")]
            LayerStacksAccStack::L1024x16x32($entry) => $body,
            #[cfg(feature = "layerstacks-3072x16x32")]
            LayerStacksAccStack::L3072x16x32($entry) => $body,
        }
    };
}

fn snapshot(evaluator: &SearchEvaluator) -> Snapshot {
    match evaluator {
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKP { stack, .. } => halfkx_observe!(
            stack,
            HalfKPStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKaSplit { stack, .. } => halfkx_observe!(
            stack,
            HalfKaSplitStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKaMerged { stack, .. } => halfkx_observe!(
            stack,
            HalfKaMergedStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKaHmSplit { stack, .. } => halfkx_observe!(
            stack,
            HalfKaHmSplitStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKaHmMerged { stack, .. } => halfkx_observe!(
            stack,
            HalfKaHmMergedStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "layerstack-arch")]
        SearchEvaluator::LayerStacks { stack, .. } => {
            layer_observe!(stack, s, layer_snapshot(s.current()))
        }
        #[cfg(feature = "nnue-runtime-dimensions")]
        SearchEvaluator::DynamicHalfKx { stack, .. } => Snapshot {
            accumulation: stack.test_accumulation().to_vec(),
            ..Snapshot::default()
        },
        #[cfg(feature = "nnue-runtime-dimensions")]
        SearchEvaluator::DynamicLayerStacks { stack, .. } => dynamic_layer_snapshot(stack),
        SearchEvaluator::Material { .. } | SearchEvaluator::Uninitialized => Snapshot::default(),
    }
}

fn legacy_snapshot(evaluator: &AccumulatorStackVariant) -> Snapshot {
    match evaluator {
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKP(stack) => halfkx_observe!(
            stack,
            HalfKPStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaSplit(stack) => halfkx_observe!(
            stack,
            HalfKaSplitStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaMerged(stack) => halfkx_observe!(
            stack,
            HalfKaMergedStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaHmSplit(stack) => halfkx_observe!(
            stack,
            HalfKaHmSplitStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "halfkx-arch")]
        AccumulatorStackVariant::HalfKaHmMerged(stack) => halfkx_observe!(
            stack,
            HalfKaHmMergedStack,
            s,
            Snapshot {
                accumulation: s
                    .current()
                    .accumulator
                    .accumulation
                    .iter()
                    .flat_map(|a| a.0)
                    .collect(),
                ..Snapshot::default()
            }
        ),
        #[cfg(feature = "layerstack-arch")]
        AccumulatorStackVariant::LayerStacks(stack) => {
            layer_observe!(stack, s, layer_snapshot(s.current()))
        }
        #[cfg(feature = "nnue-runtime-dimensions")]
        AccumulatorStackVariant::DynamicHalfKx(stack) => Snapshot {
            accumulation: stack.borrow().test_accumulation().to_vec(),
            ..Snapshot::default()
        },
        #[cfg(feature = "nnue-runtime-dimensions")]
        AccumulatorStackVariant::DynamicLayerStacks(stack) => {
            dynamic_layer_snapshot(&stack.borrow())
        }
        #[cfg(not(feature = "halfkx-arch"))]
        _ => unreachable!("loader rejects HalfKX in this build"),
    }
}

fn compare(
    evaluator: &mut SearchEvaluator,
    legacy: &mut AccumulatorStackVariant,
    cache: &mut Option<nnue::LayerStacksAccCache>,
    pos: &Position,
) {
    let expected = nnue::evaluate_dispatch(pos, legacy, cache);
    assert_eq!(evaluator.evaluate(pos), expected, "{}", pos.to_sfen());
    assert_eq!(snapshot(evaluator), legacy_snapshot(legacy), "{}", pos.to_sfen());
    if let Some(progress) = snapshot(evaluator).progress {
        assert_eq!(
            progress,
            nnue::compute_progresskpabs_sum(pos, nnue::get_layer_stack_progress_kpabs_weights())
                .to_bits()
        );
    }
    let network = get_network().unwrap();
    let mut refreshed = SearchEvaluator::from_network(&network);
    assert_eq!(refreshed.evaluate(pos), expected);
    assert_eq!(snapshot(evaluator), snapshot(&refreshed), "{}", pos.to_sfen());
}

fn check_models() {
    material::disable_material();
    nnue::configure_layer_stack_routing(nnue::LayerStackBucketMode::ProgressKPAbs, 4, Some(4))
        .unwrap();
    install_progress_weights();
    check_loader_matrix();
    search_models::for_each_model(|name, mut bytes| {
        if name == "LayerStacks" {
            bytes = with_auxiliary_weights(bytes, 1536, nnue::HALFKA_HM_DIMENSIONS);
        }
        nnue::init_nnue_from_bytes(&bytes).unwrap();
        let network = get_network().unwrap();
        let mut eager = prepared();
        let mut lazy = prepared();
        let mut legacy = AccumulatorStackVariant::from_network(&network);
        let mut cache = None;
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        compare(&mut eager, &mut legacy, &mut cache, &pos);
        let original = snapshot(&eager);
        if name == "LayerStacks" {
            if cfg!(feature = "nnue-psqt") {
                assert!(original.psqt.iter().any(|&value| value != 0));
            }
            if cfg!(feature = "nnue-threat") {
                assert!(original.threat.iter().any(|&value| value != 0));
            }
            if cfg!(all(feature = "nnue-progress-diff", not(feature = "nnue-runtime-dimensions"))) {
                assert!(original.progress.is_some());
            }
        }
        let original_value = eager.evaluate(&pos);
        let root_storage = accumulation_address(&eager);
        eager.prepare();
        assert_eq!(accumulation_address(&eager), root_storage, "{name}: stack reuse");
        compare(&mut eager, &mut legacy, &mut cache, &pos);
        let mut buckets = std::collections::BTreeSet::new();
        let mut progress_values = std::collections::BTreeSet::new();
        record_progress(&pos, &mut buckets, &mut progress_values);
        lazy.evaluate(&pos);
        let mut moves = Vec::new();
        for (index, usi) in [
            "7g7f", "5a6b", "5i6h", "3c3d", "6h5i", "6b5a", "2g2f", "8c8d", "8h2b+", "3a2b",
            "B*7g", "B*3c", "2f2e", "8d8e", "7f7e",
        ]
        .iter()
        .enumerate()
        {
            let mv = pos.to_move(crate::types::Move::from_usi(usi).unwrap()).unwrap();
            assert!(pos.is_legal(mv));
            let dirty = pos.do_move(mv, pos.gives_check(mv));
            moves.push(mv);
            eager.push(dirty);
            lazy.push(dirty);
            legacy.push(dirty);
            compare(&mut eager, &mut legacy, &mut cache, &pos);
            record_progress(&pos, &mut buckets, &mut progress_values);
            if index % 3 == 2 {
                assert_eq!(lazy.evaluate(&pos), eager.evaluate(&pos));
                assert_eq!(snapshot(&lazy), snapshot(&eager));
            }
        }
        assert!(buckets.len() > 1, "{name}: bucket must change");
        assert!(progress_values.len() > 1, "{name}: progress must change");
        if name == "LayerStacks" && cfg!(feature = "nnue-psqt") {
            assert_ne!(snapshot(&eager).psqt, original.psqt);
        }
        if name == "LayerStacks" && cfg!(feature = "nnue-threat") {
            assert_ne!(snapshot(&eager).threat, original.threat);
        }
        // 未評価の子孫からの再準備でも root と cache を無効化する。
        let mut reused = prepared();
        reused.evaluate(&pos);
        let reused_root = accumulation_address(&reused);
        reused.push(DirtyPiece::default());
        reused.prepare();
        assert_eq!(accumulation_address(&reused), reused_root);
        let mut initial = Position::new();
        initial.set_sfen(SFEN_HIRATE).unwrap();
        assert_eq!(reused.evaluate(&initial), original_value);
        assert_eq!(snapshot(&reused), original);
        reused.prepare();
        assert_eq!(reused.evaluate(&pos), eager.evaluate(&pos));
        assert_eq!(snapshot(&reused), snapshot(&eager));
        for mv in moves.into_iter().rev() {
            pos.undo_move(mv);
            eager.pop();
            lazy.pop();
            legacy.pop();
            compare(&mut eager, &mut legacy, &mut cache, &pos);
            assert_eq!(lazy.evaluate(&pos), eager.evaluate(&pos));
            assert_eq!(snapshot(&lazy), snapshot(&eager));
        }
        assert_eq!(snapshot(&eager), original);
        // null move の空差分も同じスタック寿命で扱う。
        pos.do_null_move();
        eager.push(DirtyPiece::default());
        legacy.push(DirtyPiece::default());
        compare(&mut eager, &mut legacy, &mut cache, &pos);
        pos.undo_null_move();
        eager.pop();

        material::set_material_level(MaterialLevel::Lv1);
        let mut selected = prepared();
        if cfg!(feature = "layerstack-arch")
            && matches!(name, "LayerStacks")
            && !cfg!(feature = "nnue-runtime-dimensions")
        {
            assert_eq!(selected.evaluate(&pos), original_value);
        } else {
            assert_eq!(selected.evaluate(&pos), material::evaluate_material(&pos));
        }
        material::disable_material();
        drop(selected);
        drop(lazy);
        drop(network);
        // Material の設定・net の解放は、準備済み評価器の所有する重みを変えない。
        nnue::clear_nnue();
        material::set_material_level(MaterialLevel::Lv9);
        assert_eq!(eager.evaluate(&pos), original_value);
        material::disable_material();
        if name == "LayerStacks" {
            use nnue::net_delta::{NetCoefficientId, NetDelta, NetTensorKind};
            let mut standalone = NNUENetwork::from_bytes(&bytes).unwrap();
            let owner = SearchEvaluator::from_network(&standalone);
            let id = NetCoefficientId {
                kind: NetTensorKind::FtBias,
                bucket: None,
                index: 0,
            };
            let before = standalone.net_coefficient(&id).unwrap();
            let deltas = [NetDelta {
                id: id.clone(),
                delta: 17,
            }];
            assert_eq!(
                standalone.apply_net_deltas(&deltas),
                Err(nnue::NetDeltaError::SharedNetwork)
            );
            assert_eq!(standalone.net_coefficient(&id).unwrap(), before);
            drop(owner);
            standalone.apply_net_deltas(&deltas).unwrap();
            assert_eq!(standalone.net_coefficient(&id).unwrap(), before + 17);
            nnue::init_nnue_from_bytes_with_deltas(
                &bytes,
                &[NetDelta {
                    id: NetCoefficientId {
                        kind: NetTensorKind::FtBias,
                        bucket: None,
                        index: 0,
                    },
                    delta: 17,
                }],
            )
            .unwrap();
        } else {
            let arch_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
            let bias = 12 + arch_len + 4;
            let old = i16::from_le_bytes(bytes[bias..bias + 2].try_into().unwrap());
            bytes[bias..bias + 2].copy_from_slice(&(old + 17).to_le_bytes());
            nnue::init_nnue_from_bytes(&bytes).unwrap();
        }
        let mut replaced = prepared();
        replaced.evaluate(&pos);
        reused.prepare();
        assert_eq!(reused.evaluate(&pos), replaced.evaluate(&pos));
        assert_eq!(snapshot(&reused), snapshot(&replaced));
        assert_ne!(snapshot(&replaced), original, "{name}: net 再読み込みの bias");
        assert_eq!(eager.evaluate(&pos), original_value);
        assert_eq!(snapshot(&eager), original);
    });
    nnue::clear_nnue();
    let mut pos = Position::new();
    pos.set_sfen(SFEN_HIRATE).unwrap();
    for level in [
        MaterialLevel::Lv1,
        MaterialLevel::Lv2,
        MaterialLevel::Lv3,
        MaterialLevel::Lv4,
        MaterialLevel::Lv7,
        MaterialLevel::Lv8,
        MaterialLevel::Lv9,
    ] {
        if !cfg!(feature = "halfkx-arch") && level.value() > 2 {
            continue;
        }
        material::set_material_level(level);
        let mut evaluator = prepared();
        let expected = material::evaluate_material(&pos);
        material::disable_material();
        assert_eq!(evaluator.evaluate(&pos), expected);
    }
}

#[test]
fn typed_evaluator_matches_dispatch_refresh_and_reload() {
    const CHILD: &str = "RSHOGI_TYPED_EVALUATOR_CHILD";
    if std::env::var_os(CHILD).is_some() {
        check_models();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "nnue::search_evaluator::tests::typed_evaluator_matches_dispatch_refresh_and_reload",
            "--nocapture",
        ])
        .env(CHILD, "1")
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
#[should_panic(
    expected = "NNUE network not loaded and MaterialLevel not set. Use 'setoption name EvalFile' or 'setoption name MaterialLevel'."
)]
fn unloaded_evaluator_preserves_error() {
    SearchEvaluator::Uninitialized.evaluate(&Position::new());
}

fn install_progress_weights() {
    use crate::types::{Color, Square};
    let mut weights = vec![0.0; nnue::SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS];
    let mut pos = Position::new();
    pos.set_sfen(SFEN_HIRATE).unwrap();
    let pawn = pos.piece_on(Square::from_usi("7g").unwrap());
    // 2 進数で厳密に表現できる係数で差分更新と全再計算の bit 一致を検証する。
    for (square, weight) in [("7g", -4.0), ("7f", 4.0)] {
        let bp = nnue::BonaPiece::from_piece_square(
            pawn,
            Square::from_usi(square).unwrap(),
            Color::Black,
        );
        for king in 0..Square::NUM {
            weights[king * nnue::FE_OLD_END + bp.value() as usize] = weight;
        }
    }
    nnue::set_layer_stack_progress_kpabs_weights(weights.into_boxed_slice()).unwrap();
}

fn record_progress(
    pos: &Position,
    buckets: &mut std::collections::BTreeSet<usize>,
    sums: &mut std::collections::BTreeSet<u32>,
) {
    let sum = nnue::compute_progresskpabs_sum(pos, nnue::get_layer_stack_progress_kpabs_weights());
    sums.insert(sum.to_bits());
    buckets.insert(nnue::progress_sum_to_bucket(sum, 4));
}

fn check_loader_matrix() {
    let mut pos = Position::new();
    pos.set_sfen(SFEN_HIRATE).unwrap();
    let mut evaluator = SearchEvaluator::Uninitialized;
    let mut check = |name: &str, bytes: Vec<u8>, enabled: bool| {
        let result = nnue::init_nnue_from_bytes(&bytes);
        if enabled {
            result.unwrap_or_else(|error| panic!("{name}: {error}"));
            evaluator.prepare();
            let network = get_network().unwrap();
            let mut legacy = AccumulatorStackVariant::from_network(&network);
            compare(&mut evaluator, &mut legacy, &mut None, &pos);
            let mut search = crate::search::Search::new_with_eval_hash(1, 1);
            let limits = crate::search::LimitsType {
                depth: 1,
                ..Default::default()
            };
            let result = search.go(&mut pos, limits, None::<fn(&crate::search::SearchInfo)>);
            assert_eq!(result.depth, 1, "{name}");
        } else {
            let error = result.expect_err(name);
            assert!(
                matches!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported | std::io::ErrorKind::InvalidData
                ),
                "{name}: {error}"
            );
            assert!(!error.to_string().is_empty());
        }
        nnue::clear_nnue();
    };
    // FT slot で絞らず、edition と異なる全 FT も loader へ渡す。
    search_models::for_each_halfkx_model(|name, bytes| {
        check(
            name,
            bytes,
            cfg!(any(feature = "halfkx-arch", feature = "nnue-runtime-dimensions")),
        );
    });
    for (name, dimensions, enabled) in [
        ("HalfKP", nnue::HALFKP_DIMENSIONS, cfg!(feature = "ft-halfkp")),
        ("HalfKaSplit", nnue::HALFKA_DIMENSIONS, cfg!(feature = "ft-halfka_split")),
        (
            "HalfKaMerged",
            nnue::HALFKA_MERGED_DIMENSIONS,
            cfg!(feature = "ft-halfka_merged"),
        ),
        (
            "HalfKaHmSplit",
            nnue::HALFKA_HM_SPLIT_DIMENSIONS,
            cfg!(feature = "ft-halfka_hm_split"),
        ),
        (
            "HalfKaHmMerged",
            nnue::HALFKA_HM_DIMENSIONS,
            cfg!(feature = "ft-halfka_hm_merged"),
        ),
    ] {
        // 各 edition の有効形状と、どの静的 edition にも存在しない形状を含める。
        for (l1, l2, l3, shape_enabled) in [
            (1536, 16, 32, cfg!(feature = "layerstacks-1536x16x32")),
            (1536, 32, 32, cfg!(feature = "layerstacks-1536x32x32")),
            (768, 16, 32, cfg!(feature = "layerstacks-768x16x32")),
            (768, 8, 32, cfg!(feature = "layerstacks-768x8x32")),
            (512, 16, 32, cfg!(feature = "layerstacks-512x16x32")),
            (1024, 16, 32, cfg!(feature = "layerstacks-1024x16x32")),
            (3072, 16, 32, cfg!(feature = "layerstacks-3072x16x32")),
            (128, 8, 16, false),
        ] {
            if !shape_enabled && l1 != 128 && (l1, l2) != (1536, 16) {
                continue;
            }
            let model = nnue::net_delta::test_utils::build_synthetic_layer_stacks(
                name, dimensions, l1, l2, l3, 4,
            );
            check(
                name,
                model.bytes,
                cfg!(feature = "nnue-runtime-dimensions")
                    || (cfg!(feature = "layerstack-arch")
                        && !cfg!(feature = "nnue-effect-bucket")
                        && enabled
                        && shape_enabled),
            );
        }
    }
    #[cfg(feature = "nnue-effect-bucket")]
    for config in [
        nnue::EffectBucketConfig::KINGFIXED_2X2,
        nnue::EffectBucketConfig::KINGFIXED_3X3,
    ] {
        let l1 = if cfg!(feature = "layerstacks-512x16x32") {
            512
        } else {
            1024
        };
        let mut model = nnue::net_delta::test_utils::build_synthetic_layer_stacks(
            "HalfKaHmMerged",
            config.dimensions(),
            l1,
            16,
            32,
            4,
        );
        let old_len = u32::from_le_bytes(model.bytes[8..12].try_into().unwrap()) as usize;
        let arch = format!(
            "Features=HalfKaHmMerged[{}->{l1}x2],LayerStacks,l2=16,l3=32,EffectBucket={}x{}fixed",
            config.dimensions(),
            if config.nb == 4 { 2 } else { 3 },
            if config.nb == 4 { 2 } else { 3 },
        );
        model.bytes.splice(
            8..12 + old_len,
            (arch.len() as u32).to_le_bytes().into_iter().chain(arch.bytes()),
        );
        check(
            "EffectBucket",
            model.bytes,
            config == nnue::bona_piece_effect_bucket::EFFECT_BUCKET_CONFIG,
        );
    }
}

fn with_auxiliary_weights(bytes: Vec<u8>, l1: usize, dimensions: usize) -> Vec<u8> {
    let has_psqt = cfg!(feature = "nnue-psqt");
    let has_threat = cfg!(feature = "nnue-threat");
    if !has_psqt && !has_threat {
        return bytes;
    }
    let arch_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    let mut arch = String::from_utf8(bytes[12..12 + arch_len].to_vec()).unwrap();
    if has_psqt {
        arch.push_str(",PSQT=4");
    }
    #[cfg(feature = "nnue-threat")]
    arch.push_str(&format!(
        ",Threat={},ThreatProfile={}",
        nnue::threat_features::THREAT_DIMENSIONS,
        nnue::threat_exclusion::THREAT_PROFILE_ID
    ));
    let block_size_offset = 12 + arch_len + 8 + nnue::leb128::LEB128_MAGIC.len();
    let block_size =
        u32::from_le_bytes(bytes[block_size_offset..block_size_offset + 4].try_into().unwrap())
            as usize;
    let ft_end = block_size_offset + 4 + block_size;
    let mut result = bytes[..8].to_vec();
    result.extend_from_slice(&(arch.len() as u32).to_le_bytes());
    result.extend_from_slice(arch.as_bytes());
    result.extend_from_slice(&bytes[12 + arch_len..ft_end]);
    if has_psqt {
        for i in 0..(dimensions + 1) * 4 {
            result.extend_from_slice(&((i % 29) as i32 - 14).to_le_bytes());
        }
    }
    #[cfg(feature = "nnue-threat")]
    let threat_dimensions = nnue::threat_features::THREAT_DIMENSIONS;
    #[cfg(not(feature = "nnue-threat"))]
    let threat_dimensions = 0;
    #[cfg(feature = "nnue-threat")]
    result.extend_from_slice(&nnue::threat_exclusion::THREAT_PROFILE_ID.to_le_bytes());
    for i in 0..threat_dimensions * l1 {
        result.push(((i / l1 * 7 + i % l1) % 5) as u8);
    }
    result.extend_from_slice(&bytes[ft_end..]);
    result
}

fn accumulation_address(evaluator: &SearchEvaluator) -> *const i16 {
    match evaluator {
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKP { stack, .. } => halfkx_observe!(
            stack,
            HalfKPStack,
            s,
            s.current().accumulator.accumulation[0].0.as_ptr()
        ),
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKaSplit { stack, .. } => halfkx_observe!(
            stack,
            HalfKaSplitStack,
            s,
            s.current().accumulator.accumulation[0].0.as_ptr()
        ),
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKaMerged { stack, .. } => halfkx_observe!(
            stack,
            HalfKaMergedStack,
            s,
            s.current().accumulator.accumulation[0].0.as_ptr()
        ),
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKaHmSplit { stack, .. } => halfkx_observe!(
            stack,
            HalfKaHmSplitStack,
            s,
            s.current().accumulator.accumulation[0].0.as_ptr()
        ),
        #[cfg(feature = "halfkx-arch")]
        SearchEvaluator::HalfKaHmMerged { stack, .. } => halfkx_observe!(
            stack,
            HalfKaHmMergedStack,
            s,
            s.current().accumulator.accumulation[0].0.as_ptr()
        ),
        #[cfg(feature = "layerstack-arch")]
        SearchEvaluator::LayerStacks { stack, .. } => {
            layer_observe!(stack, s, s.current().accumulator.accumulation[0].as_ptr())
        }
        #[cfg(feature = "nnue-runtime-dimensions")]
        SearchEvaluator::DynamicHalfKx { stack, .. } => stack.test_accumulation().as_ptr(),
        #[cfg(feature = "nnue-runtime-dimensions")]
        SearchEvaluator::DynamicLayerStacks { stack, .. } => stack.test_accumulation().as_ptr(),
        SearchEvaluator::Material { .. } | SearchEvaluator::Uninitialized => std::ptr::null(),
    }
}
