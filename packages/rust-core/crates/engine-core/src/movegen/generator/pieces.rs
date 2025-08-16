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

    gen.add_moves_with_type(from, targets, PieceType::Silver);
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

    gen.add_moves_with_type(from, targets, PieceType::Knight);
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

    gen.add_single_move(from, to, PieceType::Pawn);
}
