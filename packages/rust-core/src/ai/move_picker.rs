//! Move picker for staged move generation and ordering
//!
//! Generates moves in stages for better search efficiency:
//! 1. TT move
//! 2. Good captures (MVV-LVA)
//! 3. Killer moves
//! 4. Bad captures
//! 5. Quiet moves (history ordered)

use super::board::{PieceType, Position};
use super::history::History;
use super::movegen::MoveGen;
use super::moves::{Move, MoveList};
use super::search_enhanced::SearchStack;
use std::sync::Arc;

/// Scored move for ordering
#[derive(Clone, Copy, Debug)]
struct ScoredMove {
    mv: Move,
    score: i32,
}

impl ScoredMove {
    fn new(mv: Move, score: i32) -> Self {
        ScoredMove { mv, score }
    }
}

/// Stage of move generation
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MovePickerStage {
    /// TT move
    TTMove,
    /// Generate captures
    GenerateCaptures,
    /// Good captures
    GoodCaptures,
    /// Killer moves
    Killers,
    /// Generate quiet moves
    GenerateQuiets,
    /// Bad captures
    BadCaptures,
    /// All quiet moves
    QuietMoves,
    /// End of moves
    End,
}

/// Move picker for efficient move ordering
pub struct MovePicker {
    /// Current position
    pos: Position,
    /// TT move
    tt_move: Option<Move>,
    /// History heuristics
    history: Arc<History>,
    /// Search stack entry
    stack: SearchStack,
    /// Current stage
    stage: MovePickerStage,
    /// Generated moves
    moves: Vec<ScoredMove>,
    /// Bad captures
    bad_captures: Vec<ScoredMove>,
    /// Current index in moves
    current: usize,
    /// Skip quiet moves (for quiescence search)
    skip_quiets: bool,
}

impl MovePicker {
    /// Create new move picker for main search
    pub fn new(
        pos: &Position,
        tt_move: Option<Move>,
        history: &Arc<History>,
        stack: &SearchStack,
    ) -> Self {
        MovePicker {
            pos: pos.clone(),
            tt_move,
            history: Arc::clone(history),
            stack: stack.clone(),
            stage: MovePickerStage::TTMove,
            moves: Vec::new(),
            bad_captures: Vec::new(),
            current: 0,
            skip_quiets: false,
        }
    }

    /// Create new move picker for quiescence search (captures only)
    pub fn new_quiescence(
        pos: &Position,
        tt_move: Option<Move>,
        history: &Arc<History>,
        stack: &SearchStack,
    ) -> Self {
        let mut picker = Self::new(pos, tt_move, history, stack);
        picker.skip_quiets = true;
        picker
    }

    /// Get next move
    pub fn next_move(&mut self) -> Option<Move> {
        loop {
            match self.stage {
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
                        if Some(mv) != self.tt_move {
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
                        self.current = 0;
                    }
                }

                MovePickerStage::Killers => {
                    if self.current < 2 {
                        if let Some(killer) = self.stack.killers[self.current] {
                            self.current += 1;
                            if Some(killer) != self.tt_move
                                && !self.is_capture(killer)
                                && self.pos.is_legal_move(killer)
                            {
                                return Some(killer);
                            }
                        } else {
                            self.current += 1;
                        }
                    } else {
                        self.stage = MovePickerStage::GenerateQuiets;
                        self.current = 0;
                    }
                }

                MovePickerStage::GenerateQuiets => {
                    self.generate_quiets();
                    self.score_quiets();
                    self.stage = MovePickerStage::BadCaptures;
                }

                MovePickerStage::BadCaptures => {
                    if self.current < self.bad_captures.len() {
                        let mv = self.bad_captures[self.current].mv;
                        self.current += 1;
                        if Some(mv) != self.tt_move {
                            return Some(mv);
                        }
                    } else {
                        self.stage = MovePickerStage::QuietMoves;
                        self.current = 0;
                    }
                }

                MovePickerStage::QuietMoves => {
                    if let Some(mv) = self.pick_best() {
                        if Some(mv) != self.tt_move && !self.is_killer(mv) {
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

    /// Generate capture moves
    fn generate_captures(&mut self) {
        self.moves.clear();
        let mut move_list = MoveList::new();
        let mut gen = MoveGen::new();
        gen.generate_captures(&self.pos, &mut move_list);

        for &mv in move_list.as_slice() {
            self.moves.push(ScoredMove::new(mv, 0));
        }
    }

    /// Generate quiet moves
    fn generate_quiets(&mut self) {
        self.moves.clear();
        let mut move_list = MoveList::new();
        let mut gen = MoveGen::new();
        gen.generate_all(&self.pos, &mut move_list);

        // Add only non-captures
        for &mv in move_list.as_slice() {
            if !self.is_capture(mv) {
                self.moves.push(ScoredMove::new(mv, 0));
            }
        }
    }

    /// Score captures using MVV-LVA
    fn score_captures(&mut self) {
        for i in 0..self.moves.len() {
            let mv = self.moves[i].mv;
            if let Some(captured) = self.get_captured_piece(mv) {
                let victim_value = Self::piece_value(captured);
                let attacker_value = if mv.is_drop() {
                    Self::piece_value(mv.drop_piece_type())
                } else {
                    let from = mv.from().unwrap();
                    if let Some(piece) = self.pos.board.piece_on(from) {
                        Self::piece_value(piece.piece_type)
                    } else {
                        0
                    }
                };

                // MVV-LVA: Most Valuable Victim - Least Valuable Attacker
                self.moves[i].score = victim_value * 100 - attacker_value;

                // Promotion bonus
                if mv.is_promote() {
                    self.moves[i].score += 500;
                }
            }
        }
    }

    /// Score quiet moves using history
    fn score_quiets(&mut self) {
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
    fn pick_best(&mut self) -> Option<Move> {
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
        let result = self.moves[self.current].mv;
        self.current += 1;

        Some(result)
    }

    /// Check if move is a capture
    fn is_capture(&self, mv: Move) -> bool {
        !mv.is_drop() && self.pos.board.piece_on(mv.to()).is_some()
    }

    /// Check if move is a killer
    fn is_killer(&self, mv: Move) -> bool {
        self.stack.killers[0] == Some(mv) || self.stack.killers[1] == Some(mv)
    }

    /// Get captured piece
    fn get_captured_piece(&self, mv: Move) -> Option<PieceType> {
        if mv.is_drop() {
            None
        } else {
            self.pos.board.piece_on(mv.to()).map(|p| p.piece_type)
        }
    }

    /// Static Exchange Evaluation (simplified)
    fn see(&self, mv: Move) -> i32 {
        if mv.is_drop() {
            return 0; // Drops are always good
        }

        let to = mv.to();
        let from = mv.from().unwrap();

        // Get initial material gain
        let mut gain = if let Some(captured) = self.pos.board.piece_on(to) {
            Self::piece_value(captured.piece_type)
        } else {
            return 0; // Not a capture
        };

        // Get attacker value
        if let Some(attacker) = self.pos.board.piece_on(from) {
            let attacker_value = Self::piece_value(attacker.piece_type);

            // Simple SEE: assume piece is recaptured
            // TODO: Implement full SEE with attack detection
            gain -= attacker_value;

            // Promotion value
            if mv.is_promote() {
                gain += Self::promotion_value(attacker.piece_type);
            }
        }

        gain
    }

    /// Get piece value for MVV-LVA and SEE
    fn piece_value(piece_type: PieceType) -> i32 {
        match piece_type {
            PieceType::King => 10000,
            PieceType::Rook => 900,
            PieceType::Bishop => 700,
            PieceType::Gold => 600,
            PieceType::Silver => 500,
            PieceType::Knight => 400,
            PieceType::Lance => 300,
            PieceType::Pawn => 100,
        }
    }

    /// Get promotion value
    fn promotion_value(piece_type: PieceType) -> i32 {
        match piece_type {
            PieceType::Silver => 100, // Silver -> Gold
            PieceType::Knight => 200, // Knight -> Gold
            PieceType::Lance => 300,  // Lance -> Gold
            PieceType::Pawn => 500,   // Pawn -> Gold
            _ => 0,
        }
    }
}

// Extension for Position to check legal moves
impl Position {
    /// Check if a move is legal (simplified version)
    pub fn is_legal_move(&self, mv: Move) -> bool {
        // Generate all legal moves and check if mv is among them
        let mut moves = MoveList::new();
        let mut gen = MoveGen::new();
        gen.generate_all(self, &mut moves);

        moves.as_slice().contains(&mv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::board::Square;

    #[test]
    fn test_move_picker_stages() {
        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        // Use a known legal move from starting position
        // 2g2f: file 7, rank 2 -> rank 3
        let tt_move = Some(Move::normal(
            Square::new(7, 2), // 2g
            Square::new(7, 3), // 2f
            false,
        ));

        let mut picker = MovePicker::new(&pos, tt_move, &history, &stack);

        // First move should be TT move
        let first_move = picker.next_move();
        assert_eq!(first_move, tt_move);

        // Subsequent moves should not include TT move
        let mut moves = Vec::new();
        while let Some(mv) = picker.next_move() {
            assert_ne!(Some(mv), tt_move);
            moves.push(mv);
        }

        // Should have generated all legal moves except TT move
        assert!(!moves.is_empty());
    }

    #[test]
    fn test_quiescence_picker() {
        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let stack = SearchStack::default();

        let mut picker = MovePicker::new_quiescence(&pos, None, &history, &stack);

        // In starting position, there should be no captures
        assert!(picker.next_move().is_none());
    }

    #[test]
    fn test_mvv_lva_ordering() {
        // This test would require setting up a position with captures
        // For now, we just test the scoring function
        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let stack = SearchStack::default();
        let _picker = MovePicker::new(&pos, None, &history, &stack);

        // Test piece values
        assert!(
            MovePicker::piece_value(PieceType::Rook) > MovePicker::piece_value(PieceType::Pawn)
        );
        assert!(
            MovePicker::piece_value(PieceType::Bishop) > MovePicker::piece_value(PieceType::Lance)
        );
    }

    #[test]
    fn test_killer_moves() {
        let pos = Position::startpos();
        let history = Arc::new(History::new());
        let mut stack = SearchStack::default();

        // Set killer moves (using legal moves from starting position)
        // 2g2f: file 7, rank 2 -> rank 3
        let killer1 = Move::normal(Square::new(7, 2), Square::new(7, 3), false);
        // 7g7f: file 2, rank 2 -> rank 3
        let killer2 = Move::normal(Square::new(2, 2), Square::new(2, 3), false);
        stack.killers[0] = Some(killer1);
        stack.killers[1] = Some(killer2);

        let mut picker = MovePicker::new(&pos, None, &history, &stack);

        // Collect moves and track when killers appear
        let mut move_count = 0;
        let mut killer_positions = vec![];
        while let Some(mv) = picker.next_move() {
            if mv == killer1 || mv == killer2 {
                killer_positions.push(move_count);
            }
            move_count += 1;
        }

        // Killers should be generated
        assert!(!killer_positions.is_empty(), "Killer moves should be generated");

        // Killers should appear relatively early (after captures)
        for &pos in &killer_positions {
            assert!(pos < 10, "Killer move at position {} is too late", pos);
        }
    }
}
