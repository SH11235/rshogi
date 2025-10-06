//! Position extensions for move legality checking

#[cfg(any(debug_assertions, feature = "diagnostics"))]
use crate::search::ab::diagnostics as ab_diagnostics;
use crate::shogi::attacks;
use crate::shogi::board::{Bitboard, Color, PieceType, Square};
use crate::shogi::moves::Move;
#[cfg(any(debug_assertions, feature = "diagnostics"))]
use crate::usi::{move_to_usi, position_to_sfen};
#[cfg(any(debug_assertions, feature = "diagnostics"))]
use log::warn;

use super::Position;

// Extension for Position to check legal moves
impl Position {
    /// Check if the current player has a pawn in the given file
    pub(crate) fn has_pawn_in_file(&self, file: u8) -> bool {
        self.has_pawn_in_file_for_color(file, self.side_to_move)
    }

    /// Check if the specified color has a pawn in the given file
    pub(crate) fn has_pawn_in_file_for_color(&self, file: u8, color: Color) -> bool {
        let pawn_bb = self.board.piece_bb[color as usize][PieceType::Pawn as usize];
        let file_mask = attacks::file_mask(file);

        // Get unpromoted pawns in this file
        let unpromoted_pawns_in_file = pawn_bb & file_mask & !self.board.promoted_bb;

        !unpromoted_pawns_in_file.is_empty()
    }

    /// Check if dropping a pawn would result in checkmate
    pub(crate) fn is_checkmate_by_pawn_drop(&self, pawn_drop: Move) -> bool {
        // The pawn must give check to the opponent's king
        let defense_color = self.side_to_move.opposite();
        let king_sq = match self.board.king_square(defense_color) {
            Some(sq) => sq,
            None => return false, // No king
        };

        // Check if pawn would give check (pawn attacks one square forward)
        let pawn_sq = pawn_drop.to();

        // Debug assert: pawn drops should already be filtered to valid positions
        #[cfg(debug_assertions)]
        {
            debug_assert!(
                !(self.side_to_move == Color::Black && pawn_sq.rank() == 0),
                "Black pawn cannot be dropped on rank 1"
            );
            debug_assert!(
                !(self.side_to_move == Color::White && pawn_sq.rank() == 8),
                "White pawn cannot be dropped on rank 9"
            );
        }

        // Black pawn attacks upward (toward rank 0), white pawn attacks downward (toward rank 8)
        let expected_king_sq = if self.side_to_move == Color::Black {
            // Black pawn at rank N attacks rank N-1
            let rank = pawn_sq.rank();
            if rank == 0 {
                return false; // Black pawn at rank 0 cannot attack (invalid position)
            }
            Square::new(pawn_sq.file(), rank - 1)
        } else {
            // White pawn at rank N attacks rank N+1
            let rank = pawn_sq.rank();
            if rank == 8 {
                return false; // White pawn at rank 8 cannot attack (invalid position)
            }
            Square::new(pawn_sq.file(), rank + 1)
        };

        if king_sq != expected_king_sq {
            return false; // Pawn doesn't give check
        }

        // Step 1: Simulate pawn drop to check support and captures in the actual position
        let mut test_pos = self.clone();
        test_pos.do_move(pawn_drop);

        // Check if the pawn has support (can't be captured by king)
        if !test_pos.is_attacked(pawn_sq, self.side_to_move) {
            return false; // The pawn has no followers, king can capture it
        }

        // Step 2: Check if opponent's pieces (except king/lance/pawn) can capture the pawn
        let capture_candidates = test_pos.get_attackers_to(pawn_sq, defense_color);

        // Exclude king, lance, and pawn from capture candidates
        let king_bb = test_pos.board.piece_bb[defense_color as usize][PieceType::King as usize];
        let lance_bb = test_pos.board.piece_bb[defense_color as usize][PieceType::Lance as usize];
        let pawn_bb = test_pos.board.piece_bb[defense_color as usize][PieceType::Pawn as usize];
        let excluded = king_bb | lance_bb | pawn_bb;

        let valid_capture_candidates = capture_candidates & !excluded;

        // Check for pinned pieces
        let pinned = test_pos.get_blockers_for_king(defense_color);
        let pawn_file_mask = attacks::file_mask(pawn_sq.file());
        let not_pinned_for_capture = !pinned | pawn_file_mask;

        let can_capture = valid_capture_candidates & not_pinned_for_capture;
        if !can_capture.is_empty() {
            return false; // Some piece can capture the pawn
        }

        // Step 3: Check if king can escape
        let king_moves = attacks::king_attacks(king_sq);

        // King cannot capture its own pieces (Shogi rule)
        let friend_blocks = test_pos.board.occupied_bb[defense_color as usize];
        let mut king_escape_candidates = king_moves & !friend_blocks;

        // Remove the pawn square (king can't capture it due to support)
        let mut pawn_sq_bb = Bitboard::EMPTY;
        pawn_sq_bb.set(pawn_sq);
        king_escape_candidates &= !pawn_sq_bb;

        // Check each escape square
        let mut candidates = king_escape_candidates;
        while let Some(escape_sq) = candidates.pop_lsb() {
            // Simulate king moving to escape square
            let king_move = Move::normal(king_sq, escape_sq, false);
            let mut escape_test_pos = test_pos.clone();

            // Try to make the king move
            // If there's a piece to capture, it will be handled by do_move
            escape_test_pos.do_move(king_move);

            // Check if king is safe after moving
            let is_safe = !escape_test_pos.is_check(defense_color);

            if is_safe {
                return false; // King has a safe escape
            }
        }

        // All conditions met - it's checkmate by pawn drop
        true
    }

    /// Check if a move is legal
    ///
    /// This optimized version uses do_move/undo_move to check legality.
    /// It's much faster than generating all legal moves (O(1) vs O(N)).
    pub fn is_legal_move(&self, mv: Move) -> bool {
        // Basic validation
        if mv.is_drop() {
            // Check if we have the piece to drop
            let piece_type = mv.drop_piece_type();
            let color_idx = self.side_to_move as usize;
            let Some(hand_idx) = piece_type.hand_index() else {
                return false; // Can't drop King or invalid type
            };

            if self.hands[color_idx][hand_idx] == 0 {
                return false;
            }

            // Check if target square is empty
            if self.board.piece_on(mv.to()).is_some() {
                return false;
            }

            // Check piece-specific drop restrictions
            match piece_type {
                PieceType::Pawn => {
                    let to_rank = mv.to().rank();

                    // Check rank restrictions - pawn cannot be dropped on the last rank
                    if (self.side_to_move == Color::Black && to_rank == 0)
                        || (self.side_to_move == Color::White && to_rank == 8)
                    {
                        return false;
                    }

                    // Check nifu (double pawn)
                    if self.has_pawn_in_file(mv.to().file()) {
                        return false;
                    }

                    // Check uchifuzume (checkmate by pawn drop)
                    if self.is_checkmate_by_pawn_drop(mv) {
                        return false;
                    }
                }
                PieceType::Lance => {
                    let to_rank = mv.to().rank();

                    // Check rank restrictions - lance cannot be dropped on the last rank
                    if (self.side_to_move == Color::Black && to_rank == 0)
                        || (self.side_to_move == Color::White && to_rank == 8)
                    {
                        return false;
                    }
                }
                PieceType::Knight => {
                    let to_rank = mv.to().rank();

                    // Check rank restrictions - knight cannot be dropped on the last two ranks
                    if (self.side_to_move == Color::Black && to_rank <= 1)
                        || (self.side_to_move == Color::White && to_rank >= 7)
                    {
                        return false;
                    }
                }
                _ => {} // Other pieces have no special drop restrictions
            }
        } else {
            // Normal move
            if let Some(from) = mv.from() {
                // Check if there's a piece at the from square
                if let Some(piece) = self.board.piece_on(from) {
                    // Check if it's our piece
                    if piece.color != self.side_to_move {
                        return false;
                    }

                    // Check if capturing our own piece
                    if let Some(to_piece) = self.board.piece_on(mv.to()) {
                        if to_piece.color == self.side_to_move {
                            return false;
                        }

                        if to_piece.piece_type == PieceType::King {
                            #[cfg(any(debug_assertions, feature = "diagnostics"))]
                            diagnostics::report_enemy_king_capture(self, mv, "is_legal_move");
                            // Capturing the opponent king is illegal in Shogi and indicates a generator bug.
                            return false;
                        }
                    }
                } else {
                    return false;
                }
            } else {
                return false;
            }
        }

        // Try to make the move and check if it leaves king in check
        let mut test_pos = self.clone();
        test_pos.do_move(mv);

        // Check if the side that made the move left their king in check
        let king_in_check = test_pos.is_check(self.side_to_move);

        !king_in_check
    }
}

#[cfg(any(debug_assertions, feature = "diagnostics"))]
mod diagnostics {
    use super::*;
    use crate::search::ab::diagnostics as ab_diag;
    use crate::shogi::Move;
    use std::collections::HashSet;
    use std::env;
    use std::sync::{Mutex, OnceLock};

    static REPORTED: OnceLock<Mutex<HashSet<(u64, u32)>>> = OnceLock::new();
    static PANIC_ON_GUARD: OnceLock<bool> = OnceLock::new();

    fn should_panic_on_enemy_king_capture() -> bool {
        *PANIC_ON_GUARD.get_or_init(|| match env::var("SHOGI_PANIC_ON_KING_CAPTURE") {
            Ok(value) => {
                let normalized = value.trim().to_ascii_lowercase();
                !(normalized == "0"
                    || normalized == "false"
                    || normalized == "no"
                    || normalized == "off")
            }
            Err(_) => true,
        })
    }

    pub(super) fn report_enemy_king_capture(pos: &Position, mv: Move, origin: &str) {
        let mut guard = REPORTED
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .expect("report mutex poisoned");
        let key = (pos.hash, mv.to_u32());
        if !guard.insert(key) {
            return;
        }
        drop(guard);

        let sfen = position_to_sfen(pos);
        let mv_str = move_to_usi(&mv);
        ab_diag::dump("legality_guard", pos, Some(mv));
        warn!(
            "[legality] enemy king capture blocked: origin={} move={} side={:?} ply={} sfen={}",
            origin, mv_str, pos.side_to_move, pos.ply, sfen
        );
        super::ab_diagnostics::note_fault("king_capture_detected");
        if should_panic_on_enemy_king_capture() {
            panic!(
                "Position::is_legal_move detected enemy king capture attempt ({} from {})",
                mv_str, origin
            );
        }
    }
}
