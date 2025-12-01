// 駒打ちによる1手詰め判定（YaneuraOu移植）

use crate::bitboard::{gold_effect, king_effect, knight_effect, silver_effect, Bitboard};
use crate::mate::helpers::{can_king_escape, can_piece_capture};
use crate::mate::{bishop_step_effect, cross45_step_effect, lance_step_effect, rook_step_effect};
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

    // 飛車打ち: 玉の上下左右（十字の隣接）で空きマス
    if our_hand.has(PieceType::Rook) {
        let mut bb = rook_step_effect(sq_king) & king_effect(sq_king) & bb_drop;
        while bb.is_not_empty() {
            let to = bb.pop();
            // toに対して自駒が利いているか
            if (pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                continue;
            }

            let bb_attacks = rook_step_effect(to);
            if can_king_escape(pos, them, to, bb_attacks, pos.occupied()) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, pos.occupied()) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Rook, to));
        }
    }

    // 香打ち: 玉の前1マス
    if our_hand.has(PieceType::Lance) {
        let mut bb = crate::bitboard::pawn_effect(them, sq_king) & bb_drop;
        if bb.is_not_empty() {
            let to = bb.pop();
            if !(pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                let bb_attacks = lance_step_effect(us, to);
                if !can_king_escape(pos, them, to, bb_attacks, pos.occupied())
                    && !can_piece_capture(pos, them, to, pinned, pos.occupied())
                {
                    return Some(Move::new_drop(PieceType::Lance, to));
                }
            }
        }
    }

    // 角打ち: 玉の斜め隣接
    if our_hand.has(PieceType::Bishop) {
        let mut bb = cross45_step_effect(sq_king) & bb_drop;
        while bb.is_not_empty() {
            let to = bb.pop();
            if (pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                continue;
            }

            let bb_attacks = bishop_step_effect(to);
            if can_king_escape(pos, them, to, bb_attacks, pos.occupied()) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, pos.occupied()) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Bishop, to));
        }
    }

    // 金打ち: 玉の金移動圏（先に飛車打ち済みなので前方重複を外す）
    if our_hand.has(PieceType::Gold) {
        let mut bb = gold_effect(them, sq_king) & bb_drop;
        if our_hand.has(PieceType::Rook) {
            bb &= !crate::bitboard::pawn_effect(us, sq_king);
        }
        while bb.is_not_empty() {
            let to = bb.pop();
            if (pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                continue;
            }
            let bb_attacks = gold_effect(us, to);
            if can_king_escape(pos, them, to, bb_attacks, pos.occupied()) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, pos.occupied()) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Gold, to));
        }
    }

    // 銀打ち: 玉の銀移動圏
    if our_hand.has(PieceType::Silver) {
        let mut bb = silver_effect(them, sq_king) & bb_drop;
        if our_hand.has(PieceType::Gold) {
            // 前方は金打ちで判定済み
            bb &= !gold_effect(them, sq_king);
        }
        while bb.is_not_empty() {
            let to = bb.pop();
            if (pos.attackers_to(to) & pos.pieces_c(us)).is_empty() {
                continue;
            }
            let bb_attacks = silver_effect(us, to);
            if can_king_escape(pos, them, to, bb_attacks, pos.occupied()) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, pos.occupied()) {
                continue;
            }
            return Some(Move::new_drop(PieceType::Silver, to));
        }
    }

    // 桂打ち: 玉の桂利き
    if our_hand.has(PieceType::Knight) {
        let mut bb = knight_effect(them, sq_king) & bb_drop;
        while bb.is_not_empty() {
            let to = bb.pop();
            // 桂はto以外は玉が行けないので利き確認不要
            if can_king_escape(pos, them, to, Bitboard::EMPTY, pos.occupied()) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, pos.occupied()) {
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
