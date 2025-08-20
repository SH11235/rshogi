//! Move scoring methods for move picker

use super::MovePicker;
use crate::shogi::Move;

/// Number of bits to shift SEE value left for score packing
pub(crate) const SEE_PACK_SHIFT: i32 = 8;

/// Number of bits available for tie-breaking values
const TIE_BREAK_BITS: i32 = SEE_PACK_SHIFT;

/// Mask for tie-breaking values (lower 8 bits)
const TIE_BREAK_MASK: i32 = (1 << TIE_BREAK_BITS) - 1;

/// Promotion bonus for quiet moves (significant compared to typical history scores ±1000)
pub(crate) const QUIET_PROMO_BONUS: i32 = 300;

/// Tie-break bonus for promoting captures (in lower bits of packed score)
pub(crate) const CAPTURE_PROMO_TIE_BREAK: i32 = 1;

impl<'a> MovePicker<'a> {
    /// Score captures using Static Exchange Evaluation (SEE)
    ///
    /// Score format (32-bit integer):
    /// - Upper 24 bits (bit 31-8): SEE value (preserves sign for good/bad capture classification)
    /// - Lower 8 bits (bit 7-0): Tie-breaking values (currently only promotion bonus)
    pub(super) fn score_captures(&mut self) {
        for i in 0..self.moves.len() {
            let mv = self.moves[i].mv;
            if self.get_captured_piece(mv).is_some() {
                // Calculate SEE value for this capture
                let see_value = self.see(mv);

                // Debug check: ensure SEE value won't overflow when shifted
                debug_assert!(
                    see_value.abs() <= (i32::MAX >> SEE_PACK_SHIFT),
                    "SEE value {see_value} would overflow when packed"
                );

                // Calculate tie-break value with range check
                let tie_break =
                    (i32::from(mv.is_promote()) * CAPTURE_PROMO_TIE_BREAK) & TIE_BREAK_MASK;
                debug_assert!(
                    (0..=TIE_BREAK_MASK).contains(&tie_break),
                    "Tie-break value {tie_break} exceeds allowed range"
                );

                // Pack score: upper bits for SEE (preserves sign), lower bits for tie-breaking
                // Use bitwise OR to make the intent clearer
                self.moves[i].score = (see_value << SEE_PACK_SHIFT) | tie_break;
            }
        }
    }

    /// Score quiet moves using history
    pub(super) fn score_quiets(&mut self) {
        for scored_move in &mut self.moves {
            let mv = scored_move.mv;
            scored_move.score = self.history.get_score(self.pos.side_to_move, mv, None);

            // Promotion bonus: bias towards promotions in move ordering
            // (history score typically ranges ±1000, so +300 is significant)
            if mv.is_promote() {
                scored_move.score += QUIET_PROMO_BONUS;
            }
        }
    }

    /// Pick best move from current list
    pub(super) fn pick_best(&mut self) -> Option<Move> {
        self.pick_best_scored().map(|(mv, _)| mv)
    }

    /// Pick best move from current list with its score
    /// Returns the move and its ordering score (may include tie-breaking bonuses)
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
