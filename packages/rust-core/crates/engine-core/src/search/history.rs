//! History heuristics for move ordering
//!
//! Tracks the success rate of moves in different contexts to improve move ordering

use crate::shogi::board::NUM_PIECE_TYPES;
use crate::shogi::SHOGI_BOARD_SIZE;
use crate::{shogi::Move, Color, PieceType, Square};

/// Maximum history score
const MAX_HISTORY_SCORE: i32 = 10000;

/// History aging divisor (applied periodically to prevent overflow)
const HISTORY_AGING_DIVISOR: i32 = 2;

/// Counter move history - tracks which moves work well after specific moves
#[derive(Clone)]
pub struct CounterMoveHistory {
    /// [color][from_square][to_square] -> counter move
    table: [[[Option<Move>; SHOGI_BOARD_SIZE]; SHOGI_BOARD_SIZE]; 2],
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
            table: [[[None; SHOGI_BOARD_SIZE]; SHOGI_BOARD_SIZE]; 2],
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
        self.table = [[[None; SHOGI_BOARD_SIZE]; SHOGI_BOARD_SIZE]; 2];
    }
}

/// Butterfly history - tracks move success by from-to squares
#[derive(Clone)]
pub struct ButterflyHistory {
    /// [color][from_square][to_square] -> score
    scores: [[[i32; SHOGI_BOARD_SIZE]; SHOGI_BOARD_SIZE]; 2],
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
            scores: [[[0; SHOGI_BOARD_SIZE]; SHOGI_BOARD_SIZE]; 2],
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
        self.scores = [[[0; SHOGI_BOARD_SIZE]; SHOGI_BOARD_SIZE]; 2];
    }
}

/// Continuation history - tracks move success in context of previous moves
#[derive(Clone)]
pub struct ContinuationHistory {
    /// [color][piece_moved_2_ply_ago][to_square_2_ply_ago][piece_to_move][to_square] -> score
    /// Stored as i16 to reduce footprint (≈6.4MB)
    scores: Vec<i16>,
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
        let piece_dim = NUM_PIECE_TYPES;
        // 2 (colors) * piece_dim * N * piece_dim * N ≒ 6.4MB at i16
        let size = 2 * piece_dim * SHOGI_BOARD_SIZE * piece_dim * SHOGI_BOARD_SIZE;
        ContinuationHistory {
            scores: vec![0i16; size],
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
        let n = SHOGI_BOARD_SIZE;
        let piece_dim = NUM_PIECE_TYPES;
        let idx = color_idx * (piece_dim * n * piece_dim * n)
            + prev_piece * (n * piece_dim * n)
            + prev_to.index() * (piece_dim * n)
            + curr_piece * n
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
        i32::from(self.scores[idx])
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
        let score = i32::from(self.scores[idx]);
        let updated = score + bonus - (score * bonus.abs() / MAX_HISTORY_SCORE);
        let clamped = updated.clamp(-MAX_HISTORY_SCORE, MAX_HISTORY_SCORE);
        self.scores[idx] = clamped as i16;
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
        let score = i32::from(self.scores[idx]);
        let updated = score + penalty - (score * penalty.abs() / MAX_HISTORY_SCORE);
        let clamped = updated.clamp(-MAX_HISTORY_SCORE, MAX_HISTORY_SCORE);
        self.scores[idx] = clamped as i16;
    }

    /// Age all continuation history scores
    pub fn age_scores(&mut self) {
        for score in &mut self.scores {
            *score = (*score as i32 / HISTORY_AGING_DIVISOR) as i16;
        }
    }

    /// Clear all continuation history
    pub fn clear(&mut self) {
        self.scores.fill(0);
    }
}

/// Capture history - tracks success of captures by attacker/victim/to square
#[derive(Clone)]
pub struct CaptureHistory {
    /// [color][attacker_piece][victim_piece][to_square] -> score
    scores: [[[[i32; SHOGI_BOARD_SIZE]; NUM_PIECE_TYPES]; NUM_PIECE_TYPES]; 2],
}

impl Default for CaptureHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl CaptureHistory {
    /// Create new capture history
    pub fn new() -> Self {
        CaptureHistory {
            scores: [[[[0; SHOGI_BOARD_SIZE]; NUM_PIECE_TYPES]; NUM_PIECE_TYPES]; 2],
        }
    }

    /// Get capture history score
    pub fn get(&self, color: Color, attacker: PieceType, victim: PieceType, to: Square) -> i32 {
        self.scores[color as usize][attacker as usize][victim as usize][to.index()]
    }

    /// Update capture history with bonus
    pub fn update_good(
        &mut self,
        color: Color,
        attacker: PieceType,
        victim: PieceType,
        to: Square,
        depth: i32,
    ) {
        let bonus = depth * depth;
        let score =
            &mut self.scores[color as usize][attacker as usize][victim as usize][to.index()];
        *score += bonus - (*score * bonus.abs() / MAX_HISTORY_SCORE);
        *score = (*score).clamp(-MAX_HISTORY_SCORE, MAX_HISTORY_SCORE);
    }

    /// Update capture history with penalty
    pub fn update_bad(
        &mut self,
        color: Color,
        attacker: PieceType,
        victim: PieceType,
        to: Square,
        depth: i32,
    ) {
        let penalty = -(depth * depth);
        let score =
            &mut self.scores[color as usize][attacker as usize][victim as usize][to.index()];
        *score += penalty - (*score * penalty.abs() / MAX_HISTORY_SCORE);
        *score = (*score).clamp(-MAX_HISTORY_SCORE, MAX_HISTORY_SCORE);
    }

    /// Age all capture history scores
    pub fn age_scores(&mut self) {
        for color_scores in &mut self.scores {
            for attacker_scores in color_scores {
                for victim_scores in attacker_scores {
                    for score in victim_scores {
                        *score /= HISTORY_AGING_DIVISOR;
                    }
                }
            }
        }
    }

    /// Clear all capture history
    pub fn clear(&mut self) {
        for color_scores in &mut self.scores {
            for attacker_scores in color_scores {
                for victim_scores in attacker_scores {
                    victim_scores.fill(0);
                }
            }
        }
    }
}

/// Combined history tables for move ordering
#[derive(Clone)]
pub struct History {
    /// Butterfly history (from-to)
    pub butterfly: ButterflyHistory,
    /// Counter move history
    pub counter_moves: CounterMoveHistory,
    /// Continuation history (2-ply)
    pub continuation: ContinuationHistory,
    /// Capture history
    pub capture: CaptureHistory,
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
            capture: CaptureHistory::new(),
        }
    }

    /// Get combined history score for move ordering
    pub fn get_score(&self, color: Color, mv: Move, prev_move: Option<Move>) -> i32 {
        let mut score = self.butterfly.get(color, mv);

        // Add continuation history if we have context
        if let Some(prev_mv) = prev_move {
            if let (Some(prev_piece), Some(curr_piece)) = (prev_mv.piece_type(), mv.piece_type()) {
                // Ensure prev_move was not a drop (needs a destination square)
                if !prev_mv.is_drop() {
                    let prev_to = prev_mv.to();
                    let curr_to = mv.to();
                    score += self.continuation.get(
                        color,
                        prev_piece as usize,
                        prev_to,
                        curr_piece as usize,
                        curr_to,
                    );
                }
            }
        }

        score
    }

    /// Update history tables after a cut-off (good move)
    pub fn update_cutoff(&mut self, color: Color, mv: Move, depth: i32, prev_move: Option<Move>) {
        self.butterfly.update_good(color, mv, depth);

        // Update continuation history if we have piece type info
        if let Some(prev_mv) = prev_move {
            if let (Some(prev_piece), Some(curr_piece)) = (prev_mv.piece_type(), mv.piece_type()) {
                if !prev_mv.is_drop() && !mv.is_drop() {
                    let prev_to = prev_mv.to();
                    let curr_to = mv.to();
                    self.continuation.update_good(
                        color,
                        prev_piece as usize,
                        prev_to,
                        curr_piece as usize,
                        curr_to,
                        depth,
                    );
                }
            }
        }
    }

    /// Update history tables for tried moves that didn't cause cut-off
    pub fn update_quiet(&mut self, color: Color, mv: Move, depth: i32, prev_move: Option<Move>) {
        self.butterfly.update_bad(color, mv, depth);

        // Update continuation history if we have piece type info
        if let Some(prev_mv) = prev_move {
            if let (Some(prev_piece), Some(curr_piece)) = (prev_mv.piece_type(), mv.piece_type()) {
                if !prev_mv.is_drop() && !mv.is_drop() {
                    let prev_to = prev_mv.to();
                    let curr_to = mv.to();
                    self.continuation.update_bad(
                        color,
                        prev_piece as usize,
                        prev_to,
                        curr_piece as usize,
                        curr_to,
                        depth,
                    );
                }
            }
        }
    }

    /// Age all history scores periodically
    ///
    /// Note: counter moveテーブルは「直近ヒットのみ保持」する設計なので
    /// aging の対象にはしていない。
    pub fn age_all(&mut self) {
        self.butterfly.age_scores();
        self.continuation.age_scores();
        self.capture.age_scores();
    }

    /// Clear all history tables
    pub fn clear_all(&mut self) {
        self.butterfly.clear();
        self.counter_moves.clear();
        self.continuation.clear();
        self.capture.clear();
    }
}

#[cfg(test)]
mod tests {
    use crate::{usi::parse_usi_square, Color, PieceType};

    use super::*;

    #[test]
    fn test_butterfly_history() {
        let mut history = ButterflyHistory::new();
        let color = Color::Black;
        let mv =
            Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);

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

        let prev_move =
            Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);
        let counter_move =
            Move::normal(parse_usi_square("1d").unwrap(), parse_usi_square("1e").unwrap(), false);

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
        let prev_to = parse_usi_square("7g").unwrap();
        let curr_piece = PieceType::Pawn as usize;
        let curr_to = parse_usi_square("1e").unwrap();

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
        let mv =
            Move::normal(parse_usi_square("7h").unwrap(), parse_usi_square("7g").unwrap(), false);

        // Update many times to test clamping
        for _ in 0..100 {
            history.update_good(color, mv, 10);
        }

        // Score should be clamped to MAX_HISTORY_SCORE
        assert!(history.get(color, mv) <= MAX_HISTORY_SCORE);
    }

    #[test]
    fn test_capture_history() {
        let mut history = CaptureHistory::new();
        let color = Color::Black;
        let attacker = PieceType::Knight;
        let victim = PieceType::Silver;
        let target = parse_usi_square("5e").unwrap();

        // Initial score should be 0
        assert_eq!(history.get(color, attacker, victim, target), 0);

        // Update with good capture
        history.update_good(color, attacker, victim, target, 4);
        assert!(history.get(color, attacker, victim, target) > 0);

        // Update with bad capture
        history.update_bad(color, attacker, victim, target, 2);
        let score = history.get(color, attacker, victim, target);
        assert!(score > 0); // Should still be positive but reduced

        // Test aging
        let old_score = history.get(color, attacker, victim, target);
        history.age_scores();
        assert_eq!(history.get(color, attacker, victim, target), old_score / 2);
    }
}
