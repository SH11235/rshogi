//! Pin detection functionality
//!
//! This module handles detection of pieces that are pinned to the king.

use crate::shogi::board::{Bitboard, Color, PieceType, Position, Square};
use crate::shogi::{attacks, ATTACK_TABLES};

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
        let _rook_attacks = ATTACK_TABLES.sliding_attacks(king_sq, occupied, PieceType::Rook);
        let rook_xray = ATTACK_TABLES.sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Rook);

        // Find pieces between king and enemy rooks
        let potential_rook_pinners = enemy_rooks & rook_xray;
        let mut pinners_bb = potential_rook_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            // Check if there's exactly one piece between king and pinner
            let between = self.get_between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 {
                blockers |= between;
            }
        }

        // Bishop and Horse pins (diagonal)
        let enemy_bishops = self.board.piece_bb[enemy_color as usize][PieceType::Bishop as usize];
        let _bishop_attacks = ATTACK_TABLES.sliding_attacks(king_sq, occupied, PieceType::Bishop);
        let bishop_xray =
            ATTACK_TABLES.sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Bishop);

        // Find pieces between king and enemy bishops
        let potential_bishop_pinners = enemy_bishops & bishop_xray;
        let mut pinners_bb = potential_bishop_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            // Check if there's exactly one piece between king and pinner
            let between = self.get_between_bb(king_sq, pinner_sq) & occupied;
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
            let lance_ray = if enemy_color == Color::Black {
                // Black lance attacks from below (higher ranks)
                ATTACK_TABLES.lance_rays[Color::White as usize][king_sq.index()]
            } else {
                // White lance attacks from above (lower ranks)
                ATTACK_TABLES.lance_rays[Color::Black as usize][king_sq.index()]
            };

            // Find the closest lance that can attack the king
            let potential_pinners = lances_in_file & lance_ray;
            if let Some(lance_sq) = potential_pinners.lsb() {
                // Use pre-computed between bitboard
                let between = ATTACK_TABLES.between_bb(king_sq, lance_sq) & occupied;
                if between.count_ones() == 1 {
                    blockers |= between;
                }
            }
        }

        blockers
    }

    /// Get a bitboard of all squares between two aligned squares
    ///
    /// Returns the squares strictly between sq1 and sq2 (not including the endpoints).
    /// If the squares are not aligned (horizontally, vertically, or diagonally),
    /// returns an empty bitboard.
    ///
    /// # Arguments
    /// * `sq1` - First square
    /// * `sq2` - Second square
    ///
    /// # Returns
    /// Bitboard containing all squares between the two given squares
    pub(crate) fn get_between_bb(&self, sq1: Square, sq2: Square) -> Bitboard {
        let mut between = Bitboard::EMPTY;

        let file1 = sq1.file() as i8;
        let rank1 = sq1.rank() as i8;
        let file2 = sq2.file() as i8;
        let rank2 = sq2.rank() as i8;

        let file_diff = file2 - file1;
        let rank_diff = rank2 - rank1;

        // Check if squares are aligned
        if file_diff == 0 || rank_diff == 0 || file_diff.abs() == rank_diff.abs() {
            let file_step = file_diff.signum();
            let rank_step = rank_diff.signum();

            let mut file = file1 + file_step;
            let mut rank = rank1 + rank_step;

            while file != file2 || rank != rank2 {
                between.set(Square::new(file as u8, rank as u8));
                file += file_step;
                rank += rank_step;
            }
        }

        between
    }
}
