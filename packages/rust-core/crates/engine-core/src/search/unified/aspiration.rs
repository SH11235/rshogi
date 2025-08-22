//! Aspiration window management for iterative deepening
//!
//! This module handles dynamic aspiration window calculation based on
//! position volatility to minimize re-searches while maximizing cutoffs.

use crate::search::constants::{
    ASPIRATION_RETRY_LIMIT, ASPIRATION_WINDOW_DELTA, ASPIRATION_WINDOW_EXPANSION,
    ASPIRATION_WINDOW_INITIAL, ASPIRATION_WINDOW_MAX_VOLATILITY_ADJUSTMENT, MATE_SCORE, SEARCH_INF,
};

/// Manages aspiration window calculation and score history
#[derive(Debug, Clone)]
pub struct AspirationWindow {
    /// Evaluation history for each depth (for dynamic aspiration window)
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

    /// Clear the aspiration window state
    pub fn clear(&mut self) {
        self.score_history.clear();
        self.score_volatility = 0;
    }

    /// Update score history and recalculate volatility
    pub fn update_score(&mut self, score: i32) {
        self.score_history.push(score);
        if self.score_history.len() > 1 {
            self.score_volatility = self.calculate_score_volatility();
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

            // Clamp bounds to valid range to ensure mate scores are not excluded
            // This prevents issues when asymmetric windows exceed SEARCH_INF
            let mut lo = lo.clamp(-SEARCH_INF, SEARCH_INF);
            let mut hi = hi.clamp(-SEARCH_INF, SEARCH_INF);

            // Ensure non-empty window (lo < hi)
            if lo >= hi {
                // Create minimum window of 2 centipawns centered on best_score
                lo = best_score.saturating_sub(1).max(-SEARCH_INF);
                hi = best_score.saturating_add(1).min(SEARCH_INF);
            }

            (lo, hi)
        } else {
            // Normal (non-mate) score - use dynamic window based on score history
            let window = self.calculate_window(depth);

            // Use saturating arithmetic and clamp to prevent overflow
            let lo = best_score.saturating_sub(window).clamp(-SEARCH_INF, SEARCH_INF);
            let hi = best_score.saturating_add(window).clamp(-SEARCH_INF, SEARCH_INF);

            // Ensure non-empty window (lo < hi) - though this should rarely happen for normal scores
            if lo >= hi {
                // Create minimum window of 2 centipawns centered on best_score
                (
                    best_score.saturating_sub(1).max(-SEARCH_INF),
                    best_score.saturating_add(1).min(SEARCH_INF),
                )
            } else {
                (lo, hi)
            }
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
            let expansion =
                ((alpha - best_score).abs() as f32 * ASPIRATION_WINDOW_EXPANSION) as i32;
            // Ensure minimum expansion of DELTA to guarantee progress
            new_alpha = (alpha - expansion.max(ASPIRATION_WINDOW_DELTA)).max(-SEARCH_INF);
        }

        if score >= beta {
            // Fail high - score is better than expected, expand beta upward
            // Calculate expansion based on distance from previous best score
            let expansion = ((beta - best_score).abs() as f32 * ASPIRATION_WINDOW_EXPANSION) as i32;
            // Ensure minimum expansion of DELTA to guarantee progress
            new_beta = (beta + expansion.max(ASPIRATION_WINDOW_DELTA)).min(SEARCH_INF);
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
        aw.update_score(100);
        aw.update_score(110);
        aw.update_score(95);
        aw.update_score(120);

        // Should calculate average deviation
        let volatility = aw.score_volatility;
        assert!(volatility > 0);
        assert!(volatility < 50); // Reasonable range
    }

    #[test]
    fn test_volatility_edge_cases() {
        let mut aw = AspirationWindow::new();

        // Test with extreme score differences (near mate scores)
        aw.update_score(100);
        aw.update_score(30000); // Near mate score
        aw.update_score(-30000); // Opponent mate

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
        aw.update_score(100);
        aw.update_score(105);

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
        aw.update_score(100);
        aw.update_score(120);
        aw.update_score(MATE_SCORE - 10); // Mate score - should be excluded
        aw.update_score(110);
        aw.update_score(-MATE_SCORE + 5); // Losing mate - should be excluded

        // Volatility should only consider the normal scores (100, 120, 110)
        let volatility = aw.score_volatility;
        assert!(volatility > 0);
        assert!(volatility < 50); // Should be moderate, not extreme

        // Test with only mate scores
        let mut aw_mate_only = AspirationWindow::new();
        aw_mate_only.update_score(MATE_SCORE - 5);
        aw_mate_only.update_score(MATE_SCORE - 3);
        aw_mate_only.update_score(MATE_SCORE - 1);

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

        // Verify best_score is within the window
        assert!(alpha <= extreme_mate && extreme_mate <= beta);

        // Test extreme losing mate
        let extreme_losing = -MATE_SCORE + 1;
        let (alpha2, beta2) = aw.get_initial_bounds(3, extreme_losing);

        assert!(alpha2 >= -SEARCH_INF);
        assert!(beta2 <= SEARCH_INF);
        assert!(alpha2 < beta2);
        assert!(alpha2 <= extreme_losing && extreme_losing <= beta2);
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

        // Minimum window size should be at least 2
        assert!(beta - alpha >= 2, "Window size must be at least 2");

        // Test negative boundary
        let neg_boundary_score = -SEARCH_INF + 1;
        let (alpha2, beta2) = aw.get_initial_bounds(3, neg_boundary_score);

        assert!(alpha2 < beta2, "Window must be non-empty: alpha={alpha2}, beta={beta2}");
        assert!(beta2 - alpha2 >= 2, "Window size must be at least 2");
    }

    #[test]
    fn test_normal_score_clamping() {
        let mut aw = AspirationWindow::new();

        // Add some history to get non-trivial window calculation
        aw.update_score(100);
        aw.update_score(200);
        aw.update_score(150);

        // Test normal score near boundary
        let near_boundary = SEARCH_INF - 100;
        let (alpha, beta) = aw.get_initial_bounds(4, near_boundary);

        // Verify bounds are clamped
        assert!(alpha >= -SEARCH_INF);
        assert!(beta <= SEARCH_INF);
        assert!(alpha < beta);

        // Test with volatile history
        aw.update_score(1000);
        aw.update_score(-1000);
        aw.update_score(2000);

        let (alpha2, beta2) = aw.get_initial_bounds(7, 0);
        assert!(alpha2 >= -SEARCH_INF);
        assert!(beta2 <= SEARCH_INF);
        assert!(alpha2 < beta2);
    }
}
