//! Integration with the new game_phase module for engine controller
//!
//! This module provides a bridge for the engine controller to use the new game_phase module

use crate::game_phase::{detect_game_phase, Profile};
use crate::Position;

/// Re-export GamePhase enum for compatibility
pub use crate::game_phase::GamePhase;

/// Detect game phase using the new module with Search profile
#[must_use]
#[inline]
pub fn detect_game_phase_for_search(pos: &Position, ply: u32) -> GamePhase {
    detect_game_phase(pos, ply, Profile::Search)
}

/// Calculate phase score using the new module
///
/// Returns a value from 0-128 for compatibility with existing code
#[must_use]
pub fn calculate_phase_score(pos: &Position, ply: u32) -> u8 {
    use crate::game_phase::{compute_signals, PhaseParameters};

    let params = PhaseParameters::for_profile(Profile::Search);
    let signals = compute_signals(pos, ply, &params.phase_weights, &params);

    // Get combined score (0.0 - 1.0)
    let score = signals.combined_score(params.w_material, params.w_ply);

    // Convert to 0-128 scale (inverted because old system uses 128 = full material)
    ((1.0 - score) * 128.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usi::parse_sfen;

    #[test]
    fn test_detect_game_phase_for_search() {
        // Start position
        let pos =
            parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
        assert_eq!(detect_game_phase_for_search(&pos, 0), GamePhase::Opening);

        // End game position
        let endgame_pos = parse_sfen("4k4/9/9/9/9/9/9/9/4K4 b Rb 100").unwrap();
        assert_eq!(detect_game_phase_for_search(&endgame_pos, 200), GamePhase::EndGame);
    }

    #[test]
    fn test_calculate_phase_score() {
        // Start position should have high score (near 128)
        let pos =
            parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();
        let score = calculate_phase_score(&pos, 0);
        assert!(score > 100, "Start position should have high phase score, got {}", score);

        // End game position should have low score
        let endgame_pos = parse_sfen("4k4/9/9/9/9/9/9/9/4K4 b Rb 100").unwrap();
        let score = calculate_phase_score(&endgame_pos, 200);
        assert!(score < 32, "End game position should have low phase score, got {}", score);
    }
}
