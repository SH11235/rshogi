//! 時間管理のTDDテスト
//!
//! best_move_changes（PV安定性判断）と合法手1つの500ms上限のテスト

use crate::search::{
    DEFAULT_MAX_MOVES_TO_DRAW, LimitsType, SearchTuneParams, TimeManagement, TimeOptions,
};
use crate::time::Instant;
use crate::types::Color;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

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
    limits.ponder = true;
    limits.set_start_time();
    tm.init(&limits, Color::Black, 0, DEFAULT_MAX_MOVES_TO_DRAW);
    tm.reset_search_end();

    tm.apply_iteration_timing(1600, 1200.0, 0.0, 12);

    assert!(tm.stop_on_ponderhit(), "ponder中は stop_on_ponderhit が立つべき");
    assert_eq!(tm.search_end(), 0, "ponder中は search_end を設定しない");

    // ponder中はnodesEffort経路でもsearch_endを設定しない
    tm.reset_search_end();
    tm.apply_iteration_timing(1200, 1000.0, 98000.0, 12);
    assert_eq!(tm.search_end(), 0);
}

// =============================================================================
// 合法手1つの500ms上限テスト
// =============================================================================

/// 合法手1つの場合、停止閾値が500msに丸められる
#[test]
fn test_single_root_move_caps_stop_threshold() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000; // 1分
    limits.start_time = Some(Instant::now() - Duration::from_millis(600));

    tm.init_with_root_moves_count(&limits, Color::Black, 0, 256, 1);
    // total_time は大きく与えるが、single_move_limit により 500ms に丸められる
    tm.apply_iteration_timing(600, 2000.0, 0.0, 12);

    assert!(tm.should_stop_immediately(), "500ms閾値を超えているので停止すべき");
}

/// movetime指定ではsearch_endが設定され、経過時間で停止する
#[test]
fn test_movetime_sets_search_end_and_stop() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.movetime = 50;
    limits.start_time = Some(Instant::now() - Duration::from_millis(60));

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

/// SearchWorkerのdecay_best_move_changesは値を半減する
#[test]
fn test_worker_best_move_changes_decay() {
    const STACK_SIZE: usize = 64 * 1024 * 1024;
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(|| {
            use crate::eval::EvalHash;
            use crate::search::alpha_beta::SearchWorker;
            use crate::tt::TranspositionTable;
            use std::sync::Arc;

            let tt = Arc::new(TranspositionTable::new(16));
            let eval_hash = Arc::new(EvalHash::new(1));

            let mut worker = SearchWorker::new(
                tt,
                eval_hash,
                DEFAULT_MAX_MOVES_TO_DRAW,
                0,
                SearchTuneParams::default(),
            );
            assert_eq!(worker.state.best_move_changes, 0.0);
            worker.state.best_move_changes = 4.0;
            worker.decay_best_move_changes();

            assert_eq!(worker.state.best_move_changes, 2.0, "decay後は半減（4.0 → 2.0）すべき");
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

// -----------------------------------------------------------------------------
// 1.2 round_up処理
// -----------------------------------------------------------------------------

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
    // 秒読みモードでは network_delay (120) を使用:
    // remain_time = 5000 + 10000 - 120 = 14880
    assert_eq!(tm.minimum(), 14880);
    assert_eq!(tm.optimum(), 14880);
    assert_eq!(tm.maximum(), 14880);
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

// -----------------------------------------------------------------------------
// 1.5 Ponder時調整
// -----------------------------------------------------------------------------

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

#[test]
fn move_horizon_modes() {
    use super::super::time_manager::calculate_move_horizon;
    for (forfeit, ply, expected) in [
        (true, 10, 190),
        (true, 50, 160),
        (false, 10, 170),
        (false, 100, 100),
    ] {
        assert_eq!(calculate_move_horizon(forfeit, ply), expected);
    }
}

#[test]
fn round_up_boundaries_and_remaining_budget() {
    let mut tm = create_time_manager();
    for (minimum, delay, remaining, requested, expected) in [
        (2000, 120, 100000, 5500, Some(5880)),
        (2000, 120, 100000, 1500, Some(1880)),
        (2000, 120, 100000, 1, Some(1880)),
        (1000, 120, 100000, 1, Some(880)),
        (2000, 500, 100000, 2600, Some(3500)),
        (2000, 120, 5000, 10000, None),
    ] {
        tm.set_options(&TimeOptions {
            minimum_thinking_time: minimum,
            network_delay: delay,
            network_delay2: delay + 1000,
            slow_mover: 100,
            usi_ponder: false,
            stochastic_ponder: false,
        });
        let mut limits = LimitsType::new();
        limits.time[Color::Black.index()] = remaining;
        limits.set_start_time();
        tm.init(&limits, Color::Black, 1, 512);
        let actual = tm.round_up(requested);
        if let Some(expected) = expected {
            assert_eq!(actual, expected);
        }
        assert!(actual <= tm.remain_time());
    }
}
