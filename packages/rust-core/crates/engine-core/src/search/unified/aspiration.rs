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
        if depth > 1 && best_score.abs() < MATE_SCORE {
            // Calculate dynamic window based on score history
            let window = self.calculate_window(depth);
            (best_score - window, best_score + window)
        } else {
            // First depth or mate score - use full window
            (-SEARCH_INF, SEARCH_INF)
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
        let history_len = self.score_history.len();
        let start = history_len.saturating_sub(5); // Look at last 5 depths

        // Safety check: ensure we have at least 2 elements to compare
        if start + 1 >= history_len {
            return 0;
        }

        for i in (start + 1)..history_len {
            // Calculate absolute difference between consecutive scores
            // Safety: bounds are guaranteed by the check above
            let diff = (self.score_history[i] as i64 - self.score_history[i - 1] as i64).abs();

            // Cap individual differences at 1000 centipawns to handle mate scores
            // and prevent extreme volatility from skewing the average
            let capped_diff = diff.min(1000);
            total_deviation += capped_diff;
        }

        // Average deviation with proper rounding
        let count = (history_len - start - 1) as i64;
        if count > 0 {
            // Add count/2 for proper rounding before integer division
            let avg = (total_deviation + count / 2) / count;
            // Ensure result fits in i32 and is non-negative
            avg.min(i32::MAX as i64).max(0) as i32
        } else {
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
}
