//! Move execution and undo functionality
//!
//! This module handles making and unmaking moves on the position,
//! including proper hash updates and state management.

use crate::shogi::board::{Piece, PieceType};
use crate::shogi::moves::Move;
use crate::shogi::piece_constants::piece_type_to_hand_index;

use super::zobrist::ZOBRIST;
use super::{Position, UndoInfo};

impl Position {
    /// Make a move on the position
    ///
    /// IMPORTANT: When capturing a promoted piece, it is automatically unpromoted
    /// when added to hand (as per shogi rules). The promoted flag is stored in
    /// UndoInfo for proper restoration during unmake_move.
    pub fn do_move(&mut self, mv: Move) -> UndoInfo {
        // Save current position key to history (zobrist)
        self.history.push(self.zobrist_hash);

        // Initialize undo info
        let mut undo_info = UndoInfo {
            captured: None,
            moved_piece_was_promoted: false,
            previous_hash: self.hash,
            previous_ply: self.ply,
        };

        if mv.is_drop() {
            // Handle drop move
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            let piece = Piece::new(piece_type, self.side_to_move);

            // Place piece on board
            self.board.put_piece(to, piece);

            // Remove from hand
            let hand_idx = piece_type_to_hand_index(piece_type)
                .expect("Drop piece type must be valid hand piece");
            self.hands[self.side_to_move as usize][hand_idx] -= 1;

            // Update hash
            self.hash ^= self.piece_square_zobrist(piece, to);
            self.hash ^= self.hand_zobrist(
                self.side_to_move,
                piece_type,
                self.hands[self.side_to_move as usize][hand_idx] + 1,
            );
            self.hash ^= self.hand_zobrist(
                self.side_to_move,
                piece_type,
                self.hands[self.side_to_move as usize][hand_idx],
            );
        } else {
            // Handle normal move
            let from = mv.from().expect("Normal move must have from square");
            let to = mv.to();

            // Get moving piece
            let mut piece = self.board.piece_on(from).expect("Move source must have a piece");

            // CRITICAL: Validate that the moving piece belongs to the side to move
            // This prevents illegal moves where the wrong side's piece is being moved
            if piece.color != self.side_to_move {
                eprintln!("ERROR: Attempting to move opponent's piece!");
                eprintln!("Move: from={from}, to={to}");
                eprintln!("Moving piece: {piece:?}");
                eprintln!("Side to move: {:?}", self.side_to_move);
                eprintln!("Position SFEN: {}", crate::usi::position_to_sfen(self));
                panic!("Illegal move: attempting to move opponent's piece from {from} to {to}");
            }

            // Save promoted status for undo
            undo_info.moved_piece_was_promoted = piece.promoted;

            // Remove piece from source
            self.board.remove_piece(from);
            self.hash ^= self.piece_square_zobrist(piece, from);

            // Handle capture
            if let Some(captured) = self.board.piece_on(to) {
                // Save captured piece for undo
                undo_info.captured = Some(captured);
                // Debug check - should never capture king
                if captured.piece_type == PieceType::King {
                    eprintln!("ERROR: King capture detected!");
                    eprintln!("Move: from={from}, to={to}");
                    eprintln!("Moving piece: {piece:?}");
                    eprintln!("Captured piece: {captured:?}");
                    eprintln!("Side to move: {:?}", self.side_to_move);
                    eprintln!("Position SFEN: {}", crate::usi::position_to_sfen(self));
                    panic!("Illegal move: attempting to capture king at {to}");
                }

                self.board.remove_piece(to);
                self.hash ^= self.piece_square_zobrist(captured, to);

                // Add to hand (unpromoted)
                // IMPORTANT: When capturing a promoted piece, it becomes unpromoted in hand
                let captured_type = captured.piece_type;

                let hand_idx =
                    piece_type_to_hand_index(captured_type).expect("Captured piece cannot be King");

                self.hash ^= self.hand_zobrist(
                    self.side_to_move,
                    captured_type,
                    self.hands[self.side_to_move as usize][hand_idx],
                );
                self.hands[self.side_to_move as usize][hand_idx] += 1;
                self.hash ^= self.hand_zobrist(
                    self.side_to_move,
                    captured_type,
                    self.hands[self.side_to_move as usize][hand_idx],
                );
            }

            // Handle promotion
            if mv.is_promote() {
                piece.promoted = true;
            }

            // Place piece on destination
            self.board.put_piece(to, piece);
            self.hash ^= self.piece_square_zobrist(piece, to);
        }

        // Switch side to move
        self.side_to_move = self.side_to_move.opposite();
        // Always XOR with the White side hash to toggle between Black/White
        self.hash ^= ZOBRIST.side_to_move;
        self.zobrist_hash = self.hash;

        // Increment ply
        self.ply += 1;

        undo_info
    }

    /// Undo a move on the position
    pub fn undo_move(&mut self, mv: Move, undo_info: UndoInfo) {
        // Remove last key from history
        self.history.pop();

        // Restore hash value
        self.hash = undo_info.previous_hash;
        self.zobrist_hash = self.hash;

        // Restore side to move and ply
        self.side_to_move = self.side_to_move.opposite();
        self.ply = undo_info.previous_ply;

        if mv.is_drop() {
            // Undo drop move
            let to = mv.to();
            let piece_type = mv.drop_piece_type();

            // Remove piece from board
            self.board.remove_piece(to);

            // Add back to hand
            let hand_idx = piece_type_to_hand_index(piece_type)
                .expect("Drop piece type must be valid hand piece");
            self.hands[self.side_to_move as usize][hand_idx] += 1;
        } else {
            // Undo normal move
            let from = mv.from().expect("Normal move must have from square");
            let to = mv.to();

            // Get piece from destination
            let mut piece =
                self.board.piece_on(to).expect("Move destination must have a piece after move");

            // Remove piece from destination
            self.board.remove_piece(to);

            // Restore promotion status
            if mv.is_promote() {
                piece.promoted = undo_info.moved_piece_was_promoted;
            }

            // Place piece back at source
            self.board.put_piece(from, piece);

            // Restore captured piece if any
            if let Some(captured) = undo_info.captured {
                self.board.put_piece(to, captured);

                // Remove from hand
                let captured_type = captured.piece_type;
                let hand_idx =
                    piece_type_to_hand_index(captured_type).expect("Captured piece cannot be King");
                self.hands[self.side_to_move as usize][hand_idx] -= 1;
            }
        }
    }

    /// Do null move - switches side to move without making any actual move
    /// Used in null move pruning for search optimization
    /// Returns undo information to restore the position state
    pub fn do_null_move(&mut self) -> UndoInfo {
        // Save current position key to history (zobrist)
        self.history.push(self.zobrist_hash);

        // Create undo info
        let undo_info = UndoInfo {
            captured: None,
            moved_piece_was_promoted: false,
            previous_hash: self.hash,
            previous_ply: self.ply,
        };

        // Switch side to move
        self.side_to_move = self.side_to_move.opposite();

        // Update hash by toggling side to move
        self.hash ^= ZOBRIST.side_to_move;
        self.zobrist_hash = self.hash;

        // Increment ply
        self.ply += 1;

        undo_info
    }

    /// Undo null move - restores position state after null move
    pub fn undo_null_move(&mut self, undo_info: UndoInfo) {
        // Remove last key from history
        self.history.pop();

        // Restore hash value
        self.hash = undo_info.previous_hash;
        self.zobrist_hash = self.hash;

        // Restore side to move and ply
        self.side_to_move = self.side_to_move.opposite();
        self.ply = undo_info.previous_ply;
    }
}
