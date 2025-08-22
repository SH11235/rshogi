//! Tests for game_phase module integration with TimeManager

#[cfg(test)]
mod tests {
    use crate::time_management::{
        detect_game_phase_for_time, GamePhase, TimeControl, TimeLimits, TimeManager,
    };
    use crate::usi::parse_sfen;
    use crate::Color;

    #[test]
    fn test_time_manager_uses_position_based_phase() {
        // Start position - should be Opening
        let start_pos =
            parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
        let phase = detect_game_phase_for_time(&start_pos, 0);
        assert_eq!(phase, GamePhase::Opening);

        // Create TimeManager with detected phase
        let limits = TimeLimits {
            time_control: TimeControl::Fischer {
                white_ms: 60000,
                black_ms: 60000,
                increment_ms: 1000,
            },
            ..Default::default()
        };

        let tm = TimeManager::new(&limits, Color::Black, 0, phase);
        let info = tm.get_time_info();

        // Opening phase should have higher soft limit due to opening_factor
        assert!(info.soft_limit_ms > 1000);

        // Endgame position
        let endgame_pos = parse_sfen("4k4/9/9/9/9/9/9/9/4K4 b Rb 100").unwrap();
        let endgame_phase = detect_game_phase_for_time(&endgame_pos, 200);
        assert_eq!(endgame_phase, GamePhase::EndGame);

        let tm_endgame = TimeManager::new(&limits, Color::Black, 200, endgame_phase);
        let info_endgame = tm_endgame.get_time_info();

        // Endgame should have different time allocation
        assert!(info_endgame.soft_limit_ms != info.soft_limit_ms);
    }

    #[test]
    fn test_phase_aware_move_estimation() {
        use crate::time_management::estimate_moves_remaining_by_phase;

        // Test that different phases give different move estimates
        let opening_moves = estimate_moves_remaining_by_phase(GamePhase::Opening, 20);
        let middle_moves = estimate_moves_remaining_by_phase(GamePhase::MiddleGame, 80);
        let endgame_moves = estimate_moves_remaining_by_phase(GamePhase::EndGame, 200);

        assert!(opening_moves > middle_moves);
        assert!(middle_moves > endgame_moves);
        assert!(endgame_moves >= 10); // Never below 10
    }

    #[test]
    fn test_time_profile_phase_detection() {
        // Middle game position with moderate material
        let pos =
            parse_sfen("ln1gkg1nl/1r4s2/p1pppp1pp/1p4p2/9/2P6/PP1PPPPPP/7R1/LN1GKGSNL w Bb 30")
                .unwrap();

        // With Time profile (ply-heavy), this should detect differently than Search profile
        let phase_time = detect_game_phase_for_time(&pos, 60);

        // Due to Time profile's weights (0.3 material, 0.7 ply),
        // ply 60 should push towards MiddleGame
        assert_eq!(phase_time, GamePhase::MiddleGame);
    }

    #[test]
    fn test_byoyomi_with_phase() {
        let limits = TimeLimits {
            time_control: TimeControl::Byoyomi {
                main_time_ms: 10000,
                byoyomi_ms: 30000,
                periods: 3,
            },
            ..Default::default()
        };

        // Opening phase
        let tm_opening = TimeManager::new(&limits, Color::White, 10, GamePhase::Opening);
        let info_opening = tm_opening.get_time_info();

        // Endgame phase
        let tm_endgame = TimeManager::new(&limits, Color::White, 180, GamePhase::EndGame);
        let info_endgame = tm_endgame.get_time_info();

        // Different phases should still respect byoyomi constraints
        // but may allocate main time differently
        assert!(info_opening.soft_limit_ms <= 10000);
        assert!(info_endgame.soft_limit_ms <= 10000);
    }
}
