//! Pin detection functionality
//!
//! This module handles detection of pieces that are pinned to the king.

use crate::shogi::attacks;
use crate::shogi::board::{Bitboard, Color, PieceType, Position};

impl Position {
    /// Get blockers for king (simplified version)
    /// Returns a bitboard of pieces that are pinned to the king
    pub fn get_blockers_for_king(&self, king_color: Color) -> Bitboard {
        let king_bb = self.board.piece_bb[king_color as usize][PieceType::King as usize];
        let king_sq = match king_bb.lsb() {
            Some(sq) => sq,
            None => {
                // This should never happen in a valid position
                log::error!(
                    "King not found for color {king_color:?} in get_blockers_for_king - data inconsistency"
                );
                return Bitboard::EMPTY;
            }
        };

        let enemy_color = king_color.opposite();
        let occupied = self.board.all_bb;
        let mut blockers = Bitboard::EMPTY;

        // Check for pins by sliding pieces (rook, bishop, lance)

        // Rook and Dragon pins (horizontal and vertical)
        let enemy_rooks = self.board.piece_bb[enemy_color as usize][PieceType::Rook as usize];
        let rook_xray = attacks::sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Rook);

        // Find pieces between king and enemy rooks
        let potential_rook_pinners = enemy_rooks & rook_xray;
        let mut pinners_bb = potential_rook_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            // Check if there's exactly one piece between king and pinner
            let between = attacks::between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 {
                blockers |= between;
            }
        }

        // Bishop and Horse pins (diagonal)
        let enemy_bishops = self.board.piece_bb[enemy_color as usize][PieceType::Bishop as usize];
        let bishop_xray = attacks::sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Bishop);

        // Find pieces between king and enemy bishops
        let potential_bishop_pinners = enemy_bishops & bishop_xray;
        let mut pinners_bb = potential_bishop_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            // Check if there's exactly one piece between king and pinner
            let between = attacks::between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 {
                blockers |= between;
            }
        }

        // Lance pins (vertical only)
        let enemy_lances = self.board.piece_bb[enemy_color as usize][PieceType::Lance as usize]
            & !self.board.promoted_bb;

        // Use file mask to get lances in the same file
        let file_mask = attacks::file_mask(king_sq.file());
        let lances_in_file = enemy_lances & file_mask;

        if !lances_in_file.is_empty() {
            // Get the ray from king in the direction of enemy lance attacks
            let lance_ray = attacks::lance_ray_from(king_sq, enemy_color.opposite());

            // Find the closest lance that can attack the king
            let potential_pinners = lances_in_file & lance_ray;
            if let Some(lance_sq) = potential_pinners.lsb() {
                // Use pre-computed between bitboard
                let between = attacks::between_bb(king_sq, lance_sq) & occupied;
                if between.count_ones() == 1 {
                    blockers |= between;
                }
            }
        }

        blockers
    }
}
