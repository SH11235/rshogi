//! Common types for search algorithms

use crate::shogi::Move;
use std::sync::Arc;
use std::time::Duration;

/// Info callback type for search progress reporting
pub type InfoCallback = Arc<dyn Fn(u8, i32, u64, Duration, &[Move], NodeType) + Send + Sync>;

/// Search statistics
#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    /// Nodes searched
    pub nodes: u64,
    /// Nodes searched in quiescence search
    pub qnodes: u64,
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
    /// Aspiration window success count
    pub aspiration_hits: Option<u32>,
    /// Total re-searches performed
    pub re_searches: Option<u32>,
    /// Duplication percentage for parallel search (0-100)
    pub duplication_percentage: Option<f64>,
    /// Number of check extensions applied
    pub check_extensions: Option<u64>,
    /// Number of king move extensions applied
    pub king_extensions: Option<u64>,
    /// Number of checking drops searched in quiescence search
    pub qs_check_drops: Option<u64>,
}

impl SearchStats {
    /// Helper function to increment an optional counter
    #[inline]
    pub fn bump(opt: &mut Option<u64>, by: u64) {
        *opt = Some(opt.unwrap_or(0) + by);
    }
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
    /// Node type (Exact, LowerBound, UpperBound)
    pub node_type: NodeType,
    /// Information about why the search stopped (None for legacy compatibility)
    pub stop_info: Option<StopInfo>,
}

impl SearchResult {
    /// Create a new search result
    pub fn new(best_move: Option<Move>, score: i32, stats: SearchStats) -> Self {
        Self {
            best_move,
            score,
            stats,
            node_type: NodeType::Exact, // Default to Exact for backward compatibility
            stop_info: Some(StopInfo::default()), // Default stop info instead of None
        }
    }

    /// Create a new search result with node type
    pub fn with_node_type(
        best_move: Option<Move>,
        score: i32,
        stats: SearchStats,
        node_type: NodeType,
    ) -> Self {
        Self {
            best_move,
            score,
            stats,
            node_type,
            stop_info: Some(StopInfo::default()), // Default stop info instead of None
        }
    }

    /// Create a new search result with node type and stop info
    pub fn with_stop_info(
        best_move: Option<Move>,
        score: i32,
        stats: SearchStats,
        node_type: NodeType,
        stop_info: StopInfo,
    ) -> Self {
        Self {
            best_move,
            score,
            stats,
            node_type,
            stop_info: Some(stop_info),
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
                qnodes: 0,
                elapsed,
                pv,
                depth,
                ..Default::default()
            },
            node_type: NodeType::Exact, // Default to Exact for legacy format
            stop_info: Some(StopInfo {
                // Construct from available data
                reason: TerminationReason::Completed,
                elapsed_ms: elapsed.as_millis() as u64,
                nodes,
                depth_reached: depth,
                hard_timeout: false,
            }),
        }
    }
}

/// Node type in alpha-beta search
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NodeType {
    /// Exact score (PV node)
    Exact = 0,
    /// Upper bound (All node, fail low)
    UpperBound = 2,
    /// Lower bound (Cut node, fail high)
    LowerBound = 1,
}

/// Reason for search termination
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationReason {
    /// Time limit reached (soft or hard)
    TimeLimit,
    /// Node count limit reached
    NodeLimit,
    /// Maximum depth limit reached
    DepthLimit,
    /// User requested stop
    UserStop,
    /// Mate found
    Mate,
    /// Search completed normally (all iterations finished)
    Completed,
    /// Error occurred during search
    Error,
}

impl std::fmt::Display for TerminationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            TerminationReason::TimeLimit => "time_limit",
            TerminationReason::NodeLimit => "node_limit",
            TerminationReason::DepthLimit => "depth_limit",
            TerminationReason::UserStop => "user_stop",
            TerminationReason::Mate => "mate",
            TerminationReason::Completed => "completed",
            TerminationReason::Error => "error",
        };
        f.write_str(s)
    }
}

/// Information about why the search stopped
#[derive(Debug, Clone)]
pub struct StopInfo {
    /// The reason for termination
    pub reason: TerminationReason,
    /// Elapsed time in milliseconds
    pub elapsed_ms: u64,
    /// Total nodes searched
    pub nodes: u64,
    /// Maximum depth reached
    pub depth_reached: u8,
    /// Whether this was a hard timeout (no time for move recovery)
    pub hard_timeout: bool,
}

impl Default for StopInfo {
    fn default() -> Self {
        Self {
            reason: TerminationReason::Completed,
            elapsed_ms: 0,
            nodes: 0,
            depth_reached: 0,
            hard_timeout: false,
        }
    }
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

/// Search stack entry for tracking search state at each ply
///
/// This structure is used to track the state of the search at each depth level,
/// including killer moves, current move being searched, and various flags.
#[derive(Clone, Default)]
pub struct SearchStack {
    /// Current ply (depth) in the search tree
    pub ply: u16,
    /// Current move being searched
    pub current_move: Option<Move>,
    /// Static evaluation at this position (cached)
    pub static_eval: Option<i32>,
    /// Killer moves (quiet moves that caused beta cutoffs)
    pub killers: [Option<Move>; 2],
    /// Move count at this ply
    pub move_count: u32,
    /// PV node flag
    pub pv: bool,
    /// Null move tried flag
    pub null_move: bool,
    /// In check flag
    pub in_check: bool,
    /// Threat move (from null move search)
    pub threat_move: Option<Move>,
    /// History score of current move
    pub history_score: i32,
    /// Excluded move (for singular extension)
    pub excluded_move: Option<Move>,
    /// Counter move (best response to this move)
    pub counter_move: Option<Move>,
    /// Quiet moves tried at this node (for history updates)
    pub quiet_moves: Vec<Move>,
    /// Consecutive check extensions count
    pub consecutive_checks: u8,
}

impl SearchStack {
    /// Create a new search stack entry for the given ply
    pub fn new(ply: u16) -> Self {
        Self {
            ply,
            ..Default::default()
        }
    }

    /// Update killers (convenience method)
    pub fn update_killers(&mut self, mv: Move) {
        // Killer moves should be quiet moves (non-captures)
        // Promotions are tactical moves and shouldn't be stored as killers
        if mv.is_capture_hint() || mv.is_promote() {
            return;
        }

        // Don't store the same move twice
        if self.killers[0] == Some(mv) {
            return;
        }

        // Shift killers and add new one
        self.killers[1] = self.killers[0];
        self.killers[0] = Some(mv);
    }

    /// Check if a move is a killer move
    pub fn is_killer(&self, mv: Move) -> bool {
        self.killers[0] == Some(mv) || self.killers[1] == Some(mv)
    }

    /// Clear for new node
    pub fn clear_for_new_node(&mut self) {
        self.current_move = None;
        self.move_count = 0;
        self.excluded_move = None;
        self.quiet_moves.clear();
        // Note: We keep killers, static_eval, threat_move, counter_move, consecutive_checks as they may be useful
    }

    /// Check if ply is within valid range for SearchStack access
    ///
    /// Since SearchStack is pre-allocated with MAX_PLY+1 elements,
    /// this ensures we don't access out of bounds.
    #[inline(always)]
    pub fn is_valid_ply(ply: u16) -> bool {
        ply <= crate::search::constants::MAX_PLY as u16
    }
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

    #[test]
    fn test_termination_reason_display() {
        assert_eq!(TerminationReason::TimeLimit.to_string(), "time_limit");
        assert_eq!(TerminationReason::NodeLimit.to_string(), "node_limit");
        assert_eq!(TerminationReason::DepthLimit.to_string(), "depth_limit");
        assert_eq!(TerminationReason::UserStop.to_string(), "user_stop");
        assert_eq!(TerminationReason::Mate.to_string(), "mate");
        assert_eq!(TerminationReason::Completed.to_string(), "completed");
        assert_eq!(TerminationReason::Error.to_string(), "error");
    }
}
