pub mod common;
pub mod constants;
pub mod history;
pub mod limits;
pub mod search_basic;
pub mod search_enhanced;
pub mod tt;
pub mod types;

// Re-export commonly used items
pub use common::{is_mate_score, mate_distance_pruning, mate_score, LimitChecker, SearchContext};
pub use constants::*;
pub use search_enhanced::GamePhase;
pub use tt::TranspositionTable;
pub use types::{InfoCallback, NodeType, SearchResult, SearchState, SearchStats};
