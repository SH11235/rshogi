//! Sliding piece move generation (rook, bishop, lance)

use crate::{shogi::ATTACK_TABLES, PieceType, Square};

use super::core::MoveGenImpl;

/// Generate sliding moves for rook and bishop
pub(super) fn generate_sliding_moves(
    gen: &mut MoveGenImpl,
    from: Square,
    piece_type: PieceType,
    promoted: bool,
) {
    let us = gen.pos.side_to_move;
    let all_pieces = gen.pos.board.all_bb;
    let targets = !gen.pos.board.occupied_bb[us as usize];

    // Get attack squares for the piece
    let attacks = match (piece_type, promoted) {
        (PieceType::Rook, false) => {
            ATTACK_TABLES.sliding_attacks(from, all_pieces, PieceType::Rook)
        }
        (PieceType::Rook, true) => {
            // Dragon (promoted rook) = rook + king moves
            let rook_attacks = ATTACK_TABLES.sliding_attacks(from, all_pieces, PieceType::Rook);
            let king_attacks = ATTACK_TABLES.king_attacks(from);
            rook_attacks | king_attacks
        }
        (PieceType::Bishop, false) => {
            ATTACK_TABLES.sliding_attacks(from, all_pieces, PieceType::Bishop)
        }
        (PieceType::Bishop, true) => {
            // Horse (promoted bishop) = bishop + king moves
            let bishop_attacks = ATTACK_TABLES.sliding_attacks(from, all_pieces, PieceType::Bishop);
            let king_attacks = ATTACK_TABLES.king_attacks(from);
            bishop_attacks | king_attacks
        }
        _ => return,
    };

    let valid_targets = attacks & targets;

    // If pinned, can only move along pin ray
    if gen.pinned.test(from) {
        let pin_ray = gen.pin_rays[from.index()];
        let pinned_targets = valid_targets & pin_ray;
        gen.add_moves_with_type(from, pinned_targets, piece_type);
        return;
    }

    // If in check, can only block or capture checker
    if !gen.checkers.is_empty() {
        let check_mask = gen.checkers | gen.between_bb(gen.king_sq, gen.checkers.lsb().unwrap());
        let check_targets = valid_targets & check_mask;
        gen.add_moves_with_type(from, check_targets, piece_type);
        return;
    }

    // Normal moves
    gen.add_moves_with_type(from, valid_targets, piece_type);
}

/// Generate lance moves
pub(super) fn generate_lance_moves(gen: &mut MoveGenImpl, from: Square, promoted: bool) {
    super::pieces::generate_lance_moves(gen, from, promoted);
}
