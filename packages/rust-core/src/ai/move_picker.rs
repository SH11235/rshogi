//! Move picker for staged move generation and ordering
//!
//! Generates moves in stages for better search efficiency:
//! 1. TT move
//! 2. Good captures (MVV-LVA)
//! 3. Killer moves
//! 4. Bad captures
//! 5. Quiet moves (history ordered)

use super::board::{Color, PieceType, Position, Square};
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
///
/// The order is carefully chosen based on the probability of causing a beta cutoff:
/// 1. TT move - Most likely to be the best move from previous search
/// 2. Good captures - Positive SEE captures are often good moves
/// 3. Killers - Moves that caused cutoffs at the same depth
/// 4. Quiet moves - Non-captures ordered by history heuristic
/// 5. Bad captures - Negative SEE captures (least likely to be good)
///
/// Bad captures are intentionally placed last because:
/// - They have negative SEE value (lose material)
/// - They rarely cause beta cutoffs
/// - In quiescence search, they are often skipped entirely
/// - Late Move Reductions (LMR) work better with bad moves at the end
///
/// This ordering matches strong engines like Stockfish and maximizes
/// the efficiency of alpha-beta pruning.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MovePickerStage {
    /// TT move
    TTMove,
    /// Generate captures
    GenerateCaptures,
    /// Good captures (SEE >= 0)
    GoodCaptures,
    /// Killer moves
    Killers,
    /// Generate quiet moves
    GenerateQuiets,
    /// Bad captures (SEE < 0)
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
    /// Current index in moves/killers/bad_captures depending on stage
    /// - In Killers stage: index into stack.killers[] (0-1)
    /// - In BadCaptures stage: index into bad_captures vector
    /// - Reset to 0 when transitioning to a new stage
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
                        self.current = 0; // Reset index for killer moves iteration
                    }
                }

                MovePickerStage::Killers => {
                    if self.current < 2 {
                        // Check up to 2 killer moves
                        if let Some(killer) = self.stack.killers[self.current] {
                            self.current += 1; // Move to next killer slot
                            if Some(killer) != self.tt_move
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
                        if Some(mv) != self.tt_move {
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

        // Add only non-captures that are not killers or TT move
        for &mv in move_list.as_slice() {
            if !self.is_capture(mv) && Some(mv) != self.tt_move && !self.is_killer(mv) {
                self.moves.push(ScoredMove::new(mv, 0));
            }
        }
    }

    /// Score captures using Static Exchange Evaluation (SEE)
    fn score_captures(&mut self) {
        for i in 0..self.moves.len() {
            let mv = self.moves[i].mv;
            if self.get_captured_piece(mv).is_some() {
                // Calculate SEE value for this capture
                let see_value = self.calculate_see(mv);

                // Use SEE value as the primary score
                // Multiply by 100 to make it comparable with history scores
                self.moves[i].score = see_value;

                // Small promotion bonus (SEE already accounts for promoted piece value)
                if mv.is_promote() {
                    self.moves[i].score += 100;
                }
            }
        }
    }

    /// Calculate Static Exchange Evaluation for a move
    /// Returns the material gain/loss from the exchange sequence
    fn calculate_see(&self, mv: Move) -> i32 {
        // Get the target square
        let to = mv.to();

        // Get initial gain (value of captured piece)
        let gain = if let Some(captured) = self.get_captured_piece(mv) {
            Self::piece_value(captured)
        } else {
            return 0; // Not a capture
        };

        // Get the attacking piece value
        let attacker_value = if mv.is_drop() {
            Self::piece_value(mv.drop_piece_type())
        } else {
            let from = mv.from().unwrap();
            if let Some(piece) = self.pos.board.piece_on(from) {
                let base_value = Self::piece_value(piece.piece_type);
                // If promoting, use promoted piece value
                if mv.is_promote() {
                    Self::promoted_piece_value(piece.piece_type)
                } else {
                    base_value
                }
            } else {
                return 0;
            }
        };

        // Simulate the capture
        let mut pos = self.pos.clone();
        let next_player = pos.side_to_move.opposite(); // Store before do_move
        pos.do_move(mv);

        // Now recursively calculate the exchange
        let exchange_value = self.see_recursive(&mut pos, to, attacker_value, next_player);

        gain - exchange_value
    }

    /// Recursive SEE calculation
    /// Returns the best exchange value for the side to move
    fn see_recursive(
        &self,
        pos: &mut Position,
        sq: Square,
        last_piece_value: i32,
        stm: Color,
    ) -> i32 {
        // Find the least valuable attacker
        if let Some((attacker_sq, _attacker_type, attacker_value)) =
            self.find_least_attacker(pos, sq, stm)
        {
            // Make the capture
            let capture_move = Move::normal(attacker_sq, sq, false);
            let undo = pos.do_move(capture_move);

            // Recursively evaluate
            let exchange_value = self.see_recursive(pos, sq, attacker_value, stm.opposite());

            // Undo the move
            pos.undo_move(capture_move, undo);

            // We can choose not to capture
            (last_piece_value - exchange_value).max(0)
        } else {
            // No more attackers
            0
        }
    }

    /// Find the least valuable piece attacking a square
    fn find_least_attacker(
        &self,
        pos: &Position,
        sq: Square,
        color: Color,
    ) -> Option<(Square, PieceType, i32)> {
        let mut best_attacker = None;
        let mut best_value = i32::MAX;

        // Check all pieces of the given color
        for piece_type in [
            PieceType::Pawn,
            PieceType::Lance,
            PieceType::Knight,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
        ] {
            let mut pieces = pos.board.piece_bb[color as usize][piece_type as usize];

            // Check each piece of this type
            while let Some(from) = pieces.pop_lsb() {
                // Check if this piece attacks the target square
                if self.can_attack(pos, from, sq, piece_type) {
                    let value = Self::piece_value(piece_type);
                    if value < best_value {
                        best_value = value;
                        best_attacker = Some((from, piece_type, value));
                    }
                }
            }
        }

        best_attacker
    }

    /// Check if a piece can attack a square
    fn can_attack(&self, pos: &Position, from: Square, to: Square, piece_type: PieceType) -> bool {
        use crate::ai::attacks::ATTACK_TABLES;

        let piece = pos.board.piece_on(from).unwrap();
        let color = piece.color;
        let is_promoted = piece.promoted;

        // Check if the piece attacks the target square
        // For promoted pieces, use Gold movement pattern (except Bishop/Rook)
        match piece_type {
            PieceType::Pawn => {
                if is_promoted {
                    // Tokin moves like Gold
                    let attacks = ATTACK_TABLES.gold_attacks(from, color);
                    attacks.test(to)
                } else {
                    // Pawn attacks one square forward
                    let attacks = ATTACK_TABLES.pawn_attacks(from, color);
                    attacks.test(to)
                }
            }
            PieceType::Lance => {
                if is_promoted {
                    // Promoted Lance moves like Gold
                    let attacks = ATTACK_TABLES.gold_attacks(from, color);
                    attacks.test(to)
                } else {
                    // Lance attacks forward (sliding)
                    let attacks =
                        ATTACK_TABLES.sliding_attacks(from, pos.board.all_bb, PieceType::Lance);
                    attacks.test(to)
                }
            }
            PieceType::Knight => {
                if is_promoted {
                    // Promoted Knight moves like Gold
                    let attacks = ATTACK_TABLES.gold_attacks(from, color);
                    attacks.test(to)
                } else {
                    // Knight has fixed jump attacks
                    let attacks = ATTACK_TABLES.knight_attacks(from, color);
                    attacks.test(to)
                }
            }
            PieceType::Silver => {
                if is_promoted {
                    // Promoted Silver moves like Gold
                    let attacks = ATTACK_TABLES.gold_attacks(from, color);
                    attacks.test(to)
                } else {
                    // Silver has fixed attacks
                    let attacks = ATTACK_TABLES.silver_attacks(from, color);
                    attacks.test(to)
                }
            }
            PieceType::Gold => {
                // Gold has fixed attacks
                let attacks = ATTACK_TABLES.gold_attacks(from, color);
                attacks.test(to)
            }
            PieceType::Bishop => {
                if is_promoted {
                    // Dragon Horse = Bishop + King moves
                    let bishop_attacks =
                        ATTACK_TABLES.sliding_attacks(from, pos.board.all_bb, PieceType::Bishop);
                    let king_attacks = ATTACK_TABLES.king_attacks(from);
                    (bishop_attacks | king_attacks).test(to)
                } else {
                    // Bishop attacks diagonally (sliding)
                    let attacks =
                        ATTACK_TABLES.sliding_attacks(from, pos.board.all_bb, PieceType::Bishop);
                    attacks.test(to)
                }
            }
            PieceType::Rook => {
                if is_promoted {
                    // Dragon King = Rook + King moves
                    let rook_attacks =
                        ATTACK_TABLES.sliding_attacks(from, pos.board.all_bb, PieceType::Rook);
                    let king_attacks = ATTACK_TABLES.king_attacks(from);
                    (rook_attacks | king_attacks).test(to)
                } else {
                    // Rook attacks horizontally/vertically (sliding)
                    let attacks =
                        ATTACK_TABLES.sliding_attacks(from, pos.board.all_bb, PieceType::Rook);
                    attacks.test(to)
                }
            }
            PieceType::King => {
                // King has fixed attacks
                let attacks = ATTACK_TABLES.king_attacks(from);
                attacks.test(to)
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

    /// Get promoted piece value
    fn promoted_piece_value(piece_type: PieceType) -> i32 {
        match piece_type {
            PieceType::Pawn => 600,             // Tokin (same as Gold)
            PieceType::Lance => 600,            // Promoted Lance (same as Gold)
            PieceType::Knight => 600,           // Promoted Knight (same as Gold)
            PieceType::Silver => 600,           // Promoted Silver (same as Gold)
            PieceType::Bishop => 1100,          // Dragon Horse (Bishop + Rook moves)
            PieceType::Rook => 1300,            // Dragon King (Rook + Bishop moves)
            _ => Self::piece_value(piece_type), // Gold and King don't promote
        }
    }
}

// Extension for Position to check legal moves
impl Position {
    /// Check if a move is legal
    ///
    /// This optimized version uses do_move/undo_move to check legality.
    /// It's much faster than generating all legal moves (O(1) vs O(N)).
    pub fn is_legal_move(&self, mv: Move) -> bool {
        // Basic validation
        if mv.is_drop() {
            // Check if we have the piece to drop
            let piece_type = mv.drop_piece_type();
            let color_idx = self.side_to_move as usize;
            let hand_idx = match piece_type {
                PieceType::Rook => 0,
                PieceType::Bishop => 1,
                PieceType::Gold => 2,
                PieceType::Silver => 3,
                PieceType::Knight => 4,
                PieceType::Lance => 5,
                PieceType::Pawn => 6,
                _ => return false, // Can't drop King or promoted pieces
            };

            if self.hands[color_idx][hand_idx] == 0 {
                return false;
            }

            // Check if target square is empty
            if self.board.piece_on(mv.to()).is_some() {
                return false;
            }

            // TODO: Check pawn drop restrictions (nifu, uchifuzume)
        } else {
            // Normal move
            if let Some(from) = mv.from() {
                // Check if there's a piece at the from square
                if let Some(piece) = self.board.piece_on(from) {
                    // Check if it's our piece
                    if piece.color != self.side_to_move {
                        return false;
                    }

                    // Check if capturing our own piece
                    if let Some(to_piece) = self.board.piece_on(mv.to()) {
                        if to_piece.color == self.side_to_move {
                            return false;
                        }
                    }
                } else {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Try to make the move and check if it leaves king in check
        let mut test_pos = self.clone();
        test_pos.do_move(mv);

        // Check if the side that made the move left their king in check
        let king_in_check = test_pos.is_check(self.side_to_move);

        !king_in_check
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
    fn test_see_calculation() {
        // Create a position where we can test SEE
        let mut pos = Position::startpos();

        // Create a position with some captures
        // 7g7f, 3c3d, 2g2f, 3d3e, 2f2e, 3e3f (pawn advances to create capture)
        let setup_moves = [
            Move::normal(Square::new(2, 2), Square::new(2, 3), false), // 7g7f
            Move::normal(Square::new(6, 6), Square::new(6, 5), false), // 3c3d
            Move::normal(Square::new(7, 2), Square::new(7, 3), false), // 2g2f
            Move::normal(Square::new(6, 5), Square::new(6, 4), false), // 3d3e
            Move::normal(Square::new(7, 3), Square::new(7, 4), false), // 2f2e
            Move::normal(Square::new(6, 4), Square::new(6, 3), false), // 3e3f
        ];

        for mv in &setup_moves {
            pos.do_move(*mv);
        }

        let history = Arc::new(History::new());
        let stack = SearchStack::default();
        let picker = MovePicker::new(&pos, None, &history, &stack);

        // Test capturing the pawn at 3f with our pawn at 3g
        // This should be a good capture (pawn for pawn = 0)
        let capture_3f = Move::normal(Square::new(6, 2), Square::new(6, 3), false); // 3g3f
        let see_value = picker.calculate_see(capture_3f);
        assert_eq!(see_value, 100, "Pawn x Pawn should have SEE value of 100 (pawn value)");

        // If there were a more valuable piece defending, SEE would be negative
        // But in this simple position, it's just pawn for pawn
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
