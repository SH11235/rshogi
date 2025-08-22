//! Aspiration window management for iterative deepening
//!
//! This module handles dynamic aspiration window calculation based on
//! position volatility to minimize re-searches while maximizing cutoffs.

use crate::search::constants::{
    ASPIRATION_RETRY_LIMIT, ASPIRATION_WINDOW_DELTA, ASPIRATION_WINDOW_EXPANSION,
    ASPIRATION_WINDOW_INITIAL, ASPIRATION_WINDOW_MAX_VOLATILITY_ADJUSTMENT, MATE_SCORE, SEARCH_INF,
};
use crate::search::types::NodeType;

/// Minimum aspiration window size to ensure non-empty bounds
const MIN_ASPIRATION_WINDOW: i32 = 2;

/// Manages aspiration window calculation and score history
#[derive(Debug, Clone)]
pub struct AspirationWindow {
    /// Evaluation history for each depth (for dynamic aspiration window)
    /// Only EXACT root scores are stored - boundary values (LOWERBOUND/UPPERBOUND) are excluded
    score_history: Vec<i32>,

    /// Score volatility measurement for window adjustment
    score_volatility: i32,
}

impl AspirationWindow {
    /// Create a new aspiration window manager
    pub fn new() -> Self {
        Self {
            score_history: Vec::with_capacity(crate::search::constants::MAX_PLY),
            score_volatility: 0,
        }
    }

    /// Create a minimum window centered on the given score
    ///
    /// This helper ensures the window width is exactly MIN_ASPIRATION_WINDOW,
    /// even when MIN is odd (by splitting asymmetrically)
    #[inline]
    fn center_min_window(best_score: i32) -> (i32, i32) {
        let lower = MIN_ASPIRATION_WINDOW / 2;
        let upper = MIN_ASPIRATION_WINDOW - lower; // Ensures lower + upper = MIN

        let lo = best_score.saturating_sub(lower).max(-SEARCH_INF);
        let hi = best_score.saturating_add(upper).min(SEARCH_INF);

        if lo >= hi {
            // Handle boundary cases where clamping collapsed the window
            if hi == SEARCH_INF {
                (SEARCH_INF.saturating_sub(MIN_ASPIRATION_WINDOW), SEARCH_INF)
            } else if lo == -SEARCH_INF {
                (-SEARCH_INF, (-SEARCH_INF).saturating_add(MIN_ASPIRATION_WINDOW))
            } else {
                // Fallback: expand upward by minimum window
                (lo, lo.saturating_add(MIN_ASPIRATION_WINDOW).min(SEARCH_INF))
            }
        } else {
            (lo, hi)
        }
    }

    /// Ensure bounds are valid and maintain minimum window size
    ///
    /// This helper clamps bounds to valid range and ensures non-empty window
    #[inline]
    fn ensure_valid_bounds(mut lo: i32, mut hi: i32, best_score: i32) -> (i32, i32) {
        // First, clamp all values to valid range
        let clamped_best_score = best_score.clamp(-SEARCH_INF, SEARCH_INF);
        lo = lo.clamp(-SEARCH_INF, SEARCH_INF);
        hi = hi.clamp(-SEARCH_INF, SEARCH_INF);

        // Check if window is valid
        if lo < hi && hi - lo >= MIN_ASPIRATION_WINDOW {
            return (lo, hi);
        }

        // Window is invalid - need to fix it
        if clamped_best_score == SEARCH_INF {
            // Best score at upper boundary
            (SEARCH_INF.saturating_sub(MIN_ASPIRATION_WINDOW), SEARCH_INF)
        } else if clamped_best_score == -SEARCH_INF {
            // Best score at lower boundary
            (-SEARCH_INF, (-SEARCH_INF).saturating_add(MIN_ASPIRATION_WINDOW))
        } else {
            // Best score is within range - center minimum window on it
            Self::center_min_window(clamped_best_score)
        }
    }

    /// Clear the aspiration window state
    pub fn clear(&mut self) {
        self.score_history.clear();
        self.score_volatility = 0;
    }

    /// Update score history and recalculate volatility
    /// Only EXACT node scores are added to maintain PV stability accuracy
    ///
    /// # Parameters
    /// - `score`: The root score from the current iteration
    /// - `node_type`: The final node type classification for this iteration's root search
    ///   (not the TT entry type, but the result of the aspiration window search)
    pub fn update_score(&mut self, score: i32, node_type: NodeType) {
        // Only add EXACT node scores to history
        // This prevents boundary values (LOWERBOUND/UPPERBOUND) from affecting volatility
        if node_type == NodeType::Exact {
            self.score_history.push(score);
            if self.score_history.len() > 1 {
                self.score_volatility = self.calculate_score_volatility();
            }
        }
    }

    /// Get the number of scores in history
    pub fn history_len(&self) -> usize {
        self.score_history.len()
    }

    /// Calculate dynamic aspiration window based on score history
    ///
    /// The window size adapts to position characteristics:
    /// - Stable positions: Use narrow windows for more cutoffs
    /// - Volatile positions: Use wider windows to avoid re-searches
    ///
    /// The formula scales the base window by a fraction of score volatility,
    /// capped at 4x the initial window to maintain search efficiency.
    pub fn calculate_window(&self, depth: u8) -> i32 {
        // Use base window for early depths where we lack history
        if depth <= 2 || self.score_history.len() < 2 {
            return ASPIRATION_WINDOW_INITIAL;
        }

        // Use cached volatility value (already calculated when score history was updated)
        // This avoids redundant calculation
        let volatility = self.score_volatility;

        // Adjust window based on volatility
        // - Divide by 4 to scale volatility to window size (empirically determined)
        // - Cap at max adjustment to prevent excessively wide windows
        // - This ensures window stays in reasonable range: [INITIAL, INITIAL + MAX_ADJUSTMENT]
        ASPIRATION_WINDOW_INITIAL
            + (volatility / 4).min(ASPIRATION_WINDOW_MAX_VOLATILITY_ADJUSTMENT)
    }

    /// Calculate initial aspiration bounds for a given depth
    ///
    /// Returns (alpha, beta) window around the best score
    pub fn get_initial_bounds(&self, depth: u8, best_score: i32) -> (i32, i32) {
        if depth <= 1 {
            // First depth - use full window
            return (-SEARCH_INF, SEARCH_INF);
        }

        // Check if this is a mate score
        let mate_threshold = MATE_SCORE - crate::search::constants::MAX_PLY as i32;
        if best_score.abs() >= mate_threshold {
            // For mate scores, use a special narrow window
            // The window size depends on how close the mate is
            let mate_distance = MATE_SCORE - best_score.abs();

            // Use narrower windows for closer mates
            let window = if mate_distance <= 10 {
                // Very close to mate - use minimal window
                5
            } else if mate_distance <= 20 {
                // Near mate - use small window
                10
            } else {
                // Distant mate - use moderate window
                20
            };

            // For winning mates, we primarily care about finding shorter mates
            // For losing mates, we primarily care about finding longer mates
            let (lo, hi) = if best_score > 0 {
                // We're winning - look for better (shorter) mates
                // Use saturating arithmetic to prevent overflow
                (
                    best_score.saturating_sub(window),
                    best_score.saturating_add(window.saturating_mul(2)),
                )
            } else {
                // We're losing - look for ways to delay mate
                // Use saturating arithmetic to prevent overflow
                (
                    best_score.saturating_sub(window.saturating_mul(2)),
                    best_score.saturating_add(window),
                )
            };

            // Ensure bounds are valid and maintain minimum window size
            let (lo, hi) = Self::ensure_valid_bounds(lo, hi, best_score);

            // Debug assertions to verify invariants
            debug_assert!(lo < hi, "Window must be non-empty: lo={lo}, hi={hi}");
            debug_assert!(
                hi - lo >= MIN_ASPIRATION_WINDOW,
                "Window size must be at least {MIN_ASPIRATION_WINDOW}"
            );
            debug_assert!(
                lo >= -SEARCH_INF && hi <= SEARCH_INF,
                "Bounds must be within valid range"
            );

            (lo, hi)
        } else {
            // Normal (non-mate) score - use dynamic window based on score history
            let window = self.calculate_window(depth);

            // Use saturating arithmetic to prevent overflow
            let lo = best_score.saturating_sub(window);
            let hi = best_score.saturating_add(window);

            // Ensure bounds are valid and maintain minimum window size
            let (lo, hi) = Self::ensure_valid_bounds(lo, hi, best_score);

            // Debug assertions to verify invariants
            debug_assert!(lo < hi, "Window must be non-empty: lo={lo}, hi={hi}");
            debug_assert!(
                hi - lo >= MIN_ASPIRATION_WINDOW,
                "Window size must be at least {MIN_ASPIRATION_WINDOW}"
            );
            debug_assert!(
                lo >= -SEARCH_INF && hi <= SEARCH_INF,
                "Bounds must be within valid range"
            );

            (lo, hi)
        }
    }

    /// Expand aspiration window after fail low/high
    ///
    /// Returns new (alpha, beta) bounds
    pub fn expand_window(&self, score: i32, alpha: i32, beta: i32, best_score: i32) -> (i32, i32) {
        let mut new_alpha = alpha;
        let mut new_beta = beta;

        if score <= alpha {
            // Fail low - score is worse than expected, expand alpha downward
            // Calculate expansion based on distance from previous best score
            // Use i64 to avoid overflow when calculating absolute difference
            let diff_alpha =
                ((alpha as i64) - (best_score as i64)).abs().min(i32::MAX as i64) as i32;
            let expansion = ((diff_alpha as f32) * ASPIRATION_WINDOW_EXPANSION) as i32;
            // Ensure minimum expansion of DELTA to guarantee progress
            // Use saturating arithmetic to prevent overflow
            new_alpha =
                alpha.saturating_sub(expansion.max(ASPIRATION_WINDOW_DELTA)).max(-SEARCH_INF);
        }

        if score >= beta {
            // Fail high - score is better than expected, expand beta upward
            // Calculate expansion based on distance from previous best score
            // Use i64 to avoid overflow when calculating absolute difference
            let diff_beta = ((beta as i64) - (best_score as i64)).abs().min(i32::MAX as i64) as i32;
            let expansion = ((diff_beta as f32) * ASPIRATION_WINDOW_EXPANSION) as i32;
            // Ensure minimum expansion of DELTA to guarantee progress
            // Use saturating arithmetic to prevent overflow
            new_beta = beta.saturating_add(expansion.max(ASPIRATION_WINDOW_DELTA)).min(SEARCH_INF);
        }

        (new_alpha, new_beta)
    }

    /// Check if we should stop retrying aspiration window
    pub fn should_stop_retries(&self, retries: u32) -> bool {
        retries >= ASPIRATION_RETRY_LIMIT
    }

    /// Calculate score volatility from evaluation history
    ///
    /// Volatility measures how much the score changes between iterations.
    /// High volatility indicates a tactical/complex position that may need
    /// wider aspiration windows to avoid repeated re-searches.
    ///
    /// Returns: Average absolute score difference over recent iterations
    fn calculate_score_volatility(&self) -> i32 {
        if self.score_history.len() < 2 {
            return 0;
        }

        // Calculate average deviation over recent depths
        let mut total_deviation = 0i64; // Use i64 to prevent overflow
        let mut valid_comparisons = 0;
        let history_len = self.score_history.len();
        let start = history_len.saturating_sub(5); // Look at last 5 depths

        // Safety check: ensure we have at least 2 elements to compare
        if start + 1 >= history_len {
            return 0;
        }

        for i in (start + 1)..history_len {
            let prev_score = self.score_history[i - 1];
            let curr_score = self.score_history[i];

            // Special handling for mate scores - they should not contribute to volatility
            // A mate score is when abs(score) >= MATE_SCORE - MAX_PLY
            let mate_threshold = MATE_SCORE - crate::search::constants::MAX_PLY as i32;
            let prev_is_mate = prev_score.abs() >= mate_threshold;
            let curr_is_mate = curr_score.abs() >= mate_threshold;

            // Skip if either score is a mate score
            if prev_is_mate || curr_is_mate {
                continue;
            }

            // Calculate absolute difference between consecutive scores
            let diff = (curr_score as i64 - prev_score as i64).abs();

            // Cap individual differences at 1000 centipawns to handle extreme scores
            // and prevent outliers from skewing the average
            let capped_diff = diff.min(1000);
            total_deviation += capped_diff;
            valid_comparisons += 1;
        }

        // Average deviation with proper rounding
        if valid_comparisons > 0 {
            // Add valid_comparisons/2 for proper rounding before integer division
            let avg = (total_deviation + valid_comparisons / 2) / valid_comparisons;
            // Ensure result fits in i32 and is non-negative
            avg.min(i32::MAX as i64).max(0) as i32
        } else {
            // If all recent scores were mate scores, return 0 volatility
            0
        }
    }
}

impl Default for AspirationWindow {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_aspiration_window() {
        let aw = AspirationWindow::new();
        assert_eq!(aw.history_len(), 0);
        assert_eq!(aw.score_volatility, 0);
    }

    #[test]
    fn test_calculate_window_early_depths() {
        let aw = AspirationWindow::new();
        assert_eq!(aw.calculate_window(1), ASPIRATION_WINDOW_INITIAL);
        assert_eq!(aw.calculate_window(2), ASPIRATION_WINDOW_INITIAL);
    }

    #[test]
    fn test_score_volatility_calculation() {
        let mut aw = AspirationWindow::new();

        // Empty history should return 0
        assert_eq!(aw.calculate_score_volatility(), 0);

        // Add some scores
        aw.update_score(100, NodeType::Exact);
        aw.update_score(110, NodeType::Exact);
        aw.update_score(95, NodeType::Exact);
        aw.update_score(120, NodeType::Exact);

        // Should calculate average deviation
        let volatility = aw.score_volatility;
        assert!(volatility > 0);
        assert!(volatility < 50); // Reasonable range
    }

    #[test]
    fn test_volatility_edge_cases() {
        let mut aw = AspirationWindow::new();

        // Test with extreme score differences (near mate scores)
        aw.update_score(100, NodeType::Exact);
        aw.update_score(30000, NodeType::Exact); // Near mate score
        aw.update_score(-30000, NodeType::Exact); // Opponent mate

        // Should handle extreme values without overflow
        let volatility = aw.score_volatility;
        assert!(volatility >= 0);
        assert!(volatility <= 1000); // Should be capped
    }

    #[test]
    fn test_window_expansion() {
        let aw = AspirationWindow::new();
        let best_score = 100;

        // Test fail low
        let (new_alpha, new_beta) = aw.expand_window(50, 80, 120, best_score);
        assert!(new_alpha < 80); // Alpha should expand downward
        assert_eq!(new_beta, 120); // Beta unchanged

        // Test fail high
        let (new_alpha, new_beta) = aw.expand_window(150, 80, 120, best_score);
        assert_eq!(new_alpha, 80); // Alpha unchanged
        assert!(new_beta > 120); // Beta should expand upward
    }

    #[test]
    fn test_initial_bounds() {
        let mut aw = AspirationWindow::new();

        // First depth should use full window
        let (alpha, beta) = aw.get_initial_bounds(1, 100);
        assert_eq!(alpha, -SEARCH_INF);
        assert_eq!(beta, SEARCH_INF);

        // Add history for depth > 1
        aw.update_score(100, NodeType::Exact);
        aw.update_score(105, NodeType::Exact);

        // Should use aspiration window
        let (alpha, beta) = aw.get_initial_bounds(3, 105);
        assert!(alpha > -SEARCH_INF);
        assert!(beta < SEARCH_INF);
        assert_eq!(beta - alpha, 2 * aw.calculate_window(3));
    }

    #[test]
    fn test_mate_score_bounds() {
        let aw = AspirationWindow::new();

        // Test very close winning mate (mate in 5 plies)
        let (alpha, beta) = aw.get_initial_bounds(3, MATE_SCORE - 5);
        assert_eq!(alpha, MATE_SCORE - 5 - 5); // Window of 5 below score
        assert_eq!(beta, MATE_SCORE - 5 + 10); // Window of 10 above score (2x)
        assert_eq!(beta - alpha, 15); // Total window should be 15

        // Test distant winning mate (mate in 25 plies)
        let (alpha, beta) = aw.get_initial_bounds(3, MATE_SCORE - 25);
        assert!(beta - alpha <= 60); // Moderate window for distant mate

        // Test losing mate (being mated in 10 plies)
        let (alpha, beta) = aw.get_initial_bounds(3, -MATE_SCORE + 10);
        assert_eq!(alpha, -MATE_SCORE + 10 - 10); // Window of 10 below score (2x for window=5)
        assert_eq!(beta, -MATE_SCORE + 10 + 5); // Window of 5 above score
        assert_eq!(beta - alpha, 15); // Total window should be 15
    }

    #[test]
    fn test_mate_score_volatility_exclusion() {
        let mut aw = AspirationWindow::new();

        // Add mix of normal and mate scores
        aw.update_score(100, NodeType::Exact);
        aw.update_score(120, NodeType::Exact);
        aw.update_score(MATE_SCORE - 10, NodeType::Exact); // Mate score - should be excluded
        aw.update_score(110, NodeType::Exact);
        aw.update_score(-MATE_SCORE + 5, NodeType::Exact); // Losing mate - should be excluded

        // Volatility should only consider the normal scores (100, 120, 110)
        let volatility = aw.score_volatility;
        assert!(volatility > 0);
        assert!(volatility < 50); // Should be moderate, not extreme

        // Test with only mate scores
        let mut aw_mate_only = AspirationWindow::new();
        aw_mate_only.update_score(MATE_SCORE - 5, NodeType::Exact);
        aw_mate_only.update_score(MATE_SCORE - 3, NodeType::Exact);
        aw_mate_only.update_score(MATE_SCORE - 1, NodeType::Exact);

        // Volatility should be 0 when all scores are mate scores
        assert_eq!(aw_mate_only.score_volatility, 0);
    }

    #[test]
    fn test_aspiration_window_clamping() {
        let aw = AspirationWindow::new();

        // Test extreme mate score that would exceed SEARCH_INF without clamping
        let extreme_mate = MATE_SCORE - 1; // Very close mate
        let (alpha, beta) = aw.get_initial_bounds(3, extreme_mate);

        // Verify bounds are within valid range
        assert!(alpha >= -SEARCH_INF);
        assert!(beta <= SEARCH_INF);

        // Verify window is non-empty
        assert!(alpha < beta);

        // Verify best_score is within the window (if it's within valid range)
        if extreme_mate <= SEARCH_INF && extreme_mate >= -SEARCH_INF {
            assert!(alpha <= extreme_mate && extreme_mate <= beta);
        } else {
            // When best_score exceeds bounds, verify window is at boundary with minimum size
            assert_eq!(beta - alpha, MIN_ASPIRATION_WINDOW);
            if extreme_mate > SEARCH_INF {
                assert_eq!(beta, SEARCH_INF);
            } else {
                assert_eq!(alpha, -SEARCH_INF);
            }
        }

        // Test extreme losing mate
        let extreme_losing = -MATE_SCORE + 1;
        let (alpha2, beta2) = aw.get_initial_bounds(3, extreme_losing);

        assert!(alpha2 >= -SEARCH_INF);
        assert!(beta2 <= SEARCH_INF);
        assert!(alpha2 < beta2);

        // Verify best_score is within the window (if it's within valid range)
        if extreme_losing <= SEARCH_INF && extreme_losing >= -SEARCH_INF {
            assert!(alpha2 <= extreme_losing && extreme_losing <= beta2);
        } else {
            // When best_score exceeds bounds, verify window is at boundary with minimum size
            assert_eq!(beta2 - alpha2, MIN_ASPIRATION_WINDOW);
            if extreme_losing > SEARCH_INF {
                assert_eq!(beta2, SEARCH_INF);
            } else {
                assert_eq!(alpha2, -SEARCH_INF);
            }
        }
    }

    #[test]
    fn test_non_empty_window_guarantee() {
        let aw = AspirationWindow::new();

        // Create a pathological case where clamping might create empty window
        // This would happen if best_score is at boundary and window calculation
        // tries to extend beyond limits
        let boundary_score = SEARCH_INF - 1;
        let (alpha, beta) = aw.get_initial_bounds(3, boundary_score);

        // Window must be non-empty
        assert!(alpha < beta, "Window must be non-empty: alpha={alpha}, beta={beta}");

        // Minimum window size should be at least MIN_ASPIRATION_WINDOW
        assert!(
            beta - alpha >= MIN_ASPIRATION_WINDOW,
            "Window size must be at least {MIN_ASPIRATION_WINDOW}"
        );

        // Test negative boundary
        let neg_boundary_score = -SEARCH_INF + 1;
        let (alpha2, beta2) = aw.get_initial_bounds(3, neg_boundary_score);

        assert!(alpha2 < beta2, "Window must be non-empty: alpha={alpha2}, beta={beta2}");
        assert!(
            beta2 - alpha2 >= MIN_ASPIRATION_WINDOW,
            "Window size must be at least {MIN_ASPIRATION_WINDOW}"
        );
    }

    #[test]
    fn test_normal_score_clamping() {
        let mut aw = AspirationWindow::new();

        // Add some history to get non-trivial window calculation
        aw.update_score(100, NodeType::Exact);
        aw.update_score(200, NodeType::Exact);
        aw.update_score(150, NodeType::Exact);

        // Test normal score near boundary
        let near_boundary = SEARCH_INF - 100;
        let (alpha, beta) = aw.get_initial_bounds(4, near_boundary);

        // Verify bounds are clamped
        assert!(alpha >= -SEARCH_INF);
        assert!(beta <= SEARCH_INF);
        assert!(alpha < beta);

        // Test with volatile history
        aw.update_score(1000, NodeType::Exact);
        aw.update_score(-1000, NodeType::Exact);
        aw.update_score(2000, NodeType::Exact);

        let (alpha2, beta2) = aw.get_initial_bounds(7, 0);
        assert!(alpha2 >= -SEARCH_INF);
        assert!(beta2 <= SEARCH_INF);
        assert!(alpha2 < beta2);
    }

    #[test]
    fn test_expand_window_with_various_score_differences() {
        let aw = AspirationWindow::new();

        // Test case 1: alpha << best_score (fail-low with alpha much lower than best_score)
        let alpha = -1000;
        let beta = -500;
        let best_score = 0;
        let score = -1100; // fail-low
        let (new_alpha, new_beta) = aw.expand_window(score, alpha, beta, best_score);

        // Expansion should be based on |alpha - best_score| = 1000
        let expected_expansion =
            ((1000.0 * ASPIRATION_WINDOW_EXPANSION) as i32).max(ASPIRATION_WINDOW_DELTA);
        assert_eq!(new_alpha, alpha.saturating_sub(expected_expansion).max(-SEARCH_INF));
        assert_eq!(new_beta, beta); // Beta should remain unchanged
        assert!(new_alpha < alpha); // Alpha should have expanded downward

        // Test case 2: beta >> best_score (fail-high with beta much higher than best_score)
        let alpha = 500;
        let beta = 1000;
        let best_score = 0;
        let score = 1100; // fail-high
        let (new_alpha, new_beta) = aw.expand_window(score, alpha, beta, best_score);

        // Expansion should be based on |beta - best_score| = 1000
        let expected_expansion =
            ((1000.0 * ASPIRATION_WINDOW_EXPANSION) as i32).max(ASPIRATION_WINDOW_DELTA);
        assert_eq!(new_alpha, alpha); // Alpha should remain unchanged
        assert_eq!(new_beta, beta.saturating_add(expected_expansion).min(SEARCH_INF));
        assert!(new_beta > beta); // Beta should have expanded upward

        // Test case 3: alpha > best_score (fail-low with alpha higher than best_score)
        let alpha = 100;
        let beta = 200;
        let best_score = 50;
        let score = 90; // fail-low
        let (new_alpha, new_beta) = aw.expand_window(score, alpha, beta, best_score);

        // Expansion should be based on |alpha - best_score| = 50
        assert!(new_alpha < alpha); // Alpha should have expanded downward
        assert_eq!(new_beta, beta); // Beta should remain unchanged

        // Test case 4: beta < best_score (fail-high with beta lower than best_score)
        let alpha = -200;
        let beta = -100;
        let best_score = -50;
        let score = -90; // fail-high
        let (new_alpha, new_beta) = aw.expand_window(score, alpha, beta, best_score);

        // Expansion should be based on |beta - best_score| = 50
        assert_eq!(new_alpha, alpha); // Alpha should remain unchanged
        assert!(new_beta > beta); // Beta should have expanded upward
    }

    #[test]
    fn test_expand_window_overflow_safety() {
        let aw = AspirationWindow::new();

        // Test with extreme values near i32::MIN/MAX
        // Note: We need to stay within SEARCH_INF bounds
        let alpha = -SEARCH_INF / 2;
        let beta = -SEARCH_INF / 2 + 1000;
        let best_score = SEARCH_INF / 2;
        let score = alpha - 100; // fail-low

        let (new_alpha, new_beta) = aw.expand_window(score, alpha, beta, best_score);

        // Should not panic and should produce valid results
        assert!(new_alpha <= alpha);
        assert!(new_alpha >= -SEARCH_INF);
        assert_eq!(new_beta, beta);

        // Test opposite extreme
        let alpha = SEARCH_INF / 2 - 1000;
        let beta = SEARCH_INF / 2;
        let best_score = -SEARCH_INF / 2;
        let score = beta + 100; // fail-high

        let (new_alpha, new_beta) = aw.expand_window(score, alpha, beta, best_score);

        // Should not panic and should produce valid results
        assert_eq!(new_alpha, alpha);
        assert!(new_beta >= beta);
        assert!(new_beta <= SEARCH_INF);
    }

    #[test]
    fn test_search_inf_boundary_cases() {
        let aw = AspirationWindow::new();

        // Test best_score exactly at SEARCH_INF
        // Note: SEARCH_INF は mate 判定とは独立に「境界値」として扱われ、結果的に狭い窓になる
        let (alpha, beta) = aw.get_initial_bounds(3, SEARCH_INF);
        assert_eq!(beta, SEARCH_INF);
        assert!(alpha < beta);
        assert!(alpha >= -SEARCH_INF);
        // SEARCH_INF is the search evaluation upper bound; it results in a narrow window
        // because it's treated as an extreme value in the mate distance logic

        // Test best_score exactly at -SEARCH_INF
        let (alpha, beta) = aw.get_initial_bounds(3, -SEARCH_INF);
        assert_eq!(alpha, -SEARCH_INF);
        assert!(alpha < beta);
        assert!(beta <= SEARCH_INF);

        // Test best_score at a non-mate boundary value
        let non_mate_boundary = MATE_SCORE - crate::search::constants::MAX_PLY as i32 - 100;
        let (alpha, beta) = aw.get_initial_bounds(3, non_mate_boundary);
        assert!(alpha < beta);
        assert!(alpha >= -SEARCH_INF);
        assert!(beta <= SEARCH_INF);
        assert!(beta - alpha >= MIN_ASPIRATION_WINDOW);

        // Test negative non-mate boundary
        let (alpha, beta) = aw.get_initial_bounds(3, -non_mate_boundary);
        assert!(alpha < beta);
        assert!(alpha >= -SEARCH_INF);
        assert!(beta <= SEARCH_INF);
        assert!(beta - alpha >= MIN_ASPIRATION_WINDOW);
    }

    #[test]
    fn test_expand_window_delta_dominance() {
        let aw = AspirationWindow::new();

        // Test case where diff is very small (DELTA should dominate)
        let alpha = 100;
        let beta = 200;
        let best_score = 95; // Very close to alpha
        let score = 90; // fail-low

        let (new_alpha, new_beta) = aw.expand_window(score, alpha, beta, best_score);

        // Expansion should be at least ASPIRATION_WINDOW_DELTA
        let actual_expansion = alpha - new_alpha;
        assert!(actual_expansion >= ASPIRATION_WINDOW_DELTA);
        assert_eq!(new_beta, beta); // Beta unchanged

        // Test with very small diff on fail-high
        let alpha = 100;
        let beta = 200;
        let best_score = 205; // Very close to beta
        let score = 210; // fail-high

        let (new_alpha, new_beta) = aw.expand_window(score, alpha, beta, best_score);

        // Expansion should be at least ASPIRATION_WINDOW_DELTA
        let actual_expansion = new_beta - beta;
        assert!(actual_expansion >= ASPIRATION_WINDOW_DELTA);
        assert_eq!(new_alpha, alpha); // Alpha unchanged
    }

    #[test]
    fn test_center_min_window_odd_min() {
        // Test that center_min_window works correctly even if MIN is odd
        // This test verifies the helper function directly
        let best_score = 100;
        let (lo, hi) = AspirationWindow::center_min_window(best_score);

        // Window width should be exactly MIN_ASPIRATION_WINDOW
        assert_eq!(hi - lo, MIN_ASPIRATION_WINDOW);

        // Window should contain best_score (if possible)
        if best_score >= -SEARCH_INF + MIN_ASPIRATION_WINDOW / 2
            && best_score <= SEARCH_INF - MIN_ASPIRATION_WINDOW / 2
        {
            assert!(lo <= best_score && best_score <= hi);
        }

        // Test at boundaries
        let (lo, hi) = AspirationWindow::center_min_window(SEARCH_INF - 1);
        assert_eq!(hi - lo, MIN_ASPIRATION_WINDOW);
        assert!(lo >= -SEARCH_INF && hi <= SEARCH_INF);

        let (lo, hi) = AspirationWindow::center_min_window(-SEARCH_INF + 1);
        assert_eq!(hi - lo, MIN_ASPIRATION_WINDOW);
        assert!(lo >= -SEARCH_INF && hi <= SEARCH_INF);
    }

    #[test]
    fn test_boundary_node_type_filtering() {
        let mut aw = AspirationWindow::new();

        // Add EXACT scores
        aw.update_score(100, NodeType::Exact);
        aw.update_score(110, NodeType::Exact);
        assert_eq!(aw.history_len(), 2);

        // Add boundary value scores - these should be ignored
        aw.update_score(200, NodeType::LowerBound); // Fail-high score
        aw.update_score(-50, NodeType::UpperBound); // Fail-low score
        assert_eq!(aw.history_len(), 2, "Boundary values should not be added to history");

        // Add another EXACT score
        aw.update_score(105, NodeType::Exact);
        assert_eq!(aw.history_len(), 3);

        // Volatility should only consider EXACT scores (100, 110, 105)
        let volatility = aw.score_volatility;
        assert!(volatility > 0, "Should have some volatility from EXACT scores");
        assert!(volatility < 20, "Volatility should be moderate for close EXACT scores");
    }

    #[test]
    fn test_volatility_with_only_boundary_nodes() {
        let mut aw = AspirationWindow::new();

        // Add only boundary value scores
        aw.update_score(100, NodeType::LowerBound);
        aw.update_score(200, NodeType::UpperBound);
        aw.update_score(150, NodeType::LowerBound);

        // No scores should be in history
        assert_eq!(aw.history_len(), 0, "No boundary values should be in history");
        assert_eq!(aw.score_volatility, 0, "Volatility should be 0 with no EXACT scores");

        // Calculate window should return initial value
        assert_eq!(aw.calculate_window(5), ASPIRATION_WINDOW_INITIAL);
    }

    #[test]
    fn test_boundary_to_exact_transition() {
        let mut aw = AspirationWindow::new();

        // Start with boundary nodes
        aw.update_score(100, NodeType::LowerBound);
        aw.update_score(110, NodeType::UpperBound);

        // Verify initial state - no history, initial window
        assert_eq!(aw.history_len(), 0, "No scores in history after boundary nodes");
        assert_eq!(
            aw.calculate_window(3),
            ASPIRATION_WINDOW_INITIAL,
            "Should use initial window with no EXACT scores"
        );

        // Add first EXACT score
        aw.update_score(105, NodeType::Exact);
        assert_eq!(aw.history_len(), 1, "One EXACT score in history");
        assert_eq!(
            aw.calculate_window(3),
            ASPIRATION_WINDOW_INITIAL,
            "Should still use initial window with only 1 score"
        );

        // Add more boundary nodes - should not affect history
        aw.update_score(120, NodeType::LowerBound);
        aw.update_score(90, NodeType::UpperBound);
        assert_eq!(aw.history_len(), 1, "History unchanged after more boundary nodes");

        // Add second EXACT score - this should trigger dynamic window calculation
        aw.update_score(108, NodeType::Exact);
        assert_eq!(aw.history_len(), 2, "Two EXACT scores in history");

        // Now calculate_window should use dynamic calculation for depth > 2
        let window = aw.calculate_window(3);
        assert!(
            window >= ASPIRATION_WINDOW_INITIAL,
            "Dynamic window should be at least initial value"
        );

        // Volatility should be based only on the two EXACT scores (105, 108)
        assert!(aw.score_volatility > 0, "Should have some volatility");
        assert!(aw.score_volatility < 10, "Volatility should be small for close scores");
    }

    #[test]
    fn test_score_volatility_invariance_with_boundaries() {
        let mut aw = AspirationWindow::new();

        // Add two EXACT scores to establish baseline volatility
        aw.update_score(100, NodeType::Exact);
        aw.update_score(120, NodeType::Exact);
        let baseline_volatility = aw.score_volatility;
        let baseline_history_len = aw.history_len();

        assert!(baseline_volatility > 0, "Should have volatility from EXACT scores");
        assert_eq!(baseline_history_len, 2, "Should have 2 scores in history");

        // Add many boundary nodes - volatility should remain unchanged
        for i in 0..20 {
            let score = 50 + i * 10; // Widely varying scores
            let node_type = if i % 2 == 0 {
                NodeType::LowerBound
            } else {
                NodeType::UpperBound
            };
            aw.update_score(score, node_type);
        }

        // Verify invariance
        assert_eq!(
            aw.score_volatility, baseline_volatility,
            "Volatility should be unchanged after boundary nodes"
        );
        assert_eq!(
            aw.history_len(),
            baseline_history_len,
            "History length should be unchanged after boundary nodes"
        );

        // Add one more EXACT score - volatility should change
        aw.update_score(110, NodeType::Exact);
        assert_ne!(
            aw.score_volatility, baseline_volatility,
            "Volatility should change after new EXACT score"
        );
        assert_eq!(aw.history_len(), 3, "Should have 3 EXACT scores in history");
    }
}
