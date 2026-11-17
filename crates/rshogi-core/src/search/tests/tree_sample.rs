//! 探索中の局面の書き出しは探索木を変えず、同じ seed なら同じ標本を出す。
use crate::movegen::{MoveList, generate_legal};
use crate::position::{Position, StaticPosition};
use crate::search::{LimitsType, Search, SearchInfo, TreeSample, TreeSampleConfig};
use crate::types::{Color, Move, Value};

const SFENS: [&str; 2] = [
    "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
    "lnsgk1snl/1r4gb1/p1pppp1pp/6p2/1p7/2P6/PPBPPPPPP/7R1/LNSGKGSNL b - 9",
];

fn on_large_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

/// 各局面を 1 thread・固定ノード数で探索し、(nodes, bestmove, score) と標本を返す。
fn run(config: Option<TreeSampleConfig>, seed: u64) -> (Vec<(u64, Move, Value)>, Vec<TreeSample>) {
    on_large_stack(move || {
        let mut search = Search::new(16);
        search.set_tree_sample(config, seed);
        let mut results = Vec::new();
        let mut samples = Vec::new();
        for sfen in SFENS {
            let mut pos = Position::new();
            pos.set_sfen(sfen).unwrap();
            let result = search.go(
                &mut pos,
                LimitsType {
                    nodes: 20_000,
                    ..Default::default()
                },
                None::<fn(&SearchInfo)>,
            );
            results.push((result.nodes, result.best_move, result.score));
            search.drain_tree_samples(&mut samples);
        }
        (results, samples)
    })
}

fn config(probability: f64) -> Option<TreeSampleConfig> {
    Some(TreeSampleConfig {
        probability,
        capacity: 1 << 16,
    })
}

#[test]
fn sampling_does_not_change_the_search_tree() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    let (baseline, none) = run(None, 1);
    assert!(none.is_empty());
    let (zero, zero_samples) = run(config(0.0), 1);
    assert_eq!(zero, baseline);
    assert!(zero_samples.is_empty());
    let (half, half_samples) = run(config(0.5), 1);
    assert_eq!(half, baseline);
    assert!(!half_samples.is_empty());
}

#[test]
fn same_seed_gives_identical_samples() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    let (_, first) = run(config(0.05), 7);
    let (_, second) = run(config(0.05), 7);
    assert!(!first.is_empty());
    assert_eq!(first, second);
    let (_, other_seed) = run(config(0.05), 8);
    assert_ne!(first, other_seed);
}

#[test]
fn samples_are_quiet_positions() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    let (_, samples) = run(config(0.2), 3);
    assert!(samples.len() > 10, "samples={}", samples.len());
    for sample in &samples {
        let pos = position_of(sample);
        assert!(!pos.in_check());
        let mut moves = MoveList::new();
        generate_legal(&pos, &mut moves);
        let mut has_quiet = false;
        for &mv in moves.iter() {
            let capture = pos.is_capture(mv);
            assert!(!(capture && pos.see_ge(mv, Value::new(1))), "gaining capture {}", mv.to_usi());
            assert!(!(mv.is_promotion() && pos.see_ge(mv, Value::ZERO)), "gaining promotion");
            has_quiet |= !capture && !mv.is_promotion();
        }
        assert!(has_quiet);
        assert!(sample.game_ply >= 1);
    }
}

fn position_of(sample: &TreeSample) -> Position {
    let pos = StaticPosition::from_parts(&sample.board, &sample.hands, sample.side_to_move)
        .unwrap()
        .into_position()
        .unwrap();
    assert_eq!(
        [pos.king_square(Color::Black), pos.king_square(Color::White)],
        sample.king_squares
    );
    pos
}
