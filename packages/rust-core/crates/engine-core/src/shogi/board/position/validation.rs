//! Move validation and position query methods
//!
//! This module provides methods for validating moves and querying
//! the position state (check, repetition, draw, etc).

use crate::shogi::board::{Color, Piece, PieceType, Position, Square};
use crate::shogi::moves::Move;
use crate::shogi::piece_constants::piece_type_to_hand_index;

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
                            (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                        } else {
                            (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                        }
                    } else {
                        // Normal pawn
                        if piece.color == Color::Black {
                            dr == -1 && dc == 0
                        } else {
                            dr == 1 && dc == 0
                        }
                    }
                },
                
                Lance => {
                    // Lance attacks forward in straight line
                    if piece.promoted {
                        // Promoted lance moves like gold
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                        } else {
                            (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                        }
                    } else {
                        // Normal lance - only forward
                        if piece.color == Color::Black {
                            dr < 0 && dc == 0
                        } else {
                            dr > 0 && dc == 0
                        }
                    }
                },
                
                Knight => {
                    // Knight has L-shaped jump
                    if piece.promoted {
                        // Promoted knight moves like gold
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                        } else {
                            (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                        }
                    } else {
                        // Normal knight
                        if piece.color == Color::Black {
                            dr == -2 && dc_abs == 1
                        } else {
                            dr == 2 && dc_abs == 1
                        }
                    }
                },
                
                Silver => {
                    if piece.promoted {
                        // Promoted silver moves like gold
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                        } else {
                            (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                        }
                    } else {
                        // Normal silver - diagonals and forward
                        if piece.color == Color::Black {
                            (dr == -1 && dc_abs <= 1) || (dr == 1 && dc_abs == 1)
                        } else {
                            (dr == 1 && dc_abs <= 1) || (dr == -1 && dc_abs == 1)
                        }
                    }
                },
                
                Gold => {
                    // Gold general movement
                    if piece.color == Color::Black {
                        (dr == -1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == 1 && dc == 0)
                    } else {
                        (dr == 1 && dc_abs <= 1) || (dr == 0 && dc_abs == 1) || (dr == -1 && dc == 0)
                    }
                },
                
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
                    } else if piece.promoted && dr_abs <= 1 && dc_abs <= 1 {
                        // Promoted bishop (horse) can also move one square orthogonally
                        true
                    } else {
                        false
                    }
                },
                
                Rook => {
                    // Rook - orthogonal sliding piece
                    if (dr == 0 && dc != 0) || (dr != 0 && dc == 0) {
                        // Check if path is clear
                        if dr == 0 {
                            // Horizontal movement
                            let dc_sign = if dc > 0 { 1 } else { -1 };
                            for i in 1..dc_abs {
                                let mid_file = from_sq.file() as i8 + i * dc_sign;
                                if let Some(mid_sq) = Square::new_safe(mid_file as u8, from_sq.rank()) {
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
                                if let Some(mid_sq) = Square::new_safe(from_sq.file(), mid_rank as u8) {
                                    if self.board.piece_on(mid_sq).is_some() {
                                        return false; // Path blocked
                                    }
                                }
                            }
                        }
                        true
                    } else if piece.promoted && dr_abs <= 1 && dc_abs <= 1 {
                        // Promoted rook (dragon) can also move one square diagonally
                        true
                    } else {
                        false
                    }
                },
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
        } else {
            if let Some(from) = mv.from() {
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

            // Handle capture if any (before removing from source)
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

            // Explicitly remove piece from destination square if occupied
            if tmp.board.piece_on(to).is_some() {
                tmp.board.remove_piece(to);
            }

            // Remove piece from source
            tmp.board.remove_piece(from);

            // Place piece at destination (with promotion if specified)
            let final_piece = if mv.is_promote() {
                piece.promote()
            } else {
                piece
            };
            tmp.board.put_piece(to, final_piece);
        }

        // Rebuild occupancy bitboards after board modifications
        tmp.board.rebuild_occupancy_bitboards();

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
            assert_eq!(fast, slow, "Mismatch for is_in_check on position: {}", sfen);
        }
    }

    #[test]
    fn test_gives_check_fast_equals_slow() {
        // Simple position where we know moves that give check
        let sfen = "k8/9/9/9/9/9/9/9/1R6K b - 1";
        let pos = parse_sfen(sfen).unwrap();
        
        // Test a move that gives check: Rook from 2i to 2a (horizontal check)
        let check_move = parse_usi_move("2i2a").unwrap();
        
        let fast = pos.gives_check(check_move);
        let slow = pos.gives_check_slow(check_move);
        
        // Debug output
        eprintln!("Testing gives_check for move 2i2a");
        eprintln!("Position: {}", sfen);
        eprintln!("Fast result: {}", fast);
        eprintln!("Slow result: {}", slow);
        
        assert_eq!(fast, slow, "Mismatch for gives_check on move 2i2a");
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
}
