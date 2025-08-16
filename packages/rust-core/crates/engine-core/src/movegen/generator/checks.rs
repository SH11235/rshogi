//! Check and pin calculation

use crate::{
    shogi::{attacks, ATTACK_TABLES},
    Bitboard, Color, PieceType, Square,
};

use super::core::MoveGenImpl;

/// Calculate checkers and pinned pieces
pub(super) fn calculate_checkers_and_pins(gen: &mut MoveGenImpl) {
    let us = gen.pos.side_to_move;
    let them = us.opposite();
    let king_sq = gen.king_sq;
    let our_pieces = gen.pos.board.occupied_bb[us as usize];
    let _their_pieces = gen.pos.board.occupied_bb[them as usize];

    // Reset
    gen.checkers = Bitboard::EMPTY;
    gen.pinned = Bitboard::EMPTY;
    gen.pin_rays = [Bitboard::EMPTY; 81];

    // Check attacks from each enemy piece type

    // Pawn checks
    let enemy_pawns = gen.pos.board.piece_bb[them as usize][PieceType::Pawn as usize]
        & !gen.pos.board.promoted_bb;
    let pawn_attacks = ATTACK_TABLES.pawn_attacks(king_sq, them);
    gen.checkers |= enemy_pawns & pawn_attacks;

    // Knight checks
    let enemy_knights = gen.pos.board.piece_bb[them as usize][PieceType::Knight as usize]
        & !gen.pos.board.promoted_bb;
    let knight_attacks = ATTACK_TABLES.knight_attacks(king_sq, them);
    gen.checkers |= enemy_knights & knight_attacks;

    // Gold/promoted pieces checks
    let gold_attacks = ATTACK_TABLES.gold_attacks(king_sq, them);
    let enemy_golds = gen.pos.board.piece_bb[them as usize][PieceType::Gold as usize];
    gen.checkers |= enemy_golds & gold_attacks;

    // Check promoted pieces that move like gold
    let promoted_silvers = gen.pos.board.piece_bb[them as usize][PieceType::Silver as usize]
        & gen.pos.board.promoted_bb;
    let promoted_knights = gen.pos.board.piece_bb[them as usize][PieceType::Knight as usize]
        & gen.pos.board.promoted_bb;
    let promoted_lances = gen.pos.board.piece_bb[them as usize][PieceType::Lance as usize]
        & gen.pos.board.promoted_bb;
    let promoted_pawns =
        gen.pos.board.piece_bb[them as usize][PieceType::Pawn as usize] & gen.pos.board.promoted_bb;
    gen.checkers |=
        (promoted_silvers | promoted_knights | promoted_lances | promoted_pawns) & gold_attacks;

    // Silver checks
    let enemy_silvers = gen.pos.board.piece_bb[them as usize][PieceType::Silver as usize]
        & !gen.pos.board.promoted_bb;
    let silver_attacks = ATTACK_TABLES.silver_attacks(king_sq, them);
    gen.checkers |= enemy_silvers & silver_attacks;

    // Lance checks and pins
    let enemy_lances = gen.pos.board.piece_bb[them as usize][PieceType::Lance as usize]
        & !gen.pos.board.promoted_bb;
    let mut lance_bb = enemy_lances;
    while let Some(lance_sq) = lance_bb.pop_lsb() {
        // Check if lance can attack in the direction of king
        let can_attack = match them {
            Color::Black => lance_sq.rank() > king_sq.rank() && lance_sq.file() == king_sq.file(),
            Color::White => lance_sq.rank() < king_sq.rank() && lance_sq.file() == king_sq.file(),
        };

        if can_attack {
            let between = attacks::between_bb(lance_sq, king_sq);
            let blockers = between & gen.pos.board.all_bb;

            if blockers.is_empty() {
                gen.checkers.set(lance_sq);
            } else if blockers.count_ones() == 1 {
                let blocker_sq = blockers.lsb().unwrap();
                if our_pieces.test(blocker_sq) {
                    gen.pinned.set(blocker_sq);
                    gen.pin_rays[blocker_sq.index()] = between | Bitboard::from_square(lance_sq);
                }
            }
        }
    }

    // Sliding pieces (Rook/Bishop) checks and pins
    let enemy_rooks = gen.pos.board.piece_bb[them as usize][PieceType::Rook as usize];
    let enemy_bishops = gen.pos.board.piece_bb[them as usize][PieceType::Bishop as usize];

    // Dragon (promoted rook) moves like rook + king
    let dragons = enemy_rooks & gen.pos.board.promoted_bb;
    let dragon_king_attacks = ATTACK_TABLES.king_attacks(king_sq);
    gen.checkers |= dragons & dragon_king_attacks;

    // Horse (promoted bishop) moves like bishop + king
    let horses = enemy_bishops & gen.pos.board.promoted_bb;
    gen.checkers |= horses & dragon_king_attacks;

    // Check rook/dragon sliding attacks and pins
    let mut rook_bb = enemy_rooks;
    while let Some(rook_sq) = rook_bb.pop_lsb() {
        if gen.is_aligned_rook(rook_sq, king_sq) {
            let between = attacks::between_bb(rook_sq, king_sq);
            let blockers = between & gen.pos.board.all_bb;

            if blockers.is_empty() {
                gen.checkers.set(rook_sq);
            } else if blockers.count_ones() == 1 {
                let blocker_sq = blockers.lsb().unwrap();
                if our_pieces.test(blocker_sq) {
                    gen.pinned.set(blocker_sq);
                    gen.pin_rays[blocker_sq.index()] = between | Bitboard::from_square(rook_sq);
                }
            }
        }
    }

    // Check bishop/horse sliding attacks and pins
    let mut bishop_bb = enemy_bishops;
    while let Some(bishop_sq) = bishop_bb.pop_lsb() {
        if gen.is_aligned_bishop(bishop_sq, king_sq) {
            let between = attacks::between_bb(bishop_sq, king_sq);
            let blockers = between & gen.pos.board.all_bb;

            if blockers.is_empty() {
                gen.checkers.set(bishop_sq);
            } else if blockers.count_ones() == 1 {
                let blocker_sq = blockers.lsb().unwrap();
                if our_pieces.test(blocker_sq) {
                    gen.pinned.set(blocker_sq);
                    gen.pin_rays[blocker_sq.index()] = between | Bitboard::from_square(bishop_sq);
                }
            }
        }
    }
}

/// Check if a king move would put the king in check
pub(super) fn would_be_in_check(gen: &MoveGenImpl, from: Square, to: Square) -> bool {
    let us = gen.pos.side_to_move;
    let them = us.opposite();

    // Create occupancy after the move
    let mut occupancy_after = gen.pos.board.all_bb;
    occupancy_after.clear(from);
    occupancy_after.set(to);

    // Check all enemy pieces for attacks to the new king position

    // Pawn checks
    let enemy_pawns = gen.pos.board.piece_bb[them as usize][PieceType::Pawn as usize]
        & !gen.pos.board.promoted_bb;
    let pawn_attacks = ATTACK_TABLES.pawn_attacks(to, them);
    if !(enemy_pawns & pawn_attacks).is_empty() {
        return true;
    }

    // Knight checks
    let enemy_knights = gen.pos.board.piece_bb[them as usize][PieceType::Knight as usize]
        & !gen.pos.board.promoted_bb;
    let knight_attacks = ATTACK_TABLES.knight_attacks(to, them);
    if !(enemy_knights & knight_attacks).is_empty() {
        return true;
    }

    // Gold and promoted piece checks
    let gold_attacks = ATTACK_TABLES.gold_attacks(to, them);
    let enemy_golds = gen.pos.board.piece_bb[them as usize][PieceType::Gold as usize];
    if !(enemy_golds & gold_attacks).is_empty() {
        return true;
    }

    let promoted_pieces = gen.pos.board.promoted_bb & gen.pos.board.occupied_bb[them as usize];
    let promoted_gold_movers = promoted_pieces
        & (gen.pos.board.piece_bb[them as usize][PieceType::Silver as usize]
            | gen.pos.board.piece_bb[them as usize][PieceType::Knight as usize]
            | gen.pos.board.piece_bb[them as usize][PieceType::Lance as usize]
            | gen.pos.board.piece_bb[them as usize][PieceType::Pawn as usize]);
    if !(promoted_gold_movers & gold_attacks).is_empty() {
        return true;
    }

    // Silver checks
    let enemy_silvers = gen.pos.board.piece_bb[them as usize][PieceType::Silver as usize]
        & !gen.pos.board.promoted_bb;
    let silver_attacks = ATTACK_TABLES.silver_attacks(to, them);
    if !(enemy_silvers & silver_attacks).is_empty() {
        return true;
    }

    // King checks
    let enemy_king = gen.pos.board.piece_bb[them as usize][PieceType::King as usize];
    let king_attacks = ATTACK_TABLES.king_attacks(to);
    if !(enemy_king & king_attacks).is_empty() {
        return true;
    }

    // Promoted rook/bishop checks (king-like moves)
    let dragons =
        gen.pos.board.piece_bb[them as usize][PieceType::Rook as usize] & gen.pos.board.promoted_bb;
    let horses = gen.pos.board.piece_bb[them as usize][PieceType::Bishop as usize]
        & gen.pos.board.promoted_bb;
    if !((dragons | horses) & king_attacks).is_empty() {
        return true;
    }

    // Sliding piece checks

    // Rook/Dragon checks
    let enemy_rooks = gen.pos.board.piece_bb[them as usize][PieceType::Rook as usize];
    let rook_attacks = ATTACK_TABLES.sliding_attacks(to, occupancy_after, PieceType::Rook);
    if !(enemy_rooks & rook_attacks).is_empty() {
        return true;
    }

    // Bishop/Horse checks
    let enemy_bishops = gen.pos.board.piece_bb[them as usize][PieceType::Bishop as usize];
    let bishop_attacks = ATTACK_TABLES.sliding_attacks(to, occupancy_after, PieceType::Bishop);
    if !(enemy_bishops & bishop_attacks).is_empty() {
        return true;
    }

    // Lance checks
    let enemy_lances = gen.pos.board.piece_bb[them as usize][PieceType::Lance as usize]
        & !gen.pos.board.promoted_bb;
    let mut lance_bb = enemy_lances;
    while let Some(lance_sq) = lance_bb.pop_lsb() {
        // Check if lance can attack the destination
        let can_attack = match them {
            Color::Black => lance_sq.rank() > to.rank() && lance_sq.file() == to.file(),
            Color::White => lance_sq.rank() < to.rank() && lance_sq.file() == to.file(),
        };
        if can_attack {
            let between = attacks::between_bb(lance_sq, to);
            if (between & occupancy_after).is_empty() {
                return true;
            }
        }
    }

    false
}
