//! Copy-on-Write optimized Position for parallel search
//!
//! This module provides a COW-optimized version of Position that shares
//! immutable data between clones to reduce memory allocation overhead.

use super::{
    board::{Board, Color, Position, UndoInfo},
    moves::Move,
};
use std::sync::Arc;

/// Internal board data wrapped in Arc for sharing
#[derive(Clone, Debug)]
struct BoardInternal {
    /// The actual board state
    board: Board,
}

/// Internal hands data wrapped in Arc for sharing
#[derive(Clone, Debug)]
struct HandsInternal {
    /// Pieces in hand [color][piece_type] (excluding King)
    hands: [[u8; 7]; 2],
}

/// COW-optimized Position structure
#[derive(Clone, Debug)]
pub struct CowPosition {
    /// Board with bitboards (Arc-wrapped for COW)
    board_internal: Arc<BoardInternal>,
    
    /// Pieces in hand (Arc-wrapped for COW)
    hands_internal: Arc<HandsInternal>,
    
    /// Side to move (cheap to copy)
    pub side_to_move: Color,
    
    /// Ply count (cheap to copy)
    pub ply: u16,
    
    /// Zobrist hash (cheap to copy)
    pub hash: u64,
    
    /// Alias for hash (for compatibility)
    pub zobrist_hash: u64,
    
    /// History for repetition detection (Arc-wrapped for COW)
    history: Arc<Vec<u64>>,
    
    /// Flag to track if this position has been modified
    /// Used to optimize COW behavior
    modified: bool,
}

impl CowPosition {
    /// Create a new COW position from components
    pub fn new(
        board: Board,
        hands: [[u8; 7]; 2],
        side_to_move: Color,
        ply: u16,
        hash: u64,
        history: Vec<u64>,
    ) -> Self {
        Self {
            board_internal: Arc::new(BoardInternal { board }),
            hands_internal: Arc::new(HandsInternal { hands }),
            side_to_move,
            ply,
            hash,
            zobrist_hash: hash,
            history: Arc::new(history),
            modified: false,
        }
    }
    
    /// Get reference to the board
    #[inline]
    pub fn board(&self) -> &Board {
        &self.board_internal.board
    }
    
    /// Get reference to hands
    #[inline]
    pub fn hands(&self) -> &[[u8; 7]; 2] {
        &self.hands_internal.hands
    }
    
    /// Get reference to history
    #[inline]
    pub fn history(&self) -> &[u64] {
        &self.history
    }
    
    /// Get mutable reference to board (triggers COW if needed)
    fn board_mut(&mut self) -> &mut Board {
        // Only clone if there are other references
        if Arc::strong_count(&self.board_internal) > 1 {
            self.board_internal = Arc::new((*self.board_internal).clone());
            self.modified = true;
        }
        // Safe because we ensured unique ownership above
        &mut Arc::get_mut(&mut self.board_internal)
            .expect("Failed to get mutable board")
            .board
    }
    
    /// Get mutable reference to hands (triggers COW if needed)
    fn hands_mut(&mut self) -> &mut [[u8; 7]; 2] {
        // Only clone if there are other references
        if Arc::strong_count(&self.hands_internal) > 1 {
            self.hands_internal = Arc::new((*self.hands_internal).clone());
            self.modified = true;
        }
        // Safe because we ensured unique ownership above
        &mut Arc::get_mut(&mut self.hands_internal)
            .expect("Failed to get mutable hands")
            .hands
    }
    
    /// Add to history (triggers COW if needed)
    pub fn push_history(&mut self, hash: u64) {
        // Only clone if there are other references
        if Arc::strong_count(&self.history) > 1 {
            let mut new_history = (*self.history).clone();
            new_history.push(hash);
            self.history = Arc::new(new_history);
            self.modified = true;
        } else {
            // Safe because we have unique ownership
            Arc::get_mut(&mut self.history)
                .expect("Failed to get mutable history")
                .push(hash);
        }
    }
    
    /// Pop from history (triggers COW if needed)
    pub fn pop_history(&mut self) -> Option<u64> {
        // Only clone if there are other references
        if Arc::strong_count(&self.history) > 1 {
            let mut new_history = (*self.history).clone();
            let result = new_history.pop();
            self.history = Arc::new(new_history);
            self.modified = true;
            result
        } else {
            // Safe because we have unique ownership
            Arc::get_mut(&mut self.history)
                .expect("Failed to get mutable history")
                .pop()
        }
    }
    
    /// Make a move (triggers COW as needed)
    pub fn do_move(&mut self, mv: Move) -> UndoInfo {
        // This will trigger COW if needed
        let board = self.board_mut();
        
        // Update other fields
        self.ply += 1;
        self.side_to_move = self.side_to_move.flip();
        
        // Push current hash to history
        self.push_history(self.hash);
        
        // Apply move and get undo info
        // TODO: Implement actual move logic
        // For now, return dummy undo info
        UndoInfo {
            captured: None,
            moved_piece_was_promoted: false,
            previous_hash: self.hash,
            previous_ply: self.ply - 1,
        }
    }
    
    /// Undo a move (triggers COW as needed)
    pub fn undo_move(&mut self, mv: Move, undo_info: UndoInfo) {
        // This will trigger COW if needed
        let board = self.board_mut();
        
        // Restore fields
        self.ply = undo_info.previous_ply;
        self.hash = undo_info.previous_hash;
        self.zobrist_hash = undo_info.previous_hash;
        self.side_to_move = self.side_to_move.flip();
        
        // Pop from history
        self.pop_history();
        
        // TODO: Implement actual undo logic
    }
    
    /// Check if position has been modified since creation/clone
    pub fn is_modified(&self) -> bool {
        self.modified
    }
    
    /// Get reference count for board (for debugging)
    pub fn board_ref_count(&self) -> usize {
        Arc::strong_count(&self.board_internal)
    }
    
    /// Get reference count for hands (for debugging)
    pub fn hands_ref_count(&self) -> usize {
        Arc::strong_count(&self.hands_internal)
    }
    
    /// Get reference count for history (for debugging)
    pub fn history_ref_count(&self) -> usize {
        Arc::strong_count(&self.history)
    }
}

impl From<Position> for CowPosition {
    fn from(pos: Position) -> Self {
        Self::new(
            pos.board,
            pos.hands,
            pos.side_to_move,
            pos.ply,
            pos.hash,
            pos.history,
        )
    }
}

impl From<&Position> for CowPosition {
    fn from(pos: &Position) -> Self {
        Self::new(
            pos.board.clone(),
            pos.hands.clone(),
            pos.side_to_move,
            pos.ply,
            pos.hash,
            pos.history.clone(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_cow_clone_shares_data() {
        let board = Board::empty();
        let hands = [[0; 7]; 2];
        let history = vec![123456789];
        
        let pos1 = CowPosition::new(
            board,
            hands,
            Color::Black,
            0,
            987654321,
            history,
        );
        
        // Clone should share data
        let pos2 = pos1.clone();
        
        // Check reference counts
        assert_eq!(pos1.board_ref_count(), 2);
        assert_eq!(pos2.board_ref_count(), 2);
        assert_eq!(pos1.hands_ref_count(), 2);
        assert_eq!(pos2.hands_ref_count(), 2);
        assert_eq!(pos1.history_ref_count(), 2);
        assert_eq!(pos2.history_ref_count(), 2);
    }
    
    #[test]
    fn test_cow_modification_triggers_clone() {
        let board = Board::empty();
        let hands = [[0; 7]; 2];
        let history = vec![123456789];
        
        let pos1 = CowPosition::new(
            board,
            hands,
            Color::Black,
            0,
            987654321,
            history,
        );
        
        let mut pos2 = pos1.clone();
        
        // Initially shared
        assert_eq!(pos1.board_ref_count(), 2);
        
        // Modification triggers COW
        pos2.push_history(555555);
        
        // History should now be separate
        assert_eq!(pos1.history_ref_count(), 1);
        assert_eq!(pos2.history_ref_count(), 1);
        
        // Board still shared (not modified)
        assert_eq!(pos1.board_ref_count(), 2);
        assert_eq!(pos2.board_ref_count(), 2);
    }
}