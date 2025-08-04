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
    use crate::{usi::parse_usi_square, Square};

    #[test]
    fn test_killer_update() {
        let table = KillerTable::new();
        let mv1 =
            Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);
        let mv2 =
            Move::normal(parse_usi_square("2h").unwrap(), parse_usi_square("2g").unwrap(), false);

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
        let mv1 =
            Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);
        let mv2 =
            Move::normal(parse_usi_square("2h").unwrap(), parse_usi_square("2g").unwrap(), false);
        let mv3 =
            Move::normal(parse_usi_square("6h").unwrap(), parse_usi_square("6g").unwrap(), false);

        table.update(0, mv1);
        table.update(0, mv2);

        assert!(table.is_killer(0, mv1));
        assert!(table.is_killer(0, mv2));
        assert!(!table.is_killer(0, mv3));
    }

    #[test]
    fn test_killer_clear() {
        let table = KillerTable::new();
        let mv1 =
            Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);

        table.update(0, mv1);
        table.update(1, mv1);

        table.clear();

        assert_eq!(table.get(0), [None; KILLERS_PER_PLY]);
        assert_eq!(table.get(1), [None; KILLERS_PER_PLY]);
    }

    #[test]
    fn test_killer_age() {
        let table = KillerTable::new();
        let mv1 =
            Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);
        let mv2 =
            Move::normal(parse_usi_square("2h").unwrap(), parse_usi_square("2g").unwrap(), false);

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

    #[test]
    fn test_killer_table_memory_pressure() {
        use std::sync::Arc;
        use std::thread;

        // Test killer table behavior under memory pressure with many concurrent threads
        let table = Arc::new(KillerTable::new());
        let num_threads = 8; // Reduced from 32 to avoid excessive load on CI
        let iterations_per_thread = 100; // Reduced from 1000 for faster CI

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let table = Arc::clone(&table);
                thread::spawn(move || {
                    for i in 0..iterations_per_thread {
                        let ply = (thread_id + i) % MAX_KILLER_PLY;
                        // Create moves using the same pattern as other tests
                        let from_file = ((thread_id * 7 + i) % 9) as u8;
                        let from_rank = ((thread_id * 3 + i) % 9) as u8;
                        let to_file = ((thread_id * 11 + i + 1) % 9) as u8;
                        let to_rank = ((thread_id * 5 + i + 1) % 9) as u8;

                        let mv = Move::normal(
                            Square::new(from_file, from_rank),
                            Square::new(to_file, to_rank),
                            false,
                        );

                        // Simulate memory pressure by rapidly adding and retrieving
                        table.update(ply, mv);
                        let _ = table.get(ply);

                        // Occasionally age the table to stress memory reallocation
                        if i % 50 == 0 {
                            table.age();
                        }
                    }
                })
            })
            .collect();

        // Wait for all threads to complete
        for handle in handles {
            handle.join().unwrap();
        }

        // Verify table is still functional after stress test
        let test_move =
            Move::normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap(), false);
        table.update(0, test_move);
        let killers = table.get(0);
        assert!(killers.contains(&Some(test_move)));
    }
}
