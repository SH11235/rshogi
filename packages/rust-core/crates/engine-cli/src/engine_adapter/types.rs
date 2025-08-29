//! Type definitions for the engine adapter.
//!
//! This module contains common types used throughout the engine adapter,
//! including search results, ponder state, and callback function types.

use engine_core::search::types::StopInfo;

/// Extended search result containing all necessary information
pub struct ExtendedSearchResult {
    pub best_move: String,
    pub ponder_move: Option<String>,
    pub depth: u8,
    pub stop_info: Option<StopInfo>,
    pub pv_owner_mismatches: Option<u64>,
    pub pv_owner_checks: Option<u64>,
    pub pv_trim_cuts: Option<u64>,
    pub pv_trim_checks: Option<u64>,
}

/// State management for pondering
#[derive(Default)]
pub struct PonderState {
    /// Whether the engine is currently pondering
    pub is_pondering: bool,
    /// Time when pondering started
    pub ponder_start: Option<std::time::Instant>,
}

/// Source of ponder move for observability/metrics
#[derive(Clone, Copy, Debug)]
pub enum PonderSource {
    Pv,
    TT,
    None,
}

impl std::fmt::Display for PonderSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PonderSource::Pv => "pv",
            PonderSource::TT => "tt",
            PonderSource::None => "none",
        };
        write!(f, "{}", s)
    }
}
