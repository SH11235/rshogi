pub mod adaptive_prefetcher;
pub mod common;
pub mod constants;
pub mod history;
pub mod limits;
pub mod search_basic;
pub mod search_enhanced;
pub mod tt;
pub mod tt_simd;
pub mod tt_stats;
pub mod types;
pub mod unified;

#[cfg(test)]
pub mod prefetch_test;
#[cfg(test)]
pub mod test_helpers;

// Re-export commonly used items
pub use crate::time_management::GamePhase;
pub use common::{is_mate_score, mate_distance_pruning, mate_score, LimitChecker, SearchContext};
pub use constants::*;
pub use limits::{SearchLimits, SearchLimitsBuilder};
pub use tt::TranspositionTable;
pub use types::{InfoCallback, NodeType, SearchResult, SearchStack, SearchState, SearchStats};
