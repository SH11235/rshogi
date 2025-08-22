//! Integration with the new game_phase module
//!
//! This module provides a bridge from the old GamePhase enum to the new game_phase module

use crate::game_phase::{detect_game_phase, Profile};
use crate::Position;

/// Re-export GamePhase enum for compatibility
pub use crate::game_phase::GamePhase;

/// Detect game phase using the new module with Time profile
#[must_use]
#[inline]
pub fn detect_game_phase_for_time(pos: &Position, ply: u32) -> GamePhase {
    detect_game_phase(pos, ply, Profile::Time)
}

/// Estimate remaining moves based on game phase
///
/// This provides phase-aware move estimation without requiring Position
#[must_use]
#[inline]
pub fn estimate_moves_remaining_by_phase(game_phase: GamePhase, ply: u32) -> u32 {
    let moves_played = ply / 2;

    match game_phase {
        GamePhase::Opening => {
            // 60-50 moves expected, decreasing with moves played
            if moves_played < 20 {
                60
            } else {
                55
            }
        }
        GamePhase::MiddleGame => {
            // 50-30 moves expected
            if moves_played < 50 {
                45
            } else if moves_played < 70 {
                40
            } else {
                35
            }
        }
        GamePhase::EndGame => {
            // 30-10 moves expected
            if moves_played < 100 {
                25
            } else if moves_played < 110 {
                20
            } else {
                10 // Fixed at 10 for very late game
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usi::parse_sfen;

    #[test]
    fn test_detect_game_phase_for_time() {
        // Start position
        let pos =
            parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
        assert_eq!(detect_game_phase_for_time(&pos, 0), GamePhase::Opening);

        // Late game position with high ply count
        let pos = parse_sfen("4k4/9/9/9/9/9/9/9/4K4 b Rb 100").unwrap();
        assert_eq!(detect_game_phase_for_time(&pos, 200), GamePhase::EndGame);
    }

    #[test]
    fn test_estimate_moves_remaining_by_phase() {
        // Test phase-based estimation
        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::Opening, 0), 60);
        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::Opening, 40), 55); // 20 moves
        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::Opening, 50), 55); // 25 moves

        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::MiddleGame, 80), 45); // 40 moves
        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::MiddleGame, 120), 40); // 60 moves
        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::MiddleGame, 160), 35); // 80 moves

        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::EndGame, 180), 25); // 90 moves
        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::EndGame, 220), 10); // 110 moves
        assert_eq!(estimate_moves_remaining_by_phase(GamePhase::EndGame, 260), 10);
        // 130 moves, clamped
    }

    #[test]
    fn test_phase_hysteresis_stability() {
        // Test that phase detection is stable with hysteresis
        // Using a position that's near the boundary between Opening and MiddleGame

        // Position with medium material and medium ply (boundary region)
        let pos =
            parse_sfen("ln1gkg1nl/1r4s2/p1pppp1pp/1p4p2/9/2P6/PP1PPPPPP/7R1/LN1GKGSNL w Bb 30")
                .unwrap();
        let ply = 60;

        // First detection (no previous phase)
        let phase1 = detect_game_phase_for_time(&pos, ply);

        // Simulate tracking the previous phase
        let previous_phase = phase1;

        // Get the internal detector to test with previous phase
        use crate::game_phase::{detect_game_phase_with_history, Profile};

        // Detection with previous phase should maintain stability
        let phase2 = detect_game_phase_with_history(&pos, ply, Profile::Time, Some(previous_phase));

        // Should maintain the same phase due to hysteresis
        assert_eq!(phase1, phase2, "Phase should be stable with hysteresis");

        // Test boundary case: if we're in Opening, we should stay there unless clearly past threshold
        if phase1 == GamePhase::Opening {
            // Even with slightly different conditions, should stay in Opening
            let phase3 = detect_game_phase_with_history(
                &pos,
                ply + 2,
                Profile::Time,
                Some(GamePhase::Opening),
            );
            assert_eq!(phase3, GamePhase::Opening, "Should maintain Opening phase with hysteresis");
        }
    }
}
