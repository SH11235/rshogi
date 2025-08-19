//! Move generation methods for move picker

use super::types::ScoredMove;
use super::MovePicker;
use crate::shogi::{Move, MoveList};
use crate::MoveGen;

impl<'a> MovePicker<'a> {
    /// Generate capture moves
    pub(super) fn generate_captures(&mut self) {
        self.moves.clear();
        let mut move_list = MoveList::new();
        let mut gen = MoveGen::new();
        gen.generate_captures(&self.pos, &mut move_list);

        for &mv in move_list.as_slice() {
            self.moves.push(ScoredMove::new(mv, 0));
        }
    }

    /// Generate quiet moves
    pub(super) fn generate_quiets(&mut self) {
        self.moves.clear();
        let mut move_list = MoveList::new();
        let mut gen = MoveGen::new();
        gen.generate_all(&self.pos, &mut move_list);

        // Add only non-captures that are not killers, TT move, or PV move
        for &mv in move_list.as_slice() {
            if !self.is_capture(mv)
                && Some(mv) != self.tt_move
                && Some(mv) != self.pv_move
                && !self.is_killer(mv)
            {
                self.moves.push(ScoredMove::new(mv, 0));
            }
        }
    }

    /// Check if move is a capture
    pub(super) fn is_capture(&self, mv: Move) -> bool {
        !mv.is_drop() && self.pos.board.piece_on(mv.to()).is_some()
    }

    /// Check if move is a killer
    pub(super) fn is_killer(&self, mv: Move) -> bool {
        self.stack.killers[0] == Some(mv) || self.stack.killers[1] == Some(mv)
    }

    /// Get captured piece
    pub(super) fn get_captured_piece(&self, mv: Move) -> Option<crate::PieceType> {
        if mv.is_drop() {
            None
        } else {
            self.pos.board.piece_on(mv.to()).map(|p| p.piece_type)
        }
    }
}
