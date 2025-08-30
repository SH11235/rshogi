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

