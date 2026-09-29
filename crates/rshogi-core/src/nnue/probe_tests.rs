//! グローバル設定と NNUE の差し替えは専用子プロセスに隔離する。

use super::{AlignedBox, AlignedBoxBacking, WeightBox};
use crate::nnue::search_evaluator::SearchEvaluator;
use crate::nnue::{DirtyPiece, get_network, init_nnue_from_bytes};
use crate::position::{Position, SFEN_HIRATE};
use crate::probe::{prefetch_child, set_eval_hash_prefetch, set_ft_large_pages};
use crate::search::{LimitsType, Search, SearchInfo};
use crate::types::Move;

fn allocation_and_make_mut(force_heap: bool) {
    let mut allocation = AlignedBox::new_ft_zeroed(1024 * 1024);
    assert!(allocation.iter().all(|&value| value == 0));
    assert_eq!(allocation.as_ptr() as usize % 64, 0);
    if force_heap {
        assert!(matches!(allocation.backing, AlignedBoxBacking::Heap(_)));
    }
    for (index, value) in allocation.iter_mut().enumerate() {
        *value = index as i16;
    }
    let cloned = allocation.clone();
    assert_eq!(&*cloned, &*allocation);
    let mut weights = WeightBox::from(allocation);
    let address = weights.as_ptr();
    weights.make_mut()[17] = -123;
    assert_eq!(weights.as_ptr(), address, "私有 backing は複製しない");
    assert_eq!(weights[17], -123);
    assert_eq!(cloned[17], 17, "clone は独立所有");
}

#[cfg(all(windows, feature = "prepacked-nnue"))]
fn mapped_ft_and_make_mut() {
    use crate::nnue::mapped_weights::ReadOnlyMapping;
    use std::sync::Arc;
    let path = std::env::temp_dir().join(format!("rshogi-probe-ft-{}.bin", std::process::id()));
    let bytes: Vec<_> = (0i16..64).flat_map(i16::to_le_bytes).collect();
    std::fs::write(&path, &bytes).unwrap();
    {
        let owner = Arc::new(ReadOnlyMapping::open(&path).unwrap());
        let source = WeightBox::<i16>::from_mapped(owner.clone(), 0, 64).unwrap();
        let mut copied = WeightBox::<i16>::from_mapped(owner.clone(), 0, 64).unwrap();
        copied.make_mut()[3] = -3;
        assert!(matches!(copied.0.backing, AlignedBoxBacking::Heap(_)));
        let mut probe = WeightBox::<i16>::from_mapped(owner, 0, 64).unwrap().into_probe_ft();
        assert_eq!(&*probe, &*source);
        assert_eq!(probe.0.is_writable(), crate::probe::ft_large_pages());
        probe.make_mut()[3] = -4;
        assert_eq!(source[3], 3);
        assert_eq!(copied[3], -3);
        assert_eq!(probe[3], -4);
    }
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_file(path).unwrap();
}

fn static_values(sfen: &str) -> Vec<i32> {
    let net = get_network().unwrap();
    let mut pos = Position::new();
    pos.set_sfen(sfen).unwrap();
    let mut eager = SearchEvaluator::from_network(&net);
    let mut lazy = SearchEvaluator::from_network(&net);
    let mut values = vec![eager.evaluate(&pos).raw()];
    assert_eq!(lazy.evaluate(&pos).raw(), values[0]);
    if sfen == SFEN_HIRATE {
        for (index, usi) in ["7g7f", "3c3d", "2g2f", "8c8d", "5i6h", "5a6b"].into_iter().enumerate()
        {
            let mv = pos.to_move(Move::from_usi(usi).unwrap()).unwrap();
            let dirty = pos.do_move(mv, pos.gives_check(mv));
            eager.push(dirty);
            lazy.push(dirty);
            let value = eager.evaluate(&pos);
            let mut refreshed = SearchEvaluator::from_network(&net);
            assert_eq!(value, refreshed.evaluate(&pos), "refresh {usi}");
            if index % 2 == 1 {
                assert_eq!(value, lazy.evaluate(&pos), "forward {usi}");
            }
            values.push(value.raw());
        }
    }
    pos.do_null_move();
    eager.push(DirtyPiece::default());
    let mut refreshed = SearchEvaluator::from_network(&net);
    let null_value = eager.evaluate(&pos);
    assert_eq!(null_value, refreshed.evaluate(&pos));
    values.push(null_value.raw());
    values
}

fn check_modes() {
    use crate::nnue::net_delta::test_utils::build_synthetic_layer_stacks;
    let mut fixture = build_synthetic_layer_stacks(
        "HalfKaHmMerged",
        crate::nnue::HALFKA_HM_DIMENSIONS,
        1536,
        16,
        32,
        9,
    );
    // 1 byte LEB128 を保ちつつ特徴ごとの正負の重みを入れ、差分更新を観測する。
    let start = fixture.ft_biases + 1536;
    let end = start + crate::nnue::HALFKA_HM_DIMENSIONS * 1536;
    for (feature, row) in fixture.bytes[start..end].chunks_exact_mut(1536).enumerate() {
        for (lane, value) in row.iter_mut().enumerate() {
            *value = (((feature * 17 + feature / 97 + lane / 32) % 7) as i8 - 3) as u8 & 0x7f;
        }
    }
    crate::eval::disable_material();
    crate::eval::set_eval_hash_enabled(true);
    crate::nnue::configure_layer_stack_routing(
        crate::nnue::LayerStackBucketMode::KingRank9,
        9,
        None,
    )
    .unwrap();
    let positions = [SFEN_HIRATE, "4k4/9/9/4p4/4P4/9/9/9/4K4 b R2Pbr 1"];
    let mut baseline = None;
    for (enabled, force_heap) in [(false, false), (true, false), (true, true)] {
        set_ft_large_pages(enabled);
        #[cfg(windows)]
        crate::probe::FORCE_FT_HEAP.store(force_heap, std::sync::atomic::Ordering::Relaxed);
        allocation_and_make_mut(!enabled || force_heap);
        #[cfg(all(windows, feature = "prepacked-nnue"))]
        mapped_ft_and_make_mut();
        init_nnue_from_bytes(&fixture.bytes).unwrap();
        for mode in 0..=2 {
            assert!(set_eval_hash_prefetch(mode));
            assert_eq!(prefetch_child::<false>(), mode != 1);
            assert_eq!(prefetch_child::<true>(), mode == 0);
            assert!(!set_eval_hash_prefetch(3));
            assert_eq!(prefetch_child::<false>(), mode != 1);
            assert_eq!(prefetch_child::<true>(), mode == 0);
            let mut observed = Vec::new();
            for sfen in positions {
                let values = static_values(sfen);
                let mut pos = Position::new();
                pos.set_sfen(sfen).unwrap();
                let mut search = Search::new_with_eval_hash(1, 1);
                let result = search.go(
                    &mut pos,
                    LimitsType {
                        depth: 3,
                        ..Default::default()
                    },
                    None::<fn(&SearchInfo)>,
                );
                assert_eq!(result.depth, 3);
                observed.push((values, result.best_move, result.score, result.nodes, result.pv));
            }
            if let Some(expected) = &baseline {
                assert_eq!(&observed, expected, "FT={enabled}, fallback={force_heap}, mode={mode}");
            } else {
                assert!(observed[0].0.windows(2).any(|v| v[0] != v[1]));
                baseline = Some(observed);
            }
        }
    }
}

#[test]
fn probe_modes_preserve_evaluation_and_fixed_depth_search() {
    const CHILD: &str = "RSHOGI_LS_PROBE_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(check_modes)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            concat!(module_path!(), "::probe_modes_preserve_evaluation_and_fixed_depth_search")
                .strip_prefix("rshogi_core::")
                .unwrap(),
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
