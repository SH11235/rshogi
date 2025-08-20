//! Non-sliding piece attack detection
//!
//! This module handles attack detection for pieces that don't slide:
//! - Pawn
//! - Knight  
//! - King
//! - Gold
//! - Silver

use crate::shogi::attacks;
use crate::shogi::board::{Bitboard, Color, PieceType, Square};

/// Check for non-sliding piece attacks to a square
pub fn check_non_sliding_attacks(
    sq: Square,
    by_color: Color,
    piece_bb: &[[Bitboard; 8]; 2],
    promoted_bb: Bitboard,
) -> bool {
    // Check pawn attacks
    let pawn_bb = piece_bb[by_color as usize][PieceType::Pawn as usize];
    let pawn_attacks = attacks::pawn_attacks(sq, by_color.opposite());
    if !(pawn_bb & pawn_attacks).is_empty() {
        return true;
    }

    // Check knight attacks
    let knight_bb = piece_bb[by_color as usize][PieceType::Knight as usize];
    let knight_attacks = attacks::knight_attacks(sq, by_color.opposite());
    if !(knight_bb & knight_attacks).is_empty() {
        return true;
    }

    // Check king attacks
    let king_bb = piece_bb[by_color as usize][PieceType::King as usize];
    let king_attacks = attacks::king_attacks(sq);
    if !(king_bb & king_attacks).is_empty() {
        return true;
    }

    // Check gold attacks
    let gold_bb = piece_bb[by_color as usize][PieceType::Gold as usize];
    let gold_attacks = attacks::gold_attacks(sq, by_color.opposite());
    if !(gold_bb & gold_attacks).is_empty() {
        return true;
    }

    // Check silver attacks (unpromoted only)
    let silver_bb = piece_bb[by_color as usize][PieceType::Silver as usize] & !promoted_bb;
    let silver_attacks = attacks::silver_attacks(sq, by_color.opposite());
    if !(silver_bb & silver_attacks).is_empty() {
        return true;
    }

    // Check promoted pieces that move like gold
    let tokin_bb = pawn_bb & promoted_bb;
    let promoted_lance_bb = piece_bb[by_color as usize][PieceType::Lance as usize] & promoted_bb;
    let promoted_knight_bb = knight_bb & promoted_bb;
    let promoted_silver_bb = piece_bb[by_color as usize][PieceType::Silver as usize] & promoted_bb;

    if !((tokin_bb | promoted_lance_bb | promoted_knight_bb | promoted_silver_bb) & gold_attacks)
        .is_empty()
    {
        return true;
    }

    false
}

/// Get non-sliding piece attackers to a square
pub fn get_non_sliding_attackers(
    sq: Square,
    by_color: Color,
    piece_bb: &[[Bitboard; 8]; 2],
    promoted_bb: Bitboard,
) -> Bitboard {
    let mut attackers = Bitboard::EMPTY;

    // Check pawn attacks
    let pawn_bb = piece_bb[by_color as usize][PieceType::Pawn as usize];
    let pawn_attacks = attacks::pawn_attacks(sq, by_color.opposite());
    attackers |= pawn_bb & pawn_attacks;

    // Check knight attacks
    let knight_bb = piece_bb[by_color as usize][PieceType::Knight as usize];
    let knight_attacks = attacks::knight_attacks(sq, by_color.opposite());
    attackers |= knight_bb & knight_attacks;

    // Check king attacks
    let king_bb = piece_bb[by_color as usize][PieceType::King as usize];
    let king_attacks = attacks::king_attacks(sq);
    attackers |= king_bb & king_attacks;

    // Check gold attacks (including promoted pieces that move like gold)
    let gold_bb = piece_bb[by_color as usize][PieceType::Gold as usize];
    let gold_attacks = attacks::gold_attacks(sq, by_color.opposite());
    attackers |= gold_bb & gold_attacks;

    // Check promoted pawns, lances, knights, and silvers (they move like gold)
    let tokin_bb = pawn_bb & promoted_bb;
    let promoted_lance_bb = piece_bb[by_color as usize][PieceType::Lance as usize] & promoted_bb;
    let promoted_knight_bb = knight_bb & promoted_bb;
    let promoted_silver_bb = piece_bb[by_color as usize][PieceType::Silver as usize] & promoted_bb;
    attackers |=
        (tokin_bb | promoted_lance_bb | promoted_knight_bb | promoted_silver_bb) & gold_attacks;

    // Check silver attacks (unpromoted only)
    let silver_bb = piece_bb[by_color as usize][PieceType::Silver as usize] & !promoted_bb;
    let silver_attacks = attacks::silver_attacks(sq, by_color.opposite());
    attackers |= silver_bb & silver_attacks;

    attackers
}
