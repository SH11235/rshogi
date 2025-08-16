//! Compatibility wrapper for move generation

use crate::{shogi::MoveList, Position};

use super::generator::MoveGenImpl;

/// Simple move generator (for compatibility)
pub struct MoveGen;

impl Default for MoveGen {
    fn default() -> Self {
        Self::new()
    }
}

impl MoveGen {
    /// Create new move generator
    pub fn new() -> Self {
        MoveGen
    }

    /// Generate all legal moves
    pub fn generate_all(&mut self, pos: &Position, moves: &mut MoveList) {
        let mut gen = MoveGenImpl::new(pos);
        let all_moves = gen.generate_all();
        moves.clear();
        for mv in all_moves.as_slice() {
            moves.push(*mv);
        }
    }

    /// Generate only capture moves
    pub fn generate_captures(&mut self, pos: &Position, moves: &mut MoveList) {
        let mut gen = MoveGenImpl::new(pos);
        let all_moves = gen.generate_all();
        moves.clear();

        // Filter captures
        for mv in all_moves.as_slice() {
            if !mv.is_drop() {
                let to = mv.to();
                if pos.board.piece_on(to).is_some() {
                    moves.push(*mv);
                }
            }
        }
    }

    /// Generate evasion moves (when in check)
    pub fn generate_evasions(&mut self, pos: &Position, moves: &mut MoveList) {
        // For now, just generate all moves
        self.generate_all(pos, moves);
    }
}
