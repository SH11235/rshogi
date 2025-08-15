//! Sliding piece move generation (rook, bishop, lance)

use crate::{Bitboard, PieceType, Square};

use super::core::MoveGenImpl;

/// Generate sliding moves for rook and bishop
pub(super) fn generate_sliding_moves(
    gen: &mut MoveGenImpl,
    from: Square,
    piece_type: PieceType,
    promoted: bool,
) {
    let us = gen.pos.side_to_move;
    let targets = !gen.pos.board.occupied_bb[us as usize];

    match piece_type {
        PieceType::Rook => {
            generate_rook_moves_ordered(gen, from, targets);
            if promoted {
                // Dragon (promoted rook) also has king moves
                generate_king_style_moves(gen, from, targets, piece_type);
            }
        }
        PieceType::Bishop => {
            generate_bishop_moves_ordered(gen, from, targets);
            if promoted {
                // Horse (promoted bishop) also has king moves
                generate_king_style_moves(gen, from, targets, piece_type);
            }
        }
        _ => {}
    }
}

/// Generate rook moves in order along each ray (up, down, left, right)
fn generate_rook_moves_ordered(gen: &mut MoveGenImpl, from: Square, targets: Bitboard) {
    let all_pieces = gen.pos.board.all_bb;

    // Direction vectors for rook: up, down, left, right
    let directions = [
        (0, -1), // up (decreasing rank)
        (0, 1),  // down (increasing rank)
        (-1, 0), // left (decreasing file)
        (1, 0),  // right (increasing file)
    ];

    for (file_delta, rank_delta) in directions {
        generate_ray_moves(gen, from, targets, all_pieces, file_delta, rank_delta, PieceType::Rook);
    }
}

/// Generate bishop moves in order along each diagonal ray
fn generate_bishop_moves_ordered(gen: &mut MoveGenImpl, from: Square, targets: Bitboard) {
    let all_pieces = gen.pos.board.all_bb;

    // Direction vectors for bishop: diagonals
    let directions = [
        (-1, -1), // up-left
        (1, -1),  // up-right
        (-1, 1),  // down-left
        (1, 1),   // down-right
    ];

    for (file_delta, rank_delta) in directions {
        generate_ray_moves(
            gen,
            from,
            targets,
            all_pieces,
            file_delta,
            rank_delta,
            PieceType::Bishop,
        );
    }
}

/// Generate moves along a ray in a specific direction
fn generate_ray_moves(
    gen: &mut MoveGenImpl,
    from: Square,
    targets: Bitboard,
    all_pieces: Bitboard,
    file_delta: i8,
    rank_delta: i8,
    piece_type: PieceType,
) {
    let mut moves = Vec::new();
    let from_file = from.file() as i8;
    let from_rank = from.rank() as i8;

    let mut current_file = from_file + file_delta;
    let mut current_rank = from_rank + rank_delta;

    while (0..9).contains(&current_file) && (0..9).contains(&current_rank) {
        let to = Square::new(current_file as u8, current_rank as u8);

        // Check if this square is occupied
        if all_pieces.test(to) {
            // Can capture if it's an enemy piece
            if targets.test(to) {
                moves.push(to);
            }
            break; // Stop at any piece
        }

        // Empty square - can move here
        moves.push(to);

        // Continue along the ray
        current_file += file_delta;
        current_rank += rank_delta;
    }

    // Apply pin and check constraints if needed, then add moves
    if !moves.is_empty() {
        let mut valid_moves = Bitboard::EMPTY;
        for &to in &moves {
            valid_moves.set(to);
        }

        // Apply constraints
        if gen.pinned.test(from) {
            let pin_ray = gen.pin_rays[from.index()];
            valid_moves &= pin_ray;
        }

        if !gen.checkers.is_empty() {
            let check_mask =
                gen.checkers | gen.between_bb(gen.king_sq, gen.checkers.lsb().unwrap());
            valid_moves &= check_mask;
        }

        // Add moves in order
        for &to in &moves {
            if valid_moves.test(to) {
                gen.add_single_move(from, to, piece_type);
            }
        }
    }
}

/// Generate king-style moves (for promoted rook/bishop)
fn generate_king_style_moves(
    gen: &mut MoveGenImpl,
    from: Square,
    targets: Bitboard,
    piece_type: PieceType,
) {
    use crate::shogi::ATTACK_TABLES;

    let king_attacks = ATTACK_TABLES.king_attacks(from);
    let valid_targets = king_attacks & targets;

    // Apply constraints
    let mut final_targets = valid_targets;
    if gen.pinned.test(from) {
        let pin_ray = gen.pin_rays[from.index()];
        final_targets &= pin_ray;
    }

    if !gen.checkers.is_empty() {
        let check_mask = gen.checkers | gen.between_bb(gen.king_sq, gen.checkers.lsb().unwrap());
        final_targets &= check_mask;
    }

    // Add moves
    gen.add_moves_with_type(from, final_targets, piece_type);
}

/// Generate lance moves
pub(super) fn generate_lance_moves(gen: &mut MoveGenImpl, from: Square, promoted: bool) {
    super::pieces::generate_lance_moves(gen, from, promoted);
}
