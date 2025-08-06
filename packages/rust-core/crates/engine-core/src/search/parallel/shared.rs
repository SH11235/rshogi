//! Shared state for parallel search
//!
//! Lock-free data structures shared between search threads

use crate::{
    shogi::{Move, PieceType, Square},
    Color,
};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

/// Lock-free shared history table
pub struct SharedHistory {
    /// History scores using atomic operations
    /// [color][piece_type][to_square]
    table: Vec<AtomicU32>,
}

impl Default for SharedHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedHistory {
    /// Create a new shared history table
    pub fn new() -> Self {
        // 2 colors * 15 piece types * 81 squares = 2430 entries
        let size = 2 * 15 * 81;
        let mut table = Vec::with_capacity(size);
        for _ in 0..size {
            table.push(AtomicU32::new(0));
        }

        Self { table }
    }

    /// Get index for a move
    fn get_index(color: Color, piece_type: PieceType, to: Square) -> usize {
        let color_idx = color as usize;
        let piece_idx = piece_type as usize;
        let square_idx = to.index();

        color_idx * 15 * 81 + piece_idx * 81 + square_idx
    }

    /// Get history score
    pub fn get(&self, color: Color, piece_type: PieceType, to: Square) -> u32 {
        let idx = Self::get_index(color, piece_type, to);
        self.table[idx].load(Ordering::Relaxed)
    }

    /// Update history score (lock-free using fetch_add)
    pub fn update(&self, color: Color, piece_type: PieceType, to: Square, bonus: u32) {
        let idx = Self::get_index(color, piece_type, to);

        // Saturating add to prevent overflow
        let old_value = self.table[idx].load(Ordering::Relaxed);
        let new_value = old_value.saturating_add(bonus).min(10000);

        // Use compare_exchange_weak for efficiency
        let _ = self.table[idx].compare_exchange_weak(
            old_value,
            new_value,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// Age all history scores (divide by 2)
    pub fn age(&self) {
        for entry in &self.table {
            let old_value = entry.load(Ordering::Relaxed);
            entry.store(old_value / 2, Ordering::Relaxed);
        }
    }

    /// Clear all history
    pub fn clear(&self) {
        for entry in &self.table {
            entry.store(0, Ordering::Relaxed);
        }
    }
}

/// Shared search state for parallel threads
pub struct SharedSearchState {
    /// Best move found so far (encoded as u32)
    best_move: AtomicU32,

    /// Best score found so far
    best_score: AtomicI32,

    /// Depth of best score
    best_depth: AtomicU8,

    /// Generation number for PV synchronization
    current_generation: AtomicU64,

    /// Total nodes searched by all threads
    nodes_searched: AtomicU64,

    /// Stop flag for all threads
    pub stop_flag: Arc<AtomicBool>,

    /// Shared history table
    pub history: Arc<SharedHistory>,
}

impl SharedSearchState {
    /// Create new shared search state
    pub fn new(stop_flag: Arc<AtomicBool>) -> Self {
        Self {
            best_move: AtomicU32::new(0),
            best_score: AtomicI32::new(i32::MIN),
            best_depth: AtomicU8::new(0),
            current_generation: AtomicU64::new(0),
            nodes_searched: AtomicU64::new(0),
            stop_flag,
            history: Arc::new(SharedHistory::new()),
        }
    }

    /// Reset state for new search
    pub fn reset(&self) {
        self.best_move.store(0, Ordering::Relaxed);
        self.best_score.store(i32::MIN, Ordering::Relaxed);
        self.best_depth.store(0, Ordering::Relaxed);
        self.current_generation.fetch_add(1, Ordering::Relaxed);
        self.nodes_searched.store(0, Ordering::Relaxed);
        self.history.clear();
    }

    /// Try to update best move/score if better (lock-free)
    pub fn maybe_update_best(&self, score: i32, mv: Option<Move>, depth: u8, generation: u64) {
        // Check generation to avoid stale updates
        let current_gen = self.current_generation.load(Ordering::Relaxed);
        if generation != current_gen {
            return;
        }

        // Depth-based filtering
        let old_depth = self.best_depth.load(Ordering::Relaxed);
        if depth < old_depth {
            return;
        }

        // Try to update score
        let old_score = self.best_score.load(Ordering::Relaxed);
        if score > old_score || (score == old_score && depth > old_depth) {
            // Update score first
            match self.best_score.compare_exchange(
                old_score,
                score,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // Score updated successfully, now update move and depth
                    if let Some(m) = mv {
                        self.best_move.store(m.to_u16() as u32, Ordering::Relaxed);
                    }
                    self.best_depth.store(depth, Ordering::Release);
                }
                Err(_) => {
                    // Another thread updated the score, retry might be needed
                }
            }
        }
    }

    /// Get current best move
    pub fn get_best_move(&self) -> Option<Move> {
        let encoded = self.best_move.load(Ordering::Relaxed);
        if encoded == 0 {
            None
        } else {
            Some(Move::from_u16(encoded as u16))
        }
    }

    /// Get current best score
    pub fn get_best_score(&self) -> i32 {
        self.best_score.load(Ordering::Relaxed)
    }

    /// Get current best depth
    pub fn get_best_depth(&self) -> u8 {
        self.best_depth.load(Ordering::Relaxed)
    }

    /// Add to node count
    pub fn add_nodes(&self, nodes: u64) {
        self.nodes_searched.fetch_add(nodes, Ordering::Relaxed);
    }

    /// Get total nodes searched
    pub fn get_nodes(&self) -> u64 {
        self.nodes_searched.load(Ordering::Relaxed)
    }

    /// Check if search should stop
    pub fn should_stop(&self) -> bool {
        self.stop_flag.load(Ordering::Acquire)
    }

    /// Set stop flag
    pub fn set_stop(&self) {
        self.stop_flag.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shogi::{Color, Move, PieceType, Square};
    use std::sync::{atomic::AtomicBool, Arc};

    #[test]
    fn test_shared_history() {
        let history = SharedHistory::new();

        // Test initial state
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 0);

        // Test update
        history.update(Color::Black, PieceType::Pawn, Square::new(5, 5), 100);
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 100);

        // Test saturation
        history.update(Color::Black, PieceType::Pawn, Square::new(5, 5), 10000);
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 10000);

        // Test aging
        history.age();
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 5000);

        // Test clear
        history.clear();
        assert_eq!(history.get(Color::Black, PieceType::Pawn, Square::new(5, 5)), 0);
    }

    #[test]
    fn test_shared_search_state() {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let state = SharedSearchState::new(stop_flag);

        // Test initial state
        assert_eq!(state.get_best_move(), None);
        assert_eq!(state.get_best_score(), i32::MIN);
        assert_eq!(state.get_best_depth(), 0);
        assert_eq!(state.get_nodes(), 0);

        // Test node counting
        state.add_nodes(1000);
        state.add_nodes(500);
        assert_eq!(state.get_nodes(), 1500);

        // Test best move update
        let test_move = Move::normal(Square::new(7, 7), Square::new(7, 6), false);
        state.maybe_update_best(100, Some(test_move), 5, 0);

        assert_eq!(state.get_best_move(), Some(test_move));
        assert_eq!(state.get_best_score(), 100);
        assert_eq!(state.get_best_depth(), 5);

        // Test depth filtering - lower depth should not update
        let worse_move = Move::normal(Square::new(2, 8), Square::new(2, 7), false);
        state.maybe_update_best(200, Some(worse_move), 3, 0);

        assert_eq!(state.get_best_move(), Some(test_move)); // Should not change
        assert_eq!(state.get_best_depth(), 5); // Should not change

        // Test better score at same or higher depth
        state.maybe_update_best(300, Some(worse_move), 5, 0);
        assert_eq!(state.get_best_move(), Some(worse_move)); // Should update
        assert_eq!(state.get_best_score(), 300);
    }
}
