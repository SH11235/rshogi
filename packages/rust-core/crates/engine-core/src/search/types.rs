//! Common types for search algorithms

use crate::shogi::Move;
use smallvec::SmallVec;
use std::sync::Arc;
use std::time::Duration;

/// Info callback type for search progress reporting
pub type InfoCallback = Arc<dyn Fn(u8, i32, u64, Duration, &[Move], NodeType) + Send + Sync>;

/// Iteration callback type for committed iteration results
pub type IterationCallback = Arc<dyn Fn(&CommittedIteration) + Send + Sync>;

/// Callback type for lightweight `info string` diagnostics.
pub type InfoStringCallback = Arc<dyn Fn(&str) + Send + Sync>;

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
    /// Number of late move reduction trials (optional)
    pub lmr_trials: Option<u64>,
    /// Aspiration window success count
    pub aspiration_hits: Option<u32>,
    /// Total re-searches performed
    pub re_searches: Option<u32>,
    /// Number of times PV head changed (root)
    pub pv_changed: Option<u32>,
    /// Duplication percentage for parallel search (0-100)
    pub duplication_percentage: Option<f64>,
    /// Number of check extensions applied
    pub check_extensions: Option<u64>,
    /// Number of king move extensions applied
    pub king_extensions: Option<u64>,
    /// Number of checking drops searched in quiescence search
    pub qs_check_drops: Option<u64>,
    /// Number of non-capture checking moves searched in qsearch
    pub qs_noncapture_checks: Option<u64>,
    /// Number of non-capture promotion checks searched in qsearch
    pub qs_promo_checks: Option<u64>,
    /// Number of PV owner mismatches detected
    pub pv_owner_mismatches: Option<u64>,
    /// Number of PV owner mismatch checks performed
    pub pv_owner_checks: Option<u64>,
    /// Number of PV trimming checks performed
    pub pv_trim_checks: Option<u64>,
    /// Number of times PV was actually trimmed
    pub pv_trim_cuts: Option<u64>,
    /// Root-level fail-high occurrences (for diagnostics)
    pub root_fail_high_count: Option<u64>,
    /// Root TT hint existed at the start of the final iteration (diagnostic 0/1)
    pub root_tt_hint_exists: Option<u64>,
    /// Root TT hint was used as the final best move in the final iteration (diagnostic 0/1)
    pub root_tt_hint_used: Option<u64>,
}

impl SearchStats {
    /// Helper function to increment an optional counter with overflow protection
    #[inline]
    pub fn bump(opt: &mut Option<u64>, by: u64) {
        let cur = opt.unwrap_or(0);
        *opt = Some(cur.saturating_add(by));
    }
}

/// Bound kind for root lines (alias to NodeType for clarity)
pub type Bound = NodeType;

/// Teacher profile for conservative/aggressive pruning policy during data labeling
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeacherProfile {
    /// Most conservative pruning (prefer safety and verification)
    Safe,
    /// Balanced pruning (default for teacher data)
    Balanced,
    /// More aggressive pruning (faster, less verification)
    Aggressive,
}

/// Root line representation for MultiPV (index 0 is the best line)
#[derive(Clone, Debug)]
pub struct RootLine {
    /// 1-based MultiPV index (USI compatible)
    pub multipv_index: u8,
    /// Root move of this line
    pub root_move: Move,
    /// Engine internal score (mate distances retained)
    pub score_internal: i32,
    /// Evaluation score in centipawns (side to move, clipped for output)
    pub score_cp: i32,
    /// Bound type (Exact/LowerBound/UpperBound)
    pub bound: Bound,
    /// Depth reached for this line
    pub depth: u32,
    /// Selective depth for this line (if available)
    pub seldepth: Option<u8>,
    /// Principal variation for this root move
    pub pv: SmallVec<[Move; 32]>,
    /// Optional meta: nodes counted when producing this line
    pub nodes: Option<u64>,
    /// Optional meta: time in milliseconds spent on this line
    pub time_ms: Option<u64>,
    /// Optional meta: nodes per second for this line (permits logging without recompute)
    pub nps: Option<u64>,
    /// If true, we failed to exactify due to budget/time limits
    pub exact_exhausted: bool,
    /// Reason for exhaustion (e.g., "budget", "timeout")
    pub exhaust_reason: Option<String>,
    /// Mate distance if score represents mate
    pub mate_distance: Option<i32>,
}

/// Search result
#[derive(Clone, Debug)]
pub struct SearchResult {
    /// Best move found
    pub best_move: Option<Move>,
    /// Optional ponder move (second move of best PV)
    pub ponder: Option<Move>,
    /// Final depth reached (aggregated from stats)
    pub depth: u32,
    /// Final selective depth (aggregated from stats, falls back to depth)
    pub seldepth: u32,
    /// Total nodes searched (aggregated from stats)
    pub nodes: u64,
    /// Nodes per second (derived from stats.elapsed/nodes)
    pub nps: u64,
    /// Hash table occupancy estimate (permille)
    pub hashfull: u32,
    /// Termination reason (mirrors stop_info.reason when available)
    pub end_reason: TerminationReason,
    /// Evaluation score (from side to move perspective)
    pub score: i32,
    /// Search statistics
    pub stats: SearchStats,
    /// Node type (Exact, LowerBound, UpperBound)
    pub node_type: NodeType,
    /// Information about why the search stopped (None for legacy compatibility)
    pub stop_info: Option<StopInfo>,
    /// MultiPV lines (index 0 is best). None means unavailable.
    pub lines: Option<SmallVec<[RootLine; 4]>>,
}

impl SearchResult {
    /// Create a new search result
    pub fn new(best_move: Option<Move>, score: i32, stats: SearchStats) -> Self {
        Self {
            best_move,
            ponder: None,
            depth: stats.depth as u32,
            seldepth: stats.seldepth.map(|v| v as u32).unwrap_or(stats.depth as u32),
            nodes: stats.nodes,
            nps: compute_nps(stats.nodes, stats.elapsed),
            hashfull: 0,
            end_reason: TerminationReason::Completed,
            score,
            stats,
            node_type: NodeType::Exact, // Default to Exact for backward compatibility
            stop_info: None,
            lines: None,
        }
    }

    /// Create a new search result with node type
    pub fn with_node_type(
        best_move: Option<Move>,
        score: i32,
        stats: SearchStats,
        node_type: NodeType,
    ) -> Self {
        let mut result = Self::new(best_move, score, stats);
        result.node_type = node_type;
        result
    }

    /// Create a new search result with node type and stop info
    pub fn with_stop_info(
        best_move: Option<Move>,
        score: i32,
        stats: SearchStats,
        node_type: NodeType,
        stop_info: StopInfo,
    ) -> Self {
        let mut result = Self::with_node_type(best_move, score, stats, node_type);
        result.stop_info = Some(stop_info.clone());
        result.end_reason = stop_info.reason;
        result
    }

    /// Create a search result from legacy format (Option<Move>, i32)
    pub fn from_legacy(
        move_score: (Option<Move>, i32),
        nodes: u64,
        elapsed: Duration,
        pv: Vec<Move>,
        depth: u8,
    ) -> Self {
        let stats = SearchStats {
            nodes,
            qnodes: 0,
            elapsed,
            pv,
            depth,
            ..Default::default()
        };
        Self::new(move_score.0, move_score.1, stats)
    }

    /// Create a search result from MultiPV lines (index 0 is best)
    pub fn from_lines(lines: SmallVec<[RootLine; 4]>, mut stats: SearchStats) -> Self {
        let (best_move, score, node_type) = if let Some(first) = lines.first() {
            // Sync legacy fields with the best line
            let mv = first.pv.first().copied().or(Some(first.root_move));
            // Publish PV into stats for backward compatibility
            stats.pv = first.pv.iter().copied().collect();
            (mv, first.score_cp, first.bound)
        } else {
            (None, 0, NodeType::Exact)
        };

        let mut result = Self::with_node_type(best_move, score, stats, node_type);
        result.lines = Some(lines);
        result.refresh_summary();
        result
    }

    /// Compose a SearchResult with all primary fields and optional lines.
    /// Centralizes initialization to be resilient to future struct changes.
    pub fn compose(
        best_move: Option<Move>,
        score: i32,
        stats: SearchStats,
        node_type: NodeType,
        stop_info: Option<StopInfo>,
        lines: Option<SmallVec<[RootLine; 4]>>,
    ) -> Self {
        let mut result = Self::with_node_type(best_move, score, stats, node_type);
        result.stop_info = stop_info.clone();
        if let Some(info) = stop_info {
            result.end_reason = info.reason;
        }
        result.lines = lines;
        result.refresh_summary();
        result
    }

    /// Recompute derived summary fields (ponder, depth, seldepth, nps, end_reason).
    pub fn refresh_summary(&mut self) {
        if let Some(lines) = self.lines.as_ref() {
            if let Some(first) = lines.first() {
                self.ponder = first.pv.get(1).copied();
            }
        } else if self.ponder.is_none() {
            self.ponder = self.stats.pv.get(1).copied();
        }
        // depth/seldepth/nodes were already set from stats but keep them in sync in case stats mutated
        self.depth = self.stats.depth as u32;
        self.seldepth = self.stats.seldepth.map(|v| v as u32).unwrap_or(self.depth);
        self.nodes = self.stats.nodes;
        self.nps = compute_nps(self.stats.nodes, self.stats.elapsed);
        if let Some(ref info) = self.stop_info {
            self.end_reason = info.reason;
        }
    }
}

fn compute_nps(nodes: u64, elapsed: Duration) -> u64 {
    let elapsed_ms = elapsed.as_millis() as u64;
    if elapsed_ms == 0 {
        0
    } else {
        nodes.saturating_mul(1000).saturating_div(elapsed_ms.max(1))
    }
}

/// A committed iteration result emitted by the searcher
#[derive(Clone, Debug)]
pub struct CommittedIteration {
    /// Depth reached at this iteration
    pub depth: u8,
    /// Selective depth (optional)
    pub seldepth: Option<u8>,
    /// Evaluation score (engine internal scale)
    pub score: i32,
    /// Principal variation (non-empty)
    pub pv: Vec<Move>,
    /// Node type (Exact/UpperBound/LowerBound)
    pub node_type: NodeType,
    /// Total nodes searched so far (across threads)
    pub nodes: u64,
    /// Elapsed time since search start
    pub elapsed: Duration,
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
    /// Soft time limit in ms (0 if not applicable)
    pub soft_limit_ms: u64,
    /// Hard time limit in ms (0 if not applicable)
    pub hard_limit_ms: u64,
}

impl Default for StopInfo {
    fn default() -> Self {
        Self {
            reason: TerminationReason::Completed,
            elapsed_ms: 0,
            nodes: 0,
            depth_reached: 0,
            hard_timeout: false,
            soft_limit_ms: 0,
            hard_limit_ms: 0,
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
    /// Principal variation suffix from this ply
    pub pv_line: SmallVec<[Move; 16]>,
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
        self.pv_line.clear();
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
        assert_eq!(result.depth, 5);
        assert_eq!(result.seldepth, 5);
        assert_eq!(result.nodes, 1000);
        assert_eq!(result.nps, 10_000);
        assert_eq!(result.hashfull, 0);
        assert_eq!(result.end_reason, TerminationReason::Completed);
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
