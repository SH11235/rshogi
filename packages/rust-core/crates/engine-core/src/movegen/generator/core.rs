//! Core move generation implementation

use crate::{
    shogi::{MoveList, ALL_PIECE_TYPES},
    Bitboard, Color, PieceType, Position, Square,
};

/// Move generator implementation
pub struct MoveGenImpl<'a> {
    pub(super) pos: &'a Position,
    pub(super) moves: MoveList,
    pub(super) king_sq: Square,
    pub checkers: Bitboard,
    pub(super) non_king_check_mask: Bitboard, // 非玉の通常手用
    pub(super) drop_block_mask: Bitboard,     // 持駒打ち用（ブロックのみ）
    pub(super) pinned: Bitboard,
    pub(super) pin_rays: [Bitboard; 81],
}

impl<'a> MoveGenImpl<'a> {
    /// Create new move generator for position
    pub fn new(pos: &'a Position) -> Self {
        let us = pos.side_to_move;
        let king_sq = pos.board.king_square(us).expect("King must exist");

        let mut gen = MoveGenImpl {
            pos,
            moves: MoveList::new(),
            king_sq,
            checkers: Bitboard::EMPTY,
            non_king_check_mask: Bitboard::ALL, // 0チェック時は制約なし
            drop_block_mask: Bitboard::ALL,     // 0チェック時は制約なし
            pinned: Bitboard::EMPTY,
            pin_rays: [Bitboard::EMPTY; 81],
        };

        // Calculate checkers and pins
        gen.calculate_checkers_and_pins();

        gen
    }

    /// Helper to get captured piece type at a square
    #[inline]
    pub(super) fn get_captured_type(&self, to: Square) -> Option<PieceType> {
        self.pos.board.piece_on(to).map(|p| p.piece_type)
    }

    /// Check if a piece at the given square is a sliding piece
    /// Dragons and horses are considered sliding pieces for blocking purposes
    #[inline]
    pub(super) fn is_sliding_piece(&self, sq: Square) -> bool {
        if let Some(piece) = self.pos.board.piece_on(sq) {
            matches!(piece.piece_type, PieceType::Rook | PieceType::Bishop | PieceType::Lance)
        } else {
            false
        }
    }

    /// Generate all legal moves
    pub fn generate_all(&mut self) -> MoveList {
        self.moves.clear();

        let us = self.pos.side_to_move;
        let them = us.opposite();
        let _our_pieces = self.pos.board.occupied_bb[us as usize];
        let _their_pieces = self.pos.board.occupied_bb[them as usize];
        let _all_pieces = self.pos.board.all_bb;

        // If in double check, only king moves are legal
        if self.checkers.count_ones() > 1 {
            self.generate_king_moves();
            return std::mem::take(&mut self.moves);
        }

        // Generate king moves first (always needed)
        self.generate_king_moves();

        // Generate moves for other piece types
        for &piece_type_enum in &ALL_PIECE_TYPES {
            if piece_type_enum == PieceType::King {
                continue; // Already generated
            }
            let piece_type = piece_type_enum as usize;
            let mut pieces = self.pos.board.piece_bb[us as usize][piece_type];

            while let Some(from) = pieces.pop_lsb() {
                // Check if the piece is promoted
                let piece = self.pos.board.piece_on(from);
                let promoted = piece.map(|p| p.promoted).unwrap_or(false);

                match piece_type_enum {
                    PieceType::Rook => self.generate_sliding_moves(from, piece_type_enum, promoted),
                    PieceType::Bishop => {
                        self.generate_sliding_moves(from, piece_type_enum, promoted)
                    }
                    PieceType::Gold => self.generate_gold_moves(from, promoted),
                    PieceType::Silver => self.generate_silver_moves(from, promoted),
                    PieceType::Knight => self.generate_knight_moves(from, promoted),
                    PieceType::Lance => self.generate_lance_moves(from, promoted),
                    PieceType::Pawn => self.generate_pawn_moves(from, promoted),
                    _ => unreachable!("King moves already generated"),
                }
            }
        }

        // Generate drop moves
        // When in check, drops can still be legal if they block the check
        self.generate_drop_moves();

        // Note: promoted pieces are already handled in the piece-specific methods

        // Filter out any moves that would capture the enemy king (should not happen)
        let enemy_king_bb = self.pos.board.piece_bb[them as usize][PieceType::King as usize];
        if let Some(enemy_king_sq) = enemy_king_bb.lsb() {
            self.moves.as_mut_vec().retain(|m| m.to() != enemy_king_sq);
        }

        std::mem::take(&mut self.moves)
    }

    /// Check if there is any legal move from the current position
    /// Returns true as soon as the first legal move is found (early exit optimization)
    pub fn has_any_legal_move(&mut self) -> bool {
        let us = self.pos.side_to_move;
        let them = us.opposite();

        // If in double check, only king moves are legal
        if self.checkers.count_ones() > 1 {
            // Check king moves only
            self.generate_king_moves();
            return !self.moves.is_empty();
        }

        // Check king moves first (always needed)
        self.generate_king_moves();
        if !self.moves.is_empty() {
            return true;
        }

        // Check moves for other piece types
        for &piece_type_enum in &ALL_PIECE_TYPES {
            if piece_type_enum == PieceType::King {
                continue; // Already checked
            }
            let piece_type = piece_type_enum as usize;
            let mut pieces = self.pos.board.piece_bb[us as usize][piece_type];

            while let Some(from) = pieces.pop_lsb() {
                // Check if the piece is promoted
                let piece = self.pos.board.piece_on(from);
                let promoted = piece.map(|p| p.promoted).unwrap_or(false);

                // Clear moves before generating (since we check after each piece)
                self.moves.clear();

                match piece_type_enum {
                    PieceType::Rook => self.generate_sliding_moves(from, piece_type_enum, promoted),
                    PieceType::Bishop => {
                        self.generate_sliding_moves(from, piece_type_enum, promoted)
                    }
                    PieceType::Gold => self.generate_gold_moves(from, promoted),
                    PieceType::Silver => self.generate_silver_moves(from, promoted),
                    PieceType::Knight => self.generate_knight_moves(from, promoted),
                    PieceType::Lance => self.generate_lance_moves(from, promoted),
                    PieceType::Pawn => self.generate_pawn_moves(from, promoted),
                    _ => unreachable!("King moves already checked"),
                }

                // Filter out any moves that would capture the enemy king
                let enemy_king_bb =
                    self.pos.board.piece_bb[them as usize][PieceType::King as usize];
                if let Some(enemy_king_sq) = enemy_king_bb.lsb() {
                    self.moves.as_mut_vec().retain(|m| m.to() != enemy_king_sq);
                }

                if !self.moves.is_empty() {
                    return true;
                }
            }
        }

        // Check drop moves
        self.moves.clear();
        self.generate_drop_moves();

        // Filter out any drop moves that would capture the enemy king (should not happen)
        let enemy_king_bb = self.pos.board.piece_bb[them as usize][PieceType::King as usize];
        if let Some(enemy_king_sq) = enemy_king_bb.lsb() {
            self.moves.as_mut_vec().retain(|m| m.to() != enemy_king_sq);
        }

        !self.moves.is_empty()
    }
}

// Forward declarations for methods implemented in other modules
impl<'a> MoveGenImpl<'a> {
    // From pieces.rs
    pub(super) fn generate_king_moves(&mut self) {
        super::pieces::generate_king_moves(self);
    }

    pub(super) fn generate_gold_moves(&mut self, from: Square, promoted: bool) {
        super::pieces::generate_gold_moves(self, from, promoted);
    }

    pub(super) fn generate_silver_moves(&mut self, from: Square, promoted: bool) {
        super::pieces::generate_silver_moves(self, from, promoted);
    }

    pub(super) fn generate_knight_moves(&mut self, from: Square, promoted: bool) {
        super::pieces::generate_knight_moves(self, from, promoted);
    }

    pub(super) fn generate_pawn_moves(&mut self, from: Square, promoted: bool) {
        super::pieces::generate_pawn_moves(self, from, promoted);
    }

    // From sliding.rs
    pub(super) fn generate_sliding_moves(
        &mut self,
        from: Square,
        piece_type: PieceType,
        promoted: bool,
    ) {
        super::sliding::generate_sliding_moves(self, from, piece_type, promoted);
    }

    pub(super) fn generate_lance_moves(&mut self, from: Square, promoted: bool) {
        super::sliding::generate_lance_moves(self, from, promoted);
    }

    // From drops.rs
    pub(super) fn generate_drop_moves(&mut self) {
        super::drops::generate_drop_moves(self);
    }

    // Expose is_drop_pawn_mate for tests
    #[cfg(test)]
    pub fn is_drop_pawn_mate(&self, to: Square, them: Color) -> bool {
        super::drops::is_drop_pawn_mate(self, to, them)
    }

    // From checks.rs
    pub(super) fn calculate_checkers_and_pins(&mut self) {
        super::checks::calculate_checkers_and_pins(self);
    }

    pub(super) fn would_be_in_check(&self, from: Square, to: Square) -> bool {
        super::checks::would_be_in_check(self, from, to)
    }

    pub(super) fn attackers_to_with_occupancy(
        &self,
        sq: Square,
        color: Color,
        occupancy: Bitboard,
    ) -> Bitboard {
        super::attacks::attackers_to_with_occupancy(self, sq, color, occupancy)
    }
}
