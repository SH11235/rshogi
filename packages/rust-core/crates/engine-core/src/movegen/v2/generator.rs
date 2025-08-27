use crate::shogi::{Bitboard, Color, PieceType, Square, Position};
use crate::shogi::moves::Move;

use super::error::MoveGenError;
use super::movelist::MoveList;
use super::tables;

/// Move generator for generating legal moves
pub struct MoveGenerator;

impl MoveGenerator {
    /// Create a new move generator
    pub const fn new() -> Self {
        Self
    }

    /// Generate all legal moves for the given position
    pub fn generate_all(&self, pos: &Position) -> Result<MoveList, MoveGenError> {
        let mut gen = MoveGenImpl::new(pos)?;
        Ok(gen.generate_all())
    }

    /// Check if any legal move exists (early exit optimization)
    pub fn has_legal_moves(&self, pos: &Position) -> Result<bool, MoveGenError> {
        let mut gen = MoveGenImpl::new(pos)?;
        Ok(gen.has_any_legal_move())
    }

    /// Generate only capture moves
    pub fn generate_captures(&self, pos: &Position) -> Result<MoveList, MoveGenError> {
        let mut gen = MoveGenImpl::new(pos)?;
        Ok(gen.generate_captures())
    }

    /// Generate only non-capture moves
    pub fn generate_quiet(&self, pos: &Position) -> Result<MoveList, MoveGenError> {
        let mut gen = MoveGenImpl::new(pos)?;
        Ok(gen.generate_quiet())
    }
}

/// Internal move generation implementation
struct MoveGenImpl<'a> {
    pos: &'a Position,
    king_sq: Square,
    checkers: Bitboard,
    pinned: Bitboard,
    us: Color,
    them: Color,
    our_pieces: Bitboard,
    their_pieces: Bitboard,
    occupied: Bitboard,
}

impl<'a> MoveGenImpl<'a> {
    /// Create a new move generation context
    fn new(pos: &'a Position) -> Result<Self, MoveGenError> {
        let us = pos.side_to_move;
        let them = us.opposite();
        
        // Find king square
        let king_sq = pos.board.king_square(us)
            .ok_or(MoveGenError::KingNotFound(us))?;

        let our_pieces = pos.board.occupied_bb[us as usize];
        let their_pieces = pos.board.occupied_bb[them as usize];
        let occupied = pos.board.all_bb;

        // Calculate checkers and pinned pieces
        let (checkers, pinned) = calculate_pins_and_checkers(pos, king_sq, us);

        Ok(Self {
            pos,
            king_sq,
            checkers,
            pinned,
            us,
            them,
            our_pieces,
            their_pieces,
            occupied,
        })
    }

    /// Generate all legal moves
    fn generate_all(&mut self) -> MoveList {
        let mut moves = MoveList::new();

        // If in double check, only king moves are legal
        if self.checkers.count_ones() > 1 {
            self.generate_king_moves(&mut moves);
            return moves;
        }

        // Generate moves for all piece types
        self.generate_king_moves(&mut moves);
        self.generate_piece_moves(&mut moves);
        
        // Generate drop moves if not in check or single check
        if self.checkers.is_empty() || self.checkers.count_ones() == 1 {
            self.generate_drop_moves(&mut moves);
        }

        moves
    }

    /// Check if any legal move exists
    fn has_any_legal_move(&mut self) -> bool {
        // If in double check, only king moves are possible
        if self.checkers.count_ones() > 1 {
            return self.has_king_escape();
        }

        // Check king moves first (most likely to have moves)
        if self.has_king_escape() {
            return true;
        }

        // Check if any piece can block or capture checker
        if self.has_piece_move() {
            return true;
        }

        // Check drop moves
        if (self.checkers.is_empty() || self.checkers.count_ones() == 1) && self.has_drop_move() {
            return true;
        }

        false
    }

    /// Generate only capture moves
    fn generate_captures(&mut self) -> MoveList {
        let mut moves = MoveList::new();
        
        // Generate captures for all pieces
        self.generate_king_captures(&mut moves);
        self.generate_piece_captures(&mut moves);
        
        moves
    }

    /// Generate only quiet (non-capture) moves
    fn generate_quiet(&mut self) -> MoveList {
        let mut moves = MoveList::new();
        
        // Generate non-captures for all pieces
        self.generate_king_quiet(&mut moves);
        self.generate_piece_quiet(&mut moves);
        
        // Drops are always quiet
        if self.checkers.is_empty() || self.checkers.count_ones() == 1 {
            self.generate_drop_moves(&mut moves);
        }
        
        moves
    }

    /// Generate king moves
    fn generate_king_moves(&mut self, moves: &mut MoveList) {
        let attacks = tables::king_attacks(self.king_sq);
        let valid_targets = attacks & !self.our_pieces;

        for to_sq in valid_targets {
            // Check if king would be safe on this square
            if !self.is_attacked_by(to_sq, self.them) {
                let piece = self.pos.board.piece_on(self.king_sq).unwrap();
                let captured_type = self.pos.board.piece_on(to_sq).map(|p| p.piece_type);
                let mv = Move::normal_with_piece(self.king_sq, to_sq, false, piece.piece_type, captured_type);
                moves.push(mv);
            }
        }
    }

    /// Generate moves for non-king pieces
    fn generate_piece_moves(&mut self, _moves: &mut MoveList) {
        // TODO: Implement piece move generation
        // This is a placeholder - actual implementation would generate moves
        // for all piece types (pawn, lance, knight, silver, gold, bishop, rook)
    }

    /// Generate drop moves
    fn generate_drop_moves(&mut self, _moves: &mut MoveList) {
        // TODO: Implement drop move generation
        // This is a placeholder - actual implementation would check
        // pieces in hand and generate legal drop moves
    }

    /// Generate only king captures
    fn generate_king_captures(&mut self, moves: &mut MoveList) {
        let attacks = tables::king_attacks(self.king_sq);
        let captures = attacks & self.their_pieces;

        for to_sq in captures {
            if !self.is_attacked_by(to_sq, self.them) {
                let piece = self.pos.board.piece_on(self.king_sq).unwrap();
                let captured_piece = self.pos.board.piece_on(to_sq).unwrap();
                let mv = Move::normal_with_piece(self.king_sq, to_sq, false, piece.piece_type, Some(captured_piece.piece_type));
                moves.push(mv);
            }
        }
    }

    /// Generate only king quiet moves
    fn generate_king_quiet(&mut self, moves: &mut MoveList) {
        let attacks = tables::king_attacks(self.king_sq);
        let quiet = attacks & !self.occupied;

        for to_sq in quiet {
            if !self.is_attacked_by(to_sq, self.them) {
                let piece = self.pos.board.piece_on(self.king_sq).unwrap();
                let mv = Move::normal_with_piece(self.king_sq, to_sq, false, piece.piece_type, None);
                moves.push(mv);
            }
        }
    }

    /// Generate captures for non-king pieces
    fn generate_piece_captures(&mut self, _moves: &mut MoveList) {
        // TODO: Implement
    }

    /// Generate quiet moves for non-king pieces
    fn generate_piece_quiet(&mut self, _moves: &mut MoveList) {
        // TODO: Implement
    }

    /// Check if king has any escape move
    fn has_king_escape(&self) -> bool {
        let attacks = tables::king_attacks(self.king_sq);
        let valid_targets = attacks & !self.our_pieces;

        for to_sq in valid_targets {
            if !self.is_attacked_by(to_sq, self.them) {
                return true;
            }
        }

        false
    }

    /// Check if any piece can move
    fn has_piece_move(&self) -> bool {
        // TODO: Implement - check if any piece has a legal move
        false
    }

    /// Check if any drop move is possible
    fn has_drop_move(&self) -> bool {
        // TODO: Implement - check if any piece can be dropped
        false
    }

    /// Check if a square is attacked by the given side
    fn is_attacked_by(&self, sq: Square, by: Color) -> bool {
        // TODO: Implement attack detection
        // This would check all possible attackers
        false
    }
}

/// Calculate pinned pieces and checkers
fn calculate_pins_and_checkers(pos: &Position, king_sq: Square, us: Color) -> (Bitboard, Bitboard) {
    // TODO: Implement pin and checker calculation
    // This is a placeholder implementation
    (Bitboard::EMPTY, Bitboard::EMPTY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_generator_creation() {
        let gen = MoveGenerator::new();
        let pos = Position::default();
        
        // Should be able to generate moves for starting position
        let result = gen.generate_all(&pos);
        assert!(result.is_ok());
        
        let moves = result.unwrap();
        // Starting position should have legal moves
        assert!(!moves.is_empty());
    }
}