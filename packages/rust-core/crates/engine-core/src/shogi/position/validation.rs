//! Move validation and position query methods
//!
//! This module provides methods for validating moves and querying
//! the position state (check, repetition, draw, etc).

use crate::shogi::board::{Color, Piece, PieceType, Square};
use crate::shogi::moves::Move;
use crate::shogi::piece_constants::piece_type_to_hand_index;

use super::Position;

impl Position {
    /// Reference implementation: Check if square is attacked by any piece of given color
    /// This is slow but correct - doesn't rely on move generator
    pub fn is_square_attacked_by_slow(&self, sq: Square, by: Color) -> bool {
        use crate::shogi::board::PieceType::*;

        // Helper to check if attacker at from_sq can reach to_sq
        let can_attack = |from_sq: Square, piece: Piece, to_sq: Square| -> bool {
            let dr = to_sq.rank() as i8 - from_sq.rank() as i8;
            let dc = to_sq.file() as i8 - from_sq.file() as i8;

            // Direction from attacker's perspective
            let (dr_abs, dc_abs) = (dr.abs(), dc.abs());

            match piece.piece_type {
                King => dr_abs <= 1 && dc_abs <= 1,

                Pawn => {
                    // Pawn attacks one square forward (different for each color)
                    if piece.promoted {
                        // Promoted pawn (tokin) moves like gold
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1)
                                || (dr == 0 && dc_abs == 1)
                                || (dr == 1 && dc == 0)
                        } else {
                            (dr == 1 && dc_abs <= 1)
                                || (dr == 0 && dc_abs == 1)
                                || (dr == -1 && dc == 0)
                        }
                    } else {
                        // Normal pawn
                        if piece.color == Color::Black {
                            dr == -1 && dc == 0
                        } else {
                            dr == 1 && dc == 0
                        }
                    }
                }

                Lance => {
                    // Lance attacks forward in straight line
                    if piece.promoted {
                        // Promoted lance moves like gold
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1)
                                || (dr == 0 && dc_abs == 1)
                                || (dr == 1 && dc == 0)
                        } else {
                            (dr == 1 && dc_abs <= 1)
                                || (dr == 0 && dc_abs == 1)
                                || (dr == -1 && dc == 0)
                        }
                    } else {
                        // Normal lance - only forward in straight line
                        if piece.color == Color::Black {
                            if dr < 0 && dc == 0 {
                                // Check if path is clear
                                let dr_sign = -1;
                                for i in 1..dr_abs {
                                    let mid_rank = from_sq.rank() as i8 + i * dr_sign;
                                    if let Some(mid_sq) =
                                        Square::new_safe(from_sq.file(), mid_rank as u8)
                                    {
                                        if self.board.piece_on(mid_sq).is_some() {
                                            return false; // Path blocked
                                        }
                                    }
                                }
                                true
                            } else {
                                false
                            }
                        } else if dr > 0 && dc == 0 {
                            // Check if path is clear
                            let dr_sign = 1;
                            for i in 1..dr_abs {
                                let mid_rank = from_sq.rank() as i8 + i * dr_sign;
                                if let Some(mid_sq) =
                                    Square::new_safe(from_sq.file(), mid_rank as u8)
                                {
                                    if self.board.piece_on(mid_sq).is_some() {
                                        return false; // Path blocked
                                    }
                                }
                            }
                            true
                        } else {
                            false
                        }
                    }
                }

                Knight => {
                    // Knight has L-shaped jump
                    if piece.promoted {
                        // Promoted knight moves like gold
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1)
                                || (dr == 0 && dc_abs == 1)
                                || (dr == 1 && dc == 0)
                        } else {
                            (dr == 1 && dc_abs <= 1)
                                || (dr == 0 && dc_abs == 1)
                                || (dr == -1 && dc == 0)
                        }
                    } else {
                        // Normal knight
                        if piece.color == Color::Black {
                            dr == -2 && dc_abs == 1
                        } else {
                            dr == 2 && dc_abs == 1
                        }
                    }
                }

                Silver => {
                    if piece.promoted {
                        // Promoted silver moves like gold
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1)
                                || (dr == 0 && dc_abs == 1)
                                || (dr == 1 && dc == 0)
                        } else {
                            (dr == 1 && dc_abs <= 1)
                                || (dr == 0 && dc_abs == 1)
                                || (dr == -1 && dc == 0)
                        }
                    } else {
                        // Normal silver - diagonals and forward
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1) || (dr == 1 && dc_abs == 1)
                        } else {
                            (dr == 1 && dc_abs <= 1) || (dr == -1 && dc_abs == 1)
                        }
                    }
                }

                Gold => {
                    // Gold general movement
                    if piece.color == Color::Black {
                        (dr == -1 && dc_abs <= 1)
                            || (dr == 0 && dc_abs == 1)
                            || (dr == 1 && dc == 0)
                    } else {
                        (dr == 1 && dc_abs <= 1)
                            || (dr == 0 && dc_abs == 1)
                            || (dr == -1 && dc == 0)
                    }
                }

                Bishop => {
                    // Bishop - diagonal sliding piece
                    if dr_abs == dc_abs && dr_abs > 0 {
                        // Check if path is clear
                        let dr_sign = if dr > 0 { 1 } else { -1 };
                        let dc_sign = if dc > 0 { 1 } else { -1 };

                        for i in 1..dr_abs {
                            let mid_rank = from_sq.rank() as i8 + i * dr_sign;
                            let mid_file = from_sq.file() as i8 + i * dc_sign;
                            if let Some(mid_sq) = Square::new_safe(mid_file as u8, mid_rank as u8) {
                                if self.board.piece_on(mid_sq).is_some() {
                                    return false; // Path blocked
                                }
                            }
                        }
                        true
                    } else if piece.promoted && dr_abs <= 1 && dc_abs <= 1 && (dr != 0 || dc != 0) {
                        // Promoted bishop (horse) can also move one square orthogonally
                        true
                    } else {
                        false
                    }
                }

                Rook => {
                    // Rook - orthogonal sliding piece
                    if (dr == 0 && dc != 0) || (dr != 0 && dc == 0) {
                        // Check if path is clear
                        if dr == 0 {
                            // Horizontal movement
                            let dc_sign = if dc > 0 { 1 } else { -1 };
                            for i in 1..dc_abs {
                                let mid_file = from_sq.file() as i8 + i * dc_sign;
                                if let Some(mid_sq) =
                                    Square::new_safe(mid_file as u8, from_sq.rank())
                                {
                                    if self.board.piece_on(mid_sq).is_some() {
                                        return false; // Path blocked
                                    }
                                }
                            }
                        } else {
                            // Vertical movement
                            let dr_sign = if dr > 0 { 1 } else { -1 };
                            for i in 1..dr_abs {
                                let mid_rank = from_sq.rank() as i8 + i * dr_sign;
                                if let Some(mid_sq) =
                                    Square::new_safe(from_sq.file(), mid_rank as u8)
                                {
                                    if self.board.piece_on(mid_sq).is_some() {
                                        return false; // Path blocked
                                    }
                                }
                            }
                        }
                        true
                    } else if piece.promoted && dr_abs <= 1 && dc_abs <= 1 && (dr != 0 || dc != 0) {
                        // Promoted rook (dragon) can also move one square diagonally
                        true
                    } else {
                        false
                    }
                }
            }
        };

        // Check all squares for pieces of color 'by' that can attack 'sq'
        for rank in 0..9 {
            for file in 0..9 {
                let from_sq = Square::new(file, rank);
                if let Some(piece) = self.board.piece_on(from_sq) {
                    if piece.color == by && can_attack(from_sq, piece, sq) {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Slow but correct version of is_in_check using reference implementation
    pub fn is_in_check_slow(&self) -> bool {
        if let Some(king_sq) = self.board.king_square(self.side_to_move) {
            self.is_square_attacked_by_slow(king_sq, self.side_to_move.opposite())
        } else {
            false
        }
    }

    /// Slow but correct version of gives_check using reference implementation  
    pub fn gives_check_slow(&self, mv: Move) -> bool {
        // Clone position and make move
        let mut tmp = self.clone();

        // Apply move (simplified - just for testing)
        if mv.is_drop() {
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            let piece = crate::shogi::board::Piece::new(piece_type, tmp.side_to_move);
            tmp.board.put_piece(to, piece);
        } else if let Some(from) = mv.from() {
            if let Some(piece) = tmp.board.piece_on(from) {
                // Handle capture
                if tmp.board.piece_on(mv.to()).is_some() {
                    tmp.board.remove_piece(mv.to());
                }

                tmp.board.remove_piece(from);
                let final_piece = if mv.is_promote() {
                    piece.promote()
                } else {
                    piece
                };
                tmp.board.put_piece(mv.to(), final_piece);
            }
        }

        // Rebuild occupancy bitboards
        tmp.board.rebuild_occupancy_bitboards();

        // Find opponent king and check if attacked
        let opponent = tmp.side_to_move.opposite();
        if let Some(opp_king_sq) = tmp.board.king_square(opponent) {
            tmp.is_square_attacked_by_slow(opp_king_sq, tmp.side_to_move)
        } else {
            false
        }
    }
    /// Validate basic move sanity (doesn't check piece movement rules or king safety)
    ///
    /// This function only checks:
    /// - Move is not null
    /// - For drops: destination is empty and piece is in hand
    /// - For normal moves: source has a piece of correct color, destination is valid
    ///
    /// It does NOT check:
    /// - Whether the piece can legally move to the destination
    /// - Whether the path is clear for sliding pieces
    /// - Whether the move leaves the king in check
    ///
    /// For full pseudo-legal validation (including piece movement), use the move generator.
    pub fn is_basic_legal(&self, mv: Move) -> bool {
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

    /// Alias for is_basic_legal() - kept for backward compatibility
    ///
    /// Consider using is_basic_legal() for new code as it better reflects
    /// what this function actually checks.
    pub fn is_pseudo_legal(&self, mv: Move) -> bool {
        self.is_basic_legal(mv)
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
    ///
    /// This implementation uses do_move/undo_move for correctness.
    ///
    /// # Important
    /// This function assumes the move is pseudo-legal (i.e., generated by the move generator).
    /// It does NOT validate:
    /// - Whether the piece can legally move to the destination
    /// - Whether the path is clear for sliding pieces
    /// - Whether the move leaves the king in check
    ///
    /// For arbitrary moves not from the move generator, use `is_pseudo_legal()` first
    /// or expect undefined behavior/panics.
    ///
    /// IMPORTANT: This function must be called with the position BEFORE the move is made.
    /// The function internally applies and undoes the move to check if it results in check.
    ///
    /// # Panics
    /// May panic if the move source doesn't contain a piece of the correct color.
    pub fn gives_check(&self, mv: Move) -> bool {
        debug_assert!(
            !mv.is_drop()
                || mv.from().is_none()
                || self.board.piece_on(mv.from().unwrap()).is_some(),
            "gives_check called with invalid move: source must have a piece"
        );
        // Clone and apply the move using do_move
        let mut tmp = self.clone();
        let undo_info = tmp.do_move(mv);

        // Check if the opponent (now current side) is in check
        let gives = tmp.is_in_check();

        // Undo the move
        tmp.undo_move(mv, undo_info);

        gives
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usi::{parse_sfen, parse_usi_move, parse_usi_square};

    #[test]
    fn test_is_in_check_polarity_smoke() {
        // 白玉(後手)の王を横から黒飛が利かしている単純図
        // 後手玉が9a、先手飛が9hで横利き
        let sfen = "k8/9/9/9/9/9/9/9/R8 w - 1";
        let pos = parse_sfen(sfen).unwrap();

        // ここで「後手番（白）」がチェックされているはず
        // もし is_in_check() が false になるなら極性が逆、もしくは攻撃判定が壊れている
        assert!(pos.is_in_check(), "White (side_to_move) should be in check from black rook");
    }

    #[test]
    fn test_is_in_check_fast_equals_slow() {
        let sfens = [
            "k8/9/9/9/9/9/9/9/R8 w - 1", // White king in check from black rook
            "k8/9/9/9/9/9/9/9/K8 b - 1", // No check
            "9/9/9/9/9/9/9/9/k7R b - 1", // Black king in check from white rook
        ];

        for sfen in &sfens {
            let pos = parse_sfen(sfen).unwrap();
            let fast = pos.is_in_check();
            let slow = pos.is_in_check_slow();
            assert_eq!(fast, slow, "Mismatch for is_in_check on position: {sfen}");
        }
    }

    #[test]
    fn test_gives_check_fast_equals_slow() {
        // Simple position where we know moves that give check
        let sfen = "k8/9/9/9/9/9/9/9/1R6K b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // Debug: Check what's actually at 2i
        let sq_2i = parse_usi_square("2i").unwrap();
        eprintln!("Square 2i index: {}", sq_2i.index());
        eprintln!("Piece at 2i: {:?}", pos.board.piece_on(sq_2i));

        // Also check 8i where we expect the rook
        let sq_8i = parse_usi_square("8i").unwrap();
        eprintln!("Square 8i index: {}", sq_8i.index());
        eprintln!("Piece at 8i: {:?}", pos.board.piece_on(sq_8i));

        // Test a move that gives check: Rook from 8i to 8a (vertical check to king at 9a)
        let check_move = parse_usi_move("8i8a").unwrap();

        let fast = pos.gives_check(check_move);
        let slow = pos.gives_check_slow(check_move);

        // Debug output
        eprintln!("Testing gives_check for move 8i8a");
        eprintln!("Position: {sfen}");
        eprintln!("Fast result: {fast}");
        eprintln!("Slow result: {slow}");

        assert_eq!(fast, slow, "Mismatch for gives_check on move 8i8a");
    }

    #[test]
    fn test_gives_check_normal_move() {
        // Position where Rook move gives check
        let sfen = "8k/9/9/9/9/9/9/R8/K8 b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // Rook moves to give check
        let mv = parse_usi_move("9h1h").unwrap();
        assert!(pos.gives_check(mv), "Rook move should give check");

        // Rook moves forward (no check)
        let mv = parse_usi_move("9h9g").unwrap();
        assert!(!pos.gives_check(mv), "Forward rook move should not give check");
    }

    #[test]
    fn test_gives_check_drop_move() {
        // Position where dropping a lance gives check
        let sfen = "k8/9/9/9/9/9/9/9/K8 b L 1";
        let pos = parse_sfen(sfen).unwrap();

        // Drop lance to give check
        let mv = parse_usi_move("L*9b").unwrap();
        assert!(pos.gives_check(mv), "Lance drop should give check");

        // Drop lance elsewhere (no check)
        let mv = parse_usi_move("L*5e").unwrap();
        assert!(!pos.gives_check(mv), "Lance drop to 5e should not give check");
    }

    #[test]
    fn test_gives_check_promotion() {
        // Position where pawn promotion gives check
        let sfen = "k8/9/P8/9/9/9/9/9/K8 b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // Pawn promotes to tokin and gives check
        let mv = parse_usi_move("9c9b+").unwrap();
        assert!(pos.gives_check(mv), "Pawn promotion should give check");
    }

    #[test]
    fn test_lance_attack_with_obstruction() {
        // Test that lance cannot attack through pieces
        let sfen = "k8/9/9/9/9/9/P8/L8/K8 b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // White king at 9a, Black lance at 9h, Black pawn at 9g blocks the path
        // Lance should NOT be able to attack the king
        assert!(
            !pos.is_square_attacked_by_slow(parse_usi_square("9a").unwrap(), Color::Black),
            "Lance should not attack through the blocking pawn"
        );

        // Test with white lance and black king
        // Create position manually for debugging
        let mut pos2 = Position::empty();

        // Place white lance at 5a
        pos2.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Lance, Color::White));

        // Place white pawn at 5e to block
        pos2.board
            .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Pawn, Color::White));

        // Place black king at 5i
        pos2.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

        pos2.board.rebuild_occupancy_bitboards();

        // White lance at 5a should NOT be able to attack 5i through pawn at 5e
        let can_attack =
            pos2.is_square_attacked_by_slow(parse_usi_square("5i").unwrap(), Color::White);
        assert!(
            !can_attack,
            "White lance at 5a should not attack 5i through blocking pawn at 5e"
        );
    }

    #[test]
    fn test_lance_attack_clear_path() {
        // Test that lance CAN attack when path is clear
        let sfen = "k8/9/9/9/9/9/9/L8/K8 b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // White king at 9a, Black lance at 9h, no obstruction
        assert!(
            pos.is_square_attacked_by_slow(parse_usi_square("9a").unwrap(), Color::Black),
            "Lance should attack when path is clear"
        );

        // Test with white lance
        let sfen2 = "K8/l8/9/9/9/9/9/9/k8 w - 1";
        let pos2 = parse_sfen(sfen2).unwrap();

        // Black king at 9i, White lance at 9b, no obstruction
        assert!(
            pos2.is_square_attacked_by_slow(parse_usi_square("9i").unwrap(), Color::White),
            "White lance should attack when path is clear"
        );
    }

    #[test]
    fn test_promotion_gives_check() {
        // Test various pieces promoting and giving check

        // Silver promotes to give check (gains gold movement)
        let sfen = "3k5/9/3S5/9/9/9/9/9/K8 b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // Silver at 6c promotes to 6b and can now attack king at 6a sideways
        let mv = parse_usi_move("6c6b+").unwrap();
        assert!(pos.gives_check(mv), "Silver promotion should give check with new gold movement");

        // Lance promotes and gains sideways movement
        let sfen2 = "2k6/9/2L6/9/9/9/9/9/K8 b - 1";
        let pos2 = parse_sfen(sfen2).unwrap();

        // Lance at 7c promotes to 7b and can attack king at 8a diagonally
        let mv2 = parse_usi_move("7c7b+").unwrap();
        assert!(pos2.gives_check(mv2), "Lance promotion should give check with gold movement");

        // Knight promotes and gains new movements
        let sfen3 = "3k5/9/2N6/9/9/9/9/9/K8 b - 1";
        let pos3 = parse_sfen(sfen3).unwrap();

        // Knight at 7c promotes to 7b and attacks king at 6a
        let mv3 = parse_usi_move("7c7b+").unwrap();
        assert!(pos3.gives_check(mv3), "Knight promotion should give check with gold movement");
    }

    #[test]
    fn test_promotion_without_check() {
        // Test promotions that don't result in check

        // Silver promotes but doesn't threaten king
        let sfen = "k8/9/9/9/4S4/9/9/9/K8 b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // Silver promotes far from king
        let mv = parse_usi_move("5e5d+").unwrap();
        assert!(!pos.gives_check(mv), "Silver promotion far from king should not give check");
    }
}
