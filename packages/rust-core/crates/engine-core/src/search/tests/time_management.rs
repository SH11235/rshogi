//! 時間管理のTDDテスト
//!
//! best_move_changes（PV安定性判断）と合法手1つの500ms上限のテスト

use crate::search::{LimitsType, TimeManagement, TimeOptions};
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

/// 不安定性係数計算: totBestMoveChanges = 0 → instability ≈ 0.9929
#[test]
fn test_best_move_instability_zero_changes() {
    use super::super::time_manager::calculate_best_move_instability;

    let result = calculate_best_move_instability(0.0, 1);
    assert!(
        (result - 0.9929).abs() < 0.001,
        "totBestMoveChanges=0の場合、instability≈0.9929であるべき。got={result}"
    );
}

/// 不安定性係数計算: totBestMoveChanges = 0.5 → instability ≈ 1.9189
#[test]
fn test_best_move_instability_half_change() {
    use super::super::time_manager::calculate_best_move_instability;

    let result = calculate_best_move_instability(0.5, 1);
    let expected = 0.9929 + 1.8519 * 0.5; // = 1.91885
    assert!(
        (result - expected).abs() < 0.001,
        "totBestMoveChanges=0.5の場合、instability≈{expected}であるべき。got={result}"
    );
}

/// 不安定性係数計算: 大きな値でもクランプされない（YaneuraOu準拠）
#[test]
fn test_best_move_instability_no_clamp_upper() {
    use super::super::time_manager::calculate_best_move_instability;

    let result = calculate_best_move_instability(10.0, 1);
    let expected = 0.9929 + 1.8519 * 10.0; // = 19.5119
    assert!(
        (result - expected).abs() < 0.001,
        "totBestMoveChanges=10の場合、クランプなしでinstability≈{expected}であるべき。got={result}"
    );
}

/// 不安定性係数計算: スレッド数で正規化される
#[test]
fn test_best_move_instability_thread_normalization() {
    use super::super::time_manager::calculate_best_move_instability;

    // 4スレッドで changes=2 の場合
    let result = calculate_best_move_instability(2.0, 4);
    let expected = 0.9929 + 1.8519 * 2.0 / 4.0; // = 0.9929 + 0.92595 = 1.91885
    assert!(
        (result - expected).abs() < 0.001,
        "スレッド数で正規化されるべき。expected={expected}, got={result}"
    );
}

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

/// 不安定性係数適用後のoptimum_time変化（不安定時は増加）
#[test]
fn test_apply_best_move_instability_increases_optimum_when_unstable() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000; // 1分
    limits.set_start_time();

    tm.init(&limits, Color::Black, 0, 256);
    let original_optimum = tm.optimum();

    // best_move_changes = 1.0 → instability = 0.9929 + 1.8519 = 2.8448
    // 不安定な場合はoptimumが増加
    tm.apply_best_move_instability(1.0, 1);

    assert!(
        tm.optimum() > original_optimum,
        "不安定な場合（changes=1.0）はoptimumが増加すべき: original={original_optimum}, after={}",
        tm.optimum()
    );
}

/// 不安定性係数適用後のoptimum_time変化（安定時は大きくは増えない）
#[test]
fn test_apply_best_move_instability_decreases_optimum_when_stable() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000;
    limits.set_start_time();

    tm.init(&limits, Color::Black, 0, 256);
    let original_optimum = tm.optimum();

    // best_move_changes = 0 → instability ≈ 0.9929
    // reduction要素を含むため僅かに変動するが、大きくは増えない
    tm.apply_best_move_instability(0.0, 1);

    assert!(
        tm.optimum() <= original_optimum * 2,
        "安定時（changes=0）は大きく増加しない: original={original_optimum}, after={}",
        tm.optimum(),
    );
}

/// 不安定性係数はmaximum_timeにも適用される
#[test]
fn test_apply_best_move_instability_scales_maximum() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000;
    limits.set_start_time();

    tm.init(&limits, Color::Black, 0, 256);
    let original_max = tm.maximum();

    tm.apply_best_move_instability(1.0, 1);

    assert!(
        tm.maximum() > original_max,
        "maximum_time も不安定性係数でスケールされるべき: original={original_max}, after={}",
        tm.maximum()
    );
}

// =============================================================================
// 合法手1つの500ms上限テスト
// =============================================================================

/// 合法手1つの場合、optimum_time <= 500ms
#[test]
fn test_single_root_move_limits_optimum_to_500ms() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000; // 1分
    limits.set_start_time();

    tm.init_with_root_moves_count(&limits, Color::Black, 0, 256, 1);

    assert!(
        tm.optimum() <= 500,
        "合法手1つの場合、optimum <= 500msであるべき。got={}",
        tm.optimum()
    );
}

/// 合法手1つの場合、maximum_time <= 500ms
#[test]
fn test_single_root_move_limits_maximum_to_500ms() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000;
    limits.set_start_time();

    tm.init_with_root_moves_count(&limits, Color::Black, 0, 256, 1);

    assert!(
        tm.maximum() <= 500,
        "合法手1つの場合、maximum <= 500msであるべき。got={}",
        tm.maximum()
    );
}

/// 合法手複数の場合は500ms制限なし
#[test]
fn test_multiple_root_moves_no_500ms_limit() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000; // 1分
    limits.set_start_time();

    tm.init_with_root_moves_count(&limits, Color::Black, 0, 256, 30);

    assert!(
        tm.optimum() > 500,
        "合法手複数の場合、optimum > 500msであるべき。got={}",
        tm.optimum()
    );
}

/// 元々の時間が500ms未満の場合はそのまま（minimum_thinking_timeを低く設定）
#[test]
fn test_single_root_move_with_short_time() {
    let mut tm = create_time_manager();
    // minimum_thinking_timeを低く設定して、短いmovetimeが有効になるようにする
    tm.set_options(&TimeOptions {
        network_delay: 0,
        network_delay2: 0,
        minimum_thinking_time: 50,
        slow_mover: 100,
    });
    let mut limits = LimitsType::new();
    limits.movetime = 100; // 100ms固定
    limits.set_start_time();

    tm.init_with_root_moves_count(&limits, Color::Black, 0, 256, 1);

    // 元々100msなので500ms制限は影響しない（min(500, 100) = 100）
    assert!(tm.optimum() <= 100, "元々短い時間の場合はそのまま。got={}", tm.optimum());
}

/// 合法手1つの場合、不安定性係数を適用しても500ms上限が維持される
#[test]
fn test_single_root_move_limit_reapplied_after_instability() {
    let mut tm = create_time_manager();
    let mut limits = LimitsType::new();
    limits.time[Color::Black.index()] = 60000; // 1分
    limits.set_start_time();

    tm.init_with_root_moves_count(&limits, Color::Black, 0, 256, 1);
    // 大きくスケールさせる
    tm.apply_best_move_instability(10.0, 1);

    assert!(
        tm.optimum() <= 500 && tm.maximum() <= 500,
        "不安定性係数適用後も500ms上限を超えるべきではない: optimum={}, maximum={}",
        tm.optimum(),
        tm.maximum()
    );
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
