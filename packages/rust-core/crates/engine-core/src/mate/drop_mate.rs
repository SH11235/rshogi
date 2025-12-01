// 駒打ちによる1手詰め判定（YaneuraOu移植）

use crate::bitboard::{
    gold_effect, king_effect, knight_effect, lance_effect, pawn_effect, silver_effect, Bitboard,
};
use crate::mate::helpers::{can_king_escape, can_piece_capture};
use crate::mate::{bishop_step_effect, cross45_step_effect, rook_step_effect};
use crate::position::Position;
use crate::types::{Color, Move, PieceType};

/// 駒打ちによる1手詰めを判定
///
/// # Arguments
/// * `pos` - 局面（手番側が us の前提）
/// * `us` - 攻撃側の色
///
/// # Returns
/// 1手詰めの手があれば`Some(Move)`、なければ`None`
pub fn check_drop_mate(pos: &Position, us: Color) -> Option<Move> {
    let them = !us;
    let sq_king = pos.king_square(them);

    let pinned = pos.blockers_for_king(them) & pos.pieces_c(them);
    let our_hand = pos.hand(us);
    let bb_drop = !pos.occupied();
    let occupied = pos.occupied();

    // 飛車を短く打つ場合
    if our_hand.has(PieceType::Rook) {
        let mut bb = rook_step_effect(sq_king) & king_effect(sq_king) & bb_drop;
        while bb.is_not_empty() {
            let to = bb.pop();
            if (pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                continue;
            }
            let bb_attacks = rook_step_effect(to);
            if can_king_escape(pos, them, to, bb_attacks, occupied) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, occupied) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Rook, to));
        }
    }

    // 香を短く打つ場合
    if our_hand.has(PieceType::Lance) {
        let mut bb = pawn_effect(them, sq_king) & bb_drop;
        if bb.is_not_empty() {
            let to = bb.pop();
            if !(pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                let bb_attacks = lance_effect(us, to, Bitboard::EMPTY);
                if !can_king_escape(pos, them, to, bb_attacks, occupied)
                    && !can_piece_capture(pos, them, to, pinned, occupied)
                {
                    return Some(Move::new_drop(PieceType::Lance, to));
                }
            }
        }
    }

    // 角を短く打つ
    if our_hand.has(PieceType::Bishop) {
        let mut bb = cross45_step_effect(sq_king) & bb_drop;
        while bb.is_not_empty() {
            let to = bb.pop();
            if (pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                continue;
            }
            let bb_attacks = bishop_step_effect(to);
            if can_king_escape(pos, them, to, bb_attacks, occupied) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, occupied) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Bishop, to));
        }
    }

    // 金打ち
    if our_hand.has(PieceType::Gold) {
        let mut bb = gold_effect(them, sq_king) & bb_drop;
        if our_hand.has(PieceType::Rook) {
            bb &= !pawn_effect(us, sq_king);
        }
        while bb.is_not_empty() {
            let to = bb.pop();
            if (pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                continue;
            }
            let bb_attacks = gold_effect(us, to);
            if can_king_escape(pos, them, to, bb_attacks, occupied) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, occupied) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Gold, to));
        }
    }

    // 銀打ち
    if our_hand.has(PieceType::Silver) {
        let mut bb = if our_hand.has(PieceType::Gold) {
            if our_hand.has(PieceType::Bishop) {
                Bitboard::EMPTY
            } else {
                silver_effect(them, sq_king) & (bb_drop & !gold_effect(them, sq_king))
            }
        } else {
            silver_effect(them, sq_king) & bb_drop
        };

        while bb.is_not_empty() {
            let to = bb.pop();
            if (pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                continue;
            }
            let bb_attacks = silver_effect(us, to);
            if can_king_escape(pos, them, to, bb_attacks, occupied) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, occupied) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Silver, to));
        }
    }

    // 桂打ち
    if our_hand.has(PieceType::Knight) {
        let mut bb = knight_effect(them, sq_king) & bb_drop;
        while bb.is_not_empty() {
            let to = bb.pop();
            if can_king_escape(pos, them, to, Bitboard::EMPTY, occupied) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, occupied) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Knight, to));
        }
    }

    None
}

/// queen_step_effectをテストで使用するので公開
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitboard::king_effect;
    use crate::mate::queen_step_effect;
    use crate::types::{File, Rank, Square};

    #[test]
    fn test_step_effects_exist() {
        let sq = Square::new(File::File5, Rank::Rank5);
        assert!(rook_step_effect(sq).is_not_empty());
        assert!(bishop_step_effect(sq).is_not_empty());
        assert!(queen_step_effect(sq).is_not_empty());
    }

    #[test]
    fn test_cross45() {
        let sq = Square::new(File::File5, Rank::Rank5);
        let bb = cross45_step_effect(sq);
        assert!(bb.is_not_empty());
        // 斜め1ステップのみ
        assert!(bb & king_effect(sq) == bb);
    }
}
