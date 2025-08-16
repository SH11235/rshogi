//! Utility functions for move generation

use crate::{
    shogi::{attacks, Move},
    Bitboard, Color, PieceType, Square,
};

use super::core::MoveGenImpl;

/// Check if a piece must promote when moving to a certain square
#[inline]
fn must_promote(piece: PieceType, to: Square, color: Color) -> bool {
    match (color, piece) {
        (Color::Black, PieceType::Pawn | PieceType::Lance) => to.rank() == 0,
        (Color::Black, PieceType::Knight) => to.rank() <= 1,
        (Color::White, PieceType::Pawn | PieceType::Lance) => to.rank() == 8,
        (Color::White, PieceType::Knight) => to.rank() >= 7,
        _ => false,
    }
}

impl<'a> MoveGenImpl<'a> {
    /// Add moves from a square to target squares
    pub(super) fn add_moves(&mut self, from: Square, targets: Bitboard, _promoted: bool) {
        // Get piece type from the board
        let piece = self.pos.board.piece_on(from).expect("Piece must exist at from square");
        let piece_type = piece.piece_type;
        self.add_moves_with_type(from, targets, piece_type);
    }

    /// Add a single move from a square to a target square
    pub(super) fn add_single_move(&mut self, from: Square, to: Square, piece_type: PieceType) {
        // Get the piece to check if it's already promoted
        let piece = self.pos.board.piece_on(from).expect("Piece must exist at from square");
        let is_promoted = piece.promoted;
        let us = self.pos.side_to_move;
        let captured_type = self.get_captured_type(to);

        // Check if the piece must promote
        let must = must_promote(piece_type, to, us);

        // Check if the piece can promote (not already promoted and can promote based on rules)
        let may = !is_promoted
            && self.can_promote(from, to, us)
            && matches!(
                piece_type,
                PieceType::Rook
                    | PieceType::Bishop
                    | PieceType::Silver
                    | PieceType::Knight
                    | PieceType::Lance
                    | PieceType::Pawn
            );

        if must {
            // Only add promoted move if must promote
            self.moves
                .push(Move::normal_with_piece(from, to, true, piece_type, captured_type));
        } else {
            // Always add non-promotion move
            self.moves
                .push(Move::normal_with_piece(from, to, false, piece_type, captured_type));

            // Add promotion move if piece can promote
            if may {
                self.moves
                    .push(Move::normal_with_piece(from, to, true, piece_type, captured_type));
            }
        }
    }

    /// Add moves from a square to target squares with known piece type
    pub(super) fn add_moves_with_type(
        &mut self,
        from: Square,
        mut targets: Bitboard,
        piece_type: PieceType,
    ) {
        // If we're in check, only allow moves that block or capture checker
        if !self.checkers.is_empty() {
            // Double check - only king moves are legal
            if self.checkers.count_ones() >= 2 {
                return; // No non-King moves allowed
            }
            // Single check - only allow block/capture
            let checker_sq = self.checkers.lsb().unwrap();
            let block_squares = attacks::between_bb(checker_sq, self.king_sq) | self.checkers;
            targets &= block_squares;
        }

        // If piece is pinned, only allow moves along pin ray
        if self.pinned.test(from) {
            targets &= self.pin_rays[from.index()];
        }

        // Never allow capturing enemy king (should not happen in legal shogi)
        let them = self.pos.side_to_move.opposite();
        let enemy_king_bb = self.pos.board.piece_bb[them as usize][PieceType::King as usize];
        targets &= !enemy_king_bb;

        // Get the piece to check if it's already promoted
        let piece = self.pos.board.piece_on(from).expect("Piece must exist at from square");
        let is_promoted = piece.promoted;
        let us = self.pos.side_to_move;

        while let Some(to) = targets.pop_lsb() {
            let captured_type = self.get_captured_type(to);

            // Check if the piece must promote
            let must = must_promote(piece_type, to, us);

            // Check if the piece can promote (not already promoted and can promote based on rules)
            let may = !is_promoted
                && self.can_promote(from, to, us)
                && matches!(
                    piece_type,
                    PieceType::Rook
                        | PieceType::Bishop
                        | PieceType::Silver
                        | PieceType::Knight
                        | PieceType::Lance
                        | PieceType::Pawn
                );

            if must {
                // Only add promoted move if must promote
                self.moves
                    .push(Move::normal_with_piece(from, to, true, piece_type, captured_type));
            } else {
                // Always add non-promotion move
                self.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    piece_type,
                    captured_type,
                ));

                // Add promotion move if piece can promote
                if may {
                    self.moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        piece_type,
                        captured_type,
                    ));
                }
            }
        }
    }

    /// Check if a piece can promote
    pub(super) fn can_promote(&self, from: Square, to: Square, color: Color) -> bool {
        // A piece can promote if it's moving from or to the promotion zone
        // Promotion zone is the opponent's last 3 ranks
        match color {
            Color::Black => from.rank() <= 2 || to.rank() <= 2, // Ranks 0,1,2 are Black's promotion zone
            Color::White => from.rank() >= 6 || to.rank() >= 6, // Ranks 6,7,8 are White's promotion zone
        }
    }

    /// Check if a piece at given square is a sliding piece
    /// Note: Dragon (promoted Rook) and Horse (promoted Bishop) are considered sliding pieces
    /// even though they can make adjacent moves. This is correct because between_bb returns
    /// empty for adjacent squares, making drop blocking logic work properly in both cases.
    pub(super) fn is_sliding_piece(&self, sq: Square) -> bool {
        if let Some(piece) = self.pos.board.piece_on(sq) {
            matches!(piece.piece_type, PieceType::Rook | PieceType::Bishop | PieceType::Lance)
                || (piece.promoted
                    && matches!(piece.piece_type, PieceType::Rook | PieceType::Bishop))
        } else {
            false
        }
    }

    /// Check if two squares are aligned for rook movement
    pub(super) fn is_aligned_rook(&self, sq1: Square, sq2: Square) -> bool {
        sq1.file() == sq2.file() || sq1.rank() == sq2.rank()
    }

    /// Check if two squares are aligned for bishop movement
    pub(super) fn is_aligned_bishop(&self, sq1: Square, sq2: Square) -> bool {
        let file_diff = (sq1.file() as i8 - sq2.file() as i8).abs();
        let rank_diff = (sq1.rank() as i8 - sq2.rank() as i8).abs();
        file_diff == rank_diff && file_diff != 0
    }
}
