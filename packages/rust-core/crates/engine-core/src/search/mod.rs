pub mod adaptive_prefetcher;
pub mod common;
pub mod constants;
pub mod history;
pub mod limits;
pub mod parallel;
pub mod prefetch_budget;
pub mod search_basic;
pub mod search_enhanced;
pub mod sharded_tt;
pub mod tt;
pub mod tt_filter;
pub mod tt_simd;
pub mod tt_stats;
pub mod tt_trait;
pub mod types;
pub mod unified;

#[cfg(test)]
mod test_utils;

// Re-export commonly used items
pub use crate::game_phase::GamePhase;
pub use common::{is_mate_score, mate_distance_pruning, mate_score, LimitChecker, SearchContext};
pub use constants::*;
pub use limits::{SearchLimits, SearchLimitsBuilder};
pub use sharded_tt::{ShardedTranspositionTable, SharedShardedTT};
pub use tt::TranspositionTable;
pub use types::{InfoCallback, NodeType, SearchResult, SearchStack, SearchState, SearchStats};
