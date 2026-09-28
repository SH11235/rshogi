//! 探索入口からの Finny 配線と、未評価の玉移動をまたぐ再計算を検証する。

use super::*;
use crate::movegen::{MoveList, generate_legal_all};
use crate::nnue::halfkp::HalfKPStack;
use crate::nnue::init_nnue_from_bytes;
use crate::nnue::network_halfkp::{
    AccumulatorHalfKP, AccumulatorStackHalfKP, HalfKP256CReLU, halfkp_loader_fixture,
};
use crate::nnue::search_evaluator::SearchEvaluator;
use crate::position::SFEN_HIRATE;

const L1: usize = 256;
const ARCH: &str = "Features=HalfKP(Friend)[125388->256x2],l2=32,l3=32";
const MOVES: [&str; 8] = [
    "7g7f", "5a6b", "5i6h", "3c3d", "6h5i", "6b5a", "2g2f", "8c8d",
];

fn fixture(bias: i16) -> Vec<u8> {
    let mut bytes = halfkp_loader_fixture(L1, ARCH);
    let bias_start = 12 + ARCH.len() + 4;
    for value in bytes[bias_start..bias_start + 2 * L1].chunks_exact_mut(2) {
        value.copy_from_slice(&bias.to_le_bytes());
    }
    let weight_start = bias_start + 2 * L1;
    let weight_end = weight_start + 2 * crate::nnue::HALFKP_DIMENSIONS * L1;
    // 特徴ごとに異なる重みを持たせ、駒や玉の移動で accumulator が変化するようにする。
    for (feature, row) in bytes[weight_start..weight_end].chunks_exact_mut(2 * L1).enumerate() {
        let weight = ((feature * 17 + feature / 97) % 3) as i16 - 1;
        for value in row.chunks_exact_mut(2) {
            value.copy_from_slice(&weight.to_le_bytes());
        }
    }
    // dense 層の飽和を避け、FT の違いを最終評価値でも観測できるようにする。
    let dense_start = weight_end + 4 + 32 * 4;
    for (index, weight) in bytes[dense_start..dense_start + 2 * L1 * 32].iter_mut().enumerate() {
        let output = index / (2 * L1);
        let input = index % (2 * L1);
        *weight = u8::from(input % 8 == output % 8);
    }
    bytes
}

fn worker() -> Box<SearchWorker> {
    SearchWorker::new(
        Arc::new(TranspositionTable::new(1)),
        Arc::new(EvalHash::new(1)),
        0,
        0,
        SearchTuneParams::default(),
    )
}

fn stack(worker: &SearchWorker) -> &AccumulatorStackHalfKP<L1> {
    let SearchEvaluator::HalfKP {
        stack: HalfKPStack::L256(stack),
        ..
    } = &worker.state.evaluator
    else {
        panic!("const generics HalfKP L1=256 が必要");
    };
    stack
}

fn cache(worker: &SearchWorker) -> &Option<crate::nnue::AccumulatorCacheGeneric> {
    let SearchEvaluator::HalfKP { cache, .. } = &worker.state.evaluator else {
        panic!("HalfKP evaluator が必要");
    };
    cache
}

fn assert_evaluation(
    worker: &mut SearchWorker,
    pos: &Position,
    net: &HalfKP256CReLU,
    refreshed: &[Color],
) -> Value {
    let mut expected = AccumulatorHalfKP::<L1>::new();
    net.refresh_accumulator(pos, &mut expected);
    let expected_value = net.evaluate(pos, &expected);
    assert_eq!(nnue_evaluate(&mut worker.state, pos), expected_value, "{}", pos.to_sfen());
    let actual = &stack(worker).current().accumulator;
    assert!(actual.computed_accumulation);
    for perspective in [Color::Black, Color::White] {
        assert_eq!(
            actual.accumulation[perspective.index()].0,
            expected.accumulation[perspective.index()].0
        );
    }
    // 評価値だけでは cache を通らない実装も通るため、実際の cache 書き込みも確認する。
    if !crate::nnue::halfkx_finny_enabled(L1) {
        assert!(cache(worker).is_none());
        return expected_value;
    }
    let cache = cache(worker).as_ref().expect("prepare_search の cache 初期化");
    for &perspective in refreshed {
        assert_eq!(
            cache.cached_accumulation(pos.king_square(perspective), perspective),
            Some(expected.accumulation[perspective.index()].0.as_slice()),
            "cache が更新されていない: {} {perspective:?}",
            pos.to_sfen()
        );
    }
    expected_value
}

fn check_search_entry() {
    let mut eager = worker();
    let mut lazy = worker();
    let mut root_values = Vec::new();
    let mut root_accumulations = Vec::new();
    // 同じ worker・同じ L1 のまま FT の bias と net を切り替える。
    for bias in [32, 64] {
        let bytes = fixture(bias);
        let reference = HalfKP256CReLU::read(&mut std::io::Cursor::new(&bytes)).unwrap();
        init_nnue_from_bytes(&bytes).unwrap();
        for sfen in [SFEN_HIRATE, "4k4/9/9/9/9/9/9/9/4K4 b R2Pbr 1"] {
            let mut search = crate::search::Search::new_with_eval_hash(1, 1);
            let mut position = Position::new();
            position.set_sfen(sfen).unwrap();
            let result = search.go(
                &mut position,
                LimitsType {
                    depth: 4,
                    ..Default::default()
                },
                None::<fn(&crate::search::SearchInfo)>,
            );
            assert_eq!(result.depth, 4);
            assert_eq!(result.pv.first(), Some(&result.best_move));
            println!(
                "FINNY_SEARCH {bias} {sfen}: {} {:?} {} {:?}",
                result.nodes,
                result.score,
                result.best_move.to_usi(),
                result.pv.iter().map(|mv| mv.to_usi()).collect::<Vec<_>>()
            );
        }
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();
        for worker in [&mut eager, &mut lazy] {
            worker.prepare_search(&LimitsType::default());
            if crate::nnue::halfkx_finny_enabled(L1) {
                let cache = cache(worker).as_ref().expect("探索開始時の cache");
                for perspective in [Color::Black, Color::White] {
                    assert!(
                        cache
                            .cached_accumulation(pos.king_square(perspective), perspective)
                            .is_none()
                    );
                }
            } else {
                assert!(cache(worker).is_none());
            }
            assert_evaluation(worker, &pos, &reference, &[Color::Black, Color::White]);
        }
        root_values.push(nnue_evaluate(&mut eager.state, &pos));
        root_accumulations.push(stack(&eager).current().accumulator.accumulation[0].0);
        let mut values = vec![root_values[root_values.len() - 1]];
        for (index, usi) in MOVES.iter().enumerate() {
            let mut legal = MoveList::new();
            generate_legal_all(&pos, &mut legal);
            let mv = legal
                .iter()
                .copied()
                .find(|candidate| candidate.to_usi() == *usi)
                .unwrap_or_else(|| panic!("合法手が必要: {usi}"));
            let dirty = pos.do_move(mv, pos.gives_check(mv));
            eager.state.evaluator.push(dirty);
            lazy.state.evaluator.push(dirty);
            let refreshed: Vec<_> = [Color::Black, Color::White]
                .into_iter()
                .filter(|color| dirty.king_moved[color.index()])
                .collect();
            values.push(assert_evaluation(&mut eager, &pos, &reference, &refreshed));

            if index == 3 || index == 5 || index == 7 {
                let current = stack(&lazy);
                assert!(!current.current().accumulator.computed_accumulation);
                let previous = current.current().previous.unwrap();
                assert!(!current.entry_at(previous).accumulator.computed_accumulation);
                assert_eq!(current.find_usable_accumulator(), None);
                if index == 3 {
                    // 7g7f, 5a6b, 5i6h, 3c3d: 計算済み root と現局面の間に両玉の移動がある。
                    assert!(current.entry_at(0).accumulator.computed_accumulation);
                    assert!(current.entry_at(2).dirty_piece.king_moved[Color::White.index()]);
                    assert!(current.entry_at(3).dirty_piece.king_moved[Color::Black.index()]);
                    assert_eq!(current.current().dirty_piece.king_moved, [false; 2]);
                }
                assert_evaluation(&mut lazy, &pos, &reference, &[Color::Black, Color::White]);
            }
        }
        assert!(values.iter().any(|&value| value != values[0]), "手順で評価値が変わる fixture");
    }
    assert_ne!(root_values[0], root_values[1], "FT の切替が評価値にも現れる");
    assert_ne!(root_accumulations[0], root_accumulations[1]);
}

#[test]
fn halfkx_search_entry_finny_matches_full_refresh_and_net_reload() {
    const CHILD: &str = "RSHOGI_FINNY_SEARCH_CHILD";
    if std::env::var_os(CHILD).is_some() {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(check_search_entry)
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    // グローバル NNUE のロードを他の並列テストから隔離する。
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            concat!(
                module_path!(),
                "::halfkx_search_entry_finny_matches_full_refresh_and_net_reload"
            )
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
