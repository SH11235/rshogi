//! History heuristics for move ordering
//!
//! Tracks the success rate of moves in different contexts to improve move ordering

use crate::search::ab::ordering::constants::{
    CAP_HISTORY_AGING_SHIFT, CAP_HISTORY_BONUS_FACTOR, CAP_HISTORY_MAX, CAP_HISTORY_SHIFT,
    CONT_HISTORY_AGING_SHIFT, CONT_HISTORY_BONUS_FACTOR, CONT_HISTORY_MAX, CONT_HISTORY_SHIFT,
    QUIET_HISTORY_AGING_SHIFT, QUIET_HISTORY_BONUS_FACTOR, QUIET_HISTORY_MAX, QUIET_HISTORY_SHIFT,
};
use crate::shogi::board::NUM_PIECE_TYPES;
use crate::shogi::piece_constants::NUM_HAND_PIECE_TYPES;
use crate::shogi::SHOGI_BOARD_SIZE;
use crate::{shogi::Move, Color, PieceType, Square};

/// Counter move history - tracks which moves work well after specific moves
const COUNTER_DROP_DIM: usize = NUM_HAND_PIECE_TYPES;
const COUNTER_FROM_DIM: usize = SHOGI_BOARD_SIZE + COUNTER_DROP_DIM;
/// Offset to the dedicated "drop-from" slots (one per hand piece type).
const COUNTER_DROP_BASE: usize = SHOGI_BOARD_SIZE;
const CONT_PREV_DROP_DIM: usize = 2;
const CONT_CURR_DROP_DIM: usize = 2;

#[derive(Clone)]
pub struct CounterMoveHistory {
    /// [color][from_square_or_drop][to_square] -> counter move
    table: [[[Option<Move>; SHOGI_BOARD_SIZE]; COUNTER_FROM_DIM]; 2],
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
            table: [[[None; SHOGI_BOARD_SIZE]; COUNTER_FROM_DIM]; 2],
        }
    }

    /// Get counter move for previous move
    #[inline]
    pub fn get(&self, color: Color, prev_move: Move) -> Option<Move> {
        let from_idx = if prev_move.is_drop() {
            drop_from_index(prev_move)
        } else {
            prev_move.from().unwrap().index()
        };
        let to = prev_move.to();
        self.table[color as usize][from_idx][to.index()]
    }

    /// Update counter move
    #[inline]
    pub fn update(&mut self, color: Color, prev_move: Move, counter_move: Move) {
        let from_idx = if prev_move.is_drop() {
            drop_from_index(prev_move)
        } else {
            prev_move.from().unwrap().index()
        };
        let to = prev_move.to();
        self.table[color as usize][from_idx][to.index()] = Some(counter_move);
    }

    /// Clear all counter moves
    pub fn clear(&mut self) {
        self.table = [[[None; SHOGI_BOARD_SIZE]; COUNTER_FROM_DIM]; 2];
    }

    pub(crate) fn merge_from(&mut self, other: &Self) {
        for color in 0..2 {
            for from in 0..COUNTER_FROM_DIM {
                for to in 0..SHOGI_BOARD_SIZE {
                    if self.table[color][from][to].is_none() {
                        self.table[color][from][to] = other.table[color][from][to];
                    }
                }
            }
        }
    }
}

#[inline]
fn drop_from_index(mv: Move) -> usize {
    debug_assert!(mv.is_drop(), "drop_from_index called with non-drop move");
    mv.drop_piece_type()
        .hand_index()
        .map(|hand_idx| COUNTER_DROP_BASE + hand_idx)
        .unwrap_or_else(|| panic!("CounterMoveHistory: drop piece must have hand index (mv={mv})"))
}

/// Butterfly history - tracks move success by from-to squares
#[derive(Clone)]
pub struct ButterflyHistory {
    /// [color][from_square_or_drop][to_square] -> score
    scores: [[[i16; SHOGI_BOARD_SIZE]; BUTTERFLY_FROM_DIM]; 2],
}

const BUTTERFLY_FROM_DIM: usize = SHOGI_BOARD_SIZE + 1;
/// Shared "drop-from" slot for all piece types (non-overlapping with board squares).
const BUTTERFLY_DROP_INDEX: usize = SHOGI_BOARD_SIZE;
const _: () = {
    // Ensure drop slot remains disjoint from board squares.
    assert!(BUTTERFLY_DROP_INDEX == SHOGI_BOARD_SIZE);
};

impl Default for ButterflyHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl ButterflyHistory {
    /// Create new butterfly history
    pub fn new() -> Self {
        ButterflyHistory {
            scores: [[[0; SHOGI_BOARD_SIZE]; BUTTERFLY_FROM_DIM]; 2],
        }
    }

    /// Get history score for a move
    #[inline]
    pub fn get(&self, color: Color, mv: Move) -> i32 {
        let raw = if mv.is_drop() {
            self.scores[color as usize][BUTTERFLY_DROP_INDEX][mv.to().index()]
        } else {
            let from = mv.from().unwrap();
            let to = mv.to();
            self.scores[color as usize][from.index()][to.index()]
        };
        i32::from(raw)
    }

    /// Update history score with bonus
    pub fn update_good(&mut self, color: Color, mv: Move, depth: i32) {
        let bonus = scaled_bonus(depth, QUIET_HISTORY_BONUS_FACTOR);
        self.add_bonus(color, mv, bonus);
    }

    /// Update history score with penalty
    pub fn update_bad(&mut self, color: Color, mv: Move, depth: i32) {
        let penalty = -scaled_bonus(depth, QUIET_HISTORY_BONUS_FACTOR);
        self.add_bonus(color, mv, penalty);
    }

    /// Add bonus/penalty to history score
    fn add_bonus(&mut self, color: Color, mv: Move, bonus: i32) {
        let (from_idx, to_idx) = if mv.is_drop() {
            (BUTTERFLY_DROP_INDEX, mv.to().index())
        } else {
            (mv.from().unwrap().index(), mv.to().index())
        };

        let score = &mut self.scores[color as usize][from_idx][to_idx];
        apply_history_update(score, bonus, QUIET_HISTORY_MAX, QUIET_HISTORY_SHIFT);
    }

    /// Age all history scores to prevent drift and overflow
    pub fn age_scores(&mut self) {
        for color_scores in &mut self.scores {
            for from_scores in color_scores {
                for score in from_scores {
                    age_value(score, QUIET_HISTORY_AGING_SHIFT);
                }
            }
        }
    }

    /// Clear all history scores
    pub fn clear(&mut self) {
        self.scores = [[[0; SHOGI_BOARD_SIZE]; BUTTERFLY_FROM_DIM]; 2];
    }

    pub(crate) fn merge_from(&mut self, other: &Self) {
        for color in 0..2 {
            for from in 0..BUTTERFLY_FROM_DIM {
                for to in 0..SHOGI_BOARD_SIZE {
                    merge_history_value(
                        &mut self.scores[color][from][to],
                        other.scores[color][from][to],
                        QUIET_HISTORY_MAX,
                    );
                }
            }
        }
    }
}

/// Continuation history - tracks move success in context of previous moves
#[derive(Clone)]
pub struct ContinuationHistory {
    /// [color][prev_piece][prev_to][prev_is_drop?][curr_piece][curr_to][curr_is_drop?] -> score
    /// Stored as i16 to reduce footprint（約6.7MB）
    scores: Vec<i16>,
    size: usize,
}

/// 2手継続（Continuation）ヒストリ参照キー
#[derive(Copy, Clone, Debug)]
pub struct ContinuationKey {
    pub color: Color,
    pub prev_piece: usize,
    pub prev_to: Square,
    pub prev_is_drop: bool,
    pub curr_piece: usize,
    pub curr_to: Square,
    pub curr_is_drop: bool,
}

impl ContinuationKey {
    #[inline]
    pub fn new(
        color: Color,
        prev_piece: usize,
        prev_to: Square,
        prev_is_drop: bool,
        curr_piece: usize,
        curr_to: Square,
        curr_is_drop: bool,
    ) -> Self {
        Self {
            color,
            prev_piece,
            prev_to,
            prev_is_drop,
            curr_piece,
            curr_to,
            curr_is_drop,
        }
    }
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
        // 2 (colors) * CONT_PREV_DROP_DIM * piece_dim * N * CONT_CURR_DROP_DIM * piece_dim * N ≒ 6.7MB at i16
        let size = 2
            * CONT_PREV_DROP_DIM
            * piece_dim
            * SHOGI_BOARD_SIZE
            * CONT_CURR_DROP_DIM
            * piece_dim
            * SHOGI_BOARD_SIZE;
        ContinuationHistory {
            scores: vec![0i16; size],
            size,
        }
    }

    /// Calculate index for continuation history
    fn index(&self, key: &ContinuationKey) -> usize {
        let color_idx = key.color as usize;
        let n = SHOGI_BOARD_SIZE;
        let piece_dim = NUM_PIECE_TYPES;
        let prev_drop_idx = if key.prev_is_drop { 1 } else { 0 };
        let curr_drop_idx = if key.curr_is_drop { 1 } else { 0 };

        let stride_prev_drop = piece_dim * n * CONT_CURR_DROP_DIM * piece_dim * n;
        let stride_prev_piece = n * CONT_CURR_DROP_DIM * piece_dim * n;
        let stride_prev_to = CONT_CURR_DROP_DIM * piece_dim * n;
        let stride_curr_drop = piece_dim * n;
        let stride_curr_piece = n;

        let mut idx = color_idx * CONT_PREV_DROP_DIM * stride_prev_drop;
        idx += prev_drop_idx * stride_prev_drop;
        idx += key.prev_piece * stride_prev_piece;
        idx += key.prev_to.index() * stride_prev_to;
        idx += curr_drop_idx * stride_curr_drop;
        idx += key.curr_piece * stride_curr_piece;
        idx += key.curr_to.index();

        debug_assert!(idx < self.size);
        idx
    }

    /// Get continuation history score
    pub fn get(&self, key: ContinuationKey) -> i32 {
        let idx = self.index(&key);
        i32::from(self.scores[idx])
    }

    /// Update continuation history with bonus
    pub fn update_good(&mut self, key: ContinuationKey, depth: i32) {
        self.apply_update(key, depth, true);
    }

    /// Update continuation history with penalty (bad move)
    pub fn update_bad(&mut self, key: ContinuationKey, depth: i32) {
        self.apply_update(key, depth, false);
    }

    #[inline]
    fn apply_update(&mut self, key: ContinuationKey, depth: i32, good: bool) {
        let bonus = scaled_bonus(depth, CONT_HISTORY_BONUS_FACTOR);
        let delta = if good { bonus } else { -bonus };
        let idx = self.index(&key);
        let score = &mut self.scores[idx];
        apply_history_update(score, delta, CONT_HISTORY_MAX, CONT_HISTORY_SHIFT);
    }

    /// Age all continuation history scores
    pub fn age_scores(&mut self) {
        for score in &mut self.scores {
            age_value(score, CONT_HISTORY_AGING_SHIFT);
        }
    }

    /// Clear all continuation history
    pub fn clear(&mut self) {
        self.scores.fill(0);
    }

    pub(crate) fn merge_from(&mut self, other: &Self) {
        for (dst, src) in self.scores.iter_mut().zip(&other.scores) {
            merge_history_value(dst, *src, CONT_HISTORY_MAX);
        }
    }
}

/// Capture history - tracks success of captures by attacker/victim/to square
#[derive(Clone)]
pub struct CaptureHistory {
    /// [color][attacker_piece][victim_piece][to_square] -> score
    scores: [[[[i16; SHOGI_BOARD_SIZE]; NUM_PIECE_TYPES]; NUM_PIECE_TYPES]; 2],
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
        i32::from(self.scores[color as usize][attacker as usize][victim as usize][to.index()])
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
        let bonus = scaled_bonus(depth, CAP_HISTORY_BONUS_FACTOR);
        let score =
            &mut self.scores[color as usize][attacker as usize][victim as usize][to.index()];
        apply_history_update(score, bonus, CAP_HISTORY_MAX, CAP_HISTORY_SHIFT);
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
        let penalty = -scaled_bonus(depth, CAP_HISTORY_BONUS_FACTOR);
        let score =
            &mut self.scores[color as usize][attacker as usize][victim as usize][to.index()];
        apply_history_update(score, penalty, CAP_HISTORY_MAX, CAP_HISTORY_SHIFT);
    }

    /// Age all capture history scores
    pub fn age_scores(&mut self) {
        for color_scores in &mut self.scores {
            for attacker_scores in color_scores {
                for victim_scores in attacker_scores {
                    for score in victim_scores {
                        age_value(score, CAP_HISTORY_AGING_SHIFT);
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

    pub(crate) fn merge_from(&mut self, other: &Self) {
        for color in 0..2 {
            for attacker in 0..NUM_PIECE_TYPES {
                for victim in 0..NUM_PIECE_TYPES {
                    for to in 0..SHOGI_BOARD_SIZE {
                        merge_history_value(
                            &mut self.scores[color][attacker][victim][to],
                            other.scores[color][attacker][victim][to],
                            CAP_HISTORY_MAX,
                        );
                    }
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
                let prev_to = prev_mv.to();
                let curr_to = mv.to();
                let key = ContinuationKey::new(
                    color,
                    prev_piece as usize,
                    prev_to,
                    prev_mv.is_drop(),
                    curr_piece as usize,
                    curr_to,
                    mv.is_drop(),
                );
                score += self.continuation.get(key);
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
                let prev_to = prev_mv.to();
                let curr_to = mv.to();
                let key = ContinuationKey::new(
                    color,
                    prev_piece as usize,
                    prev_to,
                    prev_mv.is_drop(),
                    curr_piece as usize,
                    curr_to,
                    mv.is_drop(),
                );
                self.continuation.update_good(key, depth);
            }
        }
    }

    /// Update history tables for tried moves that didn't cause cut-off
    pub fn update_quiet(&mut self, color: Color, mv: Move, depth: i32, prev_move: Option<Move>) {
        self.butterfly.update_bad(color, mv, depth);

        // Update continuation history if we have piece type info
        if let Some(prev_mv) = prev_move {
            if let (Some(prev_piece), Some(curr_piece)) = (prev_mv.piece_type(), mv.piece_type()) {
                let prev_to = prev_mv.to();
                let curr_to = mv.to();
                let key = ContinuationKey::new(
                    color,
                    prev_piece as usize,
                    prev_to,
                    prev_mv.is_drop(),
                    curr_piece as usize,
                    curr_to,
                    mv.is_drop(),
                );
                self.continuation.update_bad(key, depth);
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

fn scaled_bonus(depth: i32, factor: i32) -> i32 {
    let d = depth.max(1);
    let sq = d.saturating_mul(d);
    sq.saturating_mul(factor)
}

fn apply_history_update(value: &mut i16, bonus: i32, max: i16, shift: u32) {
    let current = i32::from(*value);
    let delta = if shift == 0 {
        bonus - current
    } else {
        (bonus - current) >> shift
    };
    let next = current.saturating_add(delta);
    let limit = i32::from(max);
    *value = next.clamp(-limit, limit) as i16;
}

fn age_value(value: &mut i16, shift: u32) {
    if shift == 0 {
        return;
    }
    let current = i32::from(*value);
    let delta = current >> shift;
    let next = current - delta;
    *value = next as i16;
}

fn merge_history_value(target: &mut i16, other: i16, max: i16) {
    if other == 0 {
        return;
    }
    let sum = i32::from(*target) + i32::from(other);
    let limit = i32::from(max);
    *target = sum.clamp(-limit, limit) as i16;
}

#[cfg(test)]
mod tests {
    use crate::search::ab::ordering::constants::{
        CAP_HISTORY_MAX, CONT_HISTORY_MAX, QUIET_HISTORY_MAX,
    };
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
        let after_good = history.get(color, mv);
        assert!(after_good > 0);
        assert!(after_good <= i32::from(QUIET_HISTORY_MAX));

        // Update with bad move
        history.update_bad(color, mv, 3);
        let after_bad = history.get(color, mv);
        assert!(after_bad <= after_good);
        assert!(after_bad >= -i32::from(QUIET_HISTORY_MAX));

        // Test aging
        let magnitude_before = after_bad.abs();
        history.age_scores();
        let aged = history.get(color, mv);
        assert!(aged.abs() <= magnitude_before);
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
    fn test_counter_move_drop_piece_type_separation() {
        let mut history = CounterMoveHistory::new();
        let color = Color::Black;

        let drop_square = parse_usi_square("5e").unwrap();
        let board_from_files = ["1a", "2a", "3a", "4a", "5a", "6a", "7a"];
        let board_to_files = ["1b", "2b", "3b", "4b", "5b", "6b", "7b"];

        for hand_idx in 0..NUM_HAND_PIECE_TYPES {
            let piece = PieceType::from_hand_index(hand_idx).expect("hand index maps to piece");
            let drop = Move::drop(piece, drop_square);
            let counter = Move::normal(
                parse_usi_square(board_from_files[hand_idx]).unwrap(),
                parse_usi_square(board_to_files[hand_idx]).unwrap(),
                false,
            );
            history.update(color, drop, counter);
            assert_eq!(history.get(color, drop), Some(counter));
        }

        // 再度ループして他駒種の学習結果が混ざっていないことを確認
        for hand_idx in 0..NUM_HAND_PIECE_TYPES {
            let piece = PieceType::from_hand_index(hand_idx).expect("hand index maps to piece");
            let drop = Move::drop(piece, drop_square);
            let expected = Move::normal(
                parse_usi_square(board_from_files[hand_idx]).unwrap(),
                parse_usi_square(board_to_files[hand_idx]).unwrap(),
                false,
            );
            assert_eq!(history.get(color, drop), Some(expected));
        }
    }

    #[test]
    fn test_counter_move_drop_isolated_from_board_index_zero() {
        let mut history = CounterMoveHistory::new();
        let color = Color::Black;

        let drop_prev = Move::drop(PieceType::Pawn, parse_usi_square("5e").unwrap());
        let drop_counter =
            Move::normal(parse_usi_square("5h").unwrap(), parse_usi_square("5g").unwrap(), false);

        let normal_prev =
            Move::normal(parse_usi_square("9a").unwrap(), parse_usi_square("9b").unwrap(), false);
        let normal_counter =
            Move::normal(parse_usi_square("8a").unwrap(), parse_usi_square("8b").unwrap(), false);

        history.update(color, drop_prev, drop_counter);
        history.update(color, normal_prev, normal_counter);

        assert_eq!(history.get(color, drop_prev), Some(drop_counter));
        assert_eq!(history.get(color, normal_prev), Some(normal_counter));
    }

    #[test]
    fn test_butterfly_drop_separate_slot() {
        let mut history = ButterflyHistory::new();
        let color = Color::Black;

        let drop_mv = Move::drop(PieceType::Pawn, parse_usi_square("5e").unwrap());
        let board_mvs = [
            Move::normal(parse_usi_square("1a").unwrap(), parse_usi_square("1b").unwrap(), false),
            Move::normal(parse_usi_square("9a").unwrap(), parse_usi_square("9b").unwrap(), false),
        ];

        history.update_good(color, drop_mv, 3);

        assert!(history.get(color, drop_mv) > 0);
        for mv in board_mvs {
            assert_eq!(history.get(color, mv), 0);
        }
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
        assert_eq!(
            history.get(ContinuationKey::new(
                color, prev_piece, prev_to, false, curr_piece, curr_to, false
            )),
            0
        );

        // Update with good continuation
        history.update_good(
            ContinuationKey::new(color, prev_piece, prev_to, false, curr_piece, curr_to, false),
            4,
        );
        let after_good = history.get(ContinuationKey::new(
            color, prev_piece, prev_to, false, curr_piece, curr_to, false,
        ));
        assert!(after_good > 0);
        assert!(after_good <= i32::from(CONT_HISTORY_MAX));

        // Apply penalty and ensure値が縮む
        history.update_bad(
            ContinuationKey::new(color, prev_piece, prev_to, false, curr_piece, curr_to, false),
            2,
        );
        let after_bad = history.get(ContinuationKey::new(
            color, prev_piece, prev_to, false, curr_piece, curr_to, false,
        ));
        assert!(after_bad.abs() <= after_good.abs());

        let magnitude_before = after_bad.abs();
        history.age_scores();
        let aged = history.get(ContinuationKey::new(
            color, prev_piece, prev_to, false, curr_piece, curr_to, false,
        ));
        assert!(aged.abs() <= magnitude_before);
    }
    #[test]
    fn test_continuation_history_handles_drop() {
        let mut history = ContinuationHistory::new();
        let color = Color::Black;

        let prev_piece = PieceType::Pawn as usize;
        let prev_to = parse_usi_square("5e").unwrap();
        let curr_piece = PieceType::Pawn as usize;
        let curr_to = parse_usi_square("5d").unwrap();

        // Update drop vs non-drop to ensure indices differ
        history.update_good(
            ContinuationKey::new(color, prev_piece, prev_to, true, curr_piece, curr_to, true),
            3,
        );
        let drop_score = history.get(ContinuationKey::new(
            color, prev_piece, prev_to, true, curr_piece, curr_to, true,
        ));
        assert!(drop_score > 0);

        let normal_score = history.get(ContinuationKey::new(
            color, prev_piece, prev_to, false, curr_piece, curr_to, false,
        ));
        assert_eq!(normal_score, 0);
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

        // Score should remain within the configured quiet-history bounds
        assert!(history.get(color, mv).abs() <= i32::from(QUIET_HISTORY_MAX));
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
        let after_good = history.get(color, attacker, victim, target);
        assert!(after_good > 0);
        assert!(after_good <= i32::from(CAP_HISTORY_MAX));

        // Update with bad capture
        history.update_bad(color, attacker, victim, target, 2);
        let after_bad = history.get(color, attacker, victim, target);
        assert!(after_bad.abs() <= after_good.abs());
        assert!(after_bad.abs() <= i32::from(CAP_HISTORY_MAX));

        // Test aging
        let magnitude_before = after_bad.abs();
        history.age_scores();
        let aged = history.get(color, attacker, victim, target);
        assert!(aged.abs() <= magnitude_before);
    }
}
