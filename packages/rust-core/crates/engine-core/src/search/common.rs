//! Common utilities for search algorithms

use super::constants::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
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

/// Common search limit checking functionality
#[derive(Default)]
pub struct LimitChecker {
    /// External stop flag
    pub stop_flag: Option<Arc<AtomicBool>>,
    /// Time limit
    pub time_limit: Option<Instant>,
    /// Node limit
    pub node_limit: Option<u64>,
}

impl LimitChecker {
    /// Create a new limit checker
    pub fn new() -> Self {
        Self::default()
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
    score.abs() >= MATE_SCORE - MAX_PLY as i32
}

/// Adjust mate score when storing in transposition table
/// Mate scores are relative to root, not to current position
#[inline]
pub fn adjust_mate_score_for_tt(score: i32, ply: u8) -> i32 {
    if score >= MATE_SCORE - MAX_PLY as i32 {
        score + ply as i32
    } else if score <= -MATE_SCORE + MAX_PLY as i32 {
        score - ply as i32
    } else {
        score
    }
}

/// Adjust mate score when retrieving from transposition table
#[inline]
pub fn adjust_mate_score_from_tt(score: i32, ply: u8) -> i32 {
    if score >= MATE_SCORE - MAX_PLY as i32 {
        score - ply as i32
    } else if score <= -MATE_SCORE + MAX_PLY as i32 {
        score + ply as i32
    } else {
        score
    }
}

/// Check if we're in the endgame based on piece count
#[inline]
pub fn is_endgame(total_pieces: u32) -> bool {
    total_pieces <= 20
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

        // Positive mate score
        let score = MATE_SCORE - 20;
        let adjusted = adjust_mate_score_for_tt(score, ply);
        assert_eq!(adjusted, score + ply as i32);
        assert_eq!(adjust_mate_score_from_tt(adjusted, ply), score);

        // Negative mate score
        let score = -MATE_SCORE + 20;
        let adjusted = adjust_mate_score_for_tt(score, ply);
        assert_eq!(adjusted, score - ply as i32);
        assert_eq!(adjust_mate_score_from_tt(adjusted, ply), score);

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
}
