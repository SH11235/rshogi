//! Principal Variation (PV) management
//!
//! Tracks the best move sequence found during search

use crate::{
    search::constants::MAX_PLY,
    shogi::{Move, Position},
};

/// NULL move constant for uninitialized entries
const NULL_MOVE: Move = Move::NULL;

/// Principal Variation table with strict length management
///
/// Uses epoch-based invalidation for O(1) clear operations
pub struct PVTable {
    /// Move storage [ply][move_index]
    mv: [[Move; MAX_PLY]; MAX_PLY],
    /// Length of PV at each ply
    len: [usize; MAX_PLY],
    /// Owner hash for each PV line (the zobrist hash of the position that wrote this line)
    owner: [u64; MAX_PLY],
    /// Epoch when each row was last written
    row_epoch: [u32; MAX_PLY],
    /// Current epoch number (incremented at each iteration)
    cur_epoch: u32,
}

impl PVTable {
    /// Create a new PV table
    pub fn new() -> Self {
        Self {
            mv: [[NULL_MOVE; MAX_PLY]; MAX_PLY],
            len: [0; MAX_PLY],
            owner: [0; MAX_PLY],
            row_epoch: [0; MAX_PLY],
            cur_epoch: 1, // Start from 1 to distinguish from uninitialized (0)
        }
    }

    /// Begin a new iteration - O(1) operation
    #[inline]
    pub fn begin_iteration(&mut self) {
        self.cur_epoch = self.cur_epoch.wrapping_add(1);
        // Handle epoch wraparound
        if self.cur_epoch == 0 {
            // Rare case: epoch wrapped around to 0
            // Clear all row epochs to ensure they're invalid
            self.row_epoch.fill(0);
            self.cur_epoch = 1;
        }
    }

    /// Clear all PV lines (for new iteration) - O(1) operation
    #[inline]
    pub fn clear_all(&mut self) {
        // Simply increment epoch - old data becomes invisible
        self.begin_iteration();
    }

    /// Check if a row is from the current epoch
    #[inline]
    fn is_current(&self, ply: usize) -> bool {
        ply < MAX_PLY && self.row_epoch[ply] == self.cur_epoch
    }

    /// Mark a row as written in the current epoch
    #[inline]
    fn mark_written(&mut self, ply: usize) {
        if ply < MAX_PLY {
            self.row_epoch[ply] = self.cur_epoch;
        }
    }

    /// Clear PV at specific ply (on node entry)
    #[inline]
    pub fn clear_len_at(&mut self, ply: usize) {
        if ply < MAX_PLY {
            // Debug (before clear): inspect existing PV content
            #[cfg(all(debug_assertions, feature = "pv_debug_logs"))]
            {
                let old_len = self.len[ply];
                if ply <= 10 && old_len > 0 {
                    for i in 0..old_len.min(MAX_PLY) {
                        let mv = self.mv[ply][i];
                        if mv != NULL_MOVE {
                            let mv_str = crate::usi::move_to_usi(&mv);
                            if mv_str == "3i3h" || mv_str == "4b5b" {
                                eprintln!(
                                    "[PV CLEAR] Found {} at ply={ply}, index={i} before clearing",
                                    mv_str
                                );
                            }
                        }
                    }
                }
            }

            // Clear length/owner and mark epoch
            self.len[ply] = 0;
            self.owner[ply] = 0;
            self.mark_written(ply);
            // Setting length to 0 is sufficient for safety as readers always check len[ply]
            // Full array clearing is only done in debug mode for visibility

            // Debug (after clear): optionally blank moves for visibility
            #[cfg(all(debug_assertions, feature = "pv_debug_logs"))]
            {
                for i in 0..MAX_PLY {
                    self.mv[ply][i] = NULL_MOVE;
                }
            }
        }
    }

    /// Get PV line with length (no allocation)
    #[inline]
    pub fn line(&self, ply: usize) -> (&[Move], usize) {
        if self.is_current(ply) {
            let len = self.len[ply];
            (&self.mv[ply][..len], len)
        } else {
            // Row is from a previous epoch - treat as empty
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
            crate::pv_debug!("[WARNING] Attempted to add NULL move to PV at ply {ply}");
            return;
        }

        let tail_len = tail.len().min(MAX_PLY - 1);
        self.mv[ply][0] = head;
        if tail_len > 0 {
            // Use exclusive range to avoid panic when tail_len == 0
            self.mv[ply][1..(1 + tail_len)].copy_from_slice(&tail[..tail_len]);
        }
        self.len[ply] = tail_len + 1;
        // Mark this row as written in current epoch
        self.mark_written(ply);
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
            // Mark root line as written
            self.mark_written(0);
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

    /// Set the owner hash for a PV line
    #[inline]
    pub fn set_owner(&mut self, ply: usize, hash: u64) {
        if ply < MAX_PLY {
            self.owner[ply] = hash;
        }
    }

    /// Get the owner hash for a PV line
    ///
    /// Returns Some(hash) only when the line has valid content (len > 0).
    /// Note: 0 is a valid zobrist hash value, so we don't use it as a sentinel.
    /// The owner is only meaningful for non-empty PV lines.
    #[inline]
    pub fn owner(&self, ply: usize) -> Option<u64> {
        if ply < MAX_PLY && self.len[ply] > 0 && self.is_current(ply) {
            Some(self.owner[ply])
        } else {
            None
        }
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

        // Validate child row is from current epoch
        #[cfg(debug_assertions)]
        debug_assert!(self.is_current(child_ply), "child row must be current epoch");

        // Skip null moves
        if best_move == Move::NULL {
            #[cfg(debug_assertions)]
            crate::pv_debug!("[WARNING] Attempted to add NULL move to PV at ply {ply}");
            return;
        }

        // Debug logging for PV updates in problematic positions
        #[cfg(debug_assertions)]
        if cfg!(feature = "pv_debug_logs") && ply <= 10 {
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
        // Mark this row as written in current epoch
        self.mark_written(ply);
    }
}

impl Default for PVTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Trim PV to contain only legal moves
///
/// This function validates each move in the PV and returns a new vector
/// containing only the legal prefix. It stops at the first illegal move.
///
/// # Arguments
/// * `pos` - Current position (will be cloned internally)
/// * `pv` - Principal variation to validate
///
/// # Returns
/// A vector containing only the legal moves from the start of the PV
pub fn trim_legal_pv(pos: Position, pv: &[Move]) -> Vec<Move> {
    // Early returns for invalid PVs
    if pv.is_empty() || pv.len() > MAX_PLY || pv.contains(&Move::NULL) {
        return Vec::new();
    }

    let mut clean = Vec::with_capacity(pv.len());
    let mut temp_pos = pos;

    for &mv in pv {
        if temp_pos.is_legal_move(mv) {
            clean.push(mv);
            let _undo = temp_pos.do_move(mv);
            // Don't undo - keep position updated for next move check
        } else {
            // First illegal move found - stop here
            log::debug!(
                "[PV TRIM] Trimming PV at move {}: {}",
                clean.len(),
                crate::usi::move_to_usi(&mv)
            );
            break;
        }
    }

    clean
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{search::unified::core::node::search_node, shogi::Move, usi::parse_usi_square};

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

        assert_eq!(pv.line(0).1, 2);
        assert_eq!(pv.line(1).1, 1);

        // Clear all
        pv.clear_all();

        // All PVs should be empty (epoch-based invisibility)
        assert_eq!(pv.line(0).1, 0);
        assert_eq!(pv.line(1).1, 0);
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

    #[test]
    fn test_pv_owner_hash() {
        // Test owner hash functionality
        let mut pv = PVTable::new();

        // Initially, no owner
        assert_eq!(pv.owner(0), None);
        assert_eq!(pv.owner(5), None);

        // Set a PV line and owner
        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        pv.set_line(0, move1, &[]);
        pv.set_owner(0, 0x123456789ABCDEF0);

        // Owner should be retrievable
        assert_eq!(pv.owner(0), Some(0x123456789ABCDEF0));

        // Clear should reset owner
        pv.clear_len_at(0);
        assert_eq!(pv.owner(0), None);

        // Test clear_all
        pv.set_line(2, move1, &[]);
        pv.set_owner(2, 0xFEDCBA9876543210);
        pv.set_line(5, move1, &[]);
        pv.set_owner(5, 0x1111111111111111);

        assert_eq!(pv.owner(2), Some(0xFEDCBA9876543210));
        assert_eq!(pv.owner(5), Some(0x1111111111111111));

        pv.clear_all();
        assert_eq!(pv.owner(2), None);
        assert_eq!(pv.owner(5), None);
    }

    #[test]
    fn test_pv_owner_mismatch_keeps_head_only() {
        // Test that owner mismatch results in head-only PV
        let mut pv = PVTable::new();

        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        let move2 =
            Move::normal(parse_usi_square("6c").unwrap(), parse_usi_square("6d").unwrap(), false);
        let move3 =
            Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);

        // Set up child PV at ply 1 with owner
        pv.set_line(1, move2, &[move3]);
        pv.set_owner(1, 0xAAAAAAAAAAAAAAAA);

        // Set parent owner
        pv.set_owner(0, 0xBBBBBBBBBBBBBBBB);

        // Simulate owner mismatch scenario:
        // When search_node checks owner(child_ply) != expected_child_hash,
        // it should use set_line with empty tail
        let child_owner = pv.owner(1);
        let expected_child = 0xCCCCCCCCCCCCCCCC; // Different from actual

        if child_owner != Some(expected_child) {
            // This is what search_node does on mismatch
            pv.set_line(0, move1, &[]);
        } else {
            // This would be the normal case
            pv.update_from_child(0, move1, 1);
        }

        // Verify parent has only the head move
        let (line, len) = pv.line(0);
        assert_eq!(len, 1, "Parent should have only head move on owner mismatch");
        assert_eq!(line[0], move1);
    }

    #[test]
    fn test_pv_owner_stats_counts() {
        use crate::{
            evaluation::evaluate::MaterialEvaluator,
            search::{unified::UnifiedSearcher, SearchLimits},
            shogi::Position,
        };
        let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
        searcher.context.set_limits(SearchLimits::builder().depth(3).build());
        // 明示初期化（なくてもbumpがSome化するが、期待値比較のため）
        searcher.stats.pv_owner_checks = Some(0);
        searcher.stats.pv_owner_mismatches = Some(0);

        let mut pos = Position::startpos();
        let _ = search_node(&mut searcher, &mut pos, 2, -1000, 1000, 0);

        // どちらも Some であることと、関係が破綻していないことを緩く確認（回帰検知用）
        let checks = searcher.stats.pv_owner_checks.expect("pv_owner_checks is None");
        let mismatches = searcher.stats.pv_owner_mismatches.expect("pv_owner_mismatches is None");
        assert!(
            mismatches <= checks,
            "pv_owner_mismatches({mismatches}) > pv_owner_checks({checks})"
        );
    }

    #[test]
    fn test_trim_legal_pv() {
        use crate::shogi::Position;

        // Test empty PV
        let pos = Position::startpos();
        let pv = trim_legal_pv(pos.clone(), &[]);
        assert!(pv.is_empty(), "Empty PV should remain empty");

        // Test PV with NULL moves
        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        let pv_with_null = vec![move1, Move::NULL, move1];
        let pv = trim_legal_pv(pos.clone(), &pv_with_null);
        assert!(pv.is_empty(), "PV with NULL moves should be cleared");

        // Test legal PV
        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        let move2 =
            Move::normal(parse_usi_square("3c").unwrap(), parse_usi_square("3d").unwrap(), false);
        let legal_pv = vec![move1, move2];
        let pv = trim_legal_pv(pos.clone(), &legal_pv);
        assert_eq!(pv.len(), 2, "Legal PV should be preserved");
        assert_eq!(pv[0], move1);
        assert_eq!(pv[1], move2);

        // Test PV with illegal move
        let illegal_move =
            Move::normal(parse_usi_square("5g").unwrap(), parse_usi_square("5f").unwrap(), false);
        let mixed_pv = vec![move1, illegal_move, move2];
        let pv = trim_legal_pv(pos.clone(), &mixed_pv);
        assert_eq!(pv.len(), 1, "PV should be trimmed at first illegal move");
        assert_eq!(pv[0], move1);

        // Test PV exceeding MAX_PLY
        let mut long_pv = Vec::with_capacity(MAX_PLY + 10);
        for _ in 0..MAX_PLY + 10 {
            long_pv.push(move1);
        }
        let pv = trim_legal_pv(pos.clone(), &long_pv);
        assert!(pv.is_empty(), "PV exceeding MAX_PLY should be cleared");
    }

    #[test]
    fn test_epoch_mismatch_is_empty() {
        // Test that previous epoch data is invisible
        let mut pv = PVTable::new();

        // Set up some data
        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        let move2 =
            Move::normal(parse_usi_square("6c").unwrap(), parse_usi_square("6d").unwrap(), false);

        // Write to ply 0 and 1
        pv.set_line(0, move1, &[move2]);
        pv.set_line(1, move2, &[]);
        pv.set_owner(0, 0x1234567890ABCDEF);
        pv.set_owner(1, 0xFEDCBA0987654321);

        // Verify data is visible
        assert_eq!(pv.line(0).1, 2);
        assert_eq!(pv.line(1).1, 1);
        assert_eq!(pv.owner(0), Some(0x1234567890ABCDEF));
        assert_eq!(pv.owner(1), Some(0xFEDCBA0987654321));

        // Start new iteration
        pv.begin_iteration();

        // Old data should be invisible
        assert_eq!(pv.line(0).1, 0, "Old epoch data should be invisible");
        assert_eq!(pv.line(1).1, 0, "Old epoch data should be invisible");
        assert_eq!(pv.owner(0), None, "Old epoch owner should be None");
        assert_eq!(pv.owner(1), None, "Old epoch owner should be None");

        // Write new data to ply 0 only
        pv.set_line(0, move2, &[]);
        pv.set_owner(0, 0xAAAAAAAAAAAAAAAA);

        // New data should be visible, old data still invisible
        assert_eq!(pv.line(0).1, 1);
        assert_eq!(pv.owner(0), Some(0xAAAAAAAAAAAAAAAA));
        assert_eq!(pv.line(1).1, 0, "Unwritten row should remain invisible");
        assert_eq!(pv.owner(1), None, "Unwritten row owner should be None");
    }

    #[test]
    fn test_clear_all_is_o1() {
        // Test that clear_all doesn't write to arrays
        let mut pv = PVTable::new();

        // Fill with data
        for ply in 0..10 {
            let move1 = Move::normal(
                parse_usi_square("7g").unwrap(),
                parse_usi_square("7f").unwrap(),
                false,
            );
            pv.set_line(ply, move1, &[]);
        }

        // Get the current epoch
        let epoch_before = pv.cur_epoch;

        // Clear all - should just increment epoch
        pv.clear_all();

        // Verify epoch was incremented
        assert_eq!(pv.cur_epoch, epoch_before + 1);

        // All data should be invisible
        for ply in 0..10 {
            assert_eq!(pv.line(ply).1, 0);
        }
    }

    #[test]
    fn test_epoch_wraparound() {
        // Test epoch wraparound handling
        let mut pv = PVTable::new();

        // Set epoch near wraparound
        pv.cur_epoch = u32::MAX - 1;

        // Write some data
        let move1 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
        pv.set_line(0, move1, &[]);

        // Trigger wraparound
        pv.begin_iteration(); // MAX
        pv.begin_iteration(); // Wraps to 0, then resets to 1

        assert_eq!(pv.cur_epoch, 1, "Epoch should reset to 1 after wraparound");

        // Old data should be invisible
        assert_eq!(pv.line(0).1, 0);

        // New data should work normally
        pv.set_line(0, move1, &[]);
        assert_eq!(pv.line(0).1, 1);
    }
}
