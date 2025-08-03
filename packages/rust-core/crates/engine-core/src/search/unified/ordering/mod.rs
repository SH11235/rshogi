//! Move ordering for unified searcher
//!
//! Implements various move ordering heuristics

mod killer_table;

use crate::{
    search::{history::History, types::SearchStack},
    shogi::{Move, MoveList, Position},
};
use std::sync::{Arc, Mutex};

pub use killer_table::KillerTable;

/// Move ordering state
pub struct MoveOrdering {
    /// History heuristic reference (thread-safe)
    history: Arc<Mutex<History>>,
    /// Global killer moves table
    killer_table: Arc<KillerTable>,
}

impl MoveOrdering {
    /// Create new move ordering
    pub fn new(history: Arc<Mutex<History>>) -> Self {
        Self {
            history,
            killer_table: Arc::new(KillerTable::new()),
        }
    }

    /// Create with existing killer table (for sharing between threads)
    pub fn with_killer_table(history: Arc<Mutex<History>>, killer_table: Arc<KillerTable>) -> Self {
        Self {
            history,
            killer_table,
        }
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

        // Killer moves from both SearchStack and global KillerTable
        if crate::search::types::SearchStack::is_valid_ply(ply) {
            // First check SearchStack killers
            let stack_entry = &search_stack[ply as usize];
            for (slot, &killer) in stack_entry.killers.iter().enumerate() {
                if Some(mv) == killer {
                    return 90_000 - slot as i32;
                }
            }

            // Then check global KillerTable
            let global_killers = self.killer_table.get(ply as usize);
            for (slot, &killer) in global_killers.iter().enumerate() {
                if Some(mv) == killer {
                    return 89_000 - slot as i32;
                }
            }
        }

        // History heuristic with improved fallback
        // Use try_lock() instead of lock() to avoid blocking on contention
        // This prioritizes performance over perfect history accuracy in high-contention scenarios
        let history_score = match self.history.try_lock() {
            Ok(history) => {
                let prev_move =
                    if ply > 0 && crate::search::types::SearchStack::is_valid_ply(ply - 1) {
                        search_stack[(ply - 1) as usize].current_move
                    } else {
                        None
                    };
                history.get_score(pos.side_to_move, mv, prev_move)
            }
            Err(_) => {
                // Mutex is busy (contention) or poisoned
                // Use enhanced static evaluation as fallback
                let static_score = self.get_static_move_score(pos, mv);

                // Add bonuses based on move characteristics that don't require history
                let mut bonus = 0;

                // Bonus for moves that attack opponent pieces
                if self.is_attacking_move(pos, mv) {
                    bonus += 500;
                }

                // Bonus for defensive moves
                if self.is_defensive_move(pos, mv) {
                    bonus += 300;
                }

                static_score + bonus
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

    /// Check if a move attacks opponent pieces
    fn is_attacking_move(&self, pos: &Position, mv: Move) -> bool {
        use crate::Color;

        let to_sq = mv.to();
        let _enemy_color = pos.side_to_move.flip();

        // Check if the destination square is near enemy pieces
        // This is a simplified heuristic - proper attack detection would require
        // generating attacks from the destination square
        match pos.side_to_move {
            Color::Black => {
                // For black, attacking moves go towards lower ranks (enemy territory)
                to_sq.rank() <= 3
            }
            Color::White => {
                // For white, attacking moves go towards higher ranks
                to_sq.rank() >= 7
            }
        }
    }

    /// Check if a move is defensive
    fn is_defensive_move(&self, pos: &Position, mv: Move) -> bool {
        use crate::Color;

        // Drops in own territory are often defensive
        if mv.is_drop() {
            let to_sq = mv.to();
            match pos.side_to_move {
                Color::Black => to_sq.rank() >= 7,
                Color::White => to_sq.rank() <= 3,
            }
        } else {
            false
        }
    }

    /// Update killer moves in global table
    pub fn update_killer(&self, ply: u16, mv: Move) {
        self.killer_table.update(ply as usize, mv);
    }

    /// Clear killer table for new search
    pub fn clear_killers(&self) {
        self.killer_table.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::history::History;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn test_move_ordering_under_time_pressure() {
        // Test move ordering behavior under time pressure with mutex contention
        let history = Arc::new(Mutex::new(History::new()));
        let ordering = Arc::new(MoveOrdering::new(Arc::clone(&history)));

        let num_threads = 4; // Reduced from 16 to avoid CI timeout
        let time_limit = Duration::from_millis(50); // Reduced from 100ms for faster CI
        let start_time = Instant::now();

        let handles: Vec<_> = (0..num_threads)
            .map(|thread_id| {
                let ordering = Arc::clone(&ordering);
                let history = Arc::clone(&history);

                thread::spawn(move || {
                    let mut iterations = 0;

                    // Simulate time pressure - keep accessing shared resources until time runs out
                    while start_time.elapsed() < time_limit {
                        // Test killer table operations
                        let test_move = crate::shogi::Move::normal(
                            crate::shogi::Square::new((thread_id % 9) as u8, 6),
                            crate::shogi::Square::new((thread_id % 9) as u8, 5),
                            false,
                        );

                        // Update killer table
                        ordering.update_killer(thread_id as u16 % 64, test_move);

                        // Get killers to verify table is accessible
                        let killers = ordering.killer_table.get(thread_id % 64);
                        let _ = killers; // Use to avoid warning

                        iterations += 1;

                        // Occasionally update history to create more contention
                        if iterations % 10 == 0 {
                            if let Ok(mut hist) = history.try_lock() {
                                // Use update_good method from butterfly history
                                hist.butterfly.update_good(
                                    crate::shogi::Color::Black,
                                    test_move,
                                    5, // depth
                                );
                            }
                        }

                        // Clear killers occasionally to stress the system
                        if iterations % 50 == 0 {
                            ordering.clear_killers();
                        }
                    }

                    iterations
                })
            })
            .collect();

        // Wait for all threads and collect results
        let mut total_iterations = 0;
        for handle in handles {
            let iterations = handle.join().unwrap();
            total_iterations += iterations;
        }

        // Verify that all threads made progress despite contention
        assert!(total_iterations > 0, "Threads should complete iterations under time pressure");

        // Verify ordering is still functional after stress test
        let test_move = crate::shogi::Move::normal(
            crate::shogi::Square::new(6, 6),
            crate::shogi::Square::new(6, 5),
            false,
        );
        ordering.update_killer(0, test_move);
        let killers = ordering.killer_table.get(0);
        assert!(killers.contains(&Some(test_move)));
    }

    #[test]
    fn test_move_ordering_consistency() {
        // Test that move ordering produces consistent results
        let history = Arc::new(Mutex::new(History::new()));
        let ordering = MoveOrdering::new(history);

        // Test killer moves consistency
        let test_moves = vec![
            crate::shogi::Move::normal(
                crate::shogi::Square::new(6, 6),
                crate::shogi::Square::new(6, 5),
                false,
            ),
            crate::shogi::Move::normal(
                crate::shogi::Square::new(2, 6),
                crate::shogi::Square::new(2, 5),
                false,
            ),
            crate::shogi::Move::normal(
                crate::shogi::Square::new(7, 6),
                crate::shogi::Square::new(7, 5),
                false,
            ),
        ];

        // Add killers at different plies
        for (i, &mv) in test_moves.iter().enumerate() {
            ordering.update_killer(i as u16, mv);
        }

        // Verify killers are stored correctly
        for (i, &mv) in test_moves.iter().enumerate() {
            let killers = ordering.killer_table.get(i);
            assert!(killers.contains(&Some(mv)), "Killer move should be stored at ply {}", i);
        }

        // Clear and verify
        ordering.clear_killers();
        for i in 0..3 {
            let killers = ordering.killer_table.get(i);
            assert_eq!(killers, [None; 2], "Killers should be cleared at ply {}", i);
        }

        // Test that the same sequence produces the same results
        for &mv in &test_moves {
            ordering.update_killer(0, mv);
        }
        let final_killers = ordering.killer_table.get(0);

        // Should contain the last two moves added (FIFO behavior)
        assert!(final_killers[0] == Some(test_moves[2]));
        assert!(final_killers[1] == Some(test_moves[1]));
    }
}

// MoveOrdering is now automatically Send+Sync because Arc<Mutex<T>> is Send+Sync
