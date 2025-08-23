//! Type definitions for the engine adapter.
//!
//! This module contains common types used throughout the engine adapter,
//! including search results, ponder state, and callback function types.

use engine_core::search::types::{NodeType, StopInfo};
use engine_core::shogi::Move;

/// Extended search result containing all necessary information
pub struct ExtendedSearchResult {
    pub best_move: String,
    pub ponder_move: Option<String>,
    pub depth: u8,
    pub seldepth: Option<u8>,
    pub score: i32,
    pub pv: Vec<Move>,
    pub node_type: NodeType,
    pub stop_info: Option<StopInfo>,
}

/// State management for pondering
#[derive(Default)]
pub struct PonderState {
    /// Whether the engine is currently pondering
    pub is_pondering: bool,
    /// Time when pondering started
    pub ponder_start: Option<std::time::Instant>,
}
