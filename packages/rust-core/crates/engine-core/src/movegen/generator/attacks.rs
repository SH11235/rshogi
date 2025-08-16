//! Attack detection functions

use crate::{shogi::ATTACK_TABLES, Bitboard, Color, PieceType, Square};

use super::core::MoveGenImpl;

/// Get all attackers to a square with custom occupancy
pub(super) fn attackers_to_with_occupancy(
    gen: &MoveGenImpl,
    sq: Square,
    color: Color,
    occupancy: Bitboard,
) -> Bitboard {
    let pieces = gen.pos.board.occupied_bb[color as usize];
    let mut attackers = Bitboard::EMPTY;

    // Pawn
    let pawn_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]
        & !gen.pos.board.promoted_bb;
    let pawn_attacks = ATTACK_TABLES.pawn_attacks(sq, color);
    attackers |= pawn_pieces & pawn_attacks;

    // Gold and promoted pieces
    let gold_attacks = ATTACK_TABLES.gold_attacks(sq, color);
    let gold_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Gold as usize];
    attackers |= gold_pieces & gold_attacks;

    // Promoted pieces that move like gold
    let promoted_pieces = gen.pos.board.promoted_bb & pieces;
    let promoted_gold_movers = promoted_pieces
        & (gen.pos.board.piece_bb[color as usize][PieceType::Silver as usize]
            | gen.pos.board.piece_bb[color as usize][PieceType::Knight as usize]
            | gen.pos.board.piece_bb[color as usize][PieceType::Lance as usize]
            | gen.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]);
    attackers |= promoted_gold_movers & gold_attacks;

    // Silver
    let silver_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Silver as usize]
        & !gen.pos.board.promoted_bb;
    let silver_attacks = ATTACK_TABLES.silver_attacks(sq, color);
    attackers |= silver_pieces & silver_attacks;

    // Knight
    let knight_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Knight as usize]
        & !gen.pos.board.promoted_bb;
    let knight_attacks = ATTACK_TABLES.knight_attacks(sq, color);
    attackers |= knight_pieces & knight_attacks;

    // King
    let king_pieces = gen.pos.board.piece_bb[color as usize][PieceType::King as usize];
    let king_attacks = ATTACK_TABLES.king_attacks(sq);
    attackers |= king_pieces & king_attacks;

    // Rook/Dragon
    let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupancy, PieceType::Rook);
    let rook_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Rook as usize];
    attackers |= rook_attacks & rook_pieces;

    // Bishop/Horse
    let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupancy, PieceType::Bishop);
    let bishop_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Bishop as usize];
    attackers |= bishop_attacks & bishop_pieces;

    // Lance
    let lance_pieces = gen.pos.board.piece_bb[color as usize][PieceType::Lance as usize]
        & !gen.pos.board.promoted_bb;
    // Check each lance individually
    let mut lance_attackers = Bitboard::EMPTY;
    let mut lances = lance_pieces;
    while let Some(lance_sq) = lances.pop_lsb() {
        // Check if lance can attack the square based on direction
        let can_attack = match color {
            Color::Black => lance_sq.rank() > sq.rank() && lance_sq.file() == sq.file(),
            Color::White => lance_sq.rank() < sq.rank() && lance_sq.file() == sq.file(),
        };
        if can_attack {
            // Check if path is clear
            let between = ATTACK_TABLES.between_bb(lance_sq, sq);
            if (between & occupancy).is_empty() {
                lance_attackers.set(lance_sq);
            }
        }
    }
    attackers |= lance_attackers;

    // Promoted rook/bishop king-like moves
    let dragons = rook_pieces & gen.pos.board.promoted_bb;
    let horses = bishop_pieces & gen.pos.board.promoted_bb;
    attackers |= (dragons | horses) & king_attacks;

    attackers
}
