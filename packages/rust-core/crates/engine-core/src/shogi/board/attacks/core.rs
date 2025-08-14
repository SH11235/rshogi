//! Core attack detection functionality
//!
//! This module provides the main attack detection methods that combine
//! non-sliding and sliding piece attacks.

use super::{non_sliding, sliding};
use crate::shogi::board::{Bitboard, Color, PieceType, Position, Square};

impl Position {
    /// Check if specific color is in check
    pub fn is_check(&self, color: Color) -> bool {
        let king_bb = self.board.piece_bb[color as usize][PieceType::King as usize];
        if let Some(king_sq) = king_bb.lsb() {
            self.is_attacked(king_sq, color.opposite())
        } else {
            false
        }
    }

    /// Check if a square is attacked by a given color
    pub fn is_attacked(&self, sq: Square, by_color: Color) -> bool {
        // Check non-sliding piece attacks first (usually faster)
        if non_sliding::check_non_sliding_attacks(
            sq,
            by_color,
            &self.board.piece_bb,
            self.board.promoted_bb,
        ) {
            return true;
        }

        // Check sliding piece attacks
        sliding::check_sliding_attacks(
            sq,
            by_color,
            &self.board.piece_bb,
            self.board.promoted_bb,
            self.board.all_bb,
            |sq, color, lance_bb, occupied| {
                self.get_lance_attackers_to(sq, color, lance_bb, occupied)
            },
        )
    }

    /// Get all pieces of a given color attacking a square
    /// Returns a bitboard with all attacking pieces
    pub fn get_attackers_to(&self, sq: Square, by_color: Color) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;

        // Get non-sliding attackers
        attackers |= non_sliding::get_non_sliding_attackers(
            sq,
            by_color,
            &self.board.piece_bb,
            self.board.promoted_bb,
        );

        // Get sliding attackers
        attackers |= sliding::get_sliding_attackers(
            sq,
            by_color,
            &self.board.piece_bb,
            self.board.promoted_bb,
            self.board.all_bb,
            |sq, color, lance_bb, occupied| {
                self.get_lance_attackers_to(sq, color, lance_bb, occupied)
            },
        );

        attackers
    }
}
