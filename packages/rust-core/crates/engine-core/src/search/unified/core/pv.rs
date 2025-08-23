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
            // Setting length to 0 is sufficient for safety as readers always check len[ply]
            // Full array clearing is only done in debug mode for visibility

            #[cfg(debug_assertions)]
            if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                // Check for suspicious moves before clearing (debug only)
                if ply <= 10 && self.len[ply] > 0 {
                    for i in 0..self.len[ply].min(10) {
                        let mv = self.mv[ply][i];
                        if mv != NULL_MOVE {
                            let mv_str = crate::usi::move_to_usi(&mv);
                            if mv_str == "3i3h" {
                                eprintln!(
                                    "[PV CLEAR] Found 3i3h at ply={ply}, index={i} before clearing"
                                );
                            }
                        }
                    }
                }

                // Clear all moves for debugging visibility (performance impact acceptable in debug)
                for i in 0..MAX_PLY {
                    self.mv[ply][i] = NULL_MOVE;
                }
            }
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

    /// Update PV by copying from child ply without allocation
    /// This method avoids the need to create a temporary vector
    #[inline]
    pub fn update_from_child(&mut self, ply: usize, best_move: Move, child_ply: usize) {
        if ply >= MAX_PLY || child_ply >= MAX_PLY {
            return;
        }

        // Validate non-overlapping copy precondition
        debug_assert!(child_ply != ply, "PV copy must be across different rows");
        debug_assert!(
            child_ply > ply,
            "Child ply ({child_ply}) should be greater than parent ply ({ply})"
        );

        // Skip null moves
        if best_move == Move::NULL {
            #[cfg(debug_assertions)]
            if std::env::var("SHOGI_DEBUG_PV").is_ok() {
                eprintln!("[WARNING] Attempted to add NULL move to PV at ply {ply}");
            }
            return;
        }

        // Debug logging for PV updates in problematic positions
        #[cfg(debug_assertions)]
        if std::env::var("SHOGI_DEBUG_PV").is_ok() && ply <= 10 {
            eprintln!(
                "[PV UPDATE] ply={ply}, best_move={}, child_ply={child_ply}, child_len={}",
                crate::usi::move_to_usi(&best_move),
                self.len[child_ply]
            );
            if self.len[child_ply] > 0 {
                eprintln!(
                    "  Child PV: {}",
                    self.mv[child_ply][..self.len[child_ply]]
                        .iter()
                        .map(crate::usi::move_to_usi)
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }

            // Check for suspicious 3i3h in the resulting PV
            let resulting_len = self.len[child_ply] + 1;
            if resulting_len >= 8 && ply == 0 {
                eprintln!("  Checking resulting PV at ply 0 (len={resulting_len}):");
                for i in 0..resulting_len.min(10) {
                    let mv = if i == 0 {
                        best_move
                    } else {
                        self.mv[child_ply][i - 1]
                    };
                    let mv_str = crate::usi::move_to_usi(&mv);
                    eprintln!("    PV[{}] = {}", i, mv_str);
                    if mv_str == "3i3h" {
                        eprintln!("    ^^^ FOUND 3i3h at index {}!", i);
                    }
                }
            }
        }

        // Get child PV length
        let child_len = self.len[child_ply];
        let copy_len = child_len.min(MAX_PLY - 1);

        // Set the best move
        self.mv[ply][0] = best_move;

        // Copy child PV directly without temporary allocation
        if copy_len > 0 {
            // Additional validation: ensure child PV doesn't contain NULL moves
            #[cfg(debug_assertions)]
            {
                for i in 0..copy_len {
                    if self.mv[child_ply][i] == Move::NULL {
                        eprintln!("[WARNING] Child PV contains NULL move at index {i}, truncating");
                        self.len[ply] = 1; // Only keep the best move
                        return;
                    }
                }
            }

            // We must use unsafe here because we're borrowing different rows of the same 2D array
            // Safe because debug_assert ensures child_ply != ply (non-overlapping)
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.mv[child_ply].as_ptr(),
                    self.mv[ply][1..].as_mut_ptr(),
                    copy_len,
                );
            }
        }

        self.len[ply] = copy_len + 1;
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

    #[test]
    fn test_update_from_child() {
        // Test the new update_from_child method
        let mut pv = PVTable::new();

        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        let move2 =
            Move::normal(parse_usi_square("6c").unwrap(), parse_usi_square("6d").unwrap(), false);
        let move3 =
            Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);

        // Set up child PV at ply 1
        pv.set_line(1, move2, &[move3]);
        assert_eq!(pv.line(1).1, 2);

        // Update parent PV from child
        pv.update_from_child(0, move1, 1);

        // Check that PV was properly updated
        let (line, len) = pv.line(0);
        assert_eq!(len, 3);
        assert_eq!(line[0], move1);
        assert_eq!(line[1], move2);
        assert_eq!(line[2], move3);

        // Test with empty child PV
        pv.clear_len_at(2);
        pv.update_from_child(1, move1, 2);
        let (line, len) = pv.line(1);
        assert_eq!(len, 1);
        assert_eq!(line[0], move1);

        // Test NULL move handling
        pv.update_from_child(0, Move::NULL, 1);
        // Should not update due to NULL move
        let (_, len) = pv.line(0);
        assert_eq!(len, 3); // Should remain unchanged
    }

    #[test]
    fn test_pv_table_clear_len_safety() {
        // Test that clearing length prevents reading stale data
        let mut pv = PVTable::new();

        // Simulate the problematic scenario from PV validation error
        let _move_3i4h =
            Move::normal(parse_usi_square("3i").unwrap(), parse_usi_square("4h").unwrap(), false);
        let move_3i3h =
            Move::normal(parse_usi_square("3i").unwrap(), parse_usi_square("3h").unwrap(), false);
        let _move_8b5b =
            Move::normal(parse_usi_square("8b").unwrap(), parse_usi_square("5b").unwrap(), false);

        // First search path: Set PV at ply 8 with move 3i3h
        pv.set_line(8, move_3i3h, &[]);
        assert_eq!(pv.line(8).1, 1, "PV should have 1 move");

        // Clear ply 8 length (simulating node entry)
        pv.clear_len_at(8);

        // Check that after clearing, the line reports empty
        let (line, len) = pv.line(8);
        assert_eq!(len, 0, "Cleared ply should report length 0");
        assert!(line.is_empty(), "Line slice should be empty when len=0");

        // Verify that get_line also returns empty (public API)
        assert!(pv.get_line(8).is_empty(), "get_line should return empty slice");

        // The key safety property: even if buffer contains old data,
        // it won't be read because all accessors respect len[ply]
    }
}
