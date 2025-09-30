//! Time allocation algorithms

use super::{TimeControl, TimeParameters};
use crate::search::GamePhase;
use crate::Color;

/// Calculate time allocation for the current move
pub fn calculate_time_allocation(
    time_control: &TimeControl,
    side: Color,
    ply: u32,
    moves_to_go: Option<u32>,
    game_phase: GamePhase,
    params: &TimeParameters,
) -> (u64, u64) {
    // (soft_limit_ms, hard_limit_ms)
    match time_control {
        TimeControl::Fischer {
            white_ms,
            black_ms,
            increment_ms,
        } => calculate_fischer_time(
            if side == Color::White {
                *white_ms
            } else {
                *black_ms
            },
            *increment_ms,
            ply,
            moves_to_go,
            game_phase,
            params,
        ),

        TimeControl::FixedTime { ms_per_move } => calculate_fixed_time(*ms_per_move, params),

        TimeControl::Byoyomi {
            main_time_ms,
            byoyomi_ms,
            ..
        } => calculate_byoyomi_time(*main_time_ms, *byoyomi_ms, params),

        TimeControl::FixedNodes { .. } => {
            // No time limits for fixed nodes
            (u64::MAX, u64::MAX)
        }

        TimeControl::Infinite => {
            // No time limits for infinite
            (u64::MAX, u64::MAX)
        }

        TimeControl::Ponder(_) => {
            // No time limits during ponder
            // The inner time control will be used after ponderhit
            (u64::MAX, u64::MAX)
        }
    }
}

/// Calculate time for Fischer time control
fn calculate_fischer_time(
    remain_ms: u64,
    increment_ms: u64,
    ply: u32,
    moves_to_go: Option<u32>,
    game_phase: GamePhase,
    params: &TimeParameters,
) -> (u64, u64) {
    // Safety fail: critically low time
    if remain_ms < params.critical_fischer_ms && increment_ms == 0 {
        return (50, 100); // Minimal time to return a move
    }

    let moves_left =
        moves_to_go.unwrap_or_else(|| super::estimate_moves_remaining_by_phase(game_phase, ply));

    // Base allocation: (remaining_time / moves_left) + increment * usage_factor
    // Use integer arithmetic to avoid rounding errors when close to default value (0.8)
    let approx_default = (params.increment_usage - 0.8).abs() < 1e-6;
    let increment_bonus = if approx_default {
        (increment_ms * 8) / 10
    } else {
        ((increment_ms as f64 * params.increment_usage) + 0.5) as u64 // Round to nearest
    };
    let base_ms = (remain_ms / moves_left as u64) + increment_bonus;

    // Apply game phase factor
    // Note: We apply phase_factor first, then soft_multiplier for clarity
    let phase_factor = match game_phase {
        GamePhase::Opening => params.opening_factor,
        GamePhase::MiddleGame => 1.0,
        GamePhase::EndGame => params.endgame_factor,
    };

    let mut soft_ms = ((base_ms as f64 * phase_factor * params.soft_multiplier) + 0.5) as u64;
    let mut hard_ms =
        (((soft_ms as f64 * params.hard_multiplier) + 0.5) as u64).min(remain_ms * 8 / 10);

    // Apply SlowMover (%) to soft budget
    soft_ms = ((soft_ms as u128 * params.slow_mover_pct as u128 + 50) / 100) as u64;

    // Clamp hard to soft * max_time_ratio
    if params.max_time_ratio > 0.0 {
        let max_hard = ((soft_ms as f64 * params.max_time_ratio) + 0.5) as u64;
        if hard_ms > max_hard {
            hard_ms = max_hard;
        }
    }

    // Optional move horizon guard (disabled by default)
    if increment_ms == 0
        && params.move_horizon_trigger_ms > 0
        && remain_ms <= params.move_horizon_trigger_ms
        && params.move_horizon_min_moves > 0
    {
        let guard_share = remain_ms / (params.move_horizon_min_moves as u64);
        if hard_ms > guard_share {
            hard_ms = guard_share.max(soft_ms.saturating_add(50));
        }
    }

    // Apply overhead
    let overhead = params.overhead_ms;
    (soft_ms.saturating_sub(overhead), hard_ms.saturating_sub(overhead))
}

/// Calculate time for fixed time per move
fn calculate_fixed_time(ms_per_move: u64, params: &TimeParameters) -> (u64, u64) {
    // Use integer arithmetic: 90% = 9/10, then apply SlowMover
    let mut soft = (ms_per_move * 9) / 10;
    soft = ((soft as u128 * params.slow_mover_pct as u128 + 50) / 100) as u64;
    // Use minimal overhead for FixedTime to ensure responsiveness
    let overhead = 10u64.min(params.overhead_ms); // Max 10ms overhead for fixed time
    let mut hard = ms_per_move;
    if params.max_time_ratio > 0.0 {
        let max_hard = ((soft as f64 * params.max_time_ratio) + 0.5) as u64;
        if hard > max_hard {
            hard = max_hard;
        }
    }
    (soft.saturating_sub(overhead), hard.saturating_sub(overhead))
}

/// Calculate time for byoyomi
fn calculate_byoyomi_time(
    main_time_ms: u64,
    byoyomi_ms: u64,
    params: &TimeParameters,
) -> (u64, u64) {
    if main_time_ms > 0 {
        // YaneuraOu-style FinalPush detection
        // Activate FinalPush when remaining time < 1.2 * byoyomi period
        let final_push_threshold = (byoyomi_ms as f64 * 1.2) as u64;

        if byoyomi_ms > 0 && main_time_ms < final_push_threshold {
            // FinalPush mode: use all available time (main_time + byoyomi)
            // This follows YaneuraOu's design: minimum = optimum = maximum = time + byoyomi
            let total_available = main_time_ms + byoyomi_ms;

            // Apply minimal safety margins for network delay
            let overhead = params.overhead_ms;
            let network_delay = params.network_delay2_ms;

            // Both soft and hard limits are set to use maximum available time
            // with only essential safety margins
            let target = total_available.saturating_sub(overhead).saturating_sub(network_delay);

            // Ensure we have at least some time to make a move
            let final_time = target.max(params.critical_byoyomi_ms);

            // In FinalPush, soft and hard are nearly equal (small delta for safety)
            (final_time.saturating_sub(50), final_time)
        } else {
            // Normal main time allocation
            // Conservative allocation: 20% soft, 50% hard
            let mut soft = main_time_ms / 5; // 20% = 1/5
            let mut hard = main_time_ms / 2; // 50% = 1/2

            // Apply SlowMover and ratio clamp
            soft = ((soft as u128 * params.slow_mover_pct as u128 + 50) / 100) as u64;
            if params.max_time_ratio > 0.0 {
                let max_hard = ((soft as f64 * params.max_time_ratio) + 0.5) as u64;
                if hard > max_hard {
                    hard = max_hard;
                }
            }
            (soft, hard)
        }
    } else {
        // In byoyomi period
        // Incorporate GUI/IPC delay (network_delay2_ms) into budgeting.
        // hard = byoyomi - overhead - safety - network_delay2
        // soft = (byoyomi * ratio) - overhead - (network_delay2 / 2)
        // Ensure soft <= hard (keep a small margin when needed).
        let overhead = params.overhead_ms;
        let nd2 = params.network_delay2_ms;

        let mut soft = (((byoyomi_ms as f64 * params.byoyomi_soft_ratio) + 0.5) as u64)
            .saturating_sub(overhead)
            .saturating_sub(nd2 / 2);

        // Apply SlowMover to soft in byoyomi as well (keeps behavior consistent)
        soft = ((soft as u128 * params.slow_mover_pct as u128 + 50) / 100) as u64;

        let mut hard = byoyomi_ms
            .saturating_sub(overhead)
            .saturating_sub(params.byoyomi_hard_limit_reduction_ms)
            .saturating_sub(nd2);

        // Clamp hard by ratio if needed
        if params.max_time_ratio > 0.0 {
            let max_hard = ((soft as f64 * params.max_time_ratio) + 0.5) as u64;
            if hard > max_hard {
                hard = max_hard;
            }
        }

        (soft, hard)
    }
}

/// Estimate remaining moves in the game
#[cfg(test)]
pub fn estimate_moves_remaining(ply: u32) -> u32 {
    // For tests, use a simple ply-based estimation
    let moves_played = ply / 2;
    if moves_played < 30 {
        60
    } else if moves_played < 80 {
        40
    } else {
        20
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fischer_allocation() {
        let params = TimeParameters::default();
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            Color::White,
            0,
            None,
            GamePhase::Opening,
            &params,
        );

        // Opening gets 1.2x factor
        assert!(soft > 1000);
        assert!(hard > soft);
        assert!(hard <= 48000); // 80% of 60000
    }

    #[test]
    fn test_critical_time() {
        let params = TimeParameters::default();
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::Fischer {
                white_ms: 200, // Less than critical threshold
                black_ms: 200,
                increment_ms: 0,
            },
            Color::White,
            100,
            None,
            GamePhase::EndGame,
            &params,
        );

        // Should return minimal time
        assert_eq!(soft, 50);
        assert_eq!(hard, 100);
    }

    #[test]
    fn test_fixed_time() {
        let params = TimeParameters::default();
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::FixedTime { ms_per_move: 1000 },
            Color::Black,
            50,
            None,
            GamePhase::MiddleGame,
            &params,
        );

        // FixedTime uses minimal overhead (10ms max)
        let expected_overhead = 10u64.min(params.overhead_ms);
        assert_eq!(soft, 900 - expected_overhead); // 90% - minimal overhead
        assert_eq!(hard, 1000 - expected_overhead);
    }

    #[test]
    fn test_byoyomi_main_time() {
        let params = TimeParameters::default();
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::Byoyomi {
                main_time_ms: 50000, // 50 seconds main time (> 1.2 * 30000 = 36000)
                byoyomi_ms: 30000,   // 30 seconds per period
                periods: 3,
            },
            Color::White,
            0,
            None,
            GamePhase::Opening,
            &params,
        );

        // Conservative allocation during main time (not in FinalPush)
        assert_eq!(soft, 10000); // 20% of 50000
        assert_eq!(hard, 25000); // 50% of 50000
    }

    #[test]
    fn test_byoyomi_period() {
        let params = TimeParameters::default();
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::Byoyomi {
                main_time_ms: 0,   // No main time, already in byoyomi
                byoyomi_ms: 30000, // 30 seconds per period
                periods: 3,
            },
            Color::Black,
            80,
            None,
            GamePhase::EndGame,
            &params,
        );

        // Should use 80% of period as soft limit minus overhead and half of network_delay2
        // 30000 * 0.8 = 24000, minus overhead (50) and half network_delay2 (400) = 23550
        assert_eq!(soft, 23550);
        // Hard limit should subtract overhead (50), byoyomi safety (100), and full network_delay2 (800)
        // 30000 - 50 - 100 - 800 = 29050
        assert_eq!(hard, 29050);
    }

    #[test]
    fn test_integer_arithmetic_precision() {
        let params = TimeParameters::default();

        // Test increment calculation with default 0.8 factor
        let (soft1, _) = calculate_time_allocation(
            &TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            Color::White,
            60,       // Late game
            Some(40), // 40 moves to go
            GamePhase::MiddleGame,
            &params,
        );

        // Verify integer arithmetic produces consistent results
        let (soft2, _) = calculate_time_allocation(
            &TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            Color::White,
            60,
            Some(40),
            GamePhase::MiddleGame,
            &params,
        );

        assert_eq!(soft1, soft2, "Integer arithmetic should be deterministic");
    }

    #[test]
    fn test_fixed_time_slow_mover_and_ratio() {
        let mut params = TimeParameters::default();
        // Slow mover 150%
        params.slow_mover_pct = 150;
        // Ratio clamp 1.05
        params.max_time_ratio = 1.05;

        let (soft, hard) = calculate_time_allocation(
            &TimeControl::FixedTime { ms_per_move: 1000 },
            Color::Black,
            0,
            None,
            GamePhase::MiddleGame,
            &params,
        );

        // soft: 900 * 1.5 - overhead(<=10) = 1350 - 10 = 1340
        assert!((1330..=1340).contains(&soft));
        // hard: clamp to soft*1.05 (rounded), then - overhead
        let max_hard = ((soft as f64 * 1.05) + 0.5) as u64;
        assert!(hard <= max_hard);
    }

    #[test]
    fn test_fischer_move_horizon_guard() {
        let mut params = TimeParameters::default();
        params.move_horizon_trigger_ms = 6000;
        params.move_horizon_min_moves = 10; // guard share = remain/10

        // remain=5000ms, inc=0 → guard 有効
        let (soft, hard) = calculate_fischer_time(5000, 0, 0, None, GamePhase::MiddleGame, &params);
        // guard で hard は guard_share 以下（overhead 差し引きで更に下がる）
        assert!(hard <= 5000 / 10);
        // soft は hard より小さい
        assert!(soft < hard);
    }

    #[test]
    fn test_fixed_time_slowmover_scales_soft() {
        // ms_per_move = 1000, base soft = 900, slowmover 150% => 1350, overhead (min 10) => 1340
        let mut params = TimeParameters::default();
        params.slow_mover_pct = 150;
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::FixedTime { ms_per_move: 1000 },
            Color::Black,
            0,
            None,
            GamePhase::Opening,
            &params,
        );
        assert_eq!(soft, 1340);
        assert_eq!(hard, 990);
    }

    #[test]
    fn test_fixed_time_max_time_ratio_clamps_hard() {
        // With small ratio, hard should be clamped to soft * ratio
        let mut params = TimeParameters::default();
        params.max_time_ratio = 1.1; // 110%
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::FixedTime { ms_per_move: 1000 },
            Color::Black,
            0,
            None,
            GamePhase::Opening,
            &params,
        );
        // soft_out = 900 - 10 = 890; clamp uses pre-overhead soft (900)
        assert_eq!(soft, 890);
        let pre_soft = (1000 * 9) / 10; // 900
        let expected_hard = (((pre_soft as f64) * params.max_time_ratio) + 0.5) as u64 - 10; // subtract overhead
        assert_eq!(hard, expected_hard);
    }

    #[test]
    fn test_fischer_move_horizon_guard_reduces_hard() {
        // Compare with and without move horizon guard in sudden-death (inc=0)
        let base_params = TimeParameters::default();
        // Guard disabled
        let (soft0, hard0) = calculate_time_allocation(
            &TimeControl::Fischer {
                white_ms: 2000,
                black_ms: 2000,
                increment_ms: 0,
            },
            Color::Black,
            40,
            None,
            GamePhase::MiddleGame,
            &base_params,
        );

        // Enable guard with trigger above remain and min_moves positive
        let mut guard_params = base_params;
        guard_params.move_horizon_trigger_ms = 5000;
        guard_params.move_horizon_min_moves = 10;
        let (_soft1, hard1) = calculate_time_allocation(
            &TimeControl::Fischer {
                white_ms: 2000,
                black_ms: 2000,
                increment_ms: 0,
            },
            Color::Black,
            40,
            None,
            GamePhase::MiddleGame,
            &guard_params,
        );

        // Hard with guard should be <= hard without guard
        assert!(hard1 <= hard0, "move horizon should not increase hard: {} <= {}", hard1, hard0);

        // Soft should be unaffected by guard
        let _ = soft0; // suppress unused warning by asserting equal with recomputation
        let (soft1_re, _) = calculate_time_allocation(
            &TimeControl::Fischer {
                white_ms: 2000,
                black_ms: 2000,
                increment_ms: 0,
            },
            Color::Black,
            40,
            None,
            GamePhase::MiddleGame,
            &guard_params,
        );
        assert_eq!(soft1_re, soft0, "guard must not affect soft");
    }

    #[test]
    fn test_byoyomi_final_push_activation() {
        let params = TimeParameters::default();

        // Test 1: FinalPush should activate when main_time < 1.2 * byoyomi
        // byoyomi = 10s, threshold = 12s, main_time = 11s
        let (soft1, hard1) = calculate_time_allocation(
            &TimeControl::Byoyomi {
                main_time_ms: 11000, // 11 seconds (< 12s threshold)
                byoyomi_ms: 10000,   // 10 seconds
                periods: 3,
            },
            Color::Black,
            50,
            None,
            GamePhase::MiddleGame,
            &params,
        );

        // In FinalPush, we should use nearly all available time (11s + 10s = 21s)
        // minus overhead and network delay
        let expected_total = 21000 - params.overhead_ms - params.network_delay2_ms;
        let expected_soft = expected_total.saturating_sub(50);
        let expected_hard = expected_total;

        // Allow small tolerance for safety margins
        assert!(
            soft1 >= expected_soft - 100 && soft1 <= expected_soft + 100,
            "FinalPush soft limit should be close to {}, got {}",
            expected_soft,
            soft1
        );
        assert!(
            hard1 >= expected_hard - 100 && hard1 <= expected_hard + 100,
            "FinalPush hard limit should be close to {}, got {}",
            expected_hard,
            hard1
        );

        // Test 2: FinalPush should NOT activate when main_time >= 1.2 * byoyomi
        // byoyomi = 10s, threshold = 12s, main_time = 15s
        let (soft2, hard2) = calculate_time_allocation(
            &TimeControl::Byoyomi {
                main_time_ms: 15000, // 15 seconds (> 12s threshold)
                byoyomi_ms: 10000,   // 10 seconds
                periods: 3,
            },
            Color::Black,
            50,
            None,
            GamePhase::MiddleGame,
            &params,
        );

        // Should use normal conservative allocation
        assert_eq!(soft2, 3000); // 20% of 15000
        assert_eq!(hard2, 7500); // 50% of 15000
    }

    #[test]
    fn test_byoyomi_final_push_already_in_byoyomi() {
        let params = TimeParameters::default();

        // When already in byoyomi (main_time = 0), should use byoyomi period
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::Byoyomi {
                main_time_ms: 0,   // Already in byoyomi
                byoyomi_ms: 10000, // 10 seconds
                periods: 2,
            },
            Color::White,
            100,
            None,
            GamePhase::EndGame,
            &params,
        );

        // Should use byoyomi allocation with safety margins
        let expected_soft = (10000.0 * params.byoyomi_soft_ratio) as u64
            - params.overhead_ms
            - params.network_delay2_ms / 2;
        let expected_hard = 10000
            - params.overhead_ms
            - params.byoyomi_hard_limit_reduction_ms
            - params.network_delay2_ms;

        assert_eq!(soft, expected_soft);
        assert_eq!(hard, expected_hard);
    }
}
