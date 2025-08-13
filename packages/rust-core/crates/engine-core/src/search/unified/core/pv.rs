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

        let tail_len = tail.len().min(MAX_PLY - 1);
        self.mv[ply][0] = head;
        self.mv[ply][1..=tail_len].copy_from_slice(&tail[..tail_len]);
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
}
