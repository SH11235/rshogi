//! Baseline tests for current game phase detection logic
//! This file captures the current behavior before refactoring

#[cfg(test)]
mod baseline_tests {
    use super::super::*;
    use crate::shogi::Color;
    use crate::usi::parse_sfen;

    /// Test position with expected phase result
    struct TestCase {
        name: &'static str,
        sfen: &'static str,
        ply: u16,
        expected_phase: GamePhase,
        expected_material_score: u8,
        expected_active_threads: usize,
    }

    /// Representative test positions for baseline
    fn get_baseline_positions() -> Vec<TestCase> {
        vec![
            TestCase {
                name: "Initial position",
                sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
                ply: 0,
                expected_phase: GamePhase::Opening,
                expected_material_score: 128,
                expected_active_threads: 8, // Assuming 8 threads
            },
            TestCase {
                name: "Early opening (10 moves)",
                sfen: "lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL w - 10",
                ply: 20,
                expected_phase: GamePhase::Opening,
                expected_material_score: 128,
                expected_active_threads: 8,
            },
            TestCase {
                name: "Late opening (20 moves)",
                sfen: "lnsgkg1nl/1r4sb1/p1pppp1pp/1p4p2/9/2P6/PPBPPPPPP/7R1/LNSGKGSNL b - 20",
                ply: 40,
                expected_phase: GamePhase::Opening,
                expected_material_score: 128,
                expected_active_threads: 8,
            },
            TestCase {
                name: "Middle game with exchanges",
                sfen: "ln1gkg1nl/1r4s2/p1pppp1pp/1p4p2/9/2P6/PP1PPPPPP/7R1/LN1GKGSNL w Bb 30",
                ply: 60,
                expected_phase: GamePhase::Opening, // Still high material
                expected_material_score: 118,       // Some pieces captured
                expected_active_threads: 8,
            },
            TestCase {
                name: "Late middle game",
                sfen: "3gkg3/9/4pp3/2p3p2/9/2P3P2/4PP3/9/3GKG3 b RBSNLPrbsnlp 80",
                ply: 160,
                expected_phase: GamePhase::Opening, // Actually still opening due to high material score
                expected_material_score: 98,        // Actual value
                expected_active_threads: 8,
            },
            TestCase {
                name: "Endgame K+R vs K+B",
                sfen: "4k4/9/9/9/9/9/9/9/4K4 b Rb 100",
                ply: 200,
                expected_phase: GamePhase::EndGame,
                expected_material_score: 19, // Actual value
                expected_active_threads: 4,  // Half threads in endgame
            },
            TestCase {
                name: "Pure king endgame",
                sfen: "4k4/9/9/9/9/9/9/9/4K4 b - 120",
                ply: 240,
                expected_phase: GamePhase::EndGame,
                expected_material_score: 0,
                expected_active_threads: 4,
            },
            TestCase {
                name: "Repetition position (high ply, full material)",
                sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 100",
                ply: 200,
                expected_phase: GamePhase::Opening, // Full material overrides high ply
                expected_material_score: 128,
                expected_active_threads: 8,
            },
        ]
    }

    #[test]
    fn test_baseline_phase_detection() {
        let mut engine = Engine::new(EngineType::Material);
        engine.num_threads = 8; // Set to 8 threads for consistent testing

        for test_case in get_baseline_positions() {
            println!("\nTesting: {}", test_case.name);

            let mut pos = parse_sfen(test_case.sfen).expect("Valid SFEN");
            pos.ply = test_case.ply;

            // Test material phase score
            let material_score = engine.material_phase_score(&pos);
            println!(
                "  Material score: {} (expected: {})",
                material_score, test_case.expected_material_score
            );

            // Test game phase detection
            let detected_phase = engine.detect_game_phase(&pos);
            println!(
                "  Detected phase: {:?} (expected: {:?})",
                detected_phase, test_case.expected_phase
            );

            // Test active threads calculation
            let active_threads = engine.calculate_active_threads(&pos);
            println!(
                "  Active threads: {} (expected: {})",
                active_threads, test_case.expected_active_threads
            );

            // Record assertions for baseline
            assert_eq!(
                detected_phase, test_case.expected_phase,
                "Phase mismatch for {}",
                test_case.name
            );

            // Allow some tolerance for material score
            let score_diff =
                (material_score as i32 - test_case.expected_material_score as i32).abs();
            assert!(
                score_diff <= 2,
                "Material score {} differs too much from expected {} for {}",
                material_score,
                test_case.expected_material_score,
                test_case.name
            );

            assert_eq!(
                active_threads, test_case.expected_active_threads,
                "Thread count mismatch for {}",
                test_case.name
            );
        }
    }

    #[test]
    fn test_baseline_phase_transitions() {
        let _engine = Engine::new(EngineType::Material);

        // Test material-based transitions
        let test_scores: Vec<(u8, GamePhase)> = vec![
            (128, GamePhase::Opening),   // Full material
            (100, GamePhase::Opening),   // Still above threshold
            (96, GamePhase::Opening),    // At threshold
            (95, GamePhase::MiddleGame), // Just below
            (50, GamePhase::MiddleGame), // Clear middle game
            (32, GamePhase::MiddleGame), // At endgame threshold
            (31, GamePhase::EndGame),    // Just into endgame
            (10, GamePhase::EndGame),    // Deep endgame
            (0, GamePhase::EndGame),     // No material
        ];

        for (score, expected_phase) in test_scores {
            // Create a position that would yield this material score
            // This is a simplified test - actual positions would be more complex
            println!("Testing material score {} -> phase {:?}", score, expected_phase);

            // For now, we can't easily test this without creating specific positions
            // This documents the expected behavior
        }
    }

    #[test]
    fn test_baseline_time_allocation() {
        use crate::time_management::{calculate_time_allocation, TimeControl, TimeParameters};

        let params = TimeParameters::default();

        // Test cases for time allocation with different phases
        let test_cases = vec![
            ("Opening", GamePhase::Opening, 1.2), // opening_factor
            ("MiddleGame", GamePhase::MiddleGame, 1.0),
            ("EndGame", GamePhase::EndGame, 0.8), // endgame_factor
        ];

        for (name, phase, expected_factor) in test_cases {
            let (soft, _hard) = calculate_time_allocation(
                &TimeControl::Fischer {
                    white_ms: 60000,
                    black_ms: 60000,
                    increment_ms: 1000,
                },
                Color::White,
                30, // ply
                None,
                phase,
                &params,
            );

            println!("{} phase soft limit: {}ms (factor: {})", name, soft, expected_factor);

            // The actual calculation is complex, but we can verify relative ordering
            // This documents current behavior
        }
    }

    #[test]
    fn test_baseline_estimate_moves_remaining() {
        use crate::time_management::estimate_moves_remaining;

        let test_cases = vec![
            (0, 60), // Opening
            (20, 60),
            (40, 60),
            (60, 40), // Middle game
            (100, 40),
            (160, 20), // Endgame
            (200, 20),
        ];

        for (ply, expected) in test_cases {
            let remaining = estimate_moves_remaining(ply);
            assert_eq!(remaining, expected, "Moves remaining mismatch at ply {}", ply);
        }
    }
}
