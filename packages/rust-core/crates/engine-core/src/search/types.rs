//! Common types for search algorithms

use crate::shogi::Move;
use std::time::Duration;

/// Info callback type for search progress reporting
pub type InfoCallback = Box<dyn Fn(u8, i32, u64, Duration, &[Move]) + Send>;

/// Search statistics
#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    /// Nodes searched
    pub nodes: u64,
    /// Time elapsed
    pub elapsed: Duration,
    /// Principal variation
    pub pv: Vec<Move>,
    /// Search depth reached
    pub depth: u8,
    /// Selective depth reached (optional for enhanced search)
    pub seldepth: Option<u8>,
    /// Number of aspiration window failures (optional for enhanced search)
    pub aspiration_failures: Option<u32>,
    /// Number of transposition table hits (optional)
    pub tt_hits: Option<u64>,
    /// Number of null move pruning cuts (optional)
    pub null_cuts: Option<u64>,
    /// Number of late move reductions (optional)
    pub lmr_count: Option<u64>,
}

/// Search result
#[derive(Clone, Debug)]
pub struct SearchResult {
    /// Best move found
    pub best_move: Option<Move>,
    /// Evaluation score (from side to move perspective)
    pub score: i32,
    /// Search statistics
    pub stats: SearchStats,
}

impl SearchResult {
    /// Create a new search result
    pub fn new(best_move: Option<Move>, score: i32, stats: SearchStats) -> Self {
        Self {
            best_move,
            score,
            stats,
        }
    }

    /// Create a search result from legacy format (Option<Move>, i32)
    pub fn from_legacy(
        move_score: (Option<Move>, i32),
        nodes: u64,
        elapsed: Duration,
        pv: Vec<Move>,
        depth: u8,
    ) -> Self {
        Self {
            best_move: move_score.0,
            score: move_score.1,
            stats: SearchStats {
                nodes,
                elapsed,
                pv,
                depth,
                ..Default::default()
            },
        }
    }
}

/// Node type in alpha-beta search
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeType {
    /// Exact score (PV node)
    Exact,
    /// Upper bound (All node, fail low)
    UpperBound,
    /// Lower bound (Cut node, fail high)  
    LowerBound,
}

/// Search state for tracking node types during search
#[derive(Debug, Clone, Copy)]
pub struct SearchState {
    /// Original alpha value when entering the node
    pub original_alpha: i32,
    /// Original beta value when entering the node
    pub original_beta: i32,
    /// Final score returned from the node
    pub score: i32,
}

impl SearchState {
    /// Determine node type based on original bounds and final score
    pub fn node_type(&self) -> NodeType {
        if self.score <= self.original_alpha {
            NodeType::UpperBound
        } else if self.score >= self.original_beta {
            NodeType::LowerBound
        } else {
            NodeType::Exact
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_state_node_type() {
        // Exact node: score is between original bounds
        let state = SearchState {
            original_alpha: -100,
            original_beta: 100,
            score: 50,
        };
        assert_eq!(state.node_type(), NodeType::Exact);

        // Upper bound: score <= original alpha
        let state = SearchState {
            original_alpha: 0,
            original_beta: 100,
            score: -50,
        };
        assert_eq!(state.node_type(), NodeType::UpperBound);

        // Lower bound: score >= original beta
        let state = SearchState {
            original_alpha: -100,
            original_beta: 0,
            score: 50,
        };
        assert_eq!(state.node_type(), NodeType::LowerBound);
    }

    #[test]
    fn test_search_result_from_legacy() {
        let move_score = (Some(Move::null()), 42);
        let result = SearchResult::from_legacy(
            move_score,
            1000,
            Duration::from_millis(100),
            vec![Move::null()],
            5,
        );

        assert_eq!(result.best_move, Some(Move::null()));
        assert_eq!(result.score, 42);
        assert_eq!(result.stats.nodes, 1000);
        assert_eq!(result.stats.depth, 5);
    }
}
