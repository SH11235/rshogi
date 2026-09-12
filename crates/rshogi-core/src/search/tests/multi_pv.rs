//! MultiPV（候補手複数探索）のテスト

use crate::search::SearchTuneParams;
use crate::search::engine::compute_aspiration_window;
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

// =============================================================================
// Phase 2.2: MultiPVループのソート
// =============================================================================

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

// =============================================================================
// Phase 3: 統合テスト（MultiPVループの実動作確認）
// =============================================================================

/// MultiPV=3で3つのPVライン出力
#[test]
fn test_multi_pv_3_integration() {
    // NNUE 未ロードでも探索できるよう material 評価を有効化 (guard が終了時に復元)
    let guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);

    use crate::position::Position;
    use crate::search::LimitsType;
    use crate::search::engine::{Search, SearchInfo};

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

    drop(guard);
}

/// MultiPV=1でも multipv 1 を出力することを確認
#[test]
fn test_multi_pv_1_outputs_multipv_field() {
    // NNUE 未ロードでも探索できるよう material 評価を有効化 (guard が終了時に復元)
    let guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);

    use crate::position::Position;
    use crate::search::LimitsType;
    use crate::search::engine::{Search, SearchInfo};

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

    drop(guard);
}

/// 合法手数を超えるMultiPV値がクランプされることを確認
#[test]
fn test_multi_pv_clamped_to_legal_moves_integration() {
    // NNUE 未ロードでも探索できるよう material 評価を有効化 (guard が終了時に復元)
    let guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);

    use crate::position::Position;
    use crate::search::LimitsType;
    use crate::search::engine::{Search, SearchInfo};

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

        for (i, &value) in multipv_values.iter().enumerate() {
            assert_eq!(value, i + 1, "multipv値が1から連続している");
        }
    });

    drop(guard);
}

/// MultiPV出力がスコア降順で並ぶことを確認
#[test]
fn test_multi_pv_scores_sorted_desc() {
    // NNUE 未ロードでも探索できるよう material 評価を有効化 (guard が終了時に復元)
    let guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);

    use crate::position::Position;
    use crate::search::LimitsType;
    use crate::search::engine::{Search, SearchInfo};

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

    drop(guard);
}

/// aspiration window が平均・二乗平均スコアを使うことを確認
#[test]
fn test_aspiration_window_uses_average_and_mean_squared() {
    let mut rm = RootMove::new(Move::from_usi("7g7f").unwrap());
    rm.average_score = Value::new(120);
    rm.mean_squared_score = Some(11131 * 10); // abs(111310) / 9000 = 12, delta=5+0+12=17

    let (alpha, beta, delta) = compute_aspiration_window(&rm, 0, &SearchTuneParams::default());
    assert_eq!(delta.raw(), 17);
    assert_eq!(alpha.raw(), 103);
    assert_eq!(beta.raw(), 137);
}

/// 未シード時はフルウィンドウになる
#[test]
fn test_aspiration_window_defaults_to_full_window_when_unseeded() {
    let rm = RootMove::new(Move::from_usi("7g7f").unwrap());
    let (alpha, beta, _) = compute_aspiration_window(&rm, 0, &SearchTuneParams::default());

    assert_eq!(alpha.raw(), -Value::INFINITE.raw());
    assert_eq!(beta, Value::INFINITE);
}

/// depthごとにMultiPV本数分のinfoが出て、最後に採択ラインが再出力される
#[test]
fn test_multi_pv_outputs_once_per_depth() {
    // NNUE 未ロードでも探索できるよう material 評価を有効化 (guard が終了時に復元)
    let guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);

    use crate::position::Position;
    use crate::search::LimitsType;
    use crate::search::engine::{Search, SearchInfo};

    run_with_large_stack(|| {
        let mut search = Search::new(16);
        let mut pos = Position::new();
        pos.set_hirate();

        let limits = LimitsType {
            depth: 1,
            multi_pv: 2,
            ..Default::default()
        };
        let expected_multipv = limits.multi_pv;

        let mut depth1_infos = Vec::new();
        search.go(
            &mut pos,
            limits,
            Some(|info: &SearchInfo| {
                if info.depth == 1 {
                    depth1_infos.push(info.clone());
                }
            }),
        );

        assert_eq!(
            depth1_infos.len(),
            expected_multipv + 1,
            "depthごとのMultiPV出力に加えて採択ラインを最後に再出力する"
        );

        let multipv: Vec<_> = depth1_infos.iter().map(|info| info.multi_pv).collect();
        assert_eq!(multipv, vec![1, 2, 1], "最後は採択ラインのmultipv 1を再出力する");
    });

    drop(guard);
}

// =============================================================================
// Phase 3.1: YaneuraOu準拠バグ修正テスト
// =============================================================================
