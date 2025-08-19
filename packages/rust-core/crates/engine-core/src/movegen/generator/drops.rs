//! Drop move generation

use crate::{
    shogi::{attacks, Move, ATTACK_TABLES},
    Bitboard, Color, PieceType, Square,
};

use super::core::MoveGenImpl;

/// Generate all drop moves
pub(super) fn generate_drop_moves(gen: &mut MoveGenImpl) {
    let us = gen.pos.side_to_move;
    let empty_squares = !gen.pos.board.all_bb;

    // Apply drop block mask to determine valid drop targets
    let drop_targets = empty_squares & gen.drop_block_mask;

    // Early return if no valid drop targets
    if drop_targets.is_empty() {
        return;
    }

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
            // Pawns cannot be dropped on files where we already have an unpromoted pawn
            let pawns = gen.pos.board.piece_bb[us as usize][PieceType::Pawn as usize]
                & !gen.pos.board.promoted_bb;
            for file in 0..9u8 {
                let file_mask = attacks::file_mask(file);
                if !(pawns & file_mask).is_empty() {
                    valid &= !file_mask; // 一括でその筋を禁止
                }
            }

            // Pawns cannot be dropped on last rank
            match us {
                Color::Black => {
                    valid &= !ATTACK_TABLES.rank_mask(0); // Black's last rank
                }
                Color::White => {
                    valid &= !ATTACK_TABLES.rank_mask(8); // White's last rank
                }
            }

            // Check for illegal pawn drop checkmate
            let them = us.opposite();
            if let Some(king_sq) = gen.pos.board.king_square(them) {
                // A pawn gives check if it's one square behind the king (from where it can attack)
                // Black pawns at rank n attack rank n-1, so to attack king at rank k, pawn must be at rank k+1
                // White pawns at rank n attack rank n+1, so to attack king at rank k, pawn must be at rank k-1
                let pawn_check_sq = match us {
                    Color::Black => {
                        if king_sq.rank() < 8 {
                            Some(Square::new(king_sq.file(), king_sq.rank() + 1))
                        } else {
                            None // King at rank 8, pawn can't be placed at rank 9
                        }
                    }
                    Color::White => {
                        if king_sq.rank() > 0 {
                            Some(Square::new(king_sq.file(), king_sq.rank() - 1))
                        } else {
                            None // King at rank 0, pawn can't be placed at rank -1
                        }
                    }
                };

                if let Some(check_sq) = pawn_check_sq {
                    if valid.test(check_sq) && is_drop_pawn_mate(gen, check_sq, them) {
                        valid.clear(check_sq);
                    }
                }
            }
        }
        PieceType::Lance => {
            // Lances cannot be dropped on last rank
            match us {
                Color::Black => {
                    valid &= !ATTACK_TABLES.rank_mask(0);
                }
                Color::White => {
                    valid &= !ATTACK_TABLES.rank_mask(8);
                }
            }
        }
        PieceType::Knight => {
            // Knights cannot be dropped on last two ranks
            match us {
                Color::Black => {
                    valid &= !ATTACK_TABLES.rank_mask(0);
                    valid &= !ATTACK_TABLES.rank_mask(1);
                }
                Color::White => {
                    valid &= !ATTACK_TABLES.rank_mask(7);
                    valid &= !ATTACK_TABLES.rank_mask(8);
                }
            }
        }
        _ => {} // Other pieces can be dropped anywhere empty
    }

    valid
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

    // Simulate position after pawn drop
    let occupied_after_drop = gen.pos.board.all_bb | Bitboard::from_square(to);

    // 1) Check if king can capture the pawn safely
    {
        // King captures the pawn - create virtual occupancy
        let mut occ_after_king_capture = occupied_after_drop;
        occ_after_king_capture.clear(their_king_sq);
        // The pawn at 'to' is captured, so we just have the king at 'to'

        // Check if 'to' is attacked by any of our pieces after king capture
        let attackers = gen.attackers_to_with_occupancy(to, us, occ_after_king_capture);
        if attackers.is_empty() {
            return false; // King can safely capture the pawn
        }
    }

    // 2) Check if any piece (except king) can legally capture the dropped pawn
    let defenders_all = gen.attackers_to_with_occupancy(to, them, occupied_after_drop);
    let king_bb = gen.pos.board.piece_bb[them as usize][PieceType::King as usize];
    let mut defenders = defenders_all & !king_bb;

    while let Some(def_sq) = defenders.pop_lsb() {
        // Simulate: defender on def_sq captures the pawn on `to`
        let mut occ_after_def_capture = occupied_after_drop;
        occ_after_def_capture.clear(def_sq); // defender leaves its square
                                             // `to` remains occupied (was pawn, now defender)

        // Check if king is still in check after the capture
        let still_in_check = !gen
            .attackers_to_with_occupancy(their_king_sq, us, occ_after_def_capture)
            .is_empty();
        if !still_in_check {
            return false; // Defender can capture and resolve the check
        }
    }

    // 3) Check if king has any escape squares
    let king_attacks = ATTACK_TABLES.king_attacks(their_king_sq);
    let their_pieces = gen.pos.board.occupied_bb[them as usize];
    let our_pieces = gen.pos.board.occupied_bb[us as usize];

    // 自駒マスは除外、敵駒マスは「取り逃げ」も評価する
    let escape_squares = king_attacks & !their_pieces;

    let mut escapes = escape_squares;
    while let Some(escape_sq) = escapes.pop_lsb() {
        let mut occ_after_escape = occupied_after_drop;
        occ_after_escape.clear(their_king_sq);

        // 取りながら移動のシミュレーション：攻撃側の駒がいたら先に取り除く
        if our_pieces.test(escape_sq) {
            occ_after_escape.clear(escape_sq);
        }

        // 王が escape_sq へ
        occ_after_escape.set(escape_sq);

        // 安全なら詰みではない
        let attackers = gen.attackers_to_with_occupancy(escape_sq, us, occ_after_escape);
        if attackers.is_empty() {
            return false;
        }
    }

    // No escapes, no captures - it's mate
    true
}
