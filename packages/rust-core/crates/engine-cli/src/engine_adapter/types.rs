//! Type definitions for the engine adapter.
//!
//! This module contains common types used throughout the engine adapter,
//! including search results, ponder state, and callback function types.

use engine_core::search::types::StopInfo;
use engine_core::shogi::Move;

/// Extended search result containing all necessary information
pub struct ExtendedSearchResult {
    pub best_move: String,
    pub ponder_move: Option<String>,
    pub depth: u8,
    pub seldepth: Option<u8>,
    pub score: i32,
    pub pv: Vec<Move>,
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
    CurrentIteration,
    TT,
    None,
}

impl std::fmt::Display for PonderSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            PonderSource::Pv => "pv",
            PonderSource::CurrentIteration => "current_iter",
            PonderSource::TT => "tt",
            PonderSource::None => "none",
        };
        write!(f, "{}", s)
    }
}
