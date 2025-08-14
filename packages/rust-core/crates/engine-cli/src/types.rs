//! Type definitions for the engine-cli crate
//!
//! This module contains shared type definitions used throughout the engine-cli crate,
//! including search results, ponder state, and callback types.

use engine_core::shogi::Move;
use std::sync::Arc;
use std::time::Instant;

use crate::usi::output::SearchInfo;

/// Extended search result containing all necessary information
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
pub type UsiInfoCallback = Arc<dyn Fn(SearchInfo) + Send + Sync>;

/// Type alias for engine info callback
pub type EngineInfoCallback =
    Arc<dyn Fn(u8, i32, u64, std::time::Duration, &[engine_core::shogi::Move]) + Send + Sync>;
