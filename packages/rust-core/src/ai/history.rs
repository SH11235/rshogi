//! History heuristics for move ordering
//!
//! Tracks the success rate of moves in different contexts to improve move ordering

use super::board::{Color, Square};
use super::moves::Move;

/// Maximum history score
const MAX_HISTORY_SCORE: i32 = 10000;

/// History aging divisor (applied periodically to prevent overflow)
const HISTORY_AGING_DIVISOR: i32 = 2;

/// Counter move history - tracks which moves work well after specific moves
pub struct CounterMoveHistory {
    /// [color][from_square][to_square] -> counter move
    table: [[[Option<Move>; 81]; 81]; 2],
}

impl Default for CounterMoveHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl CounterMoveHistory {
    /// Create new counter move history
    pub fn new() -> Self {
        CounterMoveHistory {
            table: [[[None; 81]; 81]; 2],
        }
    }

    /// Get counter move for previous move
    pub fn get(&self, color: Color, prev_move: Move) -> Option<Move> {
        if prev_move.is_drop() {
            // For drops, use a special index (e.g., square 0)
            self.table[color as usize][0][prev_move.to().index()]
        } else {
            let from = prev_move.from().unwrap();
            let to = prev_move.to();
            self.table[color as usize][from.index()][to.index()]
        }
    }

    /// Update counter move
    pub fn update(&mut self, color: Color, prev_move: Move, counter_move: Move) {
        if prev_move.is_drop() {
            self.table[color as usize][0][prev_move.to().index()] = Some(counter_move);
        } else {
            let from = prev_move.from().unwrap();
            let to = prev_move.to();
            self.table[color as usize][from.index()][to.index()] = Some(counter_move);
        }
    }

    /// Clear all counter moves
    pub fn clear(&mut self) {
        self.table = [[[None; 81]; 81]; 2];
    }
}

/// Butterfly history - tracks move success by from-to squares
pub struct ButterflyHistory {
    /// [color][from_square][to_square] -> score
    scores: [[[i32; 81]; 81]; 2],
}

impl Default for ButterflyHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl ButterflyHistory {
    /// Create new butterfly history
    pub fn new() -> Self {
        ButterflyHistory {
            scores: [[[0; 81]; 81]; 2],
        }
    }

    /// Get history score for a move
    pub fn get(&self, color: Color, mv: Move) -> i32 {
        if mv.is_drop() {
            // For drops, use a special scoring
            self.scores[color as usize][0][mv.to().index()]
        } else {
            let from = mv.from().unwrap();
            let to = mv.to();
            self.scores[color as usize][from.index()][to.index()]
        }
    }

    /// Update history score with bonus
    pub fn update_good(&mut self, color: Color, mv: Move, depth: i32) {
        let bonus = depth * depth; // Quadratic bonus based on depth
        self.add_bonus(color, mv, bonus);
    }

    /// Update history score with penalty
    pub fn update_bad(&mut self, color: Color, mv: Move, depth: i32) {
        let penalty = -(depth * depth); // Quadratic penalty based on depth
        self.add_bonus(color, mv, penalty);
    }

    /// Add bonus/penalty to history score
    fn add_bonus(&mut self, color: Color, mv: Move, bonus: i32) {
        let (from_idx, to_idx) = if mv.is_drop() {
            (0, mv.to().index())
        } else {
            (mv.from().unwrap().index(), mv.to().index())
        };

        let score = &mut self.scores[color as usize][from_idx][to_idx];

        // Use a formula that prevents overflow and maintains relative ordering
        *score += bonus - (*score * bonus.abs() / MAX_HISTORY_SCORE);

        // Clamp to maximum
        *score = (*score).clamp(-MAX_HISTORY_SCORE, MAX_HISTORY_SCORE);
    }

    /// Age all history scores (divide by 2) to prevent overflow
    pub fn age_scores(&mut self) {
        for color_scores in &mut self.scores {
            for from_scores in color_scores {
                for score in from_scores {
                    *score /= HISTORY_AGING_DIVISOR;
                }
            }
        }
    }

    /// Clear all history scores
    pub fn clear(&mut self) {
        self.scores = [[[0; 81]; 81]; 2];
    }
}

/// Continuation history - tracks move success in context of previous moves
pub struct ContinuationHistory {
    /// [color][piece_moved_2_ply_ago][to_square_2_ply_ago][piece_to_move][to_square] -> score
    /// Using simplified indexing for memory efficiency
    scores: Vec<i32>,
    size: usize,
}

impl Default for ContinuationHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl ContinuationHistory {
    /// Create new continuation history
    pub fn new() -> Self {
        // Simplified: 2 colors * 16 piece types * 81 squares * 16 piece types * 81 squares
        // This is large but manageable (~3.4MB)
        let size = 2 * 16 * 81 * 16 * 81;
        ContinuationHistory {
            scores: vec![0; size],
            size,
        }
    }

    /// Calculate index for continuation history
    fn index(
        &self,
        color: Color,
        prev_piece: usize,
        prev_to: Square,
        curr_piece: usize,
        curr_to: Square,
    ) -> usize {
        let color_idx = color as usize;
        let idx = color_idx * (16 * 81 * 16 * 81)
            + prev_piece * (81 * 16 * 81)
            + prev_to.index() * (16 * 81)
            + curr_piece * 81
            + curr_to.index();

        debug_assert!(idx < self.size);
        idx
    }

    /// Get continuation history score
    pub fn get(
        &self,
        color: Color,
        prev_piece: usize,
        prev_to: Square,
        curr_piece: usize,
        curr_to: Square,
    ) -> i32 {
        let idx = self.index(color, prev_piece, prev_to, curr_piece, curr_to);
        self.scores[idx]
    }

    /// Update continuation history with bonus
    pub fn update_good(
        &mut self,
        color: Color,
        prev_piece: usize,
        prev_to: Square,
        curr_piece: usize,
        curr_to: Square,
        depth: i32,
    ) {
        let bonus = depth * depth;
        let idx = self.index(color, prev_piece, prev_to, curr_piece, curr_to);

        let score = &mut self.scores[idx];
        *score += bonus - (*score * bonus.abs() / MAX_HISTORY_SCORE);
        *score = (*score).clamp(-MAX_HISTORY_SCORE, MAX_HISTORY_SCORE);
    }

    /// Update continuation history with penalty
    pub fn update_bad(
        &mut self,
        color: Color,
        prev_piece: usize,
        prev_to: Square,
        curr_piece: usize,
        curr_to: Square,
        depth: i32,
    ) {
        let penalty = -(depth * depth);
        let idx = self.index(color, prev_piece, prev_to, curr_piece, curr_to);

        let score = &mut self.scores[idx];
        *score += penalty - (*score * penalty.abs() / MAX_HISTORY_SCORE);
        *score = (*score).clamp(-MAX_HISTORY_SCORE, MAX_HISTORY_SCORE);
    }

    /// Age all continuation history scores
    pub fn age_scores(&mut self) {
        for score in &mut self.scores {
            *score /= HISTORY_AGING_DIVISOR;
        }
    }

    /// Clear all continuation history
    pub fn clear(&mut self) {
        self.scores.fill(0);
    }
}

/// Combined history tables for move ordering
pub struct History {
    /// Butterfly history (from-to)
    pub butterfly: ButterflyHistory,
    /// Counter move history
    pub counter_moves: CounterMoveHistory,
    /// Continuation history (2-ply)
    pub continuation: ContinuationHistory,
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

impl History {
    /// Create new history tables
    pub fn new() -> Self {
        History {
            butterfly: ButterflyHistory::new(),
            counter_moves: CounterMoveHistory::new(),
            continuation: ContinuationHistory::new(),
        }
    }

    /// Get combined history score for move ordering
    pub fn get_score(&self, color: Color, mv: Move, prev_move: Option<(usize, Square)>) -> i32 {
        let score = self.butterfly.get(color, mv);

        // Add continuation history if we have context
        if let Some((_prev_piece, _prev_to)) = prev_move {
            if !mv.is_drop() {
                // For normal moves, we need to get the piece type from somewhere
                // Since Move doesn't store piece type, we'll skip continuation history for now
                // TODO: Pass piece type separately or store it in Move
            }
        }

        score
    }

    /// Update history tables after a cut-off (good move)
    pub fn update_cutoff(
        &mut self,
        color: Color,
        mv: Move,
        depth: i32,
        _prev_move: Option<(usize, Square)>,
    ) {
        self.butterfly.update_good(color, mv, depth);

        // Skip continuation history update since Move doesn't store piece type
        // TODO: Pass piece type separately
    }

    /// Update history tables for tried moves that didn't cause cut-off
    pub fn update_quiet(
        &mut self,
        color: Color,
        mv: Move,
        depth: i32,
        _prev_move: Option<(usize, Square)>,
    ) {
        self.butterfly.update_bad(color, mv, depth);

        // Skip continuation history update since Move doesn't store piece type
        // TODO: Pass piece type separately
    }

    /// Age all history scores periodically
    pub fn age_all(&mut self) {
        self.butterfly.age_scores();
        self.continuation.age_scores();
    }

    /// Clear all history tables
    pub fn clear_all(&mut self) {
        self.butterfly.clear();
        self.counter_moves.clear();
        self.continuation.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::board::PieceType;

    #[test]
    fn test_butterfly_history() {
        let mut history = ButterflyHistory::new();
        let color = Color::Black;
        let mv = Move::normal(Square::new(2, 7), Square::new(2, 6), false);

        // Initial score should be 0
        assert_eq!(history.get(color, mv), 0);

        // Update with good move
        history.update_good(color, mv, 5);
        assert!(history.get(color, mv) > 0);

        // Update with bad move
        history.update_bad(color, mv, 3);
        let score = history.get(color, mv);
        assert!(score < 25); // Should be reduced but still positive

        // Test aging
        let old_score = history.get(color, mv);
        history.age_scores();
        assert_eq!(history.get(color, mv), old_score / 2);
    }

    #[test]
    fn test_counter_move_history() {
        let mut history = CounterMoveHistory::new();
        let color = Color::Black;

        let prev_move = Move::normal(Square::new(2, 7), Square::new(2, 6), false);
        let counter_move = Move::normal(Square::new(8, 3), Square::new(8, 4), false);

        // Initially no counter move
        assert!(history.get(color, prev_move).is_none());

        // Update counter move
        history.update(color, prev_move, counter_move);
        assert_eq!(history.get(color, prev_move), Some(counter_move));
    }

    #[test]
    fn test_continuation_history() {
        let mut history = ContinuationHistory::new();
        let color = Color::Black;

        let prev_piece = PieceType::Pawn as usize;
        let prev_to = Square::new(2, 6);
        let curr_piece = PieceType::Pawn as usize;
        let curr_to = Square::new(8, 4);

        // Initial score should be 0
        assert_eq!(history.get(color, prev_piece, prev_to, curr_piece, curr_to), 0);

        // Update with good continuation
        history.update_good(color, prev_piece, prev_to, curr_piece, curr_to, 4);
        assert!(history.get(color, prev_piece, prev_to, curr_piece, curr_to) > 0);
    }

    #[test]
    fn test_history_score_clamping() {
        let mut history = ButterflyHistory::new();
        let color = Color::Black;
        let mv = Move::normal(Square::new(2, 7), Square::new(2, 6), false);

        // Update many times to test clamping
        for _ in 0..100 {
            history.update_good(color, mv, 10);
        }

        // Score should be clamped to MAX_HISTORY_SCORE
        assert!(history.get(color, mv) <= MAX_HISTORY_SCORE);
    }
}
