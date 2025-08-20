//! Attack detection functions

use crate::{shogi::attacks, Bitboard, Color, PieceType, Square};

use super::core::MoveGenImpl;

/// Get all attackers to a square with custom occupancy
pub(super) fn attackers_to_with_occupancy(
    gen: &MoveGenImpl,
    sq: Square,
    color: Color,
    occupancy: Bitboard,
) -> Bitboard {
    // Only consider pieces that exist in the given occupancy
    let pieces = gen.pos.board.occupied_bb[color as usize] & occupancy;

    // Early return if no pieces exist
    if pieces.is_empty() {
        return Bitboard::EMPTY;
    }

    let mut attackers = Bitboard::EMPTY;

    // Pawn - Check which pawns can attack sq
    let pawn_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]
        & !gen.pos.board.promoted_bb
        & pieces;
    // For each pawn, check if it can attack sq
    let mut pawn_copy = pawn_pieces;
    while let Some(pawn_sq) = pawn_copy.pop_lsb() {
        let pawn_attacks = attacks::pawn_attacks(pawn_sq, color);
        if pawn_attacks.test(sq) {
            attackers.set(pawn_sq);
        }
    }

    // Gold - Check which golds can attack sq
    let gold_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Gold as usize] & pieces;
    let mut gold_copy = gold_pieces;
    while let Some(gold_sq) = gold_copy.pop_lsb() {
        let gold_attacks = attacks::gold_attacks(gold_sq, color);
        if gold_attacks.test(sq) {
            attackers.set(gold_sq);
        }
    }

    // Promoted pieces that move like gold
    let promoted_pieces = gen.pos.board.promoted_bb & pieces;
    let promoted_gold_movers = promoted_pieces
        & (gen.pos.board.piece_bb[color as usize][PieceType::Silver as usize]
            | gen.pos.board.piece_bb[color as usize][PieceType::Knight as usize]
            | gen.pos.board.piece_bb[color as usize][PieceType::Lance as usize]
            | gen.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]);
    let mut promoted_copy = promoted_gold_movers;
    while let Some(promoted_sq) = promoted_copy.pop_lsb() {
        let gold_attacks = attacks::gold_attacks(promoted_sq, color);
        if gold_attacks.test(sq) {
            attackers.set(promoted_sq);
        }
    }

    // Silver
    let silver_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Silver as usize]
        & !gen.pos.board.promoted_bb
        & pieces;
    let mut silver_copy = silver_pieces;
    while let Some(silver_sq) = silver_copy.pop_lsb() {
        let silver_attacks = attacks::silver_attacks(silver_sq, color);
        if silver_attacks.test(sq) {
            attackers.set(silver_sq);
        }
    }

    // Knight
    let knight_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Knight as usize]
        & !gen.pos.board.promoted_bb
        & pieces;
    let mut knight_copy = knight_pieces;
    while let Some(knight_sq) = knight_copy.pop_lsb() {
        let knight_attacks = attacks::knight_attacks(knight_sq, color);
        if knight_attacks.test(sq) {
            attackers.set(knight_sq);
        }
    }

    // King
    let king_pieces = gen.pos.board.piece_bb[color as usize][PieceType::King as usize] & pieces;
    let mut king_copy = king_pieces;
    while let Some(king_sq) = king_copy.pop_lsb() {
        let king_attacks = attacks::king_attacks(king_sq);
        if king_attacks.test(sq) {
            attackers.set(king_sq);
        }
    }

    // Rook/Dragon
    let rook_attacks = attacks::sliding_attacks(sq, occupancy, PieceType::Rook);
    let rook_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Rook as usize] & pieces;
    attackers |= rook_attacks & rook_pieces;

    // Bishop/Horse
    let bishop_attacks = attacks::sliding_attacks(sq, occupancy, PieceType::Bishop);
    let bishop_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Bishop as usize] & pieces;
    attackers |= bishop_attacks & bishop_pieces;

    // Lance - Can only attack forward (up for Black, down for White)
    let lance_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Lance as usize]
        & !gen.pos.board.promoted_bb
        & pieces;
    // Check each lance individually
    let mut lance_attackers = Bitboard::EMPTY;
    let mut lances = lance_pieces;
    while let Some(lance_sq) = lances.pop_lsb() {
        // Check if lance can attack the square based on direction
        // Black lance attacks upward (higher rank), White lance attacks downward (lower rank)
        let can_attack = match color {
            Color::Black => lance_sq.rank() > sq.rank() && lance_sq.file() == sq.file(),
            Color::White => lance_sq.rank() < sq.rank() && lance_sq.file() == sq.file(),
        };
        if can_attack {
            // Check if path is clear
            let between = attacks::between_bb(lance_sq, sq);
            if (between & occupancy).is_empty() {
                lance_attackers.set(lance_sq);
            }
        }
    }
    attackers |= lance_attackers;

    // Promoted rook/bishop king-like moves
    // Dragon (promoted rook) and Horse (promoted bishop) have both their original sliding attacks
    // AND additional king-like (adjacent square) attacks
    let dragons = rook_pieces & gen.pos.board.promoted_bb;
    let horses = bishop_pieces & gen.pos.board.promoted_bb;
    let promoted_rb = dragons | horses;
    let mut promoted_rb_copy = promoted_rb;
    while let Some(prb_sq) = promoted_rb_copy.pop_lsb() {
        let king_attacks = attacks::king_attacks(prb_sq);
        if king_attacks.test(sq) {
            attackers.set(prb_sq);
        }
    }

    attackers
}
