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

    /// Check if a move gives check to the opponent
    /// Note: This is a simple implementation that clones the position
    /// For performance-critical code, consider caching results
    pub fn gives_check(&self, mv: Move) -> bool {
        // Validate move first
        if !self.is_pseudo_legal(mv) {
            return false;
        }

        // Make the move and check if opponent is in check
        let mut tmp = self.clone();

        // Apply move without full validation (we already checked pseudo-legal)
        if mv.is_drop() {
            // Handle drop moves
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            let piece = crate::shogi::board::Piece::new(piece_type, tmp.side_to_move);
            tmp.board.put_piece(to, piece);

            // Update hand count
            let hand_idx = match piece_type_to_hand_index(piece_type) {
                Ok(idx) => idx,
                Err(_) => return false,
            };
            tmp.hands[tmp.side_to_move as usize][hand_idx] -= 1;
        } else {
            // Handle normal moves
            let from = match mv.from() {
                Some(f) => f,
                None => return false,
            };
            let to = mv.to();

            // Get piece at from square
            let piece = match tmp.board.piece_on(from) {
                Some(p) => p,
                None => return false,
            };

            // Remove piece from source
            tmp.board.remove_piece(from);

            // Handle capture if any
            if let Some(captured) = tmp.board.piece_on(to) {
                // Add to hand (unpromote captured piece)
                let unpromoted_type = if captured.promoted {
                    // Manually unpromote the piece type
                    match captured.piece_type {
                        PieceType::Pawn
                        | PieceType::Lance
                        | PieceType::Knight
                        | PieceType::Silver => captured.piece_type,
                        PieceType::Bishop | PieceType::Rook => captured.piece_type,
                        _ => captured.piece_type, // Gold and King cannot be promoted
                    }
                } else {
                    captured.piece_type
                };

                let hand_idx = match piece_type_to_hand_index(unpromoted_type) {
                    Ok(idx) => idx,
                    Err(_) => return false,
                };
                tmp.hands[tmp.side_to_move as usize][hand_idx] += 1;
            }

            // Place piece at destination (with promotion if specified)
            let final_piece = if mv.is_promote() {
                piece.promote()
            } else {
                piece
            };
            tmp.board.put_piece(to, final_piece);
        }

        // Switch side to move
        tmp.side_to_move = tmp.side_to_move.opposite();

        // Check if opponent is in check
        tmp.is_in_check()
    }
}

#[cfg(test)]
mod tests {
    use crate::usi::{parse_sfen, parse_usi_move};

    #[test]
    fn test_gives_check_normal_move() {
        // Position where Rook move gives check
        let sfen = "k8/9/9/9/9/9/9/R8/K8 b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // Rook moves to give check
        let mv = parse_usi_move("1h1a").unwrap();
        assert!(pos.gives_check(mv), "Rook move should give check");

        // Rook moves sideways (no check)
        let mv = parse_usi_move("1h2h").unwrap();
        assert!(!pos.gives_check(mv), "Sideways rook move should not give check");
    }

    #[test]
    fn test_gives_check_drop_move() {
        // Position where dropping a lance gives check
        let sfen = "k8/9/9/9/9/9/9/9/K8 b L 1";
        let pos = parse_sfen(sfen).unwrap();

        // Drop lance to give check
        let mv = parse_usi_move("L*1b").unwrap();
        assert!(pos.gives_check(mv), "Lance drop should give check");

        // Drop lance elsewhere (no check)
        let mv = parse_usi_move("L*5e").unwrap();
        assert!(!pos.gives_check(mv), "Lance drop to 5e should not give check");
    }

    #[test]
    fn test_gives_check_promotion() {
        // Position where pawn promotion gives check
        let sfen = "k8/P8/9/9/9/9/9/9/K8 b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // Pawn promotes to tokin and gives check
        let mv = parse_usi_move("1b1a+").unwrap();
        assert!(pos.gives_check(mv), "Pawn promotion should give check");
    }
}
