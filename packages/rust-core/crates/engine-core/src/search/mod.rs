pub mod adaptive_prefetcher;
pub mod common;
pub mod config;
pub mod constants;
pub mod history;
pub mod limits;
pub mod parallel;
pub mod search_basic;
pub mod search_enhanced;
pub mod snapshot;
pub mod tt;
pub mod types;
pub mod unified;

#[cfg(test)]
mod test_utils;

// Re-export commonly used items
pub use crate::game_phase::GamePhase;
pub use common::{is_mate_score, mate_distance_pruning, mate_score, LimitChecker};
pub use constants::*;
pub use limits::{SearchLimits, SearchLimitsBuilder};
pub use tt::TranspositionTable;
pub use types::{
    CommittedIteration, InfoCallback, IterationCallback, NodeType, SearchResult, SearchStack,
    SearchState, SearchStats,
};
pub use unified::context::SearchContext;
