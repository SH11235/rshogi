//! Phase classification with hysteresis

use super::config::PhaseParameters;
use super::signals::PhaseSignals;

/// Game phase enum (matches time_management version)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GamePhase {
    /// Opening phase
    Opening,
    /// Middle game
    MiddleGame,
    /// End game
    EndGame,
}

/// Phase detection output
#[derive(Debug, Clone, Copy)]
pub struct PhaseOutput {
    /// Combined score (0.0 - 1.0)
    pub score: f32,
    /// Discrete phase
    pub phase: GamePhase,
}

/// Classify phase with hysteresis to prevent oscillation
#[inline]
pub fn classify(
    previous: Option<GamePhase>,
    signals: &PhaseSignals,
    params: &PhaseParameters,
) -> PhaseOutput {
    let score = signals.combined_score(params.w_material, params.w_ply);

    let phase = match previous {
        None => {
            // No history, use simple thresholds
            classify_without_hysteresis(score, params)
        }
        Some(prev_phase) => {
            // Apply hysteresis
            classify_with_hysteresis(score, prev_phase, params)
        }
    };

    PhaseOutput { score, phase }
}

/// Simple classification without hysteresis
fn classify_without_hysteresis(score: f32, params: &PhaseParameters) -> GamePhase {
    if score < params.endgame_threshold {
        GamePhase::Opening
    } else if score > params.opening_threshold {
        GamePhase::EndGame
    } else {
        GamePhase::MiddleGame
    }
}

/// Classification with hysteresis
fn classify_with_hysteresis(
    score: f32,
    previous: GamePhase,
    params: &PhaseParameters,
) -> GamePhase {
    let h = params.hysteresis;

    match previous {
        GamePhase::Opening => {
            // To transition out of opening, need to clearly exceed threshold
            if score > params.endgame_threshold + h {
                if score > params.opening_threshold {
                    GamePhase::EndGame
                } else {
                    GamePhase::MiddleGame
                }
            } else {
                GamePhase::Opening
            }
        }
        GamePhase::MiddleGame => {
            // Can transition to either opening or endgame
            if score < params.endgame_threshold - h {
                GamePhase::Opening
            } else if score > params.opening_threshold + h {
                GamePhase::EndGame
            } else {
                GamePhase::MiddleGame
            }
        }
        GamePhase::EndGame => {
            // To transition out of endgame, need to clearly go below threshold
            if score < params.opening_threshold - h {
                if score < params.endgame_threshold {
                    GamePhase::Opening
                } else {
                    GamePhase::MiddleGame
                }
            } else {
                GamePhase::EndGame
            }
        }
    }
}
