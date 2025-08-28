//! Common utilities for search algorithms

use super::constants::*;
use super::types::{StopInfo, TerminationReason};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// Common search context trait for shared functionality
pub trait SearchContext {
    /// Get current node count
    fn nodes(&self) -> u64;

    /// Increment node count
    fn increment_nodes(&mut self);

    /// Get search start time
    fn start_time(&self) -> Instant;

    /// Check if search should stop
    fn should_stop(&self) -> bool;

    /// Increment node count and check if search should stop
    /// Returns true if search can continue, false if it should stop
    #[inline]
    fn bump_nodes_and_check(&mut self) -> bool {
        // Check limits BEFORE incrementing to avoid exceeding
        if self.should_stop() {
            return false;
        }
        self.increment_nodes();
        true
    }
}

/// Shared stop information that can be set once
pub struct SharedStopInfo {
    inner: OnceLock<StopInfo>,
}

impl SharedStopInfo {
    /// Create a new shared stop info
    pub fn new() -> Self {
        Self {
            inner: OnceLock::new(),
        }
    }

    /// Try to set stop info (only first call succeeds)
    pub fn try_set(&self, info: StopInfo) -> bool {
        self.inner.set(info).is_ok()
    }

    /// Get the stop info if set
    pub fn get(&self) -> Option<&StopInfo> {
        self.inner.get()
    }

    /// Create an Arc<SharedStopInfo>
    pub fn new_arc() -> Arc<Self> {
        Arc::new(Self::new())
    }
}

impl Default for SharedStopInfo {
    fn default() -> Self {
        Self::new()
    }
}

/// Common search limit checking functionality
pub struct LimitChecker {
    /// External stop flag
    pub stop_flag: Option<Arc<AtomicBool>>,
    /// Time limit
    pub time_limit: Option<Instant>,
    /// Node limit
    pub node_limit: Option<u64>,
    /// Shared stop info for recording termination reason
    pub stop_info: Arc<SharedStopInfo>,
}

impl LimitChecker {
    /// Create a new limit checker
    pub fn new() -> Self {
        Self {
            stop_flag: None,
            time_limit: None,
            node_limit: None,
            stop_info: SharedStopInfo::new_arc(),
        }
    }

    /// Create with shared stop info
    pub fn with_stop_info(stop_info: Arc<SharedStopInfo>) -> Self {
        Self {
            stop_flag: None,
            time_limit: None,
            node_limit: None,
            stop_info,
        }
    }

    /// Check if any limit has been exceeded
    #[inline(always)]
    pub fn should_stop(&self, nodes: u64, current_time: Instant) -> bool {
        // Check external stop flag
        if let Some(ref stop_flag) = self.stop_flag {
            if stop_flag.load(Ordering::Acquire) {
                return true;
            }
        }

        // Check node limit
        if let Some(max_nodes) = self.node_limit {
            if nodes >= max_nodes {
                return true;
            }
        }

        // Check time limit
        if let Some(time_limit) = self.time_limit {
            if current_time >= time_limit {
                return true;
            }
        }

        false
    }

    /// Check if any limit has been exceeded and record the reason
    #[inline(always)]
    pub fn should_stop_with_reason(
        &self,
        nodes: u64,
        current_time: Instant,
        start_time: Instant,
        depth: u8,
        hard_timeout: bool,
    ) -> bool {
        // Priority order: UserStop > TimeLimit > NodeLimit > DepthLimit

        // Check external stop flag (highest priority)
        if let Some(ref stop_flag) = self.stop_flag {
            if stop_flag.load(Ordering::Acquire) {
                let elapsed_ms = start_time.elapsed().as_millis() as u64;
                self.stop_info.try_set(StopInfo {
                    reason: TerminationReason::UserStop,
                    elapsed_ms,
                    nodes,
                    depth_reached: depth,
                    hard_timeout: false,
                    soft_limit_ms: 0,
                    hard_limit_ms: 0,
                });
                return true;
            }
        }

        // Check time limit
        if let Some(time_limit) = self.time_limit {
            if current_time >= time_limit {
                let elapsed_ms = start_time.elapsed().as_millis() as u64;
                self.stop_info.try_set(StopInfo {
                    reason: TerminationReason::TimeLimit,
                    elapsed_ms,
                    nodes,
                    depth_reached: depth,
                    hard_timeout,
                    soft_limit_ms: 0,
                    hard_limit_ms: 0,
                });
                return true;
            }
        }

        // Check node limit
        if let Some(max_nodes) = self.node_limit {
            if nodes >= max_nodes {
                let elapsed_ms = start_time.elapsed().as_millis() as u64;
                self.stop_info.try_set(StopInfo {
                    reason: TerminationReason::NodeLimit,
                    elapsed_ms,
                    nodes,
                    depth_reached: depth,
                    hard_timeout: false,
                    soft_limit_ms: 0,
                    hard_limit_ms: 0,
                });
                return true;
            }
        }

        false
    }
}

impl Default for LimitChecker {
    fn default() -> Self {
        Self::new()
    }
}

/// Apply mate distance pruning to alpha-beta bounds
/// Returns true if the position can be pruned
#[inline]
pub fn mate_distance_pruning(alpha: &mut i32, beta: &mut i32, ply: u8) -> bool {
    // We can't find a mate closer than the current ply
    *alpha = (*alpha).max(-MATE_SCORE + ply as i32);
    // Opponent can't find a mate closer than the next ply
    *beta = (*beta).min(MATE_SCORE - ply as i32 - 1);

    // If alpha >= beta, we can prune
    *alpha >= *beta
}

/// Calculate mate score for the given ply
/// Negative for getting mated, positive for giving mate
#[inline]
pub fn mate_score(ply: u8, is_giving_mate: bool) -> i32 {
    if is_giving_mate {
        MATE_SCORE - ply as i32
    } else {
        -MATE_SCORE + ply as i32
    }
}

/// Check if a score represents a mate
#[inline]
pub fn is_mate_score(score: i32) -> bool {
    score.abs() >= MATE_SCORE - MAX_PLY as i32 && score.abs() < SEARCH_INF
}

/// Adjust mate score when storing in transposition table
/// Mate scores are relative to root, not to current position
///
/// When storing:
/// - Winning mate: score represents "mate in N plies from current position"
///   To convert to root-relative: subtract current ply (mate gets further from root)
/// - Losing mate: score represents "mated in N plies from current position"  
///   To convert to root-relative: add current ply (mate gets further from root)
#[inline]
pub fn adjust_mate_score_for_tt(score: i32, ply: u8) -> i32 {
    if !is_mate_score(score) {
        score
    } else if score > 0 {
        // Winning mate: MATE_SCORE - distance_from_current
        // Root-relative: MATE_SCORE - (distance_from_current + ply)
        score - ply as i32
    } else {
        // Losing mate: -MATE_SCORE + distance_from_current
        // Root-relative: -MATE_SCORE + (distance_from_current + ply)
        score + ply as i32
    }
}

/// Adjust mate score when retrieving from transposition table
/// Converts from root-relative back to current-position-relative
///
/// When retrieving:
/// - Winning mate: stored as "mate in N plies from root"
///   To convert to current-relative: add current ply (mate gets closer)
/// - Losing mate: stored as "mated in N plies from root"
///   To convert to current-relative: subtract current ply (mate gets closer)
#[inline]
pub fn adjust_mate_score_from_tt(score: i32, ply: u8) -> i32 {
    if !is_mate_score(score) {
        score
    } else if score > 0 {
        // Winning mate: root-relative to current-relative
        score + ply as i32
    } else {
        // Losing mate: root-relative to current-relative
        score - ply as i32
    }
}

/// Check if we're in the endgame based on piece count
#[inline]
pub fn is_endgame(total_pieces: u32) -> bool {
    total_pieces <= 20
}

/// Get mate distance from a mate score
/// Returns None if not a mate score
#[inline]
pub fn get_mate_distance(score: i32) -> Option<i32> {
    if is_mate_score(score) {
        // Ensure non-negative result (guard against invalid scores)
        Some((MATE_SCORE - score.abs()).max(0))
    } else {
        None
    }
}

/// Validate that a root-relative mate score makes sense for the given ply
///
/// For root-relative mate scores, the mate distance from root must be at least
/// the current ply (can't find mate closer than current position from root).
///
/// Returns true if the mate distance is reasonable
#[inline]
pub fn validate_root_relative_mate_score(score: i32, ply: u8) -> bool {
    if let Some(distance) = get_mate_distance(score) {
        // Mate distance should be at least the current ply
        // (can't find mate closer than current position)
        distance >= ply as i32
    } else {
        // Not a mate score - always valid
        true
    }
}
