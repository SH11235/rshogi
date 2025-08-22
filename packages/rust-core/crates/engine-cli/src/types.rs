//! Type definitions for the engine-cli crate
//!
//! This module contains shared type definitions used throughout the engine-cli crate,
//! including search results, ponder state, and callback types.

use std::fmt;
use std::sync::Arc;


/// Type alias for engine info callback
pub type EngineInfoCallback =
    Arc<dyn Fn(u8, i32, u64, std::time::Duration, &[engine_core::shogi::Move]) + Send + Sync>;

/// Source of bestmove emission
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BestmoveSource {
    /// Normal search session result
    Session,
    /// Emergency fallback when search fails
    EmergencyFallback,
    /// Resign due to no legal moves
    Resign,
    /// Resign when no position is set
    ResignNoPosition,
    /// Emergency fallback when no session exists
    EmergencyFallbackNoSession,
    /// Resign when fallback generation fails
    ResignFallbackFailed,
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
        // NOTE: When adding new variants to BestmoveSource, update this match expression
        let s = match self {
            Self::Session => "session",
            Self::EmergencyFallback => "emergency_fallback",
            Self::Resign => "resign",
            Self::ResignNoPosition => "resign_no_position",
            Self::EmergencyFallbackNoSession => "emergency_fallback_no_session",
            Self::ResignFallbackFailed => "resign_fallback_failed",
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
