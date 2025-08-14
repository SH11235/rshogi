//! Move validation and position query methods
//!
//! This module provides methods for validating moves and querying
//! the position state (check, repetition, draw, etc).

use crate::shogi::board::{PieceType, Position};
use crate::shogi::moves::Move;
use crate::shogi::piece_constants::piece_type_to_hand_index;

impl Position {
    /// Validate if a move is pseudo-legal (doesn't check for leaving king in check)
    /// Returns true if the move appears to be legal based on basic rules
    pub fn is_pseudo_legal(&self, mv: Move) -> bool {
        if mv.is_null() {
            return false;
        }

        if mv.is_drop() {
            let to = mv.to();
            // Check destination is empty
            if self.board.piece_on(to).is_some() {
                return false;
            }
            // Check we have the piece in hand
            let piece_type = mv.drop_piece_type();
            let hand_idx = match piece_type_to_hand_index(piece_type) {
                Ok(idx) => idx,
                Err(_) => return false,
            };
            if self.hands[self.side_to_move as usize][hand_idx] == 0 {
                return false;
            }
        } else {
            let from = match mv.from() {
                Some(f) => f,
                None => return false,
            };
            let to = mv.to();

            // Check source has a piece
            let piece = match self.board.piece_on(from) {
                Some(p) => p,
                None => return false,
            };

            // Check piece belongs to side to move
            if piece.color != self.side_to_move {
                return false;
            }

            // Check destination - if occupied, must be opponent's piece
            if let Some(dest_piece) = self.board.piece_on(to) {
                if dest_piece.color == self.side_to_move {
                    return false;
                }
                // Never allow king capture
                if dest_piece.piece_type == PieceType::King {
                    return false;
                }
            }
        }

        true
    }

    /// Check if the current side to move is in check
    pub fn is_in_check(&self) -> bool {
        self.is_check(self.side_to_move)
    }

    /// Check for repetition
    pub fn is_repetition(&self) -> bool {
        if self.history.len() < 4 {
            return false;
        }

        let current_hash = self.hash;
        let mut count = 0;

        // Four-fold repetition
        for &hash in self.history.iter() {
            if hash == current_hash {
                count += 1;
                if count >= 3 {
                    // Current position + 3 in history = 4 total
                    return true;
                }
            }
        }

        false
    }

    /// Check if position is draw (simplified check)
    pub fn is_draw(&self) -> bool {
        // Simple repetition detection would go here
        // For now, return false
        false
    }
}
