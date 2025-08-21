//! Time management utilities for search
//!
//! This module handles TimeManager creation and game phase estimation
//! to reduce code duplication in the main search logic.

use crate::{
    search::SearchLimits,
    shogi::Color,
    time_management::{GamePhase, TimeControl, TimeLimits, TimeManager},
};
use std::sync::Arc;

/// Estimate the current game phase based on move count
///
/// This is a simple heuristic based on the number of moves played:
/// - Opening: 0-30 moves (序盤)
/// - Middle game: 31-70 moves (中盤)
/// - End game: 71+ moves (終盤)
///
/// These boundaries are based on typical shogi game statistics where
/// most games end around 100-120 moves.
pub fn estimate_game_phase(ply: u16) -> GamePhase {
    // Convert ply to moves (2 ply = 1 full move)
    let moves = ply / 2;

    if moves <= 30 {
        GamePhase::Opening
    } else if moves <= 70 {
        GamePhase::MiddleGame
    } else {
        GamePhase::EndGame
    }
}

/// Create a TimeManager instance based on search limits
///
/// Returns Some(TimeManager) if time control is needed, None for infinite analysis
pub fn create_time_manager(
    limits: &SearchLimits,
    side_to_move: Color,
    ply: u16,
) -> Option<Arc<TimeManager>> {
    let game_phase = estimate_game_phase(ply);

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
    use crate::search::SearchLimitsBuilder;

    #[test]
    fn test_game_phase_estimation() {
        // Test with ply values (remember: 2 ply = 1 move)
        // Opening: 0-30 moves
        assert_eq!(estimate_game_phase(0), GamePhase::Opening); // 0 moves
        assert_eq!(estimate_game_phase(30), GamePhase::Opening); // 15 moves
        assert_eq!(estimate_game_phase(60), GamePhase::Opening); // 30 moves
        assert_eq!(estimate_game_phase(61), GamePhase::Opening); // 30 moves (61/2 = 30)
        
        // Middle game: 31-70 moves
        assert_eq!(estimate_game_phase(62), GamePhase::MiddleGame); // 31 moves
        assert_eq!(estimate_game_phase(100), GamePhase::MiddleGame); // 50 moves
        assert_eq!(estimate_game_phase(140), GamePhase::MiddleGame); // 70 moves
        assert_eq!(estimate_game_phase(141), GamePhase::MiddleGame); // 70 moves (141/2 = 70)
        
        // End game: 71+ moves
        assert_eq!(estimate_game_phase(142), GamePhase::EndGame); // 71 moves
        assert_eq!(estimate_game_phase(200), GamePhase::EndGame); // 100 moves
        assert_eq!(estimate_game_phase(240), GamePhase::EndGame); // 120 moves
    }

    #[test]
    fn test_create_time_manager_infinite() {
        let limits = SearchLimitsBuilder::default().build();
        let tm = create_time_manager(&limits, Color::Black, 0);
        assert!(tm.is_none());
    }

    #[test]
    fn test_create_time_manager_with_depth() {
        let limits = SearchLimitsBuilder::default().depth(10).build();
        let tm = create_time_manager(&limits, Color::Black, 0);
        assert!(tm.is_some());
    }

    #[test]
    fn test_create_time_manager_with_time() {
        let limits = SearchLimitsBuilder::default().fixed_time_ms(1000).build();
        let tm = create_time_manager(&limits, Color::Black, 50);
        assert!(tm.is_some());
    }
}
