//! Byoyomi-specific rounding and scheduling tests

use crate::time_management::test_utils::mock_set_time;
use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
use crate::Color;

#[test]
fn test_pure_byoyomi_schedules_before_hard_with_margin() {
    // 純秒読み（main=0, byoyomi=10s）で、opt_limit 超過時に丸め停止が計画され、
    // かつ hard - safety_margin を超えないことを確認する。
    mock_set_time(0);

    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 10_000,
            periods: 1,
        },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 0, GamePhase::MiddleGame);

    // opt_limit 到達直後に丸め停止をスケジュールさせる
    let opt = tm.opt_limit_ms();
    let hard = tm.hard_limit_ms();

    // opt を少し超過
    mock_set_time(opt.saturating_add(50));
    // 一度チェックしてスケジュールを立てる（この時点では停止しない設計）
    let stop_now = tm.should_stop(1_000);
    assert!(!stop_now, "opt_limit 超過時点では即停止しない");

    let scheduled = tm.scheduled_end_ms();
    assert_ne!(scheduled, u64::MAX, "丸め停止が計画されているはず");

    // セーフティマージン適用後の上限以下であること
    // safety は hard_limit に応じて段階適用される（>=5000ms なら nd2）
    let safety = tm.network_delay2_ms().min(if hard >= 5000 {
        tm.network_delay2_ms()
    } else if hard >= 1000 {
        500
    } else if hard >= 500 {
        200
    } else {
        100
    });
    assert!(scheduled <= hard.saturating_sub(safety));

    // 計画時刻に到達したら停止すること
    mock_set_time(scheduled);
    assert!(tm.should_stop(2_000));
}

#[test]
fn test_pure_byoyomi_respects_min_think_under_latency() {
    mock_set_time(0);

    let params = crate::time_management::TimeParameters {
        network_delay2_ms: 1_200,
        min_think_ms: 300,
        critical_byoyomi_ms: 150,
        byoyomi_hard_limit_reduction_ms: 100,
        ..Default::default()
    };

    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 1_000,
            periods: 2,
        },
        time_parameters: Some(params),
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::Black, 120, GamePhase::EndGame);

    assert!(tm.soft_limit_ms() >= params.min_think_ms);
    assert!(tm.hard_limit_ms() >= tm.soft_limit_ms());
    assert!(tm.hard_limit_ms() <= 1_000);
}

#[test]
fn test_fixed_nodes_time_manager_respects_node_limit() {
    // FixedNodes 指定時は時間ではなくノード数で停止する
    mock_set_time(0);
    let limits = TimeLimits {
        time_control: TimeControl::FixedNodes { nodes: 10_000 },
        ..Default::default()
    };
    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::Opening);

    // ノード未達では停止しない
    assert!(!tm.should_stop(9_999));
    // ちょうど到達で停止
    assert!(tm.should_stop(10_000));
}
