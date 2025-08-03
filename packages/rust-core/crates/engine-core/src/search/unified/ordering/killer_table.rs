//! Killer moves table for improved move ordering
//!
//! This module provides a global killer moves table that stores
//! quiet moves that caused beta cutoffs at each ply.

use crate::shogi::Move;
use std::sync::RwLock;

/// Maximum number of plies to track killer moves
const MAX_KILLER_PLY: usize = 128;

/// Number of killer moves per ply
const KILLERS_PER_PLY: usize = 2;

/// Global killer moves table
///
/// This table stores killer moves (quiet moves that caused beta cutoffs)
/// organized by ply. It's designed to be thread-safe and efficient.
pub struct KillerTable {
    /// Killer moves indexed by [ply][slot]
    killers: RwLock<Vec<[Option<Move>; KILLERS_PER_PLY]>>,
}

impl KillerTable {
    /// Create a new killer table
    pub fn new() -> Self {
        Self {
            killers: RwLock::new(vec![[None; KILLERS_PER_PLY]; MAX_KILLER_PLY]),
        }
    }

    /// Update killer moves for a given ply
    ///
    /// Adds a new killer move if it's not already present.
    /// Maintains the invariant that killers[0] is the most recent.
    pub fn update(&self, ply: usize, mv: Move) {
        // Skip if ply is out of bounds
        if ply >= MAX_KILLER_PLY {
            return;
        }

        // Killer moves should be quiet moves (non-captures, non-promotions, non-drops)
        // Drops are tactical moves that create new material on the board
        if mv.is_capture_hint() || mv.is_promote() || mv.is_drop() {
            return;
        }

        // Try to acquire write lock
        if let Ok(mut killers) = self.killers.write() {
            let ply_killers = &mut killers[ply];

            // Don't store the same move twice
            if ply_killers[0] == Some(mv) {
                return;
            }

            // Shift killers and add new one
            ply_killers[1] = ply_killers[0];
            ply_killers[0] = Some(mv);
        }
    }

    /// Get killer moves for a given ply
    ///
    /// Returns the killer moves in order of recency.
    pub fn get(&self, ply: usize) -> [Option<Move>; KILLERS_PER_PLY] {
        if ply >= MAX_KILLER_PLY {
            return [None; KILLERS_PER_PLY];
        }

        // Try to acquire read lock
        if let Ok(killers) = self.killers.read() {
            killers[ply]
        } else {
            // If lock fails, return empty
            [None; KILLERS_PER_PLY]
        }
    }

    /// Check if a move is a killer at the given ply
    pub fn is_killer(&self, ply: usize, mv: Move) -> bool {
        if ply >= MAX_KILLER_PLY {
            return false;
        }

        if let Ok(killers) = self.killers.read() {
            let ply_killers = &killers[ply];
            ply_killers[0] == Some(mv) || ply_killers[1] == Some(mv)
        } else {
            false
        }
    }

    /// Clear all killer moves
    ///
    /// Useful when starting a new search from a different position.
    pub fn clear(&self) {
        if let Ok(mut killers) = self.killers.write() {
            for ply_killers in killers.iter_mut() {
                *ply_killers = [None; KILLERS_PER_PLY];
            }
        }
    }

    /// Clear killer moves for a specific ply
    pub fn clear_ply(&self, ply: usize) {
        if ply >= MAX_KILLER_PLY {
            return;
        }

        if let Ok(mut killers) = self.killers.write() {
            killers[ply] = [None; KILLERS_PER_PLY];
        }
    }

    /// Age killer moves by one ply
    ///
    /// This is useful when starting a new search at a deeper depth.
    /// Moves from ply N become moves from ply N+1.
    pub fn age(&self) {
        if let Ok(mut killers) = self.killers.write() {
            // Shift all killers down by one ply
            for i in (1..MAX_KILLER_PLY).rev() {
                killers[i] = killers[i - 1];
            }
            // Clear ply 0
            killers[0] = [None; KILLERS_PER_PLY];
        }
    }
}

impl Default for KillerTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Square;

    #[test]
    fn test_killer_update() {
        let table = KillerTable::new();
        let mv1 = Move::normal(Square::new(2, 7), Square::new(2, 6), false);
        let mv2 = Move::normal(Square::new(7, 7), Square::new(7, 6), false);

        // Update at ply 0
        table.update(0, mv1);
        let killers = table.get(0);
        assert_eq!(killers[0], Some(mv1));
        assert_eq!(killers[1], None);

        // Update with another move
        table.update(0, mv2);
        let killers = table.get(0);
        assert_eq!(killers[0], Some(mv2));
        assert_eq!(killers[1], Some(mv1));
    }

    #[test]
    fn test_killer_is_killer() {
        let table = KillerTable::new();
        let mv1 = Move::normal(Square::new(2, 7), Square::new(2, 6), false);
        let mv2 = Move::normal(Square::new(7, 7), Square::new(7, 6), false);
        let mv3 = Move::normal(Square::new(3, 7), Square::new(3, 6), false);

        table.update(0, mv1);
        table.update(0, mv2);

        assert!(table.is_killer(0, mv1));
        assert!(table.is_killer(0, mv2));
        assert!(!table.is_killer(0, mv3));
    }

    #[test]
    fn test_killer_clear() {
        let table = KillerTable::new();
        let mv1 = Move::normal(Square::new(2, 7), Square::new(2, 6), false);

        table.update(0, mv1);
        table.update(1, mv1);

        table.clear();

        assert_eq!(table.get(0), [None; KILLERS_PER_PLY]);
        assert_eq!(table.get(1), [None; KILLERS_PER_PLY]);
    }

    #[test]
    fn test_killer_age() {
        let table = KillerTable::new();
        let mv1 = Move::normal(Square::new(2, 7), Square::new(2, 6), false);
        let mv2 = Move::normal(Square::new(7, 7), Square::new(7, 6), false);

        table.update(0, mv1);
        table.update(1, mv2);

        table.age();

        // Ply 0 should be cleared
        assert_eq!(table.get(0), [None; KILLERS_PER_PLY]);
        // Ply 1 should have what was at ply 0
        assert_eq!(table.get(1)[0], Some(mv1));
        // Ply 2 should have what was at ply 1
        assert_eq!(table.get(2)[0], Some(mv2));
    }
}
