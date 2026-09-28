//! 型付き評価器と従来 dispatch・全再計算の一致。
use super::*;
use crate as core;
#[path = "../../tests/common/search_models.rs"]
mod search_models;
use crate::nnue::{self, AccumulatorStackVariant};
use crate::position::SFEN_HIRATE;

fn snapshot(evaluator: &SearchEvaluator) -> Vec<i16> {
    match evaluator {
        #[cfg(all(
            feature = "halfkx-arch",
            any(not(feature = "mode-specific"), feature = "ft-halfkp")
        ))]
        SearchEvaluator::HalfKP { stack, .. } => {
            use crate::nnue::halfkp::HalfKPStack;
            match stack {
                HalfKPStack::L256(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKPStack::L512(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKPStack::L768(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKPStack::L1024(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
            }
        }
        #[cfg(all(
            feature = "halfkx-arch",
            any(not(feature = "mode-specific"), feature = "ft-halfka_split")
        ))]
        SearchEvaluator::HalfKaSplit { stack, .. } => {
            use crate::nnue::halfka_split::HalfKaSplitStack;
            match stack {
                HalfKaSplitStack::L256(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaSplitStack::L512(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaSplitStack::L768(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaSplitStack::L1024(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
            }
        }
        #[cfg(all(
            feature = "halfkx-arch",
            any(not(feature = "mode-specific"), feature = "ft-halfka_merged")
        ))]
        SearchEvaluator::HalfKaMerged { stack, .. } => {
            use crate::nnue::halfka_merged::HalfKaMergedStack;
            match stack {
                HalfKaMergedStack::L256(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaMergedStack::L512(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaMergedStack::L768(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaMergedStack::L1024(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
            }
        }
        #[cfg(all(
            feature = "halfkx-arch",
            any(not(feature = "mode-specific"), feature = "ft-halfka_hm_split")
        ))]
        SearchEvaluator::HalfKaHmSplit { stack, .. } => {
            use crate::nnue::halfka_hm_split::HalfKaHmSplitStack;
            match stack {
                HalfKaHmSplitStack::L256(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaHmSplitStack::L512(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaHmSplitStack::L768(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaHmSplitStack::L1024(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
            }
        }
        #[cfg(all(
            feature = "halfkx-arch",
            any(not(feature = "mode-specific"), feature = "ft-halfka_hm_merged")
        ))]
        SearchEvaluator::HalfKaHmMerged { stack, .. } => {
            use crate::nnue::halfka_hm_merged::HalfKaHmMergedStack;
            match stack {
                HalfKaHmMergedStack::L256(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaHmMergedStack::L512(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaHmMergedStack::L768(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
                HalfKaHmMergedStack::L1024(s) => {
                    s.current().accumulator.accumulation.iter().flat_map(|a| a.0).collect()
                }
            }
        }
        #[cfg(feature = "nnue-runtime-dimensions")]
        SearchEvaluator::DynamicHalfKx { stack, .. } => stack.test_accumulation().to_vec(),
        #[cfg(feature = "nnue-runtime-dimensions")]
        SearchEvaluator::DynamicLayerStacks { stack, .. } => stack.test_accumulation().to_vec(),
        #[cfg(feature = "layerstack-arch")]
        SearchEvaluator::LayerStacks { stack, .. } => match stack {
            #[cfg(feature = "layerstacks-1536x16x32")]
            LayerStacksAccStack::L1536x16x32(s) => {
                s.current().accumulator.accumulation.iter().flatten().copied().collect()
            }
            #[cfg(feature = "layerstacks-1536x32x32")]
            LayerStacksAccStack::L1536x32x32(s) => {
                s.current().accumulator.accumulation.iter().flatten().copied().collect()
            }
            #[cfg(feature = "layerstacks-768x16x32")]
            LayerStacksAccStack::L768x16x32(s) => {
                s.current().accumulator.accumulation.iter().flatten().copied().collect()
            }
            #[cfg(feature = "layerstacks-768x8x32")]
            LayerStacksAccStack::L768x8x32(s) => {
                s.current().accumulator.accumulation.iter().flatten().copied().collect()
            }
            #[cfg(feature = "layerstacks-512x16x32")]
            LayerStacksAccStack::L512x16x32(s) => {
                s.current().accumulator.accumulation.iter().flatten().copied().collect()
            }
            #[cfg(feature = "layerstacks-1024x16x32")]
            LayerStacksAccStack::L1024x16x32(s) => {
                s.current().accumulator.accumulation.iter().flatten().copied().collect()
            }
            #[cfg(feature = "layerstacks-3072x16x32")]
            LayerStacksAccStack::L3072x16x32(s) => {
                s.current().accumulator.accumulation.iter().flatten().copied().collect()
            }
        },
        SearchEvaluator::Material { .. } | SearchEvaluator::Uninitialized => Vec::new(),
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
    let network = get_network().unwrap();
    let mut refreshed = SearchEvaluator::from_network(&network);
    assert_eq!(refreshed.evaluate(pos), expected);
    assert_eq!(snapshot(evaluator), snapshot(&refreshed), "{}", pos.to_sfen());
}

fn check_models() {
    material::disable_material();
    nnue::configure_layer_stack_routing(nnue::LayerStackBucketMode::ProgressKPAbs, 4, Some(4))
        .unwrap();
    search_models::for_each_model(|name, mut bytes| {
        nnue::init_nnue_from_bytes(&bytes).unwrap();
        let network = get_network().unwrap();
        let mut eager = SearchEvaluator::prepare();
        let mut lazy = SearchEvaluator::prepare();
        let mut legacy = AccumulatorStackVariant::from_network(&network);
        let mut cache = None;
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        compare(&mut eager, &mut legacy, &mut cache, &pos);
        let original = snapshot(&eager);
        let original_value = eager.evaluate(&pos);
        lazy.evaluate(&pos);
        let mut moves = Vec::new();
        for (index, usi) in [
            "7g7f", "5a6b", "5i6h", "3c3d", "6h5i", "6b5a", "2g2f", "8c8d",
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
            if index % 3 == 2 {
                assert_eq!(lazy.evaluate(&pos), eager.evaluate(&pos));
                assert_eq!(snapshot(&lazy), snapshot(&eager));
            }
        }
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
        let mut selected = SearchEvaluator::prepare();
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
        let mut replaced = SearchEvaluator::prepare();
        replaced.evaluate(&pos);
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
        let mut evaluator = SearchEvaluator::prepare();
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
