use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
use crate::Color;

#[test]
fn test_advise_after_iteration_schedules_end() {
    // FixedTime 1000ms -> soft ~= 900 - overhead(<=10) = 890, hard ~= 990
    let limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 1000 },
        ..Default::default()
    };

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);
    let hard = tm.hard_limit_ms();
    assert!(hard > 0 && hard < 1000);

    // Advise after exceeding opt (soft) to schedule a rounded stop before hard
    // We pass a synthetic elapsed that is safely >= soft
    let soft = tm.soft_limit_ms();
    tm.advise_after_iteration(soft.saturating_add(10));

    let scheduled = tm.scheduled_end_ms();
    assert_ne!(scheduled, u64::MAX, "scheduled_end must be set");
    assert!(scheduled <= hard, "scheduled_end should not exceed hard limit");
}

#[test]
fn test_remain_upper_clamps_schedule() {
    // FixedTime 100ms, overhead=0 → remain_upper ≈ 100ms
    // elapsed=95ms → round_up would aim ~1000ms, must be clamped to <=100ms
    let mut limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 100 },
        ..Default::default()
    };
    let params = crate::time_management::TimeParametersBuilder::new()
        .overhead_ms(0)
        .unwrap()
        .build();
    limits.time_parameters = Some(params);

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::MiddleGame);
    // Trigger schedule (elapsed >= opt). For FixedTime soft≈90ms（overhead<=10）
    tm.advise_after_iteration(95);
    let scheduled = tm.scheduled_end_ms();
    assert!(scheduled <= 100, "scheduled_end must be clamped to remain_upper, got {scheduled}");
}

#[test]
fn test_final_push_in_byoyomi_enforces_min_vs_hard() {
    // Byoyomi: main=0, period=2000ms, ND2=300ms, Overhead=100ms
    // Final push min = 2000 - 300 - 100 = 1600ms
    // Hard (approx) = 2000 - 100 - 500 (safety) - 300 = 1100ms
    // 結果として min > hard のケース。予約停止は hard に抑えられることを確認。
    let mut limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 2000,
            periods: 1,
        },
        ..Default::default()
    };
    // カスタムパラメータ
    let params = crate::time_management::TimeParametersBuilder::new()
        .overhead_ms(100)
        .unwrap()
        .network_delay2_ms(300)
        .unwrap()
        .byoyomi_safety_ms(500)
        .unwrap()
        .build();
    limits.time_parameters = Some(params);

    let tm = TimeManager::new(&limits, Color::White, 0, GamePhase::EndGame);

    let soft = tm.soft_limit_ms();
    let hard = tm.hard_limit_ms();
    assert!(hard > 0);

    // opt は hard 以下に正規化されているはず（FinalPush適用時にハードで抑制）
    let opt = tm.opt_limit_ms();
    assert!(opt <= hard);

    // 反復終了（opt 以上の経過）で予約を入れる
    tm.advise_after_iteration(hard);

    let scheduled = tm.scheduled_end_ms();
    assert_ne!(scheduled, u64::MAX);
    // min(1600) は満たせないため、予約停止はハード上限（付近）に抑制される
    assert!(scheduled <= hard);
}
