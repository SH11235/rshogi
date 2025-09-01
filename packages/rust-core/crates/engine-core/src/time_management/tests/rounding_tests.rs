use crate::shogi::Color;
use crate::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};

#[test]
fn test_set_search_end_rounds_to_next_second_minus_overhead() {
    // FixedTime with sufficiently large hard limit to avoid near-hard cap
    let limits = TimeLimits {
        time_control: TimeControl::FixedTime {
            ms_per_move: 30_000,
        },
        ..Default::default()
    };
    let phase = GamePhase::Opening;
    let tm = TimeManager::new(&limits, Color::Black, 0, phase);

    // Elapsed 1450ms -> next second 2000ms -> 2000-50(overhead)=1950
    tm.set_search_end(1450);
    let scheduled = tm.scheduled_end_ms();
    assert_eq!(scheduled, 1950, "scheduled_end should be 1950, got {scheduled}");

    // Elapsed 980ms -> 1000-50=950 <= elapsed, so fallback to elapsed+1000=1980
    // set_search_end only tightens (keeps smaller value), so it should remain 1950
    tm.set_search_end(980);
    let scheduled2 = tm.scheduled_end_ms();
    assert_eq!(scheduled2, 1950, "scheduled_end should not expand (tighten-only)");
}

#[test]
fn test_set_search_end_clamped_by_hard_limit_small_budget() {
    // Small FixedTime ensures near-hard safety is disabled (hard < 200ms)
    let limits = TimeLimits {
        time_control: TimeControl::FixedTime { ms_per_move: 170 },
        ..Default::default()
    };
    let phase = GamePhase::Opening;
    let tm = TimeManager::new(&limits, Color::White, 0, phase);

    // For FixedTime, hard ~= ms_per_move - 10 (minimal overhead)
    let hard = tm.hard_limit_ms();
    assert!(hard < 200, "hard should be <200 to avoid near-hard safety, got {hard}");

    // Phase 4: With hard < 200ms, safety margin is min(network_delay2, 100) = 100ms
    // So the cap is hard - 100 = 160 - 100 = 60ms
    // Even though remain_upper = 120ms, the safety margin takes precedence
    tm.set_search_end(155);
    let scheduled = tm.scheduled_end_ms();

    // Phase 4: Expected to be clamped by safety margin, not remain_upper
    let expected = hard.saturating_sub(100); // safety margin for hard < 500ms
    assert_eq!(
        scheduled, expected,
        "scheduled_end should be clamped to {expected} (hard - safety), got {scheduled}"
    );
}

#[test]
fn test_set_search_end_respects_remain_upper_in_byoyomi() {
    // Byoyomi (already in byoyomi): main=0, period=3000ms
    let limits = TimeLimits {
        time_control: TimeControl::Byoyomi {
            main_time_ms: 0,
            byoyomi_ms: 3000,
            periods: 1,
        },
        ..Default::default()
    };
    let phase = GamePhase::MiddleGame;
    let tm = TimeManager::new(&limits, Color::Black, 40, phase);

    // Remain-upper bound: byoyomi - nd2(800) - overhead(50) = 2150
    // Hard is conservative: byoyomi - overhead(50) - safety(100) - nd2(800) = 2050
    let hard = tm.hard_limit_ms();
    assert!(hard > 0 && hard <= 2050);

    // Request scheduling very early; final schedule should be <= remain_upper (<= 2150) and <= hard
    tm.set_search_end(100);
    let scheduled = tm.scheduled_end_ms();
    assert!(
        scheduled <= 2150,
        "scheduled_end should not exceed remain_upper (2150), got {scheduled}"
    );
    assert!(
        scheduled <= hard,
        "scheduled_end should not exceed hard ({hard}), got {scheduled}"
    );

    // Tighten-only: a later call with larger elapsed must not increase the scheduled_end
    tm.set_search_end(500);
    let scheduled2 = tm.scheduled_end_ms();
    assert!(
        scheduled2 <= scheduled,
        "scheduled_end must only tighten, prev={scheduled}, now={scheduled2}"
    );
}
