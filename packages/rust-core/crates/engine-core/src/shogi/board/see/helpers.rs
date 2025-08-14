//! Helper functions for SEE calculations
//!
//! This module contains utility functions for attacker detection,
//! piece value calculations, and x-ray attack updates.

use crate::shogi::board::{Bitboard, Color, Piece, PieceType, Position, Square};
use crate::shogi::piece_constants::SEE_PIECE_VALUES;
use crate::shogi::ATTACK_TABLES;

use super::pin_info::SeePinInfo;

impl Position {
    /// Get piece value for SEE calculation
    #[inline]
    pub(super) fn see_piece_value(piece: Piece) -> i32 {
        SEE_PIECE_VALUES[piece.promoted as usize][piece.piece_type as usize]
    }

    /// Get base piece type value for SEE
    #[inline]
    pub(super) fn see_piece_type_value(piece_type: PieceType) -> i32 {
        SEE_PIECE_VALUES[0][piece_type as usize]
    }

    /// Get promoted piece value for SEE
    #[inline]
    pub(super) fn see_promoted_piece_value(piece_type: PieceType) -> i32 {
        SEE_PIECE_VALUES[1][piece_type as usize]
    }

    /// SEE用の軽量なピン計算（両陣営分）
    pub(super) fn calculate_pins_for_see(&self) -> (SeePinInfo, SeePinInfo) {
        let black_pins = self.calculate_pins_for_color(Color::Black);
        let white_pins = self.calculate_pins_for_color(Color::White);
        (black_pins, white_pins)
    }

    /// 特定色のピン計算
    pub(super) fn calculate_pins_for_color(&self, color: Color) -> SeePinInfo {
        // ピンが存在しない場合の早期リターン最適化
        let king_bb = self.board.piece_bb[color as usize][PieceType::King as usize];
        let king_sq = match king_bb.lsb() {
            Some(sq) => sq,
            None => return SeePinInfo::empty(),
        };

        let mut pin_info = SeePinInfo::empty();
        let enemy = color.opposite();
        let occupied = self.board.all_bb;
        let our_pieces = self.board.occupied_bb[color as usize];

        // 敵のスライダー駒を取得
        let enemy_rooks = self.board.piece_bb[enemy as usize][PieceType::Rook as usize];
        let enemy_bishops = self.board.piece_bb[enemy as usize][PieceType::Bishop as usize];
        let enemy_lances = self.board.piece_bb[enemy as usize][PieceType::Lance as usize]
            & !self.board.promoted_bb;

        // 飛車・竜による縦横のピン
        let rook_xray = ATTACK_TABLES.sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Rook);
        let potential_rook_pinners = enemy_rooks & rook_xray;

        let mut pinners_bb = potential_rook_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            let between = ATTACK_TABLES.between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 && (between & our_pieces).count_ones() == 1 {
                let pinned_sq =
                    between.lsb().expect("Between squares must have at least one square");
                pin_info.pinned.set(pinned_sq);

                // ピンの方向を判定
                if king_sq.file() == pinner_sq.file() {
                    pin_info.vertical_pins.set(pinned_sq);
                } else {
                    pin_info.horizontal_pins.set(pinned_sq);
                }
            }
        }

        // 角・馬による斜めのピン
        let bishop_xray =
            ATTACK_TABLES.sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Bishop);
        let potential_bishop_pinners = enemy_bishops & bishop_xray;

        pinners_bb = potential_bishop_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            let between = ATTACK_TABLES.between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 && (between & our_pieces).count_ones() == 1 {
                let pinned_sq =
                    between.lsb().expect("Between squares must have at least one square");
                pin_info.pinned.set(pinned_sq);

                // ピンの方向を判定
                let file_diff = king_sq.file() as i8 - pinner_sq.file() as i8;
                let rank_diff = king_sq.rank() as i8 - pinner_sq.rank() as i8;

                if file_diff == rank_diff {
                    pin_info.diag_ne_pins.set(pinned_sq);
                } else {
                    pin_info.diag_nw_pins.set(pinned_sq);
                }
            }
        }

        // 香車による縦のピン（特殊処理）
        let file_mask = ATTACK_TABLES.file_masks[king_sq.file() as usize];
        let lances_in_file = enemy_lances & file_mask;

        if !lances_in_file.is_empty() {
            // 香車は方向性があるので、敵香車の位置と王の位置関係を確認
            let mut lance_bb = lances_in_file;
            while let Some(lance_sq) = lance_bb.pop_lsb() {
                let can_attack = match enemy {
                    Color::Black => lance_sq.rank() < king_sq.rank(),
                    Color::White => lance_sq.rank() > king_sq.rank(),
                };

                if can_attack {
                    let between = ATTACK_TABLES.between_bb(lance_sq, king_sq) & occupied;
                    if between.count_ones() == 1 && (between & our_pieces).count_ones() == 1 {
                        let pinned_sq =
                            between.lsb().expect("Between squares must have at least one square");
                        pin_info.pinned.set(pinned_sq);
                        pin_info.vertical_pins.set(pinned_sq);
                    }
                }
            }
        }

        pin_info
    }

    /// Get all attackers to a square (both colors)
    pub(super) fn get_all_attackers_to(&self, sq: Square, occupied: Bitboard) -> Bitboard {
        // Get attackers from both colors with current occupancy
        let black_attackers = self.get_attackers_to_with_occupancy(sq, Color::Black, occupied);
        let white_attackers = self.get_attackers_to_with_occupancy(sq, Color::White, occupied);
        black_attackers | white_attackers
    }

    /// Get attackers to a square with custom occupancy (for X-ray detection)
    pub(super) fn get_attackers_to_with_occupancy(
        &self,
        sq: Square,
        by_color: Color,
        occupied: Bitboard,
    ) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;

        // Check pawn attacks
        let pawn_bb = self.board.piece_bb[by_color as usize][PieceType::Pawn as usize];
        let pawn_attacks = ATTACK_TABLES.pawn_attacks(sq, by_color.opposite());
        attackers |= pawn_bb & pawn_attacks;

        // Check knight attacks
        let knight_bb = self.board.piece_bb[by_color as usize][PieceType::Knight as usize];
        let knight_attacks = ATTACK_TABLES.knight_attacks(sq, by_color.opposite());
        attackers |= knight_bb & knight_attacks;

        // Check king attacks
        let king_bb = self.board.piece_bb[by_color as usize][PieceType::King as usize];
        let king_attacks = ATTACK_TABLES.king_attacks(sq);
        attackers |= king_bb & king_attacks;

        // Check gold attacks (including promoted pieces that move like gold)
        let gold_bb = self.board.piece_bb[by_color as usize][PieceType::Gold as usize];
        let gold_attacks = ATTACK_TABLES.gold_attacks(sq, by_color.opposite());
        attackers |= gold_bb & gold_attacks;

        // Check promoted pawns, lances, knights, and silvers (they move like gold)
        let promoted_bb = self.board.promoted_bb;
        let tokin_bb = pawn_bb & promoted_bb;
        let promoted_lance_bb =
            self.board.piece_bb[by_color as usize][PieceType::Lance as usize] & promoted_bb;
        let promoted_knight_bb = knight_bb & promoted_bb;
        let promoted_silver_bb =
            self.board.piece_bb[by_color as usize][PieceType::Silver as usize] & promoted_bb;
        attackers |=
            (tokin_bb | promoted_lance_bb | promoted_knight_bb | promoted_silver_bb) & gold_attacks;

        // Check silver attacks (non-promoted)
        let silver_bb =
            self.board.piece_bb[by_color as usize][PieceType::Silver as usize] & !promoted_bb;
        let silver_attacks = ATTACK_TABLES.silver_attacks(sq, by_color.opposite());
        attackers |= silver_bb & silver_attacks;

        // Check sliding attacks with custom occupancy
        let rook_bb = self.board.piece_bb[by_color as usize][PieceType::Rook as usize];
        let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        attackers |= rook_bb & rook_attacks;

        let bishop_bb =
            self.board.piece_bb[by_color as usize][PieceType::Bishop as usize] & occupied;
        let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
        attackers |= bishop_bb & bishop_attacks;

        // Check lance attacks with custom occupancy
        let lance_bb = (self.board.piece_bb[by_color as usize][PieceType::Lance as usize]
            & occupied)
            & !promoted_bb;
        attackers |= self.get_lance_attackers_to_with_occupancy(sq, by_color, lance_bb, occupied);

        attackers
    }

    /// Get lance attackers with custom occupancy
    pub(super) fn get_lance_attackers_to_with_occupancy(
        &self,
        sq: Square,
        by_color: Color,
        lance_bb: Bitboard,
        occupied: Bitboard,
    ) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;
        let file = sq.file();

        // Get all lances in the same file
        let file_mask = ATTACK_TABLES.file_masks[file as usize];
        let lances_in_file = lance_bb & file_mask;

        if lances_in_file.is_empty() {
            return attackers;
        }

        // Get potential lance attackers using pre-computed rays
        let lance_ray = ATTACK_TABLES.lance_rays[by_color.opposite() as usize][sq.index()];
        let potential_attackers = lances_in_file & lance_ray;

        // Check each potential attacker for blockers
        let mut lances = potential_attackers;
        while !lances.is_empty() {
            let from = lances.pop_lsb().expect("Lance bitboard should not be empty");

            // Use pre-computed between bitboard
            let between = ATTACK_TABLES.between_bb(from, sq);
            if (between & occupied).is_empty() {
                // Path is clear, lance can attack
                attackers.set(from);
            }
        }

        attackers
    }

    /// Pop the least valuable attacker considering pin constraints
    pub(super) fn pop_least_valuable_attacker_with_pins(
        &self,
        attackers: &mut Bitboard,
        occupied: Bitboard,
        color: Color,
        to: Square,
        pin_info: &SeePinInfo,
    ) -> Option<(Square, PieceType, i32)> {
        // Check pieces in order of increasing value
        for piece_type in [
            PieceType::Pawn,
            PieceType::Lance,
            PieceType::Knight,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
        ] {
            // Only consider pieces that are still on the board
            let pieces =
                self.board.piece_bb[color as usize][piece_type as usize] & *attackers & occupied;

            // For each potential attacker of this type
            let mut candidates = pieces;
            while let Some(sq) = candidates.pop_lsb() {
                // Check if this piece can legally move to the target square considering pins
                if pin_info.can_move(sq, to) {
                    attackers.clear(sq);

                    // Check if piece is promoted
                    let is_promoted = self.board.promoted_bb.test(sq);
                    let value = if is_promoted {
                        Self::see_promoted_piece_value(piece_type)
                    } else {
                        Self::see_piece_type_value(piece_type)
                    };

                    return Some((sq, piece_type, value));
                }
            }
        }

        // King should not normally participate in exchanges, but check anyway
        let king_bb =
            self.board.piece_bb[color as usize][PieceType::King as usize] & *attackers & occupied;
        if let Some(sq) = king_bb.lsb() {
            // Kings are never pinned
            attackers.clear(sq);
            return Some((sq, PieceType::King, Self::see_piece_type_value(PieceType::King)));
        }

        None
    }

    /// Update X-ray attacks after removing a piece
    pub(super) fn update_xray_attacks(
        &self,
        from: Square,
        to: Square,
        attackers: &mut Bitboard,
        occupied: Bitboard,
    ) {
        // Check if there's a clear line between from and to
        let between = ATTACK_TABLES.between_bb(from, to);
        if between.is_empty() {
            return; // Not aligned, no x-rays possible
        }

        // Check for rook/dragon x-rays (orthogonal)
        if from.file() == to.file() || from.rank() == to.rank() {
            let rook_attackers = (self.board.piece_bb[Color::Black as usize]
                [PieceType::Rook as usize]
                | self.board.piece_bb[Color::White as usize][PieceType::Rook as usize])
                & occupied;
            let rook_attacks = ATTACK_TABLES.sliding_attacks(to, occupied, PieceType::Rook);
            *attackers |= rook_attackers & rook_attacks;
        }

        // Check for bishop/horse x-rays (diagonal)
        if (from.file() as i8 - to.file() as i8).abs()
            == (from.rank() as i8 - to.rank() as i8).abs()
        {
            let bishop_attackers = (self.board.piece_bb[Color::Black as usize]
                [PieceType::Bishop as usize]
                | self.board.piece_bb[Color::White as usize][PieceType::Bishop as usize])
                & occupied;
            let bishop_attacks = ATTACK_TABLES.sliding_attacks(to, occupied, PieceType::Bishop);
            *attackers |= bishop_attackers & bishop_attacks;
        }

        // Check for lance x-rays (vertical only)
        if from.file() == to.file() {
            // Black lances attack upward (towards rank 8)
            if from.rank() < to.rank() {
                let black_lances = self.board.piece_bb[Color::Black as usize]
                    [PieceType::Lance as usize]
                    & occupied;
                // Find black lances that can reach the target
                let mut lance_candidates = black_lances;
                while let Some(lance_sq) = lance_candidates.lsb() {
                    lance_candidates.clear(lance_sq);
                    if lance_sq.file() == to.file() && lance_sq.rank() < to.rank() {
                        // Check if path is clear
                        let between_lance = ATTACK_TABLES.between_bb(lance_sq, to);
                        if (between_lance & occupied).is_empty() {
                            attackers.set(lance_sq);
                        }
                    }
                }
            }
            // White lances attack downward (towards rank 0)
            else if from.rank() > to.rank() {
                let white_lances = self.board.piece_bb[Color::White as usize]
                    [PieceType::Lance as usize]
                    & occupied;
                // Find white lances that can reach the target
                let mut lance_candidates = white_lances;
                while let Some(lance_sq) = lance_candidates.lsb() {
                    lance_candidates.clear(lance_sq);
                    if lance_sq.file() == to.file() && lance_sq.rank() > to.rank() {
                        // Check if path is clear
                        let between_lance = ATTACK_TABLES.between_bb(lance_sq, to);
                        if (between_lance & occupied).is_empty() {
                            attackers.set(lance_sq);
                        }
                    }
                }
            }
        }
    }

    /// Estimate maximum remaining value that can be captured
    /// Returns the total value of all remaining attacking pieces
    #[inline]
    pub(super) fn estimate_max_remaining_value(
        &self,
        attackers: &Bitboard,
        stm: Color,
        threshold: i32,
        current_eval: i32,
    ) -> i32 {
        // Extract only attacking pieces for the side to move
        let mut bb = *attackers & self.board.occupied_bb[stm as usize];

        // Total value of all remaining attackers
        let mut total = 0;

        while let Some(sq) = bb.pop_lsb() {
            // Get actual piece value including promotion status
            if let Some(piece) = self.board.piece_on(sq) {
                total += Self::see_piece_value(piece);

                // Early termination if threshold is already exceeded
                if current_eval + total >= threshold {
                    break;
                }
            }
        }

        total
    }
}
