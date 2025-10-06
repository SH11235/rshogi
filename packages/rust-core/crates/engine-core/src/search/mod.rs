pub mod ab;
pub mod adaptive_prefetcher;
pub mod api;
pub mod common;
pub mod config;
pub mod constants;
pub mod history;
pub mod limits;
pub mod parallel;
pub mod params;
pub mod snapshot;
pub mod tt;
pub mod types;

// Re-export commonly used items
pub use crate::game_phase::GamePhase;
pub use api::{InfoEvent, InfoEventCallback};
pub use common::{is_mate_score, mate_distance_pruning, mate_score, LimitChecker};
pub use constants::*;
pub use limits::{SearchLimits, SearchLimitsBuilder};
pub use tt::TranspositionTable;
pub use types::{
    CommittedIteration, IterationCallback, NodeType, SearchResult, SearchStack, SearchState,
    SearchStats,
};
