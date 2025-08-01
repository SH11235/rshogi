//! Move ordering for unified searcher
//!
//! Implements various move ordering heuristics

use crate::{
    search::{history::History, types::SearchStack},
    shogi::{Move, MoveList, Position},
};
use std::sync::{Arc, Mutex};

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
            let mvv_lva = victim_value * 10 - attacker_value;

            // Add capture history score
            let capture_history_score = match self.history.lock() {
                Ok(history) => {
                    if let (Some(attacker_type), Some(victim_type)) =
                        (mv.piece_type(), mv.captured_piece_type())
                    {
                        history.capture.get(pos.side_to_move, attacker_type, victim_type)
                    } else {
                        0
                    }
                }
                Err(_) => 0,
            };

            return 100_000 + mvv_lva + capture_history_score / 10;
        }

        // Counter move - check if this move is a known good response to the previous move
        if crate::search::types::SearchStack::is_valid_ply(ply) && ply > 0 {
            let prev_ply = ply - 1;
            if let Some(prev_move) = search_stack[prev_ply as usize].current_move {
                if let Ok(history) = self.history.lock() {
                    if history.counter_moves.get(pos.side_to_move, prev_move) == Some(mv) {
                        return 95_000;
                    }
                }
            }
        }

        // Killer moves from SearchStack
        if crate::search::types::SearchStack::is_valid_ply(ply) {
            let stack_entry = &search_stack[ply as usize];
            for (slot, &killer) in stack_entry.killers.iter().enumerate() {
                if Some(mv) == killer {
                    return 90_000 - slot as i32;
                }
            }
        }

        // History heuristic with continuation history
        let history_score = match self.history.lock() {
            Ok(history) => {
                let prev_move =
                    if ply > 0 && crate::search::types::SearchStack::is_valid_ply(ply - 1) {
                        search_stack[(ply - 1) as usize].current_move
                    } else {
                        None
                    };
                history.get_score(pos.side_to_move, mv, prev_move)
            }
            Err(e) => {
                // Mutex poisoning indicates a panic in another thread holding the lock.
                // This should be extremely rare in production, but log it for debugging.
                // Impact: Move ordering quality may degrade slightly, but search remains correct.
                log::error!("Failed to acquire history lock in move ordering: {e}");
                // Use static evaluation as fallback
                self.get_static_move_score(pos, mv)
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

    /// Get static move score when history is unavailable
    /// This provides a reasonable move ordering based on basic heuristics
    fn get_static_move_score(&self, pos: &Position, mv: Move) -> i32 {
        use crate::{Color, PieceType};

        let mut score = 0;

        // Promotions get significant bonus
        if mv.is_promote() {
            score += 2000;
        }

        // Center control bonus
        let to_sq = mv.to();
        // 5th rank (middle of the board) is important
        if to_sq.rank() == 5 {
            score += 200;
        }
        // Center files (3-7) are important
        if to_sq.file() >= 3 && to_sq.file() <= 7 {
            score += 100;
        }

        // Piece advancement bonus (moving pieces forward is generally good)
        let advancement_bonus = match pos.side_to_move {
            Color::Black => (9 - to_sq.rank()) * 10,
            Color::White => to_sq.rank() * 10,
        };
        score += advancement_bonus as i32;

        // Piece-specific movement bonuses
        match mv.piece_type() {
            Some(PieceType::Rook) | Some(PieceType::Bishop) => {
                // Major pieces moving to active squares
                score += 300;
            }
            Some(PieceType::Gold) | Some(PieceType::Silver) => {
                // Minor pieces developing
                score += 200;
            }
            Some(PieceType::Knight) | Some(PieceType::Lance) => {
                // Supporting pieces
                score += 100;
            }
            Some(PieceType::Pawn) => {
                // Pawns advancing in the center
                if to_sq.file() >= 4 && to_sq.file() <= 6 {
                    score += 50;
                }
            }
            _ => {}
        }

        // Drops to key squares
        if mv.is_drop() {
            // Drops near the enemy king are often good
            score += 150;
        }

        score
    }
}

// MoveOrdering is now automatically Send+Sync because Arc<Mutex<T>> is Send+Sync
