//! Move picker for staged move generation and ordering
//!
//! Generates moves in stages for better search efficiency:
//! 1. TT move
//! 2. Good captures (SEE >= 0)
//! 3. Killer moves
//! 4. Quiet moves (history ordered)
//! 5. Bad captures (SEE < 0)

use super::types::{MovePickerStage, ScoredMove};
use crate::search::types::SearchStack;
use crate::shogi::{Move, Position};
use crate::History;

/// Move picker for efficient move ordering
pub struct MovePicker<'a> {
    /// Current position
    pub(super) pos: Position,
    /// TT move
    pub(super) tt_move: Option<Move>,
    /// PV move (from previous iteration)
    pub(super) pv_move: Option<Move>,
    /// History heuristics
    pub(super) history: &'a History,
    /// Search stack entry
    pub(super) stack: SearchStack,
    /// Current stage
    pub(super) stage: MovePickerStage,
    /// Generated moves
    pub(super) moves: Vec<ScoredMove>,
    /// Bad captures
    pub(super) bad_captures: Vec<ScoredMove>,
    /// Current index in moves/killers/bad_captures depending on stage
    /// - In Killers stage: index into stack.killers[] (0-1)
    /// - In BadCaptures stage: index into bad_captures vector
    /// - Reset to 0 when transitioning to a new stage
    pub(super) current: usize,
    /// Skip quiet moves (for quiescence search)
    pub(super) skip_quiets: bool,
}

impl<'a> MovePicker<'a> {
    /// Create new move picker for main search
    pub fn new(
        pos: &Position,
        tt_move: Option<Move>,
        pv_move: Option<Move>,
        history: &'a History,
        stack: &SearchStack,
        ply: usize,
    ) -> Self {
        // Validate PV move: check for duplicates and legality
        let validated_pv_move = pv_move.filter(|&mv| {
            // Skip if same as TT move
            tt_move != Some(mv) &&
            // Check legality
            pos.is_legal_move(mv)
        });

        // Determine starting stage based on root node and PV availability
        let stage = if ply == 0 && validated_pv_move.is_some() {
            MovePickerStage::RootPV
        } else {
            MovePickerStage::TTMove
        };

        MovePicker {
            pos: pos.clone(),
            tt_move,
            pv_move: validated_pv_move,
            history,
            stack: stack.clone(),
            stage,
            moves: Vec::new(),
            bad_captures: Vec::new(),
            current: 0, // Initialize to 0, will be used differently in each stage
            skip_quiets: false,
        }
    }

    /// Create new move picker for quiescence search (captures only)
    ///
    /// In quiescence search, we skip quiet moves and bad captures.
    /// Only good captures (SEE >= 0) are considered to avoid search explosion.
    /// This is why bad captures are placed after quiet moves in normal search -
    /// they're often not searched at all.
    pub fn new_quiescence(
        pos: &Position,
        tt_move: Option<Move>,
        history: &'a History,
        stack: &SearchStack,
        ply: usize,
    ) -> Self {
        let mut picker = Self::new(pos, tt_move, None, history, stack, ply);
        picker.skip_quiets = true;
        picker
    }

    /// Get next move
    pub fn next_move(&mut self) -> Option<Move> {
        loop {
            match self.stage {
                MovePickerStage::RootPV => {
                    self.stage = MovePickerStage::TTMove;
                    if let Some(pv_move) = self.pv_move {
                        return Some(pv_move);
                    }
                }

                MovePickerStage::TTMove => {
                    self.stage = MovePickerStage::GenerateCaptures;
                    if let Some(tt_move) = self.tt_move {
                        if self.pos.is_legal_move(tt_move) {
                            return Some(tt_move);
                        }
                    }
                }

                MovePickerStage::GenerateCaptures => {
                    self.generate_captures();
                    self.score_captures();
                    self.stage = MovePickerStage::GoodCaptures;
                }

                MovePickerStage::GoodCaptures => {
                    if let Some(mv) = self.pick_best() {
                        if Some(mv) != self.tt_move && Some(mv) != self.pv_move {
                            // Separate good and bad captures
                            let score = self.see(mv);
                            if score >= 0 {
                                return Some(mv);
                            } else {
                                // Save bad capture for later
                                self.bad_captures.push(ScoredMove::new(mv, score));
                            }
                        }
                    } else {
                        self.stage = if self.skip_quiets {
                            MovePickerStage::End
                        } else {
                            MovePickerStage::Killers
                        };
                        self.current = 0; // Reset index for killer moves iteration
                    }
                }

                MovePickerStage::Killers => {
                    if self.current < 2 {
                        // Check up to 2 killer moves
                        if let Some(killer) = self.stack.killers[self.current] {
                            self.current += 1; // Move to next killer slot
                            if Some(killer) != self.tt_move
                                && Some(killer) != self.pv_move
                                && !self.is_capture(killer)
                                && self.pos.is_legal_move(killer)
                            {
                                return Some(killer);
                            }
                        } else {
                            self.current += 1; // Skip empty killer slot
                        }
                    } else {
                        // Transition from Killers to GenerateQuiets
                        // Next we'll generate and score quiet moves
                        self.stage = MovePickerStage::GenerateQuiets;
                        self.current = 0; // Not used in GenerateQuiets, but good practice to reset
                    }
                }

                MovePickerStage::GenerateQuiets => {
                    self.generate_quiets();
                    self.score_quiets();
                    // Transition to QuietMoves (not BadCaptures)
                    // This is intentional: quiet moves with good history scores
                    // are more likely to be good than losing captures
                    self.stage = MovePickerStage::QuietMoves;
                    self.current = 0; // Not used in QuietMoves (pick_best manages its own index)
                }

                MovePickerStage::QuietMoves => {
                    if let Some(mv) = self.pick_best() {
                        // No need to check for TT move or killers - already filtered during generation
                        return Some(mv);
                    } else {
                        // Bad captures come last - they rarely produce good moves
                        // and are often pruned by Late Move Reductions
                        self.stage = MovePickerStage::BadCaptures;
                        self.current = 0; // Reset index to iterate through bad_captures vector
                    }
                }

                MovePickerStage::BadCaptures => {
                    if self.current < self.bad_captures.len() {
                        let mv = self.bad_captures[self.current].mv;
                        self.current += 1; // Move to next bad capture
                        if Some(mv) != self.tt_move && Some(mv) != self.pv_move {
                            return Some(mv);
                        }
                    } else {
                        self.stage = MovePickerStage::End;
                    }
                }

                MovePickerStage::End => {
                    return None;
                }
            }
        }
    }
}
