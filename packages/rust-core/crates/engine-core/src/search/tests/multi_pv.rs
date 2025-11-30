//! MultiPV（候補手複数探索）のテスト
//!
//! YaneuraOu準拠のMultiPV実装の単体テスト

use crate::search::types::RootMove;
use crate::types::{Move, Value};

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
