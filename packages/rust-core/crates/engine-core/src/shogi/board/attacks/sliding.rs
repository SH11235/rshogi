//! Sliding piece attack detection
//!
//! This module handles attack detection for sliding pieces:
//! - Rook (and Dragon)
//! - Bishop (and Horse)
//! - Lance

use crate::shogi::board::{Bitboard, Color, PieceType, Position, Square};
use crate::shogi::{attacks, ATTACK_TABLES};

impl Position {
    /// Get lance attackers to a square using optimized bitboard operations
    pub(crate) fn get_lance_attackers_to(
        &self,
        sq: Square,
        by_color: Color,
        lance_bb: Bitboard,
        occupied: Bitboard,
    ) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;
        let file = sq.file();

        // Get all lances in the same file
        let file_mask = attacks::file_mask(file);
        let lances_in_file = lance_bb & file_mask;

        if lances_in_file.is_empty() {
            return attackers;
        }

        // Get potential lance attackers using pre-computed rays
        // Note: We use the opposite color because lance_rays[color][sq] gives squares a lance can ATTACK from sq,
        // but we want squares that can attack sq
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
}

/// Check for sliding piece attacks to a square
pub fn check_sliding_attacks(
    sq: Square,
    by_color: Color,
    piece_bb: &[[Bitboard; 8]; 2],
    promoted_bb: Bitboard,
    occupied: Bitboard,
    get_lance_attackers: impl Fn(Square, Color, Bitboard, Bitboard) -> Bitboard,
) -> bool {
    // Rook attacks
    let rook_bb = piece_bb[by_color as usize][PieceType::Rook as usize];
    let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
    if !(rook_bb & rook_attacks).is_empty() {
        return true;
    }

    // Bishop attacks
    let bishop_bb = piece_bb[by_color as usize][PieceType::Bishop as usize];
    let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
    if !(bishop_bb & bishop_attacks).is_empty() {
        return true;
    }

    // Lance attacks
    let lance_bb = piece_bb[by_color as usize][PieceType::Lance as usize] & !promoted_bb;
    let lance_attackers = get_lance_attackers(sq, by_color, lance_bb, occupied);
    if !lance_attackers.is_empty() {
        return true;
    }

    false
}

/// Get sliding piece attackers to a square
pub fn get_sliding_attackers(
    sq: Square,
    by_color: Color,
    piece_bb: &[[Bitboard; 8]; 2],
    promoted_bb: Bitboard,
    occupied: Bitboard,
    get_lance_attackers: impl Fn(Square, Color, Bitboard, Bitboard) -> Bitboard,
) -> Bitboard {
    let mut attackers = Bitboard::EMPTY;
    let king_attacks = ATTACK_TABLES.king_attacks(sq);

    // Rook attacks (including dragon)
    let rook_bb = piece_bb[by_color as usize][PieceType::Rook as usize];
    let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
    attackers |= rook_bb & rook_attacks;

    // Dragon (promoted rook) also has king moves
    let dragon_bb = rook_bb & promoted_bb;
    attackers |= dragon_bb & king_attacks;

    // Bishop attacks (including horse)
    let bishop_bb = piece_bb[by_color as usize][PieceType::Bishop as usize];
    let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
    attackers |= bishop_bb & bishop_attacks;

    // Horse (promoted bishop) also has king moves
    let horse_bb = bishop_bb & promoted_bb;
    attackers |= horse_bb & king_attacks;

    // Lance attacks (only unpromoted, as promoted lance moves like gold)
    let lance_bb = piece_bb[by_color as usize][PieceType::Lance as usize] & !promoted_bb;
    attackers |= get_lance_attackers(sq, by_color, lance_bb, occupied);

    attackers
}
