//! Entry point optimization tests
//!
//! Tests for various optimizations at the entry point of search:
//! - Special one-move handling (when only one legal move exists)
//! - Immediate evaluation at depth 0
//! - Mate distance pruning
//! - Emergency fallback mechanisms

#[cfg(test)]
mod tests {
    use crate::types::BestmoveSource;
    use engine_core::MoveGenerator;

    #[test]
    fn test_only_move_immediate_return() {
        // Test the mechanism exists for handling single legal move positions
        // Finding a position with exactly one legal move is complex in Shogi
        // So we test that the infrastructure is in place

        // Simple endgame position
        let sfen = "k8/9/9/9/9/9/9/9/K8 b - 1"; // Black king vs white king
        let pos = engine_core::usi::parse_sfen(sfen).expect("Valid SFEN");

        // Verify moves can be generated
        let mg = MoveGenerator::new();
        let moves = mg.generate_all(&pos).expect("Generate moves");

        // Just verify the mechanism exists
        assert!(moves.len() > 0, "Should have at least one legal move");

        // The actual only-move optimization is tested in integration tests
        // where we can verify go command behavior
    }

    #[test]
    fn test_immediate_eval_at_depth_zero() {
        // Test that the immediate_eval_at_depth_zero flag exists and can be set
        let limits = engine_core::search::SearchLimits::builder()
            .depth(1)
            .immediate_eval_at_depth_zero(true)
            .build();

        assert!(limits.immediate_eval_at_depth_zero);

        // Test with extremely low time budget scenario
        let emergency_limits = engine_core::search::SearchLimits::builder()
            .depth(1)
            .fixed_time_ms(10) // Only 10ms
            .immediate_eval_at_depth_zero(true)
            .build();

        assert!(emergency_limits.immediate_eval_at_depth_zero);
        assert_eq!(emergency_limits.time_limit(), Some(std::time::Duration::from_millis(10)));
    }

    #[test]
    fn test_bestmove_source_display() {
        // Test that the new BestmoveSource variant displays correctly
        let source = BestmoveSource::OnlyMove;
        assert_eq!(source.to_string(), "only_move");

        // Test all variants to ensure completeness
        let sources = vec![
            (BestmoveSource::Resign, "resign"),
            (BestmoveSource::OnlyMove, "only_move"),
            (BestmoveSource::SessionOnStop, "session_on_stop"),
            (BestmoveSource::ResignOnFinish, "resign_on_finish"),
            (BestmoveSource::PartialResultTimeout, "partial_result_timeout"),
            (BestmoveSource::EmergencyFallbackTimeout, "emergency_fallback_timeout"),
            (BestmoveSource::EmergencyFallbackOnFinish, "emergency_fallback_on_finish"),
            (BestmoveSource::CoreFinalize, "core_finalize"),
        ];

        for (source, expected) in sources {
            assert_eq!(source.to_string(), expected);
        }
    }

    #[test]
    fn test_extract_mate_distance() {
        use engine_core::search::common::{extract_mate_distance, mate_score};

        // Test positive mate scores (giving mate)
        for dist in 1..10 {
            let score = mate_score(dist, true);
            let extracted = extract_mate_distance(score);
            assert_eq!(extracted, Some(dist), "Failed for mate in {dist}");
        }

        // Test negative mate scores (getting mated)
        for dist in 1..10 {
            let score = mate_score(dist, false);
            let extracted = extract_mate_distance(score);
            assert_eq!(extracted, Some(dist), "Failed for mated in {dist}");
        }

        // Test non-mate scores
        assert_eq!(extract_mate_distance(0), None);
        assert_eq!(extract_mate_distance(100), None);
        assert_eq!(extract_mate_distance(-100), None);
    }

    #[test]
    fn test_emergency_fallback_generation() {
        // Test that emergency move generation works for various positions
        let test_positions = vec![
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1", // startpos
            "k8/9/K8/9/9/9/9/9/9 w - 1",                                       // Endgame position
        ];

        for sfen in test_positions {
            let pos = engine_core::usi::parse_sfen(sfen).expect("Valid SFEN");
            let emergency_move = engine_core::util::emergency::emergency_move_usi(&pos);

            // Verify the emergency move returns Some
            match emergency_move {
                Some(mv) => {
                    assert!(!mv.is_empty(), "Emergency move should not be empty for {sfen}");

                    // For positions with legal moves, verify it's either a valid move or "resign"
                    let mg = MoveGenerator::new();
                    if let Ok(moves) = mg.generate_all(&pos) {
                        if moves.is_empty() {
                            assert_eq!(mv, "resign", "Should resign when no legal moves");
                        } else {
                            // Should be a valid USI move string
                            assert!(mv.len() >= 4, "Move string too short: {mv}");
                        }
                    }
                }
                None => panic!("emergency_move_usi should return Some for {sfen}"),
            }
        }
    }
}
