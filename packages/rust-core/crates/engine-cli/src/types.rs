//! Type definitions for the engine-cli crate
//!
//! This module contains shared type definitions used throughout the engine-cli crate,
//! including search results, ponder state, and callback types.

use engine_core::shogi::Move;
use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use crate::usi::output::SearchInfo;

/// Extended search result containing all necessary information
#[allow(dead_code)]
pub struct ExtendedSearchResult {
    pub best_move: String,
    pub best_move_internal: Move, // Keep the original Move object
    pub ponder_move: Option<String>,
    pub ponder_move_internal: Option<Move>, // Keep the original ponder Move object
    pub depth: u32,
    pub score: i32,
    pub pv: Vec<Move>,
}

/// State for managing ponder (think on opponent's time) functionality
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct PonderState {
    /// Whether currently pondering
    pub is_pondering: bool,
    /// The move we're pondering on (opponent's expected move)
    pub ponder_move: Option<String>,
    /// Time when pondering started
    pub ponder_start_time: Option<Instant>,
}

/// Type alias for USI info callback
#[allow(dead_code)]
pub type UsiInfoCallback = Arc<dyn Fn(SearchInfo) + Send + Sync>;

/// Type alias for engine info callback
#[allow(dead_code)]
pub type EngineInfoCallback =
    Arc<dyn Fn(u8, i32, u64, std::time::Duration, &[engine_core::shogi::Move]) + Send + Sync>;

/// Source of bestmove emission
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Direct bestmove from worker
    WorkerBestmove,
    /// Invalid bestmove from worker, using fallback
    WorkerBestmoveInvalidFallback,
    /// Resign due to invalid bestmove
    ResignInvalidBestmove,
    /// Session result on stop command
    SessionOnStop,
    /// Worker result on stop command
    WorkerOnStop,
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
        let s = match self {
            Self::Session => "session",
            Self::EmergencyFallback => "emergency_fallback",
            Self::Resign => "resign",
            Self::ResignNoPosition => "resign_no_position",
            Self::EmergencyFallbackNoSession => "emergency_fallback_no_session",
            Self::ResignFallbackFailed => "resign_fallback_failed",
            Self::WorkerBestmove => "worker_bestmove",
            Self::WorkerBestmoveInvalidFallback => "worker_bestmove_invalid_fallback",
            Self::ResignInvalidBestmove => "resign_invalid_bestmove",
            Self::SessionOnStop => "session_on_stop",
            Self::WorkerOnStop => "worker_on_stop",
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
