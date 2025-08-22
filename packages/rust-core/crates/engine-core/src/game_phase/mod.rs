//! Game phase detection and classification module
//!
//! This module provides unified game phase detection logic used by both
//! the search engine and time management components.
//!
//! For detailed architecture and usage, see `docs/game-phase-module-guide.md`

pub mod classify;
pub mod config;
pub mod signals;

#[cfg(test)]
mod integration_tests;
#[cfg(test)]
mod tests;

// Re-export main types
pub use classify::{classify, GamePhase, PhaseOutput};
pub use config::{PhaseParameters, PhaseWeights, Profile};
pub use signals::{compute_signals, PhaseSignals};

/// Convenience function to detect game phase with default parameters
pub fn detect_game_phase(pos: &crate::Position, ply: u32, profile: Profile) -> GamePhase {
    let params = PhaseParameters::for_profile(profile);
    let signals = compute_signals(pos, ply, &params.phase_weights, &params);
    let output = classify(None, &signals, &params);
    output.phase
}

/// Convenience function to detect game phase with hysteresis
pub fn detect_game_phase_with_history(
    pos: &crate::Position,
    ply: u32,
    profile: Profile,
    previous_phase: Option<GamePhase>,
) -> GamePhase {
    let params = PhaseParameters::for_profile(profile);
    let signals = compute_signals(pos, ply, &params.phase_weights, &params);
    let output = classify(previous_phase, &signals, &params);
    output.phase
}
