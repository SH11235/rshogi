//! Time management utilities for search
//!
//! This module handles TimeManager creation and game phase estimation
//! to reduce code duplication in the main search logic.

use crate::{
    search::SearchLimits,
    shogi::Color,
    time_management::{detect_game_phase_for_time, TimeControl, TimeLimits, TimeManager},
    Position,
};
use std::sync::Arc;

/// Create a TimeManager instance based on search limits
///
/// Returns Some(TimeManager) if time control is needed, None for infinite analysis
pub fn create_time_manager(
    limits: &SearchLimits,
    side_to_move: Color,
    ply: u16,
    position: &Position,
) -> Option<Arc<TimeManager>> {
    let game_phase = detect_game_phase_for_time(position, ply as u32);

    // Log phase and estimated moves for debugging
    log::debug!(
        "phase={:?}, ply={}, est_moves_left={}",
        game_phase,
        ply,
        crate::time_management::estimate_moves_remaining_by_phase(game_phase, ply as u32)
    );

    // Special handling for Ponder mode
    if matches!(limits.time_control, TimeControl::Ponder(_)) {
        log::debug!("Creating TimeManager for PONDER mode");

        // Convert SearchLimits to TimeLimits (unwraps inner for Ponder)
        let pending_limits: TimeLimits = limits.clone().into();
        log::debug!(
            "After conversion for ponder, time_limits.time_control: {:?}",
            pending_limits.time_control
        );

        // Create Ponder-specific TimeManager
        let time_manager = Arc::new(TimeManager::new_ponder(
            &pending_limits,
            side_to_move,
            ply.into(), // Convert u16 to u32
            game_phase,
        ));

        Some(time_manager)
    } else if !matches!(limits.time_control, TimeControl::Infinite) || limits.depth.is_some() {
        // Normal time control or depth limit
        // Note: Even for Infinite time control, we create a TimeManager if there's a depth limit.
        // This allows unified handling of polling intervals and early termination when depth is reached.
        log::debug!("Creating TimeManager with time_control: {:?}", limits.time_control);

        // Convert SearchLimits to TimeLimits
        let time_limits: TimeLimits = limits.clone().into();

        log::debug!("After conversion, time_limits.time_control: {:?}", time_limits.time_control);

        let time_manager = Arc::new(TimeManager::new(
            &time_limits,
            side_to_move,
            ply.into(), // Convert u16 to u32
            game_phase,
        ));

        Some(time_manager)
    } else {
        // Infinite time control with no depth limit
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game_phase::GamePhase;
    use crate::search::SearchLimitsBuilder;
    use crate::usi::parse_sfen;

    #[test]
    fn test_game_phase_detection_with_position() {
        // Test with actual positions
        let start_pos =
            parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").unwrap();

        // Start position should be Opening
        assert_eq!(detect_game_phase_for_time(&start_pos, 0), GamePhase::Opening);

        // End game position
        let endgame_pos = parse_sfen("4k4/9/9/9/9/9/9/9/4K4 b Rb 100").unwrap();
        assert_eq!(detect_game_phase_for_time(&endgame_pos, 200), GamePhase::EndGame);

        // Middle game position
        let middle_pos =
            parse_sfen("ln1gkg1nl/1r4s2/p1pppp1pp/1p4p2/9/2P6/PP1PPPPPP/7R1/LN1GKGSNL w Bb 30")
                .unwrap();
        assert_eq!(detect_game_phase_for_time(&middle_pos, 60), GamePhase::MiddleGame);
    }

    #[test]
    fn test_create_time_manager_infinite() {
        let limits = SearchLimitsBuilder::default().build();
        let pos = Position::startpos();
        let tm = create_time_manager(&limits, Color::Black, 0, &pos);
        assert!(tm.is_none());
    }

    #[test]
    fn test_create_time_manager_with_depth() {
        let limits = SearchLimitsBuilder::default().depth(10).build();
        let pos = Position::startpos();
        let tm = create_time_manager(&limits, Color::Black, 0, &pos);
        assert!(tm.is_some());
    }

    #[test]
    fn test_create_time_manager_with_time() {
        let limits = SearchLimitsBuilder::default().fixed_time_ms(1000).build();
        let pos = Position::startpos();
        let tm = create_time_manager(&limits, Color::Black, 50, &pos);
        assert!(tm.is_some());
    }
}
