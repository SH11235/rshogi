//! Move ordering for unified searcher
//!
//! Implements various move ordering heuristics

use crate::{
    search::history::History,
    shogi::{Move, MoveList, Position},
};
use std::sync::{Arc, Mutex};

const KILLER_SLOTS: usize = 2;
const MAX_PLY: usize = 127;

/// Move ordering state
pub struct MoveOrdering {
    /// Killer moves table [ply][slot]
    killers: [[Option<Move>; KILLER_SLOTS]; MAX_PLY],

    /// History heuristic reference (thread-safe)
    history: Arc<Mutex<History>>,
}

impl MoveOrdering {
    /// Create new move ordering
    pub fn new(history: Arc<Mutex<History>>) -> Self {
        Self {
            killers: [[None; KILLER_SLOTS]; MAX_PLY],
            history,
        }
    }

    /// Order moves for search
    pub fn order_moves(
        &self,
        pos: &Position,
        moves: &MoveList,
        tt_move: Option<Move>,
        ply: u16,
    ) -> Vec<Move> {
        let mut scored_moves = Vec::with_capacity(moves.len());

        for &mv in moves.as_slice().iter() {
            let score = self.score_move(pos, mv, tt_move, ply);
            scored_moves.push((mv, score));
        }

        // Sort by score (descending) - use stable sort to ensure deterministic ordering
        // when moves have the same score
        scored_moves.sort_by(|a, b| b.1.cmp(&a.1));

        // Extract moves
        scored_moves.into_iter().map(|(mv, _)| mv).collect()
    }

    /// Score a single move
    fn score_move(&self, pos: &Position, mv: Move, tt_move: Option<Move>, ply: u16) -> i32 {
        // TT move gets highest priority
        if Some(mv) == tt_move {
            return 1_000_000;
        }

        // Good captures
        if mv.is_capture_hint() {
            // MVV-LVA (Most Valuable Victim - Least Valuable Attacker)
            let victim_value = Self::piece_value(mv.captured_piece_type());
            let attacker_value = Self::piece_value(mv.piece_type());
            return 100_000 + victim_value * 10 - attacker_value;
        }

        // Killer moves
        if ply < MAX_PLY as u16 {
            for (slot, &killer) in self.killers[ply as usize].iter().enumerate() {
                if Some(mv) == killer {
                    return 90_000 - slot as i32;
                }
            }
        }

        // History heuristic
        let history_score = if let Ok(history) = self.history.lock() {
            history.get_score(pos.side_to_move, mv, None)
        } else {
            0
        };

        // Base score with history
        10_000 + history_score
    }

    /// Update killer moves
    pub fn update_killers(&mut self, mv: Move, ply: u16) {
        if ply >= MAX_PLY as u16 || mv.is_capture_hint() {
            return;
        }

        let ply_idx = ply as usize;

        // Don't store the same move twice
        if self.killers[ply_idx][0] == Some(mv) {
            return;
        }

        // Shift killers and add new one
        self.killers[ply_idx][1] = self.killers[ply_idx][0];
        self.killers[ply_idx][0] = Some(mv);
    }

    /// Get piece value for MVV-LVA
    fn piece_value(piece_type: Option<crate::PieceType>) -> i32 {
        use crate::PieceType;

        match piece_type {
            Some(PieceType::Pawn) => 100,
            Some(PieceType::Lance) => 300,
            Some(PieceType::Knight) => 400,
            Some(PieceType::Silver) => 500,
            Some(PieceType::Gold) => 600,
            Some(PieceType::Bishop) => 800,
            Some(PieceType::Rook) => 900,
            Some(PieceType::King) => 10000,
            None => 0,
        }
    }
}

// MoveOrdering is now automatically Send+Sync because Arc<Mutex<T>> is Send+Sync
