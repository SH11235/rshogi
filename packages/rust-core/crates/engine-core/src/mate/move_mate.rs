// 駒移動による1手詰め判定（近接王手のみを探索）
//
// YaneuraOu mate1ply_without_effect.cpp の移植（離し角・飛車は未対応）

use crate::bitboard::{
    bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect, knight_effect,
    lance_effect, pawn_effect, rook_effect, silver_effect, Bitboard,
};
use crate::mate::helpers::{can_king_escape_with_from, can_piece_capture};
use crate::mate::tables::{check_cand_bb, PieceTypeCheck};
use crate::mate::{aligned, bishop_step_effect, can_promote, lance_step_effect};
use crate::mate::{queen_step_effect, rook_step_effect};
use crate::position::Position;
use crate::types::{Color, Move, PieceType, Rank, Square};

/// 駒移動による1手詰めを判定（非打ち手のみ対象）
pub fn check_move_mate(pos: &Position, us: Color) -> Option<Move> {
    if pos.in_check() {
        return None;
    }

    let them = !us;
    let sq_king = pos.king_square(them);
    let occupied = pos.occupied();

    // 両王手候補（相手玉をpinしている我駒）
    let dc_candidates = pos.blockers_for_king(them) & pos.pieces_c(us);
    // 相手玉側でpinされている駒
    let pinned = pos.blockers_for_king(them) & pos.pieces_c(them);
    // 自玉側のpin駒
    let our_pinned = pos.blockers_for_king(us) & pos.pieces_c(us);
    let our_king = pos.king_square(us);

    // 移動可能先（自駒以外）
    let bb_move = !pos.pieces_c(us);

    // DRAGON
    for from in pos.pieces(us, PieceType::Dragon).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = dragon_effect(from, slide) & bb_move & king_effect(sq_king); // 近接のみ
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }

            let diag = queen_step_effect(sq_king) & Bitboard::from_square(to);
            let bb_attacks = if diag.is_not_empty() {
                dragon_effect(to, slide)
            } else {
                rook_step_effect(to) | king_effect(to)
            };
            if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                continue;
            }
            if !dc_candidates.contains(from) && can_piece_capture(pos, them, to, new_pin, slide) {
                continue;
            }
            return Some(Move::new_move(from, to, false));
        }
    }

    // ROOK
    for from in pos.pieces(us, PieceType::Rook).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = rook_effect(from, slide) & bb_move & king_effect(sq_king); // 近接のみ
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }

            let promote = can_promote(us, from, to);
            let bb_attacks = if promote {
                rook_step_effect(to) | king_effect(to)
            } else {
                rook_step_effect(to)
            };
            if !bb_attacks.contains(sq_king) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }
            if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                continue;
            }
            if !dc_candidates.contains(from) && can_piece_capture(pos, them, to, new_pin, slide) {
                continue;
            }

            return Some(Move::new_move(from, to, promote));
        }
    }

    // HORSE
    for from in pos.pieces(us, PieceType::Horse).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = horse_effect(from, slide) & bb_move & king_effect(sq_king); // 近接のみ
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }

            let bb_attacks = bishop_step_effect(to) | king_effect(to);
            if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                continue;
            }
            if dc_candidates.contains(from) && !aligned(from, to, sq_king) {
                // 両王手なので合い利かず
            } else if can_piece_capture(pos, them, to, new_pin, slide) {
                continue;
            }

            return Some(Move::new_move(from, to, false));
        }
    }

    // BISHOP
    for from in pos.pieces(us, PieceType::Bishop).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = bishop_effect(from, slide) & bb_move & king_effect(sq_king); // 近接のみ
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }

            let promote = can_promote(us, from, to);
            let bb_attacks = if promote {
                bishop_step_effect(to) | king_effect(to)
            } else {
                bishop_step_effect(to)
            };
            if !bb_attacks.contains(sq_king) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }
            if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                continue;
            }
            if !dc_candidates.contains(from) && can_piece_capture(pos, them, to, new_pin, slide) {
                continue;
            }

            return Some(Move::new_move(from, to, promote));
        }
    }

    // LANCE
    let mut bb =
        check_cand_bb(us, PieceTypeCheck::Lance, sq_king) & pos.pieces(us, PieceType::Lance);
    while bb.is_not_empty() {
        let from = bb.pop();
        let slide = occupied ^ Bitboard::from_square(from);
        let bb_attacks_from = lance_effect(us, from, slide);
        let mut bb_check = bb_attacks_from & bb_move & gold_effect(them, sq_king);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            let promote_required = lance_must_promote(us, to);
            let promote = promote_required || can_promote(us, from, to);

            let bb_attacks = if promote {
                gold_effect(us, to)
            } else {
                lance_step_effect(us, to)
            };
            if !bb_attacks.contains(sq_king) {
                continue;
            }
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }
            if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                continue;
            }
            if !dc_candidates.contains(from) && can_piece_capture(pos, them, to, pinned, slide) {
                continue;
            }

            return Some(Move::new_move(from, to, promote));
        }
    }

    // GOLD相当（Gold/ProPawn/ProLance/ProKnight/ProSilver）
    let gold_like = pos.pieces(us, PieceType::Gold)
        | pos.pieces(us, PieceType::ProPawn)
        | pos.pieces(us, PieceType::ProLance)
        | pos.pieces(us, PieceType::ProKnight)
        | pos.pieces(us, PieceType::ProSilver);
    for from in gold_like.iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = gold_effect(us, from) & gold_effect(them, sq_king) & bb_move; // 近接のみ
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !bb_move.contains(to) {
                continue;
            }
            if !bb_attacks_and_check(pos, us, from, to, sq_king, slide, gold_effect(us, to)) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }
            if can_king_escape_with_from(pos, them, from, to, gold_effect(us, to), slide) {
                continue;
            }
            if !dc_candidates.contains(from) && can_piece_capture(pos, them, to, new_pin, slide) {
                continue;
            }
            return Some(Move::new_move(from, to, false));
        }
    }

    // SILVER
    for from in pos.pieces(us, PieceType::Silver).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = silver_effect(us, from) & silver_effect(them, sq_king) & bb_move; // 近接のみ
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }

            // 不成
            let bb_attacks_s = silver_effect(us, to);
            if bb_attacks_s.contains(sq_king)
                && !can_king_escape_with_from(pos, them, from, to, bb_attacks_s, slide)
                && (!dc_candidates.contains(from)
                    && !can_piece_capture(pos, them, to, new_pin, slide))
            {
                return Some(Move::new_move(from, to, false));
            }

            // 成り（金）
            if can_promote(us, from, to) {
                let bb_attacks_g = gold_effect(us, to);
                if bb_attacks_g.contains(sq_king)
                    && !can_king_escape_with_from(pos, them, from, to, bb_attacks_g, slide)
                    && (!dc_candidates.contains(from)
                        && !can_piece_capture(pos, them, to, new_pin, slide))
                {
                    return Some(Move::new_move(from, to, true));
                }
            }
        }
    }

    // KNIGHT
    for from in pos.pieces(us, PieceType::Knight).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = knight_effect(us, from) & knight_effect(them, sq_king) & bb_move; // 近接のみ
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }

            let must = knight_must_promote(us, to);
            // 不成
            if !must {
                let bb_attacks = knight_effect(us, to);
                if bb_attacks.contains(sq_king)
                    && !can_king_escape_with_from(pos, them, from, to, Bitboard::EMPTY, slide)
                    && (!dc_candidates.contains(from)
                        && !can_piece_capture(pos, them, to, new_pin, slide))
                {
                    return Some(Move::new_move(from, to, false));
                }
            }

            // 成り（金)
            if can_promote(us, from, to) {
                let bb_attacks = gold_effect(us, to);
                if bb_attacks.contains(sq_king)
                    && !can_king_escape_with_from(pos, them, from, to, bb_attacks, slide)
                    && (!dc_candidates.contains(from)
                        && !can_piece_capture(pos, them, to, new_pin, slide))
                {
                    return Some(Move::new_move(from, to, true));
                }
            }
        }
    }

    // PAWN
    for from in pos.pieces(us, PieceType::Pawn).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = pawn_effect(us, from) & pawn_effect(them, sq_king) & bb_move; // 近接のみ
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }

            let must = pawn_must_promote(us, to);

            // 不成
            if !must {
                let bb_attacks = pawn_effect(us, to);
                if bb_attacks.contains(sq_king)
                    && !can_king_escape_with_from(pos, them, from, to, Bitboard::EMPTY, slide)
                    && (!dc_candidates.contains(from)
                        && !can_piece_capture(pos, them, to, new_pin, slide))
                {
                    return Some(Move::new_move(from, to, false));
                }
            }

            // 成り（と金）
            if can_promote(us, from, to) {
                let bb_attacks = gold_effect(us, to);
                if bb_attacks.contains(sq_king)
                    && !can_king_escape_with_from(pos, them, from, to, bb_attacks, slide)
                    && (!dc_candidates.contains(from)
                        && !can_piece_capture(pos, them, to, new_pin, slide))
                {
                    return Some(Move::new_move(from, to, true));
                }
            }
        }
    }

    None
}

/// from以外にtoへ利いている自駒があるか
fn has_other_attacker(
    pos: &Position,
    us: Color,
    from: Square,
    to: Square,
    slide: Bitboard,
) -> bool {
    let attackers = pos.attackers_to_occ(to, slide) & pos.pieces_c(us);
    let attackers_wo_from = attackers & !Bitboard::from_square(from);
    attackers_wo_from.is_not_empty()
}

fn lance_must_promote(us: Color, to: Square) -> bool {
    match us {
        Color::Black => to.rank() == Rank::Rank1,
        Color::White => to.rank() == Rank::Rank9,
    }
}

fn knight_must_promote(us: Color, to: Square) -> bool {
    match us {
        Color::Black => to.rank() <= Rank::Rank2,
        Color::White => to.rank() >= Rank::Rank8,
    }
}

fn pawn_must_promote(us: Color, to: Square) -> bool {
    match us {
        Color::Black => to.rank() == Rank::Rank1,
        Color::White => to.rank() == Rank::Rank9,
    }
}

fn bb_attacks_and_check(
    pos: &Position,
    us: Color,
    from: Square,
    to: Square,
    sq_king: Square,
    slide: Bitboard,
    bb_attacks: Bitboard,
) -> bool {
    if !bb_attacks.contains(sq_king) {
        return false;
    }
    // from以外の利きがあるか
    has_other_attacker(pos, us, from, to, slide)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_move_mate_compile() {
        let _ = std::mem::size_of::<Option<crate::types::Move>>();
    }
}
