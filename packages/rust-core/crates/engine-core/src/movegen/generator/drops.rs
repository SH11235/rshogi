//! Drop move generation

use crate::{
    shogi::{Move, ATTACK_TABLES},
    Bitboard, Color, PieceType, Square,
};

use super::core::MoveGenImpl;

/// Generate all drop moves
pub(super) fn generate_drop_moves(gen: &mut MoveGenImpl) {
    let us = gen.pos.side_to_move;
    let empty_squares = !gen.pos.board.all_bb;

    // If in check, only consider drops that block the check
    let drop_targets = if gen.checkers.count_ones() == 1 {
        // Single check - can block
        let checker_sq = gen.checkers.lsb().unwrap();
        let king_sq = gen.king_sq;

        // For sliding pieces, we can drop on squares between checker and king
        if gen.is_sliding_piece(checker_sq) {
            gen.between_bb(checker_sq, king_sq) & empty_squares
        } else {
            // Non-sliding pieces can't be blocked
            Bitboard::EMPTY
        }
    } else if gen.checkers.count_ones() > 1 {
        // Double check - no drops can help
        Bitboard::EMPTY
    } else {
        // Not in check - can drop anywhere valid
        empty_squares
    };

    // Check each piece type in hand (King を除く HAND_ORDER)
    for (piece_idx, &piece_type) in crate::shogi::board::HAND_ORDER.iter().enumerate() {
        let count = gen.pos.hands[us as usize][piece_idx];
        if count == 0 {
            continue;
        }

        // Get valid drop squares for this piece type
        let valid_drops = get_valid_drop_squares(gen, piece_type, empty_squares) & drop_targets;

        let mut drops = valid_drops;
        while let Some(to) = drops.pop_lsb() {
            gen.moves.push(Move::drop(piece_type, to));
        }
    }
}

/// Get valid squares where a piece can be dropped
fn get_valid_drop_squares(
    gen: &MoveGenImpl,
    piece_type: PieceType,
    empty_squares: Bitboard,
) -> Bitboard {
    let us = gen.pos.side_to_move;
    let mut valid = empty_squares;

    match piece_type {
        PieceType::Pawn => {
            // Pawns cannot be dropped on files where we already have a pawn
            for file in 0..9 {
                if has_pawn_on_file(gen, file, us) {
                    // Remove all squares on this file
                    for rank in 0..9 {
                        valid.clear(Square::new(file, rank));
                    }
                }
            }

            // Pawns cannot be dropped on last rank
            match us {
                Color::Black => {
                    for file in 0..9 {
                        valid.clear(Square::new(file, 0)); // Black's last rank
                    }
                }
                Color::White => {
                    for file in 0..9 {
                        valid.clear(Square::new(file, 8)); // White's last rank
                    }
                }
            }

            // Check for illegal pawn drop checkmate
            let them = us.opposite();
            let their_king_sq = gen.pos.board.king_square(them);
            if let Some(king_sq) = their_king_sq {
                // Check if any pawn drop would give check to enemy king
                // A pawn gives check if it's one square in front of the king (from the pawn's perspective)
                let pawn_check_sq = match us {
                    Color::Black => {
                        // Black pawns move towards rank 0, so they give check from rank+1
                        if king_sq.rank() < 8 {
                            Some(Square::new(king_sq.file(), king_sq.rank() + 1))
                        } else {
                            None
                        }
                    }
                    Color::White => {
                        // White pawns move towards rank 8, so they give check from rank-1
                        if king_sq.rank() > 0 {
                            Some(Square::new(king_sq.file(), king_sq.rank() - 1))
                        } else {
                            None
                        }
                    }
                };

                // If the pawn drop would give check, verify it's not checkmate
                if let Some(check_sq) = pawn_check_sq {
                    if valid.test(check_sq) {
                        let is_mate = is_drop_pawn_mate(gen, check_sq, them);
                        if is_mate {
                            valid.clear(check_sq);
                        }
                    }
                }
            }
        }
        PieceType::Lance => {
            // Lances cannot be dropped on last rank
            match us {
                Color::Black => {
                    for file in 0..9 {
                        valid.clear(Square::new(file, 0)); // Black's last rank
                    }
                }
                Color::White => {
                    for file in 0..9 {
                        valid.clear(Square::new(file, 8)); // White's last rank
                    }
                }
            }
        }
        PieceType::Knight => {
            // Knights cannot be dropped on last two ranks
            match us {
                Color::Black => {
                    for file in 0..9 {
                        valid.clear(Square::new(file, 0)); // Black can't drop on rank 0-1
                        valid.clear(Square::new(file, 1));
                    }
                }
                Color::White => {
                    for file in 0..9 {
                        valid.clear(Square::new(file, 7)); // White can't drop on rank 7-8
                        valid.clear(Square::new(file, 8));
                    }
                }
            }
        }
        _ => {} // Other pieces can be dropped anywhere empty
    }

    valid
}

/// Check if we have a pawn on the given file
fn has_pawn_on_file(gen: &MoveGenImpl, file: u8, color: Color) -> bool {
    let pawns = gen.pos.board.piece_bb[color as usize][PieceType::Pawn as usize]
        & !gen.pos.board.promoted_bb; // Only consider unpromoted pawns
    for rank in 0..9 {
        if pawns.test(Square::new(file, rank)) {
            return true;
        }
    }
    false
}

/// Check if dropping a pawn at 'to' would be checkmate (illegal)
pub(super) fn is_drop_pawn_mate(gen: &MoveGenImpl, to: Square, them: Color) -> bool {
    // Get the enemy king square
    let their_king_sq = match gen.pos.board.king_square(them) {
        Some(sq) => sq,
        None => return false, // No king?
    };

    // Check if the pawn drop gives check
    let us = them.opposite();
    let pawn_attacks = ATTACK_TABLES.pawn_attacks(to, us);
    if !pawn_attacks.test(their_king_sq) {
        return false; // Not even a check
    }

    // Check if the pawn has support (if not, king can capture it)
    let pawn_supporters = gen.attackers_to(to, us);
    if pawn_supporters.is_empty() {
        return false; // No support - king can capture the pawn
    }

    // Check if any piece (except king, pawn, lance) can capture the dropped pawn
    let defenders = gen.attackers_to_except_king_pawn_lance(to, them);

    // Check if any unpinned piece can capture
    if defenders.count_ones() > 0 {
        // Calculate pinned pieces for the defending side
        let pinned = gen.calculate_pinned_pieces(them);

        // Check each defender individually
        let mut def_bb = defenders;
        while let Some(def_sq) = def_bb.pop_lsb() {
            // If not pinned, can capture
            if !pinned.test(def_sq) {
                return false; // Can capture the pawn
            }

            // If pinned, check if the pawn is on the pin ray
            // A pinned piece can still capture if the target is on the pin ray
            // For pawn drops, we need to check if capturing the pawn would be along the pin line

            // Get the king position
            let king_sq = match gen.pos.board.king_square(them) {
                Some(sq) => sq,
                None => continue, // No king, shouldn't happen
            };

            // Check if the pawn square (to) is between the defender and the king
            // This would mean the capture is along the pin ray
            if gen.is_aligned_rook(def_sq, king_sq) && gen.is_aligned_rook(to, king_sq) {
                // All three squares are on same rank or file
                if (def_sq.file() == king_sq.file() && to.file() == king_sq.file())
                    || (def_sq.rank() == king_sq.rank() && to.rank() == king_sq.rank())
                {
                    // The pawn is on the pin ray, so the pinned piece can capture
                    return false;
                }
            } else if gen.is_aligned_bishop(def_sq, king_sq) && gen.is_aligned_bishop(to, king_sq) {
                // Check diagonal alignment
                let def_to_king_file_diff = (def_sq.file() as i8 - king_sq.file() as i8).signum();
                let def_to_king_rank_diff = (def_sq.rank() as i8 - king_sq.rank() as i8).signum();
                let to_to_king_file_diff = (to.file() as i8 - king_sq.file() as i8).signum();
                let to_to_king_rank_diff = (to.rank() as i8 - king_sq.rank() as i8).signum();

                if def_to_king_file_diff == to_to_king_file_diff
                    && def_to_king_rank_diff == to_to_king_rank_diff
                {
                    // The pawn is on the pin ray (diagonal), so the pinned piece can capture
                    return false;
                }
            }

            // If we get here, the piece is pinned and cannot capture the pawn
        }
    }

    // Check if king has any escape squares
    let king_attacks = ATTACK_TABLES.king_attacks(their_king_sq);
    let their_pieces = gen.pos.board.occupied_bb[them as usize];
    let our_pieces = gen.pos.board.occupied_bb[us as usize];

    // King can only move to squares that are:
    // 1. Not occupied by own pieces (their_pieces)
    // 2. Not the pawn square (it has support)
    let mut escape_squares = king_attacks & !their_pieces;

    // Remove the pawn square from escape squares (can't capture it - it has support)
    escape_squares &= !Bitboard::from_square(to);

    // Simulate position after pawn drop
    let occupied_after_drop = gen.pos.board.all_bb | Bitboard::from_square(to);

    let mut escapes = escape_squares;
    while let Some(escape_sq) = escapes.pop_lsb() {
        // First check if the square is occupied by our piece (enemy king can't move there)
        if our_pieces.test(escape_sq) {
            continue; // Can't move to a square occupied by enemy piece
        }

        // Check if escape square is attacked by any enemy piece
        let attackers = gen.attackers_to_with_occupancy(escape_sq, us, occupied_after_drop);
        if attackers.is_empty() {
            return false; // King can escape
        }
    }

    // No escapes, no captures - it's mate
    true
}
