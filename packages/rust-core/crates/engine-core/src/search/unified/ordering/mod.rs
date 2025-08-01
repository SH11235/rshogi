//! Move ordering for unified searcher
//!
//! Implements various move ordering heuristics

use crate::{
    search::{history::History, types::SearchStack},
    shogi::{Move, MoveList, Position},
};
use std::sync::{Arc, Mutex};

const MAX_PLY: usize = 127;

/// Move ordering state
pub struct MoveOrdering {
    /// History heuristic reference (thread-safe)
    history: Arc<Mutex<History>>,
}

impl MoveOrdering {
    /// Create new move ordering
    pub fn new(history: Arc<Mutex<History>>) -> Self {
        Self { history }
    }

    /// Order moves for search using SearchStack
    pub fn order_moves(
        &self,
        pos: &Position,
        moves: &MoveList,
        tt_move: Option<Move>,
        search_stack: &[SearchStack],
        ply: u16,
    ) -> Vec<Move> {
        let mut scored_moves = Vec::with_capacity(moves.len());

        for &mv in moves.as_slice().iter() {
            let score = self.score_move(pos, mv, tt_move, search_stack, ply);
            scored_moves.push((mv, score));
        }

        // Sort by score (descending) - use stable sort to ensure deterministic ordering
        // when moves have the same score
        scored_moves.sort_by(|a, b| b.1.cmp(&a.1));

        // Extract moves
        scored_moves.into_iter().map(|(mv, _)| mv).collect()
    }

    /// Score a single move using SearchStack
    fn score_move(
        &self,
        pos: &Position,
        mv: Move,
        tt_move: Option<Move>,
        search_stack: &[SearchStack],
        ply: u16,
    ) -> i32 {
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

        // Killer moves from SearchStack
        if ply < MAX_PLY as u16 && ply < search_stack.len() as u16 {
            let stack_entry = &search_stack[ply as usize];
            for (slot, &killer) in stack_entry.killers.iter().enumerate() {
                if Some(mv) == killer {
                    return 90_000 - slot as i32;
                }
            }
        }

        // History heuristic
        let history_score = match self.history.lock() {
            Ok(history) => history.get_score(pos.side_to_move, mv, None),
            Err(e) => {
                // Mutex poisoning indicates a panic in another thread holding the lock.
                // This should be extremely rare in production, but log it for debugging.
                // Impact: Move ordering quality may degrade slightly, but search remains correct.
                log::error!("Failed to acquire history lock in move ordering: {e}");
                0 // Fallback to neutral score (won't affect correctness, only efficiency)
            }
        };

        // Base score with history
        10_000 + history_score
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
