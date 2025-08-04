//! Principal Variation (PV) management
//!
//! Tracks the best move sequence found during search

use crate::{search::constants::MAX_PLY, shogi::Move};

/// Principal Variation table
pub struct PVTable {
    /// PV lines for each ply [ply][move_index]
    lines: Vec<Vec<Move>>,
}

impl Default for PVTable {
    fn default() -> Self {
        Self::new()
    }
}

impl PVTable {
    /// Create a new PV table
    pub fn new() -> Self {
        Self {
            lines: vec![Vec::new(); MAX_PLY],
        }
    }

    /// Clear all PV lines
    pub fn clear(&mut self) {
        for line in &mut self.lines {
            line.clear();
        }
    }

    /// Update PV at given ply
    pub fn update(&mut self, ply: usize, best_move: Move, child_pv: &[Move]) {
        if ply >= self.lines.len() {
            return;
        }

        self.lines[ply].clear();
        self.lines[ply].push(best_move);
        self.lines[ply].extend_from_slice(child_pv);
    }

    /// Update from a complete line
    pub fn update_from_line(&mut self, pv: &[Move]) {
        if !pv.is_empty() {
            self.lines[0].clear();
            self.lines[0].extend_from_slice(pv);
        }
    }

    /// Get PV line at given ply
    pub fn get_line(&self, ply: usize) -> &[Move] {
        if ply < self.lines.len() {
            &self.lines[ply]
        } else {
            &[]
        }
    }

    /// Get the main PV (from root)
    pub fn get_pv(&self) -> &[Move] {
        &self.lines[0]
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
