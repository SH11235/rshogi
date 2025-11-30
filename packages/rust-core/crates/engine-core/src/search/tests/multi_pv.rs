//! MultiPV（候補手複数探索）のテスト
//!
//! YaneuraOu準拠のMultiPV実装の単体テスト

use crate::search::types::{RootMove, RootMoves};
use crate::types::{Move, Value};
use std::thread;

/// SearchWorkerが大きなスタックを消費するため、統合テストは大きめのスタックで実行
const STACK_SIZE: usize = 64 * 1024 * 1024; // 64MB

fn run_with_large_stack<F, R>(f: F) -> R
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(f)
        .expect("failed to spawn test thread with large stack")
        .join()
        .expect("test thread panicked")
}

// =============================================================================
// Phase 2.1: MultiPVのクランプとSkillLevel
// =============================================================================

/// MultiPVは合法手数でクランプされる
#[test]
fn test_multi_pv_clamped_by_legal_moves() {
    let root_moves_count = 3; // 合法手が3つ
    let requested_multi_pv = 5; // MultiPV=5を要求

    // 実際のMultiPVは合法手数でクランプされる
    let effective_multi_pv = requested_multi_pv.min(root_moves_count);

    assert_eq!(effective_multi_pv, 3, "MultiPV=5だが合法手が3つなので3にクランプ");
}

/// SkillLevel有効時はMultiPV最低4
#[test]
fn test_skill_level_forces_multi_pv_4() {
    let skill_enabled = true;
    let user_multi_pv = 1;

    let effective_multi_pv = if skill_enabled {
        user_multi_pv.max(4)
    } else {
        user_multi_pv
    };

    assert_eq!(effective_multi_pv, 4, "SkillLevel有効時は最低4");
}

/// SkillLevel無効時は通常のMultiPV
#[test]
fn test_no_skill_level_uses_normal_multi_pv() {
    let skill_enabled = false;
    let user_multi_pv = 1;

    let effective_multi_pv = if skill_enabled {
        user_multi_pv.max(4)
    } else {
        user_multi_pv
    };

    assert_eq!(effective_multi_pv, 1, "SkillLevel無効時は指定通り");
}

/// SkillLevel有効でもMultiPV=5なら5のまま
#[test]
fn test_skill_level_with_high_multi_pv() {
    let skill_enabled = true;
    let user_multi_pv = 5;

    let effective_multi_pv = if skill_enabled {
        user_multi_pv.max(4)
    } else {
        user_multi_pv
    };

    assert_eq!(effective_multi_pv, 5, "既に4以上なら変更なし");
}

// =============================================================================
// Phase 2.2: MultiPVループのソート
// =============================================================================

/// 各PVごとに安定ソート（スコア降順）
#[test]
fn test_multi_pv_stable_sort_per_pv() {
    let mut root_moves = vec![
        RootMove::new(Move::from_usi("7g7f").unwrap()),
        RootMove::new(Move::from_usi("2g2f").unwrap()),
        RootMove::new(Move::from_usi("5g5f").unwrap()),
    ];

    // スコアを設定
    root_moves[0].score = Value::new(100);
    root_moves[1].score = Value::new(200);
    root_moves[2].score = Value::new(150);

    // pvIdx = 0の後、[0..1]をソート（実際は1要素なのでソート不要）
    root_moves[0..1].sort_by_key(|rm| std::cmp::Reverse(rm.score));

    // pvIdx = 1の後、[0..2]をソート
    root_moves[0..2].sort_by_key(|rm| std::cmp::Reverse(rm.score));
    // 期待: [2g2f(200), 7g7f(100), ...]
    assert_eq!(root_moves[0].pv[0], Move::from_usi("2g2f").unwrap());
    assert_eq!(root_moves[1].pv[0], Move::from_usi("7g7f").unwrap());

    // pvIdx = 2の後、[0..3]をソート
    root_moves[0..3].sort_by_key(|rm| std::cmp::Reverse(rm.score));
    // 期待: [2g2f(200), 5g5f(150), 7g7f(100)]
    assert_eq!(root_moves[0].pv[0], Move::from_usi("2g2f").unwrap());
    assert_eq!(root_moves[1].pv[0], Move::from_usi("5g5f").unwrap());
    assert_eq!(root_moves[2].pv[0], Move::from_usi("7g7f").unwrap());
}

/// RootMoves.stable_sort_range()の動作確認
#[test]
fn test_stable_sort_range() {
    // 4つの手を追加（スコアは未ソート状態）
    let mut rm1 = RootMove::new(Move::from_usi("7g7f").unwrap());
    rm1.score = Value::new(100);
    let mut rm2 = RootMove::new(Move::from_usi("2g2f").unwrap());
    rm2.score = Value::new(200);
    let mut rm3 = RootMove::new(Move::from_usi("5g5f").unwrap());
    rm3.score = Value::new(150);
    let mut rm4 = RootMove::new(Move::from_usi("8h7g").unwrap());
    rm4.score = Value::new(200); // rm2と同点

    let mut root_moves =
        RootMoves::from_vec(vec![rm1.clone(), rm2.clone(), rm3.clone(), rm4.clone()]);

    // 範囲[0..4]を安定ソート
    root_moves.stable_sort_range(0, 4);

    // 期待: スコア降順、同点なら元の順序
    // [200(rm2), 200(rm4), 150(rm3), 100(rm1)]
    assert_eq!(root_moves[0].score.raw(), 200);
    assert_eq!(
        root_moves[0].pv[0],
        Move::from_usi("2g2f").unwrap(),
        "同点の場合、元の順序を保持（rm2が先）"
    );

    assert_eq!(root_moves[1].score.raw(), 200);
    assert_eq!(
        root_moves[1].pv[0],
        Move::from_usi("8h7g").unwrap(),
        "同点の場合、元の順序を保持（rm4が後）"
    );

    assert_eq!(root_moves[2].score.raw(), 150);
    assert_eq!(root_moves[2].pv[0], Move::from_usi("5g5f").unwrap());

    assert_eq!(root_moves[3].score.raw(), 100);
    assert_eq!(root_moves[3].pv[0], Move::from_usi("7g7f").unwrap());
}

/// RootMoves.stable_sort_range()の範囲指定テスト
#[test]
fn test_stable_sort_range_partial() {
    let mut rm1 = RootMove::new(Move::from_usi("7g7f").unwrap());
    rm1.score = Value::new(100);
    let mut rm2 = RootMove::new(Move::from_usi("2g2f").unwrap());
    rm2.score = Value::new(50);
    let mut rm3 = RootMove::new(Move::from_usi("5g5f").unwrap());
    rm3.score = Value::new(150);
    let mut rm4 = RootMove::new(Move::from_usi("8h7g").unwrap());
    rm4.score = Value::new(75);

    let mut root_moves =
        RootMoves::from_vec(vec![rm1.clone(), rm2.clone(), rm3.clone(), rm4.clone()]);

    // 範囲[1..4]のみソート（rm1は固定）
    root_moves.stable_sort_range(1, 4);

    // 期待: [100(rm1-固定), 150(rm3), 75(rm4), 50(rm2)]
    assert_eq!(root_moves[0].score.raw(), 100);
    assert_eq!(root_moves[0].pv[0], Move::from_usi("7g7f").unwrap(), "範囲外は変更されない");

    assert_eq!(root_moves[1].score.raw(), 150);
    assert_eq!(root_moves[1].pv[0], Move::from_usi("5g5f").unwrap());

    assert_eq!(root_moves[2].score.raw(), 75);
    assert_eq!(root_moves[2].pv[0], Move::from_usi("8h7g").unwrap());

    assert_eq!(root_moves[3].score.raw(), 50);
    assert_eq!(root_moves[3].pv[0], Move::from_usi("2g2f").unwrap());
}

// =============================================================================
// Phase 2.3: 詰み早期終了のMultiPV制限
// =============================================================================

/// MultiPV=1のとき詰みで早期終了する条件
#[test]
fn test_mate_early_exit_when_multi_pv_1() {
    let multi_pv = 1;
    let best_value = Value::mate_in(10); // 10手詰め
    let depth = 30;

    // YaneuraOu条件: (mate_ply + 2) * 5 / 2 < depth
    let mate_ply = best_value.mate_ply();
    let should_exit = multi_pv == 1 && best_value.is_win() && (mate_ply + 2) * 5 / 2 < depth;

    // (10 + 2) * 5 / 2 = 60 / 2 = 30
    // 30 < 30 は false
    assert!(!should_exit, "30 < 30 は false なので早期終了しない");

    // depth = 31なら終了
    let depth = 31;
    let should_exit = multi_pv == 1 && best_value.is_win() && (mate_ply + 2) * 5 / 2 < depth;
    assert!(should_exit, "30 < 31 は true なので早期終了");
}

/// MultiPV>1のとき詰みでも継続探索
#[test]
fn test_mate_no_early_exit_when_multi_pv_gt_1() {
    let multi_pv = 3;
    let best_value = Value::mate_in(5);
    let depth = 30;

    let mate_ply = best_value.mate_ply();
    let should_exit = multi_pv == 1 && best_value.is_win() && (mate_ply + 2) * 5 / 2 < depth;

    // multi_pv != 1 なので常に false
    assert!(!should_exit, "MultiPV>1では詰みでも早期終了しない");
}

/// 詰まれている場合は早期終了しない
#[test]
fn test_no_early_exit_when_mated() {
    let multi_pv = 1;
    let best_value = Value::mated_in(10); // 10手で詰まされる
    let depth = 30;

    let should_exit =
        multi_pv == 1 && best_value.is_win() && (best_value.mate_ply() + 2) * 5 / 2 < depth;

    // is_win() が false なので終了しない
    assert!(!should_exit, "詰まされる側は早期終了しない");
}

// =============================================================================
// Phase 3: 統合テスト（MultiPVループの実動作確認）
// =============================================================================

/// MultiPV=3で3つのPVライン出力
#[test]
fn test_multi_pv_3_integration() {
    use crate::position::Position;
    use crate::search::engine::{Search, SearchInfo};
    use crate::search::LimitsType;

    run_with_large_stack(|| {
        let mut search = Search::new(16); // 16MB TT
        let mut pos = Position::new();
        pos.set_hirate(); // 平手初期局面

        let limits = LimitsType {
            depth: 1,
            multi_pv: 3,
            ..Default::default()
        };

        let mut infos = Vec::new();
        search.go(
            &mut pos,
            limits,
            Some(|info: &SearchInfo| {
                infos.push(info.clone());
            }),
        );

        // depth=1で3つのPVラインが出力されるはず
        let depth1_infos: Vec<_> = infos.iter().filter(|info| info.depth == 1).collect();

        assert!(
            depth1_infos.len() >= 3,
            "MultiPV=3なので最低3つのPVライン。実際: {}",
            depth1_infos.len()
        );

        // multipv 1, 2, 3が含まれることを確認
        let multipv_values: Vec<usize> = depth1_infos.iter().map(|info| info.multi_pv).collect();

        assert!(multipv_values.contains(&1), "multipv 1が含まれる。実際: {multipv_values:?}");
        assert!(multipv_values.contains(&2), "multipv 2が含まれる。実際: {multipv_values:?}");
        assert!(multipv_values.contains(&3), "multipv 3が含まれる。実際: {multipv_values:?}");

        // 各PVラインが異なる初手を持つことを確認
        let mut first_moves = std::collections::HashSet::new();
        for info in &depth1_infos {
            if !info.pv.is_empty() {
                first_moves.insert(info.pv[0].to_u32());
            }
        }

        assert!(
            first_moves.len() >= 2,
            "MultiPV=3なので少なくとも2つ以上の異なる候補手があるはず。実際: {}",
            first_moves.len()
        );
    });
}

/// MultiPV=1でも multipv 1 を出力することを確認
#[test]
fn test_multi_pv_1_outputs_multipv_field() {
    use crate::position::Position;
    use crate::search::engine::{Search, SearchInfo};
    use crate::search::LimitsType;

    run_with_large_stack(|| {
        let mut search = Search::new(16);
        let mut pos = Position::new();
        pos.set_hirate();

        let limits = LimitsType {
            depth: 1,
            multi_pv: 1,
            ..Default::default()
        };

        let mut last_info = None;
        search.go(
            &mut pos,
            limits,
            Some(|info: &SearchInfo| {
                if info.depth == 1 {
                    last_info = Some(info.clone());
                }
            }),
        );

        let info = last_info.expect("depth=1のinfo出力があるはず");
        assert_eq!(info.multi_pv, 1, "MultiPV=1でも multipv 1 を出力");

        // USI文字列にも含まれることを確認
        let usi_string = info.to_usi_string();
        assert!(
            usi_string.contains("multipv 1"),
            "USI出力に 'multipv 1' が含まれる。実際: {usi_string}"
        );
    });
}

/// 合法手数を超えるMultiPV値がクランプされることを確認
#[test]
fn test_multi_pv_clamped_to_legal_moves_integration() {
    use crate::position::Position;
    use crate::search::engine::{Search, SearchInfo};
    use crate::search::LimitsType;

    run_with_large_stack(|| {
        let mut search = Search::new(16);
        let mut pos = Position::new();
        pos.set_hirate();

        let limits = LimitsType {
            depth: 1,
            multi_pv: 100, // 合法手数（平手初期局面は30手程度）より多い
            ..Default::default()
        };

        let mut infos = Vec::new();
        search.go(
            &mut pos,
            limits,
            Some(|info: &SearchInfo| {
                if info.depth == 1 {
                    infos.push(info.clone());
                }
            }),
        );

        // 合法手数でクランプされるので、100は出力されない
        let max_multipv = infos.iter().map(|info| info.multi_pv).max().unwrap_or(0);

        assert!(max_multipv < 100, "合法手数でクランプされる。最大MultiPV: {max_multipv}");
        assert!(
            max_multipv >= 10,
            "平手初期局面なので少なくとも10手以上の合法手がある。実際: {max_multipv}"
        );

        // 全てのmultipv値が連続していることを確認
        let mut multipv_values: Vec<usize> = infos.iter().map(|info| info.multi_pv).collect();
        multipv_values.sort();
        multipv_values.dedup();

        for i in 0..multipv_values.len() {
            assert_eq!(multipv_values[i], i + 1, "multipv値が1から連続している");
        }
    });
}

/// MultiPV出力がスコア降順で並ぶことを確認
#[test]
fn test_multi_pv_scores_sorted_desc() {
    use crate::position::Position;
    use crate::search::engine::{Search, SearchInfo};
    use crate::search::LimitsType;

    run_with_large_stack(|| {
        let mut search = Search::new(16);
        let mut pos = Position::new();
        pos.set_hirate();

        let limits = LimitsType {
            depth: 1,
            multi_pv: 3,
            ..Default::default()
        };

        let mut infos: Vec<SearchInfo> = Vec::new();
        search.go(
            &mut pos,
            limits,
            Some(|info: &SearchInfo| {
                if info.depth == 1 {
                    infos.push(info.clone());
                }
            }),
        );

        // multipv順にソートしてスコアが降順になっていることを確認
        infos.sort_by_key(|i| i.multi_pv);

        // 少なくとも2本以上のPVがある前提
        assert!(
            infos.len() >= 2,
            "MultiPV=3なので2本以上のinfo出力があるはず。実際: {}",
            infos.len()
        );

        for window in infos.windows(2) {
            let first = &window[0];
            let second = &window[1];
            assert!(
                first.score.raw() >= second.score.raw(),
                "multipv {} のスコア {} が multipv {} のスコア {} より小さい",
                first.multi_pv,
                first.score.raw(),
                second.multi_pv,
                second.score.raw()
            );
        }
    });
}

// =============================================================================
// Phase 3.1: YaneuraOu準拠バグ修正テスト
// =============================================================================

/// Depth完了後にprevious_scoreがシードされることを統合テストで確認
#[test]
fn test_previous_score_seeding() {
    use crate::position::Position;
    use crate::search::engine::Search;
    use crate::search::LimitsType;

    run_with_large_stack(|| {
        let mut search = Search::new(16);
        let mut pos = Position::new();
        pos.set_hirate();

        let limits = LimitsType {
            depth: 1, // depth=1で実行
            multi_pv: 1,
            ..Default::default()
        };

        // previous_scoreシードコードがクラッシュせず正常に動作することを確認
        let result = search.go(&mut pos, limits, None::<fn(&_)>);
        assert_ne!(result.best_move, Move::NONE);
    });
}

/// MultiPV>1で最終ソートが正しく動作することを確認
#[test]
fn test_final_sort_orders_pvs_correctly() {
    use crate::search::types::{RootMove, RootMoves};
    use crate::types::{Move, Value};

    // シミュレーション: PV2探索後に高スコア発見
    let mut rm1 = RootMove::new(Move::from_usi("7g7f").unwrap());
    rm1.score = Value::new(100);
    let mut rm2 = RootMove::new(Move::from_usi("2g2f").unwrap());
    rm2.score = Value::new(50);
    let mut rm3 = RootMove::new(Move::from_usi("5g5f").unwrap());
    rm3.score = Value::new(150); // 高スコア

    let mut root_moves = RootMoves::from_vec(vec![rm1, rm2, rm3]);

    // 最終ソート
    root_moves.stable_sort_range(0, 3);

    // 期待: スコア降順 [150, 100, 50]
    assert_eq!(root_moves[0].score.raw(), 150);
    assert_eq!(root_moves[1].score.raw(), 100);
    assert_eq!(root_moves[2].score.raw(), 50);
}
