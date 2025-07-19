//! Position filter for selecting quality opening book entries
//!
//! This module provides filtering logic to select high-quality positions
//! from the raw SFEN data, focusing on early game positions with balanced
//! evaluations and sufficient analysis depth.

use crate::opening_book::RawSfenEntry;

/// Filter configuration for position selection
#[derive(Debug, Clone)]
pub struct PositionFilter {
    /// Maximum move count to include (early game focus)
    pub max_moves: usize,
    /// Minimum analysis depth required (at least one move must meet this)
    pub min_depth: u32,
    /// Minimum evaluation to include (exclude losing positions)
    pub min_evaluation: i32,
    /// Maximum evaluation to include (exclude winning positions)
    pub max_evaluation: i32,
}

impl Default for PositionFilter {
    fn default() -> Self {
        Self {
            max_moves: 50,         // Focus on opening and early middle game
            min_depth: 0,          // Accept all analyzed positions
            min_evaluation: -1000, // Exclude clearly losing positions
            max_evaluation: 1000,  // Exclude clearly winning positions
        }
    }
}

impl PositionFilter {
    /// Create a new position filter with custom settings
    pub fn new(max_moves: usize, min_depth: u32, min_evaluation: i32, max_evaluation: i32) -> Self {
        Self {
            max_moves,
            min_depth,
            min_evaluation,
            max_evaluation,
        }
    }

    /// Check if a position should be included based on filter criteria
    pub fn should_include(&self, entry: &RawSfenEntry) -> bool {
        // Check move count (early game filter)
        if entry.move_count as usize > self.max_moves {
            return false;
        }

        // Must have at least one move
        if entry.moves.is_empty() {
            return false;
        }

        // Check if at least one move meets depth requirement
        let has_sufficient_depth = entry.moves.iter().any(|m| m.depth >= self.min_depth);

        if !has_sufficient_depth {
            return false;
        }

        // Check evaluation range (best move's evaluation)
        let best_evaluation = entry.moves.iter().map(|m| m.evaluation).max().unwrap_or(0);

        if best_evaluation < self.min_evaluation || best_evaluation > self.max_evaluation {
            return false;
        }

        true
    }

    /// Filter moves within a position (keep top moves, sorted by evaluation)
    pub fn filter_moves(&self, entry: &mut RawSfenEntry) {
        // Sort moves by evaluation (best first)
        entry.moves.sort_by(|a, b| b.evaluation.cmp(&a.evaluation));

        // Keep only top 8 moves
        if entry.moves.len() > 8 {
            entry.moves.truncate(8);
        }
    }

    /// Apply full filtering to an entry (position and moves)
    pub fn filter_entry(&self, entry: &mut RawSfenEntry) -> bool {
        if !self.should_include(entry) {
            return false;
        }

        self.filter_moves(entry);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opening_book::RawMove;

    #[test]
    fn test_default_values() {
        let filter = PositionFilter::default();
        assert_eq!(filter.max_moves, 50);
        assert_eq!(filter.min_depth, 0);
        assert_eq!(filter.min_evaluation, -1000);
        assert_eq!(filter.max_evaluation, 1000);
    }

    #[test]
    fn test_custom_filter() {
        let filter = PositionFilter::new(30, 5, -500, 500);
        assert_eq!(filter.max_moves, 30);
        assert_eq!(filter.min_depth, 5);
        assert_eq!(filter.min_evaluation, -500);
        assert_eq!(filter.max_evaluation, 500);
    }

    #[test]
    fn test_move_count_filtering() {
        let filter = PositionFilter {
            max_moves: 20,
            ..Default::default()
        };

        let mut entry = RawSfenEntry {
            position: "test".to_string(),
            turn: 'b',
            hand: "-".to_string(),
            move_count: 15,
            moves: vec![RawMove {
                move_notation: "7g7f".to_string(),
                move_type: "none".to_string(),
                evaluation: 50,
                depth: 1,
                nodes: 1000,
            }],
        };

        assert!(filter.should_include(&entry));

        entry.move_count = 25;
        assert!(!filter.should_include(&entry));
    }
}
