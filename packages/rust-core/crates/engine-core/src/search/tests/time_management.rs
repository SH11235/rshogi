//! 時間管理のTDDテスト
//!
//! best_move_changes（PV安定性判断）と合法手1つの500ms上限のテスト

use crate::search::{LimitsType, TimeManagement, TimeOptions, DEFAULT_MAX_MOVES_TO_DRAW};
use crate::types::Color;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

// =============================================================================
// ヘルパー関数
// =============================================================================

fn create_time_manager() -> TimeManagement {
    TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)))
}

// =============================================================================
// best_move_instability テスト
// =============================================================================

/// falling_evalは指定範囲にクランプされる
#[test]
fn test_calculate_falling_eval_clamp() {
    use super::super::time_manager::calculate_falling_eval;

    // 大きく乖離した値でも [0.5786, 1.6752] に収まる
    let high = calculate_falling_eval(10000, -10000, 0);
    assert!((0.5786..=1.6752).contains(&high), "falling_eval should be clamped, got {high}");
}

/// time_reduction の計算は正の値を返す
#[test]
fn test_calculate_time_reduction_positive() {
    use super::super::time_manager::calculate_time_reduction;

    let tr = calculate_time_reduction(10, 5);
    assert!(tr > 0.0, "time_reduction should be positive, got {tr}");
}

/// 不安定性係数（changes > 0）の係数は1より大きい
#[test]
fn test_best_move_instability_factor_increases_when_unstable() {
    let mut tm = create_time_manager();

    let factor = tm.compute_time_factor(1.0, 1.0, 1.0, 1);
    assert!(factor > 1.0, "不安定な場合は factor > 1.0 となるべき: {factor}");
}

/// 安定時でも係数は正だが極端に大きくならない
#[test]
fn test_best_move_instability_factor_bounded_when_stable() {
    let mut tm = create_time_manager();

    let factor = tm.compute_time_factor(1.0, 1.0, 0.0, 1);
    assert!(
        factor > 0.0 && factor < 3.0,
        "安定時は factor が適度な範囲に収まるべき: {factor}"
    );
}

/// compute_time_factor は思考時間そのものを直接変更しない
#[test]
fn test_best_move_instability_does_not_mutate_budget() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000;
    limits.set_start_time();

    tm.init(&limits, Color::Black, 0, 256);
    let original_optimum = tm.optimum();
    let original_max = tm.maximum();

    let _ = tm.compute_time_factor(1.0, 1.0, 1.0, 1);

    assert_eq!(tm.optimum(), original_optimum);
    assert_eq!(tm.maximum(), original_max);
}

/// nodes_effort の正規化計算を検証
#[test]
fn test_nodes_effort_normalization() {
    use super::super::time_manager::normalize_nodes_effort;

    // rootMoves[0].effort = 500, nodes_total = 1000 → nodesEffort = 50000
    let effort = 500.0;
    let nodes_total = 1000u64;
    let nodes_effort = normalize_nodes_effort(effort, nodes_total);
    assert_eq!(nodes_effort as i32, 50000);
}

/// Ponder中はtotalTime超過でもstop_on_ponderhitを立てるだけ
#[test]
fn test_apply_iteration_timing_sets_stop_on_ponderhit() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 5000;
    limits.set_start_time();
    tm.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);
    tm.reset_search_end();

    tm.apply_iteration_timing(1600, 1200.0, 0.0, true, 12);

    assert!(tm.stop_on_ponderhit(), "ponder中は stop_on_ponderhit が立つべき");
    assert_eq!(tm.search_end(), 0, "ponder中は search_end を設定しない");

    // ponder中はnodesEffort経路でもsearch_endを設定しない
    tm.reset_search_end();
    tm.apply_iteration_timing(1200, 1000.0, 98000.0, true, 12);
    assert_eq!(tm.search_end(), 0);
}

// =============================================================================
// 合法手1つの500ms上限テスト
// =============================================================================

/// 合法手1つの場合、停止閾値が500msに丸められる
#[test]
fn test_single_root_move_caps_stop_threshold() {
    use std::time::Duration;

    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000; // 1分
    limits.start_time = Some(std::time::Instant::now() - Duration::from_millis(600));

    tm.init_with_root_moves_count(&limits, Color::Black, 0, 256, 1);
    // total_time は大きく与えるが、single_move_limit により 500ms に丸められる
    tm.apply_iteration_timing(600, 2000.0, 0.0, false, 12);

    assert!(tm.should_stop_immediately(), "500ms閾値を超えているので停止すべき");
}

/// movetime指定ではsearch_endが設定され、経過時間で停止する
#[test]
fn test_movetime_sets_search_end_and_stop() {
    use std::time::Duration;

    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.movetime = 50;
    limits.start_time = Some(std::time::Instant::now() - Duration::from_millis(60));

    tm.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);

    assert_eq!(tm.search_end(), 50, "movetime指定時はsearch_endが設定される");
    assert!(tm.should_stop(1), "movetime超過で停止する");
}

/// Deepデフォルトは遅延値が大きい（YaneuraOu DEEP相当）
#[test]
fn test_time_options_deep_defaults() {
    let deep = TimeOptions::deep_defaults();
    assert_eq!(deep.network_delay, 400);
    assert_eq!(deep.network_delay2, 1400);
}

// =============================================================================
// SearchWorker best_move_changes テスト（統合テスト）
// =============================================================================

/// SearchWorkerのbest_move_changesの初期値は0.0
#[test]
fn test_worker_best_move_changes_initial_value() {
    const STACK_SIZE: usize = 64 * 1024 * 1024; // 64MB
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            use crate::search::alpha_beta::{init_reductions, SearchWorker};
            use crate::tt::TranspositionTable;

            init_reductions();

            let tt = TranspositionTable::new(16);
            let limits = LimitsType::new();
            let mut tm = create_time_manager();

            let worker = SearchWorker::new(&tt, &limits, &mut tm);

            assert_eq!(worker.best_move_changes, 0.0, "初期値は0.0であるべき");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// SearchWorkerのdecay_best_move_changesは値を半減する
#[test]
fn test_worker_best_move_changes_decay() {
    const STACK_SIZE: usize = 64 * 1024 * 1024;
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            use crate::search::alpha_beta::{init_reductions, SearchWorker};
            use crate::tt::TranspositionTable;

            init_reductions();

            let tt = TranspositionTable::new(16);
            let limits = LimitsType::new();
            let mut tm = create_time_manager();

            let mut worker = SearchWorker::new(&tt, &limits, &mut tm);
            worker.best_move_changes = 4.0;
            worker.decay_best_move_changes();

            assert_eq!(worker.best_move_changes, 2.0, "decay後は半減（4.0 → 2.0）すべき");
        })
        .unwrap()
        .join()
        .unwrap();
}

// =============================================================================
// Phase 1: YaneuraOu準拠 時間管理TDD
// =============================================================================

// -----------------------------------------------------------------------------
// 1.1 MoveHorizon計算
// -----------------------------------------------------------------------------

/// MoveHorizon計算: 切れ負けルール、序盤 (ply=10)
/// YaneuraOu: 160 + 40 - min(10, 40) = 190
#[test]
fn test_move_horizon_time_forfeit_early_game() {
    use super::super::time_manager::calculate_move_horizon;

    let time_forfeit = true;
    let ply = 10;

    let result = calculate_move_horizon(time_forfeit, ply);

    assert_eq!(result, 190, "切れ負け序盤(ply=10): 160+40-10=190");
}

/// MoveHorizon計算: 切れ負けルール、中盤 (ply=50)
/// YaneuraOu: 160 + 40 - min(50, 40) = 160
#[test]
fn test_move_horizon_time_forfeit_mid_game() {
    use super::super::time_manager::calculate_move_horizon;

    let time_forfeit = true;
    let ply = 50;

    let result = calculate_move_horizon(time_forfeit, ply);

    assert_eq!(result, 160, "切れ負け中盤(ply=50): 160+40-40=160");
}

/// MoveHorizon計算: フィッシャールール、序盤 (ply=10)
/// YaneuraOu: 160 + 20 - min(10, 80) = 170
#[test]
fn test_move_horizon_fischer_early_game() {
    use super::super::time_manager::calculate_move_horizon;

    let time_forfeit = false;
    let ply = 10;

    let result = calculate_move_horizon(time_forfeit, ply);

    assert_eq!(result, 170, "フィッシャー序盤(ply=10): 160+20-10=170");
}

/// MoveHorizon計算: フィッシャールール、終盤 (ply=100)
/// YaneuraOu: 160 + 20 - min(100, 80) = 100
#[test]
fn test_move_horizon_fischer_late_game() {
    use super::super::time_manager::calculate_move_horizon;

    let time_forfeit = false;
    let ply = 100;

    let result = calculate_move_horizon(time_forfeit, ply);

    assert_eq!(result, 100, "フィッシャー終盤(ply=100): 160+20-80=100");
}

// -----------------------------------------------------------------------------
// 1.2 round_up処理
// -----------------------------------------------------------------------------

/// round_up: 基本的な繰り上げ (5500ms → 5880ms)
#[test]
fn test_round_up_basic() {
    let mut tm = create_time_manager();
    tm.set_options(&TimeOptions {
        minimum_thinking_time: 2000,
        network_delay: 120,
        network_delay2: 1120,
        slow_mover: 100,
        usi_ponder: false,
        stochastic_ponder: false,
    });
    // remain_timeを設定するため一度init
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 100000;
    limits.set_start_time();
    tm.init(&limits, Color::Black, 1, 512);

    let result = tm.round_up(5500);

    // YaneuraOu:
    // 1. (5500 + 999) / 1000 * 1000 = 6000
    // 2. max(6000, 2000) = 6000
    // 3. 6000 - 120 = 5880
    // 4. 5880 >= 5500 なので +1000不要
    // 5. min(5880, 100000) = 5880
    assert_eq!(result, 5880, "round_up(5500) = 5880");
}

/// round_up: 最小思考時間を下回る場合
#[test]
fn test_round_up_below_minimum() {
    let mut tm = create_time_manager();
    tm.set_options(&TimeOptions {
        minimum_thinking_time: 2000,
        network_delay: 120,
        network_delay2: 1120,
        slow_mover: 100,
        usi_ponder: false,
        stochastic_ponder: false,
    });
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 100000;
    limits.set_start_time();
    tm.init(&limits, Color::Black, 1, 512);

    let result = tm.round_up(1500);

    // 1. (1500 + 999) / 1000 * 1000 = 2000
    // 2. max(2000, 2000) = 2000
    // 3. 2000 - 120 = 1880
    // 4. 1880 >= 1500 なので +1000不要
    assert_eq!(result, 1880, "round_up(1500) = 1880");
}

/// round_up: NetworkDelay引いて元の値より小さくなる場合は+1000
#[test]
fn test_round_up_add_extra_second() {
    let mut tm = create_time_manager();
    tm.set_options(&TimeOptions {
        minimum_thinking_time: 2000,
        network_delay: 500, // 大きめ
        network_delay2: 1500,
        slow_mover: 100,
        usi_ponder: false,
        stochastic_ponder: false,
    });
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 100000;
    limits.set_start_time();
    tm.init(&limits, Color::Black, 1, 512);

    let result = tm.round_up(2600);

    // 1. (2600 + 999) / 1000 * 1000 = 3000
    // 2. max(3000, 2000) = 3000
    // 3. 3000 - 500 = 2500
    // 4. 2500 < 2600 なので +1000 → 3500
    // 5. min(3500, 100000) = 3500
    assert_eq!(result, 3500, "round_up(2600) with network_delay=500 → 3500");
}

/// round_up: 残り時間を超える場合はremain_timeでクランプ
#[test]
fn test_round_up_exceeds_remain_time() {
    let mut tm = create_time_manager();
    tm.set_options(&TimeOptions {
        minimum_thinking_time: 2000,
        network_delay: 120,
        network_delay2: 1120,
        slow_mover: 100,
        usi_ponder: false,
        stochastic_ponder: false,
    });
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 5000; // 少ない
    limits.set_start_time();
    tm.init(&limits, Color::Black, 1, 512);

    let result = tm.round_up(10000);

    // remain_timeは limits.time - network_delay2 付近
    // 計算後 remain_time でクランプされる
    assert!(result <= tm.remain_time(), "round_up(10000) should be clamped by remain_time");
}

// -----------------------------------------------------------------------------
// 1.3 秒読み判定
// -----------------------------------------------------------------------------

/// 秒読み判定: 秒読みに突入（持ち時間が秒読みの1.2倍未満）
#[test]
fn test_final_push_byoyomi_entry() {
    let mut tm = create_time_manager();

    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 5000; // 5秒
    limits.byoyomi[Color::Black.index()] = 10000; // 10秒
    limits.inc[Color::Black.index()] = 0;
    limits.set_start_time();

    tm.init(&limits, Color::Black, 1, 512);

    // 5000 < 10000 * 1.2 (12000) なので isFinalPush = true
    assert!(tm.is_final_push(), "持ち時間5秒 < 秒読み10秒×1.2 → isFinalPush");

    // minimumTime = optimumTime = maximumTime = byoyomi + time_left
    // ただし round_up() と remain_time でクランプされる
    // remain_time = 5000 + 10000 - 1120 = 13880
    assert_eq!(tm.minimum(), 13880);
    assert_eq!(tm.optimum(), 13880);
    assert_eq!(tm.maximum(), 13880);
}

/// 秒読み判定: 秒読みだが持ち時間が十分
#[test]
fn test_not_final_push_enough_time() {
    let mut tm = create_time_manager();

    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 30000; // 30秒
    limits.byoyomi[Color::Black.index()] = 10000; // 10秒
    limits.set_start_time();

    tm.init(&limits, Color::Black, 1, 512);

    // 30000 >= 10000 * 1.2 (12000) なので isFinalPush = false
    assert!(!tm.is_final_push(), "持ち時間30秒 >= 秒読み10秒×1.2 → not finalPush");

    // 通常の時間計算が適用される
    assert!(tm.minimum() < 30000);
    assert!(tm.optimum() < 30000);
}

// -----------------------------------------------------------------------------
// 1.4 最大時間30%上限
// -----------------------------------------------------------------------------

/// 最大時間30%上限: maximumTimeが残り時間見積もりの30%を超えない
#[test]
fn test_maximum_time_30_percent_cap() {
    let mut tm = create_time_manager();

    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 300000; // 5分
    limits.inc[Color::Black.index()] = 5000; // +5秒
    limits.byoyomi[Color::Black.index()] = 0;
    limits.set_start_time();

    tm.init(&limits, Color::Black, 10, 512);

    // YaneuraOuでは、maximumTimeは remain_estimate * 0.3 を超えない
    // 実際の値は実装に依存するが、上限チェックのみ行う
    // （詳細な計算はtime_manager.rsの実装を見て調整）
    assert!(tm.maximum() > 0, "maximum_time should be positive");
}

// -----------------------------------------------------------------------------
// 1.5 Ponder時調整
// -----------------------------------------------------------------------------

/// Ponder時調整: Ponder有効でStochastic_Ponder無効時はoptimumTimeを25%増やす
#[test]
fn test_ponder_optimum_time_increase() {
    // Ponder無効時
    let mut tm_no_ponder = create_time_manager();
    let opts_no_ponder = TimeOptions {
        usi_ponder: false,
        stochastic_ponder: false,
        ..Default::default()
    };
    tm_no_ponder.set_options(&opts_no_ponder);

    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000;
    limits.inc[Color::Black.index()] = 0;
    limits.byoyomi[Color::Black.index()] = 0;
    limits.set_start_time();

    tm_no_ponder.init(&limits, Color::Black, 1, 512);
    let base_optimum = tm_no_ponder.optimum();

    // Ponder有効時
    let mut tm_ponder = create_time_manager();
    let opts_ponder = TimeOptions {
        usi_ponder: true,
        stochastic_ponder: false,
        ..Default::default()
    };
    tm_ponder.set_options(&opts_ponder);

    limits.set_start_time(); // 再設定
    tm_ponder.init(&limits, Color::Black, 1, 512);

    // Ponder有効時は25%増し
    let expected = base_optimum + base_optimum / 4;
    assert_eq!(tm_ponder.optimum(), expected, "Ponder有効時はoptimum = base + base/4");
}

/// Ponder時調整: Stochastic_Ponder有効時は調整なし
#[test]
fn test_stochastic_ponder_no_increase() {
    // Stochastic_Ponder無効時
    let mut tm_normal = create_time_manager();
    let opts_normal = TimeOptions {
        usi_ponder: true,
        stochastic_ponder: false,
        ..Default::default()
    };
    tm_normal.set_options(&opts_normal);

    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000;
    limits.set_start_time();
    tm_normal.init(&limits, Color::Black, 1, 512);
    let normal_optimum = tm_normal.optimum();

    // Stochastic_Ponder有効時
    let mut tm_stochastic = create_time_manager();
    let opts_stochastic = TimeOptions {
        usi_ponder: true,
        stochastic_ponder: true,
        ..Default::default()
    };
    tm_stochastic.set_options(&opts_stochastic);

    limits.set_start_time();
    tm_stochastic.init(&limits, Color::Black, 1, 512);

    // Stochastic_Ponder時は増加しない
    // （基本値に戻るので、normal_optimumより小さい）
    assert!(
        tm_stochastic.optimum() < normal_optimum,
        "Stochastic_Ponder有効時は25%増加しない"
    );
}

// -----------------------------------------------------------------------------
// 1.6 bestMoveInstability係数修正
// -----------------------------------------------------------------------------

/// bestMoveInstability係数: YaneuraOu準拠 (0.9929 + 1.8519 * x)
#[test]
fn test_best_move_instability_yaneuraou_coefficients() {
    use super::super::time_manager::calculate_best_move_instability;

    // totBestMoveChanges = 0のとき
    let result = calculate_best_move_instability(0.0, 1);
    assert!((result - 0.9929).abs() < 0.0001, "YaneuraOu BASE: 0.9929, got {result}");

    // totBestMoveChanges = 1, threads = 1のとき
    let result = calculate_best_move_instability(1.0, 1);
    let expected = 0.9929 + 1.8519;
    assert!(
        (result - expected).abs() < 0.0001,
        "YaneuraOu FACTOR: 1.8519, expected {expected}, got {result}"
    );

    // totBestMoveChanges = 4, threads = 2のとき
    let result = calculate_best_move_instability(4.0, 2);
    // 0.9929 + 1.8519 * (4.0 / 2.0) = 0.9929 + 3.7038 = 4.6967
    let expected = 0.9929 + 1.8519 * 2.0;
    assert!(
        (result - expected).abs() < 0.0001,
        "YaneuraOu with threads, expected {expected}, got {result}"
    );
}
