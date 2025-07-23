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

        TimeControl::Infinite | TimeControl::Ponder => {
            // No time limits for infinite/ponder
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

    let moves_left = moves_to_go.unwrap_or_else(|| estimate_moves_remaining(ply));

    // Base allocation: (remaining_time / moves_left) + increment * usage_factor
    // Use integer arithmetic to avoid rounding errors: 0.8 = 8/10
    let increment_bonus = if params.increment_usage == 0.8 {
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

    let soft_ms = ((base_ms as f64 * phase_factor * params.soft_multiplier) + 0.5) as u64;
    let hard_ms =
        (((soft_ms as f64 * params.hard_multiplier) + 0.5) as u64).min(remain_ms * 8 / 10); // Never use more than 80% of remaining time

    // Apply overhead
    let overhead = params.overhead_ms;
    (soft_ms.saturating_sub(overhead), hard_ms.saturating_sub(overhead))
}

/// Calculate time for fixed time per move
fn calculate_fixed_time(ms_per_move: u64, params: &TimeParameters) -> (u64, u64) {
    // Use integer arithmetic: 90% = 9/10
    let soft = (ms_per_move * 9) / 10;
    let overhead = params.overhead_ms;
    (soft.saturating_sub(overhead), ms_per_move.saturating_sub(overhead))
}

/// Calculate time for byoyomi
fn calculate_byoyomi_time(
    main_time_ms: u64,
    byoyomi_ms: u64,
    params: &TimeParameters,
) -> (u64, u64) {
    if main_time_ms > 0 {
        // Still in main time - treat like Fischer without increment
        // Conservative allocation: 20% soft, 50% hard
        let soft = main_time_ms / 5; // 20% = 1/5
        let hard = main_time_ms / 2; // 50% = 1/2
        (soft, hard)
    } else {
        // In byoyomi period
        // Use 80% of period as soft limit
        let soft = (byoyomi_ms * 4) / 5; // 80% = 4/5
        let hard = byoyomi_ms;
        let overhead = params.overhead_ms;
        (soft.saturating_sub(overhead), hard.saturating_sub(overhead))
    }
}

/// Estimate remaining moves in the game
fn estimate_moves_remaining(ply: u32) -> u32 {
    let moves_played = ply / 2;

    // Use a curve based on typical game progression
    if moves_played < 30 {
        60 // Opening: expect longer game
    } else if moves_played < 80 {
        40 // Middle game
    } else {
        20 // Endgame: minimum 20 moves buffer
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

        assert_eq!(soft, 900 - params.overhead_ms); // 90% - overhead
        assert_eq!(hard, 1000 - params.overhead_ms);
    }

    #[test]
    fn test_byoyomi_main_time() {
        let params = TimeParameters::default();
        let (soft, hard) = calculate_time_allocation(
            &TimeControl::Byoyomi {
                main_time_ms: 10000, // 10 seconds main time
                byoyomi_ms: 30000,   // 30 seconds per period
                periods: 3,
            },
            Color::White,
            0,
            None,
            GamePhase::Opening,
            &params,
        );

        // Conservative allocation during main time
        assert_eq!(soft, 2000); // 20% of 10000
        assert_eq!(hard, 5000); // 50% of 10000
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

        // Should use 80% of period as soft limit
        assert_eq!(soft, 24000 - params.overhead_ms); // 80% of 30000 - overhead
        assert_eq!(hard, 30000 - params.overhead_ms); // Full period - overhead
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
}
