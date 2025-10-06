//! Move execution and undo functionality
//!
//! This module handles making and unmaking moves on the position,
//! including proper hash updates and state management.

use crate::shogi::board::{Color, Piece, PieceType};
use crate::shogi::moves::Move;
use crate::shogi::piece_constants::piece_type_to_hand_index;

use super::zobrist::ZOBRIST;
use super::{Position, UndoInfo};

#[cfg(any(debug_assertions, feature = "diagnostics"))]
use crate::search::ab::diagnostics as ab_diagnostics;
#[cfg(any(debug_assertions, feature = "diagnostics"))]
use log::warn;
#[cfg(any(debug_assertions, feature = "diagnostics"))]
use std::collections::HashSet;
#[cfg(any(debug_assertions, feature = "diagnostics"))]
use std::sync::{Mutex, OnceLock};

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
            moved_from: None,
            moved_to: None,
            moved_piece_type: None,
            moved_piece_color: None,
            king_moved: false,
            king_bb_before: [
                self.board.piece_bb[Color::Black as usize][PieceType::King as usize],
                self.board.piece_bb[Color::White as usize][PieceType::King as usize],
            ],
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diag_from: None,
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diag_piece_type: None,
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diag_kings: [None, None],
        };

        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        {
            undo_info.diag_kings = [
                self.board.king_square(Color::Black),
                self.board.king_square(Color::White),
            ];
        }

        if mv.is_drop() {
            // Handle drop move
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            let piece = Piece::new(piece_type, self.side_to_move);

            // Place piece on board
            self.board.put_piece(to, piece);

            undo_info.moved_from = None;
            undo_info.moved_to = Some(to);
            undo_info.moved_piece_type = Some(piece_type);
            undo_info.moved_piece_color = Some(self.side_to_move);
            undo_info.king_moved = false;

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
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            ab_diagnostics::record_tag(
                self,
                "do_move_piece",
                Some(format!("from={:?} to={:?} piece={:?}", from, to, piece)),
            );
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            {
                undo_info.diag_from = Some(from);
                undo_info.diag_piece_type = Some(piece.piece_type);
            }

            let moving_color = self.side_to_move;
            let moving_piece_type = piece.piece_type;

            undo_info.moved_from = Some(from);
            undo_info.moved_to = Some(to);
            undo_info.moved_piece_type = Some(moving_piece_type);
            undo_info.moved_piece_color = Some(moving_color);
            undo_info.king_moved = moving_piece_type == PieceType::King;

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
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            ab_diagnostics::record_tag(
                self,
                "do_move_post",
                Some(format!(
                    "to={:?} piece={:?} kings=[B:{:?},W:{:?}]",
                    to,
                    self.board.piece_on(to),
                    self.board.king_square(Color::Black),
                    self.board.king_square(Color::White)
                )),
            );
        }

        let moving_color = self.side_to_move;
        let opponent_color = moving_color.opposite();

        if undo_info.king_moved {
            let from_sq = undo_info.moved_from.expect("king moves must record source square");
            let to_sq = undo_info.moved_to.expect("king moves must record destination square");
            let mut updated = undo_info.king_bb_before[moving_color as usize];
            updated.clear(from_sq);
            updated.set(to_sq);
            self.board.piece_bb[moving_color as usize][PieceType::King as usize] = updated;
        } else {
            self.board.piece_bb[moving_color as usize][PieceType::King as usize] =
                undo_info.king_bb_before[moving_color as usize];
        }
        self.board.piece_bb[opponent_color as usize][PieceType::King as usize] =
            undo_info.king_bb_before[opponent_color as usize];

        // Switch side to move
        self.side_to_move = self.side_to_move.opposite();
        // Always XOR with the White side hash to toggle between Black/White
        self.hash ^= ZOBRIST.side_to_move;
        self.zobrist_hash = self.hash;

        // 診断チェック追加
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.hash, self.zobrist_hash,
            "Hash fields out of sync after do_move: hash={:016x} zobrist={:016x}",
            self.hash, self.zobrist_hash
        );

        // Increment ply
        self.ply += 1;

        self.bump_epoch();

        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::check_integrity(self, Some(mv), "do_move");

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

        let moving_color =
            undo_info.moved_piece_color.expect("UndoInfo must record moving piece color");

        debug_assert_eq!(
            self.side_to_move, moving_color,
            "side_to_move should match stored moving color during undo"
        );

        if mv.is_drop() {
            // Undo drop move
            let to = mv.to();
            let piece_type = mv.drop_piece_type();

            // Remove piece from board
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            ab_diagnostics::record_tag(
                self,
                "undo_drop_pre_remove",
                Some(format!("to={:?} piece={:?}", to, self.board.piece_on(to))),
            );
            self.board.remove_piece(to);

            // Add back to hand
            let hand_idx = piece_type_to_hand_index(piece_type)
                .expect("Drop piece type must be valid hand piece");
            self.hands[moving_color as usize][hand_idx] += 1;
        } else {
            // Undo normal move
            let from = mv.from().expect("Normal move must have from square");
            let to = mv.to();

            // Get piece from destination
            let mut piece =
                self.board.piece_on(to).expect("Move destination must have a piece after move");
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            ab_diagnostics::record_tag(
                self,
                "undo_move_pre_remove",
                Some(format!("to={:?} piece={:?}", to, piece)),
            );
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            {
                if let Some(expected_from) = undo_info.diag_from {
                    if expected_from != from {
                        let extra = format!(
                            "expected_from={:?} actual_from={:?} mv={} hash={:016x}",
                            expected_from,
                            from,
                            crate::usi::move_to_usi(&mv),
                            self.hash
                        );
                        ab_diagnostics::record_tag(self, "undo_from_mismatch", Some(extra.clone()));
                        warn!(
                            "[undo_move] from-square mismatch detected: expected={:?} actual={:?} move={} hash={:016x}",
                            expected_from,
                            from,
                            crate::usi::move_to_usi(&mv),
                            self.hash
                        );
                        ab_diagnostics::note_fault("undo_from_mismatch");
                    }
                }
                if let Some(expected_type) = undo_info.diag_piece_type {
                    if piece.piece_type != expected_type {
                        let extra = format!(
                            "expected_type={:?} actual_piece={:?} move={} hash={:016x}",
                            expected_type,
                            piece,
                            crate::usi::move_to_usi(&mv),
                            self.hash
                        );
                        ab_diagnostics::record_tag(
                            self,
                            "undo_piece_type_mismatch",
                            Some(extra.clone()),
                        );
                        warn!(
                            "[undo_move] piece type mismatch detected: expected={:?} actual={:?} move={} hash={:016x}",
                            expected_type,
                            piece,
                            crate::usi::move_to_usi(&mv),
                            self.hash
                        );
                        ab_diagnostics::note_fault("undo_piece_type_mismatch");
                    }
                }
            }

            // Remove piece from destination
            self.board.remove_piece(to);

            // Restore promotion status
            if mv.is_promote() {
                piece.promoted = undo_info.moved_piece_was_promoted;
            }

            debug_assert_eq!(
                undo_info.moved_piece_type,
                Some(piece.piece_type),
                "stored moved piece type disagrees with board state during undo"
            );

            // Place piece back at source
            self.board.put_piece(from, piece);

            // Restore captured piece if any
            if let Some(captured) = undo_info.captured {
                self.board.put_piece(to, captured);
                #[cfg(any(debug_assertions, feature = "diagnostics"))]
                ab_diagnostics::record_tag(
                    self,
                    "undo_move_restore_capture",
                    Some(format!("to={:?} piece={:?}", to, captured)),
                );

                // Remove from hand
                let captured_type = captured.piece_type;
                let hand_idx =
                    piece_type_to_hand_index(captured_type).expect("Captured piece cannot be King");
                self.hands[moving_color as usize][hand_idx] -= 1;
            }
        }

        self.board.piece_bb[Color::Black as usize][PieceType::King as usize] =
            undo_info.king_bb_before[Color::Black as usize];
        self.board.piece_bb[Color::White as usize][PieceType::King as usize] =
            undo_info.king_bb_before[Color::White as usize];

        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        {
            static KING_INTEGRITY_WARNED: OnceLock<Mutex<HashSet<(u64, u8)>>> = OnceLock::new();
            let mut guard = KING_INTEGRITY_WARNED
                .get_or_init(|| Mutex::new(HashSet::new()))
                .lock()
                .expect("king integrity mutex poisoned");
            for &color in &[Color::Black, Color::White] {
                let key = (self.hash, color as u8);
                if !guard.insert(key) {
                    continue;
                }
                match self.board.king_square(color) {
                    Some(king_sq) => match self.board.piece_on(king_sq) {
                        Some(piece)
                            if piece.piece_type == PieceType::King && piece.color == color => {}
                        other => {
                            let extra = format!(
                                "color={:?} king_sq={:?} occupant={:?} move={} hash={:016x}",
                                color,
                                king_sq,
                                other,
                                crate::usi::move_to_usi(&mv),
                                self.hash
                            );
                            ab_diagnostics::record_tag(
                                self,
                                "undo_king_mismatch",
                                Some(extra.clone()),
                            );
                            warn!(
                                "[undo_move] king square mismatch detected: color={:?} square={:?} occupant={:?} move={} hash={:016x}",
                                color,
                                king_sq,
                                other,
                                crate::usi::move_to_usi(&mv),
                                self.hash
                            );
                            ab_diagnostics::note_fault("undo_king_mismatch");
                        }
                    },
                    None => {
                        let extra = format!(
                            "color={:?} king_sq=None move={} hash={:016x}",
                            color,
                            crate::usi::move_to_usi(&mv),
                            self.hash
                        );
                        ab_diagnostics::record_tag(self, "undo_king_missing", Some(extra.clone()));
                        warn!(
                            "[undo_move] king square missing: color={:?} move={} hash={:016x}",
                            color,
                            crate::usi::move_to_usi(&mv),
                            self.hash
                        );
                        ab_diagnostics::note_fault("undo_king_missing");
                    }
                }
            }
            if undo_info.diag_kings.iter().any(|sq| sq.is_some()) {
                let current = [
                    self.board.king_square(Color::Black),
                    self.board.king_square(Color::White),
                ];
                if current != undo_info.diag_kings {
                    let extra = format!(
                        "expected_kings={:?} actual_kings={:?} move={} hash={:016x}",
                        undo_info.diag_kings,
                        current,
                        crate::usi::move_to_usi(&mv),
                        self.hash
                    );
                    ab_diagnostics::record_tag(self, "undo_king_roundtrip", Some(extra.clone()));
                    warn!(
                        "[undo_move] king roundtrip mismatch: expected={:?} actual={:?} move={} hash={:016x}",
                        undo_info.diag_kings,
                        current,
                        crate::usi::move_to_usi(&mv),
                        self.hash
                    );
                    ab_diagnostics::note_fault("undo_king_roundtrip");
                }
            }
            drop(guard);
        }

        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.hash, self.zobrist_hash,
            "Hash fields out of sync after undo_move: hash={:016x} zobrist={:016x}",
            self.hash, self.zobrist_hash
        );

        self.bump_epoch();

        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::check_integrity(self, Some(mv), "undo_move");
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
            moved_from: None,
            moved_to: None,
            moved_piece_type: None,
            moved_piece_color: None,
            king_moved: false,
            king_bb_before: [
                self.board.piece_bb[Color::Black as usize][PieceType::King as usize],
                self.board.piece_bb[Color::White as usize][PieceType::King as usize],
            ],
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diag_from: None,
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diag_piece_type: None,
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            diag_kings: [
                self.board.king_square(Color::Black),
                self.board.king_square(Color::White),
            ],
        };

        // Switch side to move
        self.side_to_move = self.side_to_move.opposite();

        // Update hash by toggling side to move
        self.hash ^= ZOBRIST.side_to_move;
        self.zobrist_hash = self.hash;

        // Increment ply
        self.ply += 1;

        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.hash, self.zobrist_hash,
            "Hash fields out of sync after do_null_move: hash={:016x} zobrist={:016x}",
            self.hash, self.zobrist_hash
        );

        self.bump_epoch();

        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::check_integrity(self, None, "do_null_move");

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

        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.hash, self.zobrist_hash,
            "Hash fields out of sync after undo_null_move: hash={:016x} zobrist={:016x}",
            self.hash, self.zobrist_hash
        );

        self.bump_epoch();

        #[cfg(any(debug_assertions, feature = "diagnostics"))]
        diagnostics::check_integrity(self, None, "undo_null_move");
    }
}

#[cfg(any(debug_assertions, feature = "diagnostics"))]
mod diagnostics {
    use super::Position;
    use crate::search::ab::diagnostics as ab_diagnostics;
    use crate::shogi::board::Piece;
    use crate::shogi::board::NUM_PIECE_TYPES;
    use crate::shogi::moves::Move;
    use crate::shogi::{Bitboard, Color, PieceType};
    use crate::usi::{move_to_usi, position_to_sfen};
    use log::warn;
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};

    pub(super) fn check_integrity(pos: &Position, mv: Option<Move>, origin: &str) {
        let mut issues: Vec<String> = Vec::new();
        let suspect = mv.map(|m| move_to_usi(&m)).unwrap_or_else(|| "-".to_string());
        if pos.ply >= 90 && matches!(origin, "do_move" | "undo_move" | "undo_null_move") {
            let tag = match origin {
                "do_move" => "do_move",
                "undo_move" => "undo_move",
                "undo_null_move" => "undo_null_move",
                _ => "king_check",
            };
            let extra = format!(
                "move={} kings=[B:{:?},W:{:?}] hash={:016x}",
                suspect,
                pos.board.king_square(Color::Black),
                pos.board.king_square(Color::White),
                pos.hash
            );
            ab_diagnostics::record_tag(pos, tag, Some(extra));
        }

        for &color in &[Color::Black, Color::White] {
            match pos.board.king_square(color) {
                Some(king_sq) => match pos.board.piece_on(king_sq) {
                    Some(piece) if is_expected_king(piece, color) => {}
                    Some(piece) => issues.push(format!(
                        "king_square_mismatch color={:?} square={} piece={:?}",
                        color, king_sq, piece
                    )),
                    None => issues
                        .push(format!("king_square_empty color={:?} square={}", color, king_sq)),
                },
                None => issues.push(format!("king_square_none color={:?}", color)),
            }
        }

        for &color in &[Color::Black, Color::White] {
            let mut actual_bb: u128 = 0;
            for (idx, entry) in pos.board.squares.iter().enumerate() {
                if let Some(piece) = entry {
                    if is_expected_king(*piece, color) {
                        actual_bb |= 1u128 << idx;
                    }
                }
            }
            let recorded_bb = pos.board.piece_bb[color as usize][PieceType::King as usize].0;
            if actual_bb != recorded_bb {
                issues.push(format!(
                    "king_bitboard_mismatch color={:?} actual={:#034x} recorded={:#034x}",
                    color, actual_bb, recorded_bb
                ));
                ab_diagnostics::note_fault("king_bitboard_mismatch");
            }
        }

        // Occupancy consistency: piece bitboards vs cached occupancy masks
        let mut combined_from_piece_bb = Bitboard::EMPTY;
        for &color in &[Color::Black, Color::White] {
            let mut union = Bitboard::EMPTY;
            for piece_idx in 0..NUM_PIECE_TYPES {
                union |= pos.board.piece_bb[color as usize][piece_idx];
            }
            let cached = pos.board.occupied_bb[color as usize];
            if union != cached {
                issues.push(format!(
                    "occupancy_color_mismatch color={:?} union={:?} cached={:?}",
                    color, union, cached
                ));
                ab_diagnostics::note_fault("occupancy_color_mismatch");
            }
            combined_from_piece_bb |= union;
        }

        let overlap = pos.board.occupied_bb[Color::Black as usize]
            & pos.board.occupied_bb[Color::White as usize];
        if overlap != Bitboard::EMPTY {
            issues.push(format!("occupancy_overlap overlap={:?}", overlap));
            ab_diagnostics::note_fault("occupancy_overlap");
        }

        let all_cached = pos.board.all_bb;
        if combined_from_piece_bb != all_cached {
            issues.push(format!(
                "occupancy_all_mismatch combined={:?} all_cached={:?}",
                combined_from_piece_bb, all_cached
            ));
            ab_diagnostics::note_fault("occupancy_all_mismatch");
        }

        if let Some(mv) = mv {
            let enemy_color = match origin {
                "undo_move" | "undo_null_move" => pos.side_to_move.opposite(),
                _ => pos.side_to_move,
            };
            if let Some(piece) = pos.board.piece_on(mv.to()) {
                if piece.piece_type == PieceType::King && piece.color == enemy_color {
                    issues.push(format!(
                        "destination_contains_king color={:?} square={}",
                        piece.color,
                        mv.to()
                    ));
                }
            }
            if let Some(opponent_sq) = pos.board.king_square(pos.side_to_move) {
                if opponent_sq == mv.to() {
                    issues.push(format!(
                        "king_square_matches_destination color={:?} square={}",
                        pos.side_to_move, opponent_sq
                    ));
                }
            }
        }

        if issues.is_empty() {
            return;
        }

        static REPORTED: OnceLock<Mutex<HashSet<(u64, u32, u64)>>> = OnceLock::new();
        let key_move = mv.map(|m| m.to_u32()).unwrap_or(u32::MAX);
        let origin_id = origin.as_ptr() as usize as u64;
        let mut guard = REPORTED
            .get_or_init(|| Mutex::new(HashSet::new()))
            .lock()
            .expect("king integrity mutex poisoned");
        if !guard.insert((pos.hash, key_move, origin_id)) {
            return;
        }
        drop(guard);

        let sfen = position_to_sfen(pos);
        let issues_str = issues.join(" | ");
        ab_diagnostics::dump("king_integrity", pos, mv);
        warn!(
            "[position] integrity mismatch origin={} move={} ply={} side={:?} hash={:016x} issues={} sfen={}",
            origin,
            suspect,
            pos.ply,
            pos.side_to_move,
            pos.hash,
            issues_str,
            sfen
        );
    }

    fn is_expected_king(piece: Piece, color: Color) -> bool {
        piece.piece_type == PieceType::King && piece.color == color
    }
}
