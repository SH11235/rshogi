//! 固定 depth とノード上限の探索結果 (nodes / score / PV / bestmove) を検証する。

use rshogi_core as core;

#[path = "common/search_models.rs"]
mod search_models;

use rshogi_core::eval::material::{self, MaterialLevel};
use rshogi_core::nnue::{self, LayerStackBucketMode};
use rshogi_core::position::{Position, SFEN_HIRATE};
use rshogi_core::search::{LimitsType, Search, SearchInfo};

fn fixed_depth(name: &str) {
    for (index, sfen) in [
        SFEN_HIRATE,
        "4k4/9/2p3p2/3p1p3/4P4/3P1P3/2P3P2/9/4K4 b RBrb 1",
    ]
    .iter()
    .enumerate()
    {
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();
        let mut search = Search::new_with_eval_hash(1, 1);
        let mut limits = LimitsType::default();
        limits.depth = 4;
        let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);
        assert_eq!(result.depth, 4);
        let pv = result.pv.iter().map(|mv| mv.to_usi()).collect::<Vec<_>>().join(" ");
        let actual = format!(
            "SEARCH_RESULT {name} {index} {} {} {} {pv}",
            result.nodes,
            result.score.raw(),
            result.best_move.to_usi()
        );
        let prefix = format!("SEARCH_RESULT {name} {index} ");
        let expected = include_str!("common/search-results.txt")
            .lines()
            .find(|line| line.starts_with(&prefix))
            .expect("比較対象の探索結果が必要");
        assert_eq!(actual, expected);
    }
}

#[test]
fn fixed_depth_search_regression() {
    material::disable_material();
    nnue::configure_layer_stack_routing(LayerStackBucketMode::ProgressKPAbs, 4, Some(4)).unwrap();
    search_models::for_each_model(|name, bytes| {
        nnue::init_nnue_from_bytes(&bytes).unwrap();
        fixed_depth(name);
    });
    nnue::clear_nnue();
    for level in [
        MaterialLevel::Lv1,
        MaterialLevel::Lv2,
        MaterialLevel::Lv3,
        MaterialLevel::Lv4,
        MaterialLevel::Lv7,
        MaterialLevel::Lv8,
        MaterialLevel::Lv9,
    ] {
        // 静的 LayerStacks 専用 build は Material 用の利き更新を含まない。
        if !cfg!(feature = "halfkx-arch") && level.value() > 2 {
            continue;
        }
        material::set_material_level(level);
        fixed_depth(&format!("Material{}", level.value()));
    }
    material::disable_material();
    // material と NNUE のグローバル状態を並列テストで共有しないため、同じ #[test] 内で呼ぶ。
    node_budget();
}

fn node_budget() {
    material::set_material_level(MaterialLevel::Lv2);
    let mut pos = Position::new();
    pos.set_sfen(SFEN_HIRATE).unwrap();
    let mut search = Search::new_with_eval_hash(1, 1);
    let mut limits = LimitsType::default();
    limits.depth = 64;
    limits.nodes = 1000;
    let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);
    assert_eq!(result.nodes, 1000);
    assert_eq!(result.depth, 6);
    assert_eq!(result.score.raw(), 2);
    assert_eq!(result.best_move.to_usi(), "1g1f");
    let pv = result.pv.iter().map(|mv| mv.to_usi()).collect::<Vec<_>>().join(" ");
    assert_eq!(pv, "1g1f 2c2d 3g3f");
    material::disable_material();
}
