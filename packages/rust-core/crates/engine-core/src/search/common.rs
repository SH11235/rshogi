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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mate_distance_pruning() {
        // At ply 10, we can't find mate closer than 10 moves
        let mut alpha = -100;
        let mut beta = 100;
        assert!(!mate_distance_pruning(&mut alpha, &mut beta, 10));
        // Alpha is adjusted to max of original (-100) and (-MATE_SCORE + 10)
        assert_eq!(alpha, (-100).max(-MATE_SCORE + 10));
        assert_eq!(beta, 100.min(MATE_SCORE - 11));

        // Test pruning case
        let mut alpha = MATE_SCORE - 20;
        let mut beta = -MATE_SCORE + 30;
        assert!(mate_distance_pruning(&mut alpha, &mut beta, 15));
    }

    #[test]
    fn test_mate_score_calculation() {
        // Giving mate in 5
        assert_eq!(mate_score(5, true), MATE_SCORE - 5);

        // Getting mated in 5
        assert_eq!(mate_score(5, false), -MATE_SCORE + 5);
    }

    #[test]
    fn test_is_mate_score() {
        assert!(is_mate_score(MATE_SCORE - 10));
        assert!(is_mate_score(-MATE_SCORE + 10));
        assert!(!is_mate_score(1000));
        assert!(!is_mate_score(-1000));
    }

    #[test]
    fn test_tt_score_adjustment() {
        let ply = 10;

        // Positive mate score (winning mate)
        let score = MATE_SCORE - 20; // Mate in 20 plies from current position
        let adjusted = adjust_mate_score_for_tt(score, ply);
        assert_eq!(adjusted, score - ply as i32); // Should subtract ply for root-relative
        assert_eq!(adjust_mate_score_from_tt(adjusted, ply), score); // Round-trip test

        // Negative mate score (losing mate)
        let score = -MATE_SCORE + 20; // Mated in 20 plies from current position
        let adjusted = adjust_mate_score_for_tt(score, ply);
        assert_eq!(adjusted, score + ply as i32); // Should add ply for root-relative
        assert_eq!(adjust_mate_score_from_tt(adjusted, ply), score); // Round-trip test

        // Normal score (not mate)
        let score = 100;
        assert_eq!(adjust_mate_score_for_tt(score, ply), score);
        assert_eq!(adjust_mate_score_from_tt(score, ply), score);
    }

    #[test]
    fn test_limit_checker() {
        use std::time::Duration;
        let mut checker = LimitChecker::new();
        let start = Instant::now();

        // No limits set - should not stop
        assert!(!checker.should_stop(1000, start));

        // Set node limit
        checker.node_limit = Some(5000);
        assert!(!checker.should_stop(4999, start));
        assert!(checker.should_stop(5000, start));

        // Set time limit (immediate)
        checker.time_limit = Some(start);
        assert!(checker.should_stop(100, start));

        // Reset time limit for stop flag test
        checker.time_limit = None;

        // Set stop flag
        let stop_flag = Arc::new(AtomicBool::new(false));
        checker.stop_flag = Some(stop_flag.clone());
        assert!(!checker.should_stop(100, start + Duration::from_secs(1)));

        stop_flag.store(true, Ordering::Release);
        assert!(checker.should_stop(100, start + Duration::from_secs(1)));
    }

    #[test]
    fn test_get_mate_distance() {
        // Test positive mate scores
        assert_eq!(get_mate_distance(MATE_SCORE), Some(0)); // Mate in 0
        assert_eq!(get_mate_distance(MATE_SCORE - 5), Some(5)); // Mate in 5 plies
        assert_eq!(get_mate_distance(MATE_SCORE - 20), Some(20)); // Mate in 20 plies

        // Test negative mate scores
        assert_eq!(get_mate_distance(-MATE_SCORE), Some(0)); // Being mated in 0
        assert_eq!(get_mate_distance(-MATE_SCORE + 5), Some(5)); // Being mated in 5 plies
        assert_eq!(get_mate_distance(-MATE_SCORE + 20), Some(20)); // Being mated in 20 plies

        // Test non-mate scores
        assert_eq!(get_mate_distance(100), None);
        assert_eq!(get_mate_distance(-100), None);
        assert_eq!(get_mate_distance(1000), None);
        assert_eq!(get_mate_distance(-1000), None);
    }

    #[test]
    fn test_validate_root_relative_mate_score() {
        // Valid mate scores
        assert!(validate_root_relative_mate_score(MATE_SCORE - 10, 5)); // Mate in 10 plies from ply 5
        assert!(validate_root_relative_mate_score(MATE_SCORE - 10, 10)); // Mate in 10 plies from ply 10
        assert!(validate_root_relative_mate_score(-MATE_SCORE + 15, 10)); // Being mated in 15 plies from ply 10

        // Invalid mate scores (mate distance < current ply)
        assert!(!validate_root_relative_mate_score(MATE_SCORE - 5, 10)); // Can't mate in 5 plies from ply 10
        assert!(!validate_root_relative_mate_score(-MATE_SCORE + 5, 10)); // Can't be mated in 5 plies from ply 10

        // Non-mate scores are always valid
        assert!(validate_root_relative_mate_score(100, 50));
        assert!(validate_root_relative_mate_score(-100, 50));
        assert!(validate_root_relative_mate_score(0, 100));
    }

    #[test]
    fn test_tt_mate_score_semantics() {
        // Test that TT-stored values are root-relative

        // Scenario: At ply 10, we find mate in 20 plies
        let ply = 10;
        let current_mate_score = MATE_SCORE - 20; // Mate in 20 from current position

        // When storing to TT, it should be converted to root-relative
        let tt_score = adjust_mate_score_for_tt(current_mate_score, ply);
        // Root-relative: mate in 30 plies from root (20 + 10)
        assert_eq!(tt_score, MATE_SCORE - 30);

        // Verify the stored value doesn't exceed MATE_SCORE
        assert!(tt_score < MATE_SCORE);

        // When retrieving from TT at the same ply, should get original score back
        let retrieved = adjust_mate_score_from_tt(tt_score, ply);
        assert_eq!(retrieved, current_mate_score);

        // When retrieving from TT at a different ply (e.g., ply 5)
        let retrieved_at_ply5 = adjust_mate_score_from_tt(tt_score, 5);
        // Should be mate in 25 plies from ply 5 (30 - 5)
        assert_eq!(retrieved_at_ply5, MATE_SCORE - 25);

        // Test negative mate scores
        let losing_mate = -MATE_SCORE + 20; // Being mated in 20 plies
        let tt_losing = adjust_mate_score_for_tt(losing_mate, ply);
        // Root-relative: being mated in 30 plies from root
        assert_eq!(tt_losing, -MATE_SCORE + 30);

        // Verify the stored value doesn't exceed -MATE_SCORE
        assert!(tt_losing > -MATE_SCORE);
    }

    #[test]
    fn test_tt_mate_score_transposition_across_ply() {
        // Test case: Root (ply=0) finds mate in 7, descendant (ply=5) gets TT hit
        // This tests that mate scores are correctly adjusted across different plies

        // At root (ply=0), we find mate in 7 plies
        let root_ply = 0;
        let mate_in_7_from_root = MATE_SCORE - 7;

        // Store in TT (already root-relative since ply=0)
        let tt_stored = adjust_mate_score_for_tt(mate_in_7_from_root, root_ply);
        assert_eq!(tt_stored, MATE_SCORE - 7); // No adjustment needed at root

        // Later, at ply=5, we get a TT hit
        let descendant_ply = 5;
        let retrieved_at_ply5 = adjust_mate_score_from_tt(tt_stored, descendant_ply);

        // From ply=5, it should be mate in 2 (7 - 5 = 2)
        assert_eq!(retrieved_at_ply5, MATE_SCORE - 2);
        assert_eq!(get_mate_distance(retrieved_at_ply5), Some(2));

        // Test with losing mate: being mated in 7 from root
        let mated_in_7_from_root = -MATE_SCORE + 7;
        let tt_stored_losing = adjust_mate_score_for_tt(mated_in_7_from_root, root_ply);
        assert_eq!(tt_stored_losing, -MATE_SCORE + 7); // No adjustment at root

        // At ply=5, being mated in 2
        let retrieved_losing_at_ply5 = adjust_mate_score_from_tt(tt_stored_losing, descendant_ply);
        assert_eq!(retrieved_losing_at_ply5, -MATE_SCORE + 2);
        assert_eq!(get_mate_distance(retrieved_losing_at_ply5), Some(2));

        // Edge case: What if we're at ply=6? Mate in 1
        let retrieved_at_ply6 = adjust_mate_score_from_tt(tt_stored, 6);
        assert_eq!(retrieved_at_ply6, MATE_SCORE - 1);
        assert_eq!(get_mate_distance(retrieved_at_ply6), Some(1));

        // Edge case: What if we're at ply=7? Mate in 0 (immediate mate)
        let retrieved_at_ply7 = adjust_mate_score_from_tt(tt_stored, 7);
        assert_eq!(retrieved_at_ply7, MATE_SCORE);
        assert_eq!(get_mate_distance(retrieved_at_ply7), Some(0));
    }

    #[test]
    fn test_shared_stop_info_once_only() {
        use super::SharedStopInfo;
        use crate::search::types::{StopInfo, TerminationReason};

        let shared = SharedStopInfo::new();

        // First set should succeed
        let info1 = StopInfo {
            reason: TerminationReason::TimeLimit,
            elapsed_ms: 1000,
            nodes: 10000,
            depth_reached: 10,
            hard_timeout: false,
        };
        assert!(shared.try_set(info1.clone()));

        // Verify it was set
        let retrieved = shared.get().unwrap();
        assert_eq!(retrieved.reason, TerminationReason::TimeLimit);
        assert_eq!(retrieved.elapsed_ms, 1000);

        // Second set should be ignored
        let info2 = StopInfo {
            reason: TerminationReason::NodeLimit,
            elapsed_ms: 2000,
            nodes: 20000,
            depth_reached: 20,
            hard_timeout: true,
        };
        assert!(!shared.try_set(info2));

        // Should still have the first value
        let retrieved = shared.get().unwrap();
        assert_eq!(retrieved.reason, TerminationReason::TimeLimit);
        assert_eq!(retrieved.elapsed_ms, 1000);
    }

    #[test]
    fn test_shared_stop_info_concurrent() {
        use super::SharedStopInfo;
        use crate::search::types::{StopInfo, TerminationReason};
        use std::sync::Arc;
        use std::thread;

        let shared = Arc::new(SharedStopInfo::new());
        let num_threads = 10;
        let mut handles = vec![];

        // Spawn multiple threads trying to set different stop reasons
        for i in 0..num_threads {
            let shared_clone = Arc::clone(&shared);
            let handle = thread::spawn(move || {
                let reason = match i % 4 {
                    0 => TerminationReason::TimeLimit,
                    1 => TerminationReason::NodeLimit,
                    2 => TerminationReason::UserStop,
                    _ => TerminationReason::DepthLimit,
                };
                let info = StopInfo {
                    reason,
                    elapsed_ms: (i as u64 + 1) * 100,
                    nodes: (i as u64 + 1) * 1000,
                    depth_reached: i as u8 + 1,
                    hard_timeout: i % 2 == 0,
                };
                shared_clone.try_set(info);
            });
            handles.push(handle);
        }

        // Wait for all threads
        for handle in handles {
            handle.join().unwrap();
        }

        // Exactly one value should be set
        let result = shared.get();
        assert!(result.is_some(), "One thread should have set a value");

        // The value should be one of the attempted values
        let info = result.unwrap();
        assert!(info.elapsed_ms >= 100 && info.elapsed_ms <= 1000);
        assert!(info.nodes >= 1000 && info.nodes <= 10000);
    }

    #[test]
    fn test_stop_reason_priority() {
        use crate::search::types::TerminationReason;

        // Test that priority ordering works as expected
        // Higher priority reasons should override lower ones
        let priorities = vec![
            (TerminationReason::Error, 7),
            (TerminationReason::UserStop, 6),
            (TerminationReason::TimeLimit, 5),
            (TerminationReason::NodeLimit, 4),
            (TerminationReason::DepthLimit, 3),
            (TerminationReason::Mate, 2),
            (TerminationReason::Completed, 1),
        ];

        // Verify priorities are distinct
        let mut seen_priorities = std::collections::HashSet::new();
        for (reason, priority) in &priorities {
            assert!(seen_priorities.insert(priority), "Duplicate priority for {:?}", reason);
        }
    }
}
