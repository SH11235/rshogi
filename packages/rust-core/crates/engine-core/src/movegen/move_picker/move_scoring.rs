//! Move scoring methods for move picker

use super::MovePicker;
use crate::shogi::Move;

impl<'a> MovePicker<'a> {
    /// Score captures using Static Exchange Evaluation (SEE)
    pub(super) fn score_captures(&mut self) {
        for i in 0..self.moves.len() {
            let mv = self.moves[i].mv;
            if self.get_captured_piece(mv).is_some() {
                // Calculate SEE value for this capture
                let see_value = self.see(mv);

                // Use SEE value as the primary score
                self.moves[i].score = see_value;

                // Small promotion bonus (SEE already accounts for promoted piece value)
                if mv.is_promote() {
                    self.moves[i].score += 100;
                }
            }
        }
    }

    /// Score quiet moves using history
    pub(super) fn score_quiets(&mut self) {
        for scored_move in &mut self.moves {
            let mv = scored_move.mv;
            scored_move.score = self.history.get_score(self.pos.side_to_move, mv, None);

            // Promotion bonus
            if mv.is_promote() {
                scored_move.score += 300;
            }
        }
    }

    /// Pick best move from current list
    pub(super) fn pick_best(&mut self) -> Option<Move> {
        self.pick_best_scored().map(|(mv, _)| mv)
    }

    /// Pick best move from current list with its score
    pub(super) fn pick_best_scored(&mut self) -> Option<(Move, i32)> {
        if self.current >= self.moves.len() {
            return None;
        }

        // Find best remaining move
        let best_idx = self.current
            + self.moves[self.current..]
                .iter()
                .enumerate()
                .max_by_key(|(_, m)| m.score)
                .map(|(i, _)| i)?;

        // Swap with current position
        self.moves.swap(self.current, best_idx);
        let scored_move = self.moves[self.current];
        self.current += 1;

        Some((scored_move.mv, scored_move.score))
    }

    /// Static Exchange Evaluation
    pub(super) fn see(&self, mv: Move) -> i32 {
        // Use the full SEE implementation from Position
        self.pos.see(mv)
    }
}
