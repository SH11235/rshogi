//! Piece-specific move generation

use crate::{
    shogi::{attacks, Move},
    Color, PieceType, Square,
};

use super::core::MoveGenImpl;

/// Generate all king moves
pub(super) fn generate_king_moves(gen: &mut MoveGenImpl) {
    generate_king_moves_from(gen, gen.king_sq);
}

/// Generate king moves from a specific square
pub(super) fn generate_king_moves_from(gen: &mut MoveGenImpl, from: Square) {
    let us = gen.pos.side_to_move;
    let attacks = attacks::king_attacks(from);
    let targets = attacks & !gen.pos.board.occupied_bb[us as usize];

    let mut moves = targets;
    while let Some(to) = moves.pop_lsb() {
        // Check if king would be in check on target square
        if !gen.would_be_in_check(from, to) {
            let captured_type = gen.get_captured_type(to);
            gen.moves.push(Move::normal_with_piece(
                from,
                to,
                false,
                PieceType::King,
                captured_type,
            ));
        }
    }
}

/// Generate gold moves (also used for promoted silver, knight, lance, pawn)
pub(super) fn generate_gold_moves(gen: &mut MoveGenImpl, from: Square, promoted: bool) {
    let us = gen.pos.side_to_move;
    let attacks = attacks::gold_attacks(from, us);
    let targets = attacks & !gen.pos.board.occupied_bb[us as usize];

    gen.add_moves(from, targets, promoted);
}

/// Generate silver moves
pub(super) fn generate_silver_moves(gen: &mut MoveGenImpl, from: Square, promoted: bool) {
    if promoted {
        // Promoted silver moves like gold
        generate_gold_moves(gen, from, promoted);
        return;
    }

    let us = gen.pos.side_to_move;
    let attacks = attacks::silver_attacks(from, us);
    let targets = attacks & !gen.pos.board.occupied_bb[us as usize];

    // Never capture enemy king
    let them = us.opposite();
    let enemy_king_bb = gen.pos.board.piece_bb[them as usize][PieceType::King as usize];
    let valid_targets = targets & !enemy_king_bb;

    // If pinned, can only move along pin ray
    if gen.pinned.test(from) {
        let pin_ray = gen.pin_rays[from.index()];
        let pinned_targets = valid_targets & pin_ray;
        let mut moves = pinned_targets;
        while let Some(to) = moves.pop_lsb() {
            let captured_type = gen.get_captured_type(to);
            if gen.can_promote(from, to, us) {
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Silver,
                    captured_type,
                ));
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Silver,
                    captured_type,
                ));
            } else {
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Silver,
                    captured_type,
                ));
            }
        }
        return;
    }

    // If in check, can only block or capture checker
    if !gen.checkers.is_empty() {
        let check_mask = gen.checkers | gen.between_bb(gen.king_sq, gen.checkers.lsb().unwrap());
        let check_targets = valid_targets & check_mask;
        let mut moves = check_targets;
        while let Some(to) = moves.pop_lsb() {
            let captured_type = gen.get_captured_type(to);
            if gen.can_promote(from, to, us) {
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Silver,
                    captured_type,
                ));
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Silver,
                    captured_type,
                ));
            } else {
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Silver,
                    captured_type,
                ));
            }
        }
        return;
    }

    // Normal moves
    let mut moves = valid_targets;
    while let Some(to) = moves.pop_lsb() {
        let captured_type = gen.get_captured_type(to);
        if gen.can_promote(from, to, us) {
            gen.moves.push(Move::normal_with_piece(
                from,
                to,
                true,
                PieceType::Silver,
                captured_type,
            ));
            gen.moves.push(Move::normal_with_piece(
                from,
                to,
                false,
                PieceType::Silver,
                captured_type,
            ));
        } else {
            gen.moves.push(Move::normal_with_piece(
                from,
                to,
                false,
                PieceType::Silver,
                captured_type,
            ));
        }
    }
}

/// Generate knight moves
pub(super) fn generate_knight_moves(gen: &mut MoveGenImpl, from: Square, promoted: bool) {
    if promoted {
        // Promoted knight moves like gold
        generate_gold_moves(gen, from, promoted);
        return;
    }

    let us = gen.pos.side_to_move;

    // Check if knight is too close to edge to move
    let rank = from.rank();
    if (us == Color::Black && rank <= 1) || (us == Color::White && rank >= 7) {
        return; // Knight cannot move from these ranks
    }

    let attacks = attacks::knight_attacks(from, us);
    let targets = attacks & !gen.pos.board.occupied_bb[us as usize];

    // If pinned, knights can't move (they don't move along pin rays)
    if gen.pinned.test(from) {
        return;
    }

    // If in check, can only block or capture checker
    if !gen.checkers.is_empty() {
        let check_mask = gen.checkers | gen.between_bb(gen.king_sq, gen.checkers.lsb().unwrap());
        let valid_targets = targets & check_mask;
        let mut moves = valid_targets;
        while let Some(to) = moves.pop_lsb() {
            let captured_type = gen.get_captured_type(to);
            // Knight must promote if it can't move further
            let must_promote = match us {
                Color::Black => to.rank() <= 1, // Black can't move from rank 0-1
                Color::White => to.rank() >= 7, // White can't move from rank 7-8
            };

            if must_promote {
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Knight,
                    captured_type,
                ));
            } else {
                let can_promote = gen.can_promote(from, to, us);
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    false,
                    PieceType::Knight,
                    captured_type,
                ));
                if can_promote {
                    gen.moves.push(Move::normal_with_piece(
                        from,
                        to,
                        true,
                        PieceType::Knight,
                        captured_type,
                    ));
                }
            }
        }
        return;
    }

    // Normal moves
    let mut moves = targets;
    while let Some(to) = moves.pop_lsb() {
        let captured_type = gen.get_captured_type(to);
        // Knight must promote if it can't move further
        let must_promote = match us {
            Color::Black => to.rank() <= 1,
            Color::White => to.rank() >= 7,
        };

        if must_promote {
            gen.moves.push(Move::normal_with_piece(
                from,
                to,
                true,
                PieceType::Knight,
                captured_type,
            ));
        } else {
            let can_promote = gen.can_promote(from, to, us);
            gen.moves.push(Move::normal_with_piece(
                from,
                to,
                false,
                PieceType::Knight,
                captured_type,
            ));
            if can_promote {
                gen.moves.push(Move::normal_with_piece(
                    from,
                    to,
                    true,
                    PieceType::Knight,
                    captured_type,
                ));
            }
        }
    }
}

/// Generate pawn moves
pub(super) fn generate_pawn_moves(gen: &mut MoveGenImpl, from: Square, promoted: bool) {
    if promoted {
        // Promoted pawn (tokin) moves like gold
        generate_gold_moves(gen, from, promoted);
        return;
    }

    let us = gen.pos.side_to_move;
    // Pawns move one square forward
    let to = match us {
        Color::Black => {
            let rank = from.rank();
            if rank == 0 {
                return;
            } // Can't move further
            Square::new(from.file(), rank - 1)
        }
        Color::White => {
            let rank = from.rank();
            if rank == 8 {
                return;
            } // Can't move further
            Square::new(from.file(), rank + 1)
        }
    };

    // Check if destination is occupied
    if gen.pos.board.piece_on(to).is_some() {
        return;
    }

    // If pinned, can only move along pin ray
    if gen.pinned.test(from) {
        let pin_ray = gen.pin_rays[from.index()];
        if !pin_ray.test(to) {
            return;
        }
    }

    // If in check, can only block or capture checker
    if !gen.checkers.is_empty() {
        let check_mask = gen.checkers | gen.between_bb(gen.king_sq, gen.checkers.lsb().unwrap());
        if !check_mask.test(to) {
            return;
        }
    }

    // Pawn must promote if it reaches the last rank
    let must_promote = match us {
        Color::Black => to.rank() == 0,
        Color::White => to.rank() == 8,
    };

    if must_promote {
        gen.moves.push(Move::normal_with_piece(from, to, true, PieceType::Pawn, None));
    } else {
        let can_promote = gen.can_promote(from, to, us);
        gen.moves.push(Move::normal_with_piece(from, to, false, PieceType::Pawn, None));
        if can_promote {
            gen.moves.push(Move::normal_with_piece(from, to, true, PieceType::Pawn, None));
        }
    }
}
