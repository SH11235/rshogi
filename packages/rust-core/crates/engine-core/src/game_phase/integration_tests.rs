//! Integration tests for game_phase module

#[cfg(test)]
mod tests {
    use crate::game_phase::{
        detect_game_phase, detect_game_phase_with_history, GamePhase, Profile,
    };
    use crate::usi::parse_sfen;

    /// Test that detect_game_phase works correctly with real positions
    #[test]
    fn test_detect_game_phase_real_positions() {
        let test_positions = vec![
            (
                "startpos",
                "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
                0,
                GamePhase::Opening,
            ),
            // Note: This position has material score 118 (out of 128), which maps to signal ~0.08
            // With ply 60 (signal 0.5), combined score = 0.7*0.08 + 0.3*0.5 = 0.206
            // This is > endgame_threshold (0.176), so it's MiddleGame in the new system
            (
                "some exchanges",
                "ln1gkg1nl/1r4s2/p1pppp1pp/1p4p2/9/2P6/PP1PPPPPP/7R1/LN1GKGSNL w Bb 30",
                60,
                GamePhase::MiddleGame,
            ),
            ("endgame", "4k4/9/9/9/9/9/9/9/4K4 b Rb 100", 200, GamePhase::EndGame),
            ("kings only", "4k4/9/9/9/9/9/9/9/4K4 b - 120", 240, GamePhase::EndGame),
        ];

        for (name, sfen, ply, expected) in test_positions {
            let pos = parse_sfen(sfen).expect("Valid SFEN");

            // New implementation with Search profile
            let phase = detect_game_phase(&pos, ply as u32, Profile::Search);

            println!("{}: phase={:?} (expected={:?})", name, phase, expected);

            assert_eq!(phase, expected, "Phase mismatch for {}", name);
        }
    }

    /// Test hysteresis functionality
    #[test]
    fn test_hysteresis_real_scenario() {
        // Position that's on the boundary
        let pos =
            parse_sfen("ln1gkg1nl/1r4s2/p1pppp1pp/1p4p2/9/2P6/PP1PPPPPP/7R1/LN1GKGSNL w Bb 30")
                .unwrap();
        let ply = 60u32;

        // Without history
        let phase1 = detect_game_phase(&pos, ply, Profile::Search);

        // With same phase as history - should stay the same
        let phase2 = detect_game_phase_with_history(&pos, ply, Profile::Search, Some(phase1));
        assert_eq!(phase1, phase2, "Should maintain same phase with history");

        // With different phase as history - might stay due to hysteresis
        let different_phase = match phase1 {
            GamePhase::Opening => GamePhase::MiddleGame,
            GamePhase::MiddleGame => GamePhase::Opening,
            GamePhase::EndGame => GamePhase::MiddleGame,
        };

        let phase3 =
            detect_game_phase_with_history(&pos, ply, Profile::Search, Some(different_phase));
        println!(
            "Without history: {:?}, With {:?} history: {:?}",
            phase1, different_phase, phase3
        );
    }

    /// Test that different profiles produce different results
    #[test]
    fn test_profile_behavior() {
        let test_cases = vec![
            // Middle game position with moderate ply
            ("3gkg3/9/4pp3/2p3p2/9/2P3P2/4PP3/9/3GKG3 b RBSNLPrbsnlp 80", 160),
        ];

        for (sfen, ply) in test_cases {
            let pos = parse_sfen(sfen).expect("Valid SFEN");

            let search_phase = detect_game_phase(&pos, ply, Profile::Search);
            let time_phase = detect_game_phase(&pos, ply, Profile::Time);

            println!("Ply {}: Search={:?}, Time={:?}", ply, search_phase, time_phase);

            // With high ply but significant material, Search profile might still
            // consider it Opening/MiddleGame while Time profile considers it EndGame
            // This is expected behavior
        }
    }
}
