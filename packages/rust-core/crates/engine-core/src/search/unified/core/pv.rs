//! Principal Variation (PV) management
//!
//! Tracks the best move sequence found during search

use crate::{search::constants::MAX_PLY, shogi::Move};

/// NULL move constant for uninitialized entries
const NULL_MOVE: Move = Move::NULL;

/// Principal Variation table with strict length management
pub struct PVTable {
    /// Move storage [ply][move_index]
    mv: [[Move; MAX_PLY]; MAX_PLY],
    /// Length of PV at each ply
    len: [usize; MAX_PLY],
}

impl PVTable {
    /// Create a new PV table
    pub fn new() -> Self {
        Self {
            mv: [[NULL_MOVE; MAX_PLY]; MAX_PLY],
            len: [0; MAX_PLY],
        }
    }

    /// Clear all PV lines (for new iteration)
    #[inline]
    pub fn clear_all(&mut self) {
        self.len.fill(0);
    }

    /// Clear PV at specific ply (on node entry)
    #[inline]
    pub fn clear_len_at(&mut self, ply: usize) {
        if ply < MAX_PLY {
            self.len[ply] = 0;
        }
    }

    /// Get PV line with length (no allocation)
    #[inline]
    pub fn line(&self, ply: usize) -> (&[Move], usize) {
        if ply < MAX_PLY {
            let len = self.len[ply];
            (&self.mv[ply][..len], len)
        } else {
            (&[], 0)
        }
    }

    /// Set PV line with head move and tail from child
    #[inline]
    pub fn set_line(&mut self, ply: usize, head: Move, tail: &[Move]) {
        if ply >= MAX_PLY {
            return;
        }

        // Skip null moves in PV
        if head == Move::NULL {
            #[cfg(debug_assertions)]
            if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                eprintln!("[WARNING] Attempted to add NULL move to PV at ply {ply}");
            }
            return;
        }

        let tail_len = tail.len().min(MAX_PLY - 1);
        self.mv[ply][0] = head;
        if tail_len > 0 {
            // Use exclusive range to avoid panic when tail_len == 0
            self.mv[ply][1..(1 + tail_len)].copy_from_slice(&tail[..tail_len]);
        }
        self.len[ply] = tail_len + 1;
    }

    /// Get PV as Vec (only for final output)
    pub fn get_pv_snapshot(&self, ply: usize) -> Vec<Move> {
        let (line, len) = self.line(ply);
        line[..len].to_vec()
    }

    /// Clear all PV lines (backward compatibility)
    pub fn clear(&mut self) {
        self.clear_all();
    }

    /// Update PV at given ply (backward compatibility)
    pub fn update(&mut self, ply: usize, best_move: Move, child_pv: &[Move]) {
        self.set_line(ply, best_move, child_pv);
    }

    /// Update from a complete line (backward compatibility)
    pub fn update_from_line(&mut self, pv: &[Move]) {
        if !pv.is_empty() && pv.len() < MAX_PLY {
            self.mv[0][..pv.len()].copy_from_slice(pv);
            self.len[0] = pv.len();
        }
    }

    /// Get PV line at given ply (backward compatibility)
    pub fn get_line(&self, ply: usize) -> &[Move] {
        let (line, _) = self.line(ply);
        line
    }

    /// Get the main PV (from root) (backward compatibility)
    pub fn get_pv(&self) -> &[Move] {
        self.get_line(0)
    }
}

impl Default for PVTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{shogi::Move, usi::parse_usi_square};

    #[test]
    fn test_pv_table() {
        let mut pv = PVTable::new();

        // Create some test moves
        let move1 =
            Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);
        let move2 =
            Move::normal(parse_usi_square("6c").unwrap(), parse_usi_square("6d").unwrap(), false);

        // Update PV
        pv.update(1, move2, &[]);
        pv.update(0, move1, &[move2]);

        // Check PV
        let main_pv = pv.get_pv();
        assert_eq!(main_pv.len(), 2);
        assert_eq!(main_pv[0], move1);
        assert_eq!(main_pv[1], move2);
    }

    #[test]
    fn test_pv_set_line_zero_tail() {
        // Regression test for zero-length tail panic
        let mut pv = PVTable::new();
        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);

        // This should not panic
        pv.set_line(0, move1, &[]);

        let (line, len) = pv.line(0);
        assert_eq!(len, 1);
        assert_eq!(line[0], move1);
    }

    #[test]
    fn test_pv_clear_all() {
        let mut pv = PVTable::new();
        let move1 =
            Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);
        let move2 =
            Move::normal(parse_usi_square("6c").unwrap(), parse_usi_square("6d").unwrap(), false);

        // Set multiple PV lines
        pv.set_line(0, move1, &[move2]);
        pv.set_line(1, move2, &[]);

        assert_eq!(pv.len[0], 2);
        assert_eq!(pv.len[1], 1);

        // Clear all
        pv.clear_all();

        // All lengths should be zero
        assert_eq!(pv.len[0], 0);
        assert_eq!(pv.len[1], 0);
        assert_eq!(pv.get_pv().len(), 0);
    }

    #[test]
    fn test_pv_null_move_handling() {
        // Test that NULL moves are properly rejected
        let mut pv = PVTable::new();

        // Try to set a NULL move
        pv.set_line(0, Move::NULL, &[]);

        // PV should remain empty
        let (line, len) = pv.line(0);
        assert_eq!(len, 0, "PV should be empty after NULL move attempt");
        assert!(line.is_empty(), "PV line should be empty");
    }

    #[test]
    fn test_pv_edge_cases() {
        // Test boundary conditions
        let mut pv = PVTable::new();

        // Test at MAX_PLY boundary
        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        pv.set_line(MAX_PLY - 1, move1, &[]);
        let (line, len) = pv.line(MAX_PLY - 1);
        assert_eq!(len, 1);
        assert_eq!(line[0], move1);

        // Test beyond MAX_PLY (should be ignored)
        pv.set_line(MAX_PLY, move1, &[]);
        let (_line, len) = pv.line(MAX_PLY);
        assert_eq!(len, 0, "Beyond MAX_PLY should return empty");

        // Test very long PV (MAX_PLY - 1 moves)
        let mut long_tail = Vec::new();
        for _ in 0..MAX_PLY - 2 {
            long_tail.push(Move::normal(
                parse_usi_square("2g").unwrap(),
                parse_usi_square("2f").unwrap(),
                false,
            ));
        }

        pv.set_line(0, move1, &long_tail);
        let (_line, len) = pv.line(0);
        assert_eq!(len, MAX_PLY - 1, "Should handle maximum length PV");
    }

    #[test]
    fn test_pv_clear_operations() {
        // Test clear functionality
        let mut pv = PVTable::new();

        // Add some moves
        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        let move2 =
            Move::normal(parse_usi_square("6c").unwrap(), parse_usi_square("6d").unwrap(), false);

        pv.set_line(0, move1, &[move2]);
        pv.set_line(1, move2, &[]);

        // Verify they're set
        assert_eq!(pv.line(0).1, 2);
        assert_eq!(pv.line(1).1, 1);

        // Clear specific ply
        pv.clear_len_at(0);
        assert_eq!(pv.line(0).1, 0, "Ply 0 should be cleared");
        assert_eq!(pv.line(1).1, 1, "Ply 1 should remain");

        // Clear all
        pv.clear_all();
        assert_eq!(pv.line(0).1, 0, "All plies should be cleared");
        assert_eq!(pv.line(1).1, 0, "All plies should be cleared");
    }

    #[test]
    fn test_pv_consistency_after_updates() {
        // Test that PV remains consistent after multiple updates
        let mut pv = PVTable::new();

        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        let move2 =
            Move::normal(parse_usi_square("6c").unwrap(), parse_usi_square("6d").unwrap(), false);
        let move3 =
            Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);

        // Initial PV
        pv.set_line(0, move1, &[move2, move3]);
        assert_eq!(pv.line(0).1, 3);

        // Update with shorter PV
        pv.set_line(0, move2, &[move3]);
        assert_eq!(pv.line(0).1, 2);
        let (line, _) = pv.line(0);
        assert_eq!(line[0], move2);
        assert_eq!(line[1], move3);

        // Update with longer PV
        pv.set_line(0, move3, &[move1, move2, move3]);
        assert_eq!(pv.line(0).1, 4);
    }
}
