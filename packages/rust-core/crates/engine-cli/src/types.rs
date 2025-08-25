//! Type definitions for the engine-cli crate
//!
//! This module contains shared type definitions used throughout the engine-cli crate,
//! including search results, ponder state, and callback types.

use std::fmt;

/// Reason for resignation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResignReason {
    /// Position not set
    NoPositionSet,
    /// Position rebuild failed
    PositionRebuildFailed { error: &'static str },
    /// Invalid stored position command
    InvalidStoredPositionCmd,
    /// Checkmate (no legal moves and in check)
    Checkmate,
    /// No legal moves but not in check (likely error)
    NoLegalMovesButNotInCheck,
    /// Other error
    OtherError { error: &'static str },
}

impl fmt::Display for ResignReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoPositionSet => write!(f, "no_position_set"),
            Self::PositionRebuildFailed { error } => {
                write!(f, "position_rebuild_failed error={error}")
            }
            Self::InvalidStoredPositionCmd => write!(f, "invalid_stored_position_cmd"),
            Self::Checkmate => write!(f, "checkmate"),
            Self::NoLegalMovesButNotInCheck => write!(f, "no_legal_moves_but_not_in_check"),
            Self::OtherError { error } => write!(f, "error={error}"),
        }
    }
}

/// Source of bestmove emission
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BestmoveSource {
    /// Emergency fallback when search fails
    EmergencyFallback,
    /// Resign due to no legal moves
    Resign,
    /// Session result on stop command
    SessionOnStop,
    /// Resign due to timeout
    ResignTimeout,
    /// Session result in search finished handler
    SessionInSearchFinished,
    /// Resign in search finished handler
    ResignOnFinish,
    /// Partial result on timeout
    PartialResultTimeout,
    /// Emergency fallback on timeout
    EmergencyFallbackTimeout,
    /// Partial result on finish
    PartialResultOnFinish,
    /// Emergency fallback on finish
    EmergencyFallbackOnFinish,
    /// Test source
    #[cfg(test)]
    Test,
}

impl fmt::Display for BestmoveSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // IMPORTANT: When adding new variants to the BestmoveSource enum above,
        // you MUST also add a corresponding match arm here in the Display implementation.
        // This ensures proper string representation for logging and debugging.
        // The compiler will enforce this due to exhaustive matching.
        let s = match self {
            Self::EmergencyFallback => "emergency_fallback",
            Self::Resign => "resign",
            Self::SessionOnStop => "session_on_stop",
            Self::ResignTimeout => "resign_timeout",
            Self::SessionInSearchFinished => "session_in_search_finished",
            Self::ResignOnFinish => "resign_on_finish",
            Self::PartialResultTimeout => "partial_result_timeout",
            Self::EmergencyFallbackTimeout => "emergency_fallback_timeout",
            Self::PartialResultOnFinish => "partial_result_on_finish",
            Self::EmergencyFallbackOnFinish => "emergency_fallback_on_finish",
            #[cfg(test)]
            Self::Test => "test",
        };
        write!(f, "{s}")
    }
}
