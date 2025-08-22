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
}
