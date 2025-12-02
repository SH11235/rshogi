// 駒移動による1手詰め判定（近接王手のみを探索）
//
// YaneuraOu mate1ply_without_effect.cpp の移植（離し角・飛車は未対応）

use crate::bitboard::{
    bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect, knight_effect,
    lance_effect, rook_effect, silver_effect, Bitboard, FILE_BB, RANK_BB,
};
use crate::mate::helpers::{can_king_escape_with_from, can_piece_capture};
use crate::mate::tables::{check_cand_bb, PieceTypeCheck};
use crate::mate::{
    aligned, bishop_step_effect, can_promote, cross45_step_effect, lance_step_effect,
    rook_step_effect,
};
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

            let bb_attacks = if cross45_step_effect(sq_king).contains(to) {
                dragon_effect(to, slide)
            } else {
                rook_step_effect(to) | king_effect(to)
            };
            if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                continue;
            }
            if can_piece_capture(pos, them, to, new_pin, slide) {
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
                if cross45_step_effect(sq_king).contains(to) {
                    dragon_effect(to, slide)
                } else {
                    rook_step_effect(to) | king_effect(to)
                }
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
            if dc_candidates.contains(from) {
                // 両王手なので合い利かず
            } else if can_piece_capture(pos, them, to, new_pin, slide) {
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
            if dc_candidates.contains(from) {
                // 両王手なので合い利かず
            } else if can_piece_capture(pos, them, to, new_pin, slide) {
                continue;
            }

            return Some(Move::new_move(from, to, promote));
        }
    }

    // LANCE
    let mut bb =
        check_cand_bb(us, PieceTypeCheck::Lance, sq_king) & pos.pieces(us, PieceType::Lance);
    // 串刺し特例のため、玉と同筋の香も候補に含めておく
    bb |= pos.pieces(us, PieceType::Lance) & FILE_BB[sq_king.file().index()];
    while bb.is_not_empty() {
        let from = bb.pop();
        let slide = occupied ^ Bitboard::from_square(from);
        let bb_attacks_from = lance_effect(us, from, slide);
        let mut bb_check = bb_attacks_from & bb_move & gold_effect(them, sq_king);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            let bb_attacks = if can_promote(us, from, to) {
                gold_effect(us, to)
            } else {
                lance_step_effect(us, to)
            };
            if !bb_attacks.contains(sq_king) {
                // 敵陣3段目の不成り串刺し
                if (us == Color::Black && to.rank() == Rank::Rank3)
                    || (us == Color::White && to.rank() == Rank::Rank7)
                {
                    let bb_skewer = lance_step_effect(us, to);
                    if !bb_skewer.contains(sq_king) {
                        continue;
                    }
                    if pos.discovered(from, to, our_king, our_pinned) {
                        continue;
                    }
                    if can_king_escape_with_from(pos, them, from, to, bb_skewer, slide) {
                        continue;
                    }
                    if can_piece_capture(pos, them, to, pinned, slide) {
                        continue;
                    }
                    return Some(Move::new_move(from, to, false));
                }
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }
            if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                continue;
            }
            if dc_candidates.contains(from) {
                // 成って角との両王手
            } else if can_piece_capture(pos, them, to, pinned, slide) {
                continue;
            }

            if can_promote(us, from, to) {
                return Some(Move::new_move(from, to, true));
            } else {
                return Some(Move::new_move(from, to, false));
            }
        }

        // 敵陣3段目の不成り串刺し（成っても金では届かない場合のみ）
        let rank_mask = if us == Color::Black {
            RANK_BB[2]
        } else {
            RANK_BB[6]
        };
        let mut bb_skewer = bb_attacks_from & bb_move & rank_mask;
        while bb_skewer.is_not_empty() {
            let to = bb_skewer.pop();
            if gold_effect(us, to).contains(sq_king) {
                continue;
            }
            let bb_skewer_attacks = lance_step_effect(us, to);
            if !bb_skewer_attacks.contains(sq_king) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }
            if can_king_escape_with_from(pos, them, from, to, bb_skewer_attacks, slide) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, slide) {
                continue;
            }
            return Some(Move::new_move(from, to, false));
        }
    }

    // GOLD相当（Gold/ProPawn/ProLance/ProKnight/ProSilver）
    let gold_like = pos.pieces(us, PieceType::Gold)
        | pos.pieces(us, PieceType::ProPawn)
        | pos.pieces(us, PieceType::ProLance)
        | pos.pieces(us, PieceType::ProKnight)
        | pos.pieces(us, PieceType::ProSilver);
    let mut bb = check_cand_bb(us, PieceTypeCheck::Gold, sq_king) & gold_like;
    while bb.is_not_empty() {
        let from = bb.pop();
        let mut bb_check = gold_effect(us, from) & gold_effect(them, sq_king) & bb_move; // 近接のみ
        if bb_check.is_empty() {
            continue;
        }
        let slide = occupied ^ Bitboard::from_square(from);
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }
            let bb_attacks = gold_effect(us, to);
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

    // SILVER
    let mut bb =
        check_cand_bb(us, PieceTypeCheck::Silver, sq_king) & pos.pieces(us, PieceType::Silver);
    while bb.is_not_empty() {
        let from = bb.pop();
        let mut bb_check = silver_effect(us, from) & bb_move & king_effect(sq_king); // 近接のみ
        if bb_check.is_empty() {
            continue;
        }
        let slide = occupied ^ Bitboard::from_square(from);
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            let bb_attacks_s = silver_effect(us, to);
            if bb_attacks_s.contains(sq_king)
                && has_other_attacker(pos, us, from, to, slide)
                && !pos.discovered(from, to, our_king, our_pinned)
                && !can_king_escape_with_from(pos, them, from, to, bb_attacks_s, slide)
                && (dc_candidates.contains(from) && !aligned(from, to, sq_king)
                    || !can_piece_capture(pos, them, to, new_pin, slide))
            {
                return Some(Move::new_move(from, to, false));
            }

            if can_promote(us, from, to) {
                let bb_attacks_g = gold_effect(us, to);
                if bb_attacks_g.contains(sq_king)
                    && has_other_attacker(pos, us, from, to, slide)
                    && !pos.discovered(from, to, our_king, our_pinned)
                    && !can_king_escape_with_from(pos, them, from, to, bb_attacks_g, slide)
                    && (dc_candidates.contains(from) && !aligned(from, to, sq_king)
                        || !can_piece_capture(pos, them, to, new_pin, slide))
                {
                    return Some(Move::new_move(from, to, true));
                }
            }
        }
    }

    // KNIGHT
    let mut bb =
        check_cand_bb(us, PieceTypeCheck::Knight, sq_king) & pos.pieces(us, PieceType::Knight);
    while bb.is_not_empty() {
        let from = bb.pop();
        let mut bb_check = knight_effect(us, from) & bb_move; // 近接のみ
        if bb_check.is_empty() {
            continue;
        }
        let slide = occupied ^ Bitboard::from_square(from);
        let new_pin = pos.pinned_pieces_excluding(them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            let bb_attacks = knight_effect(us, to);
            if bb_attacks.contains(sq_king)
                && !pos.discovered(from, to, our_king, our_pinned)
                && !can_king_escape_with_from(pos, them, from, to, bb_attacks, slide)
                && (dc_candidates.contains(from)
                    || !can_piece_capture(pos, them, to, new_pin, slide))
            {
                return Some(Move::new_move(from, to, false));
            }

            if can_promote(us, from, to) {
                let bb_attacks_g = gold_effect(us, to);
                if bb_attacks_g.contains(sq_king)
                    && !pos.discovered(from, to, our_king, our_pinned)
                    && !can_king_escape_with_from(pos, them, from, to, bb_attacks_g, slide)
                    && (dc_candidates.contains(from)
                        || !can_piece_capture(pos, them, to, new_pin, slide))
                {
                    return Some(Move::new_move(from, to, true));
                }
            }
        }
    }

    // PAWN（不成）
    if (check_cand_bb(us, PieceTypeCheck::PawnWithNoPro, sq_king) & pos.pieces(us, PieceType::Pawn))
        .is_not_empty()
    {
        let delta_to = if us == Color::Black {
            Square::DELTA_D
        } else {
            Square::DELTA_U
        };
        if let Some(to) = sq_king.offset(delta_to) {
            if pos.pieces_c(us).contains(to) {
                // 味方駒がいる
            } else if let Some(from) = to.offset(delta_to) {
                if !pos.pieces(us, PieceType::Pawn).contains(from) {
                    // 候補に歩がない
                } else if can_promote(us, from, to) {
                    // 成りでの判定に任せる
                } else {
                    let slide = occupied ^ Bitboard::from_square(from);
                    if has_other_attacker(pos, us, from, to, slide)
                        && !pos.discovered(from, to, our_king, our_pinned)
                        && !can_king_escape_with_from(pos, them, from, to, Bitboard::EMPTY, slide)
                        && !can_piece_capture(pos, them, to, pinned, slide)
                    {
                        return Some(Move::new_move(from, to, false));
                    }
                }
            }
        }
    }

    // PAWN（成り）
    let mut bb =
        check_cand_bb(us, PieceTypeCheck::PawnWithPro, sq_king) & pos.pieces(us, PieceType::Pawn);
    while bb.is_not_empty() {
        let from = bb.pop();
        let delta_to = if us == Color::Black {
            Square::DELTA_U
        } else {
            Square::DELTA_D
        };
        if let Some(to) = from.offset(delta_to) {
            if pos.pieces_c(us).contains(to) {
                continue;
            }

            let bb_attacks = gold_effect(us, to);
            if !bb_attacks.contains(sq_king) {
                continue;
            }
            let slide = occupied ^ Bitboard::from_square(from);
            if !has_other_attacker(pos, us, from, to, slide) {
                continue;
            }
            if pos.discovered(from, to, our_king, our_pinned) {
                continue;
            }
            if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                continue;
            }
            if can_piece_capture(pos, them, to, pinned, slide) {
                continue;
            }
            return Some(Move::new_move(from, to, true));
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

#[cfg(test)]
mod tests {
    use crate::position::Position;
    use crate::types::{File, Rank, Square};

    #[test]
    fn test_move_mate_compile() {
        let _ = std::mem::size_of::<Option<crate::types::Move>>();
    }

    #[test]
    fn test_knight_promo_gold_check_from_candidate_table() {
        // 先手: 桂2四・銀3二・金2三・玉5九 / 後手: 玉1一
        // 桂2四→1二成（金）がCHECK_CAND_BB由来で検出されることを確認
        let sfen = "8k/6S2/7G1/7N1/9/9/9/9/4K4 b - 1";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();

        let mv = super::check_move_mate(&pos, crate::types::Color::Black);
        let mv_usi = mv.map(|m| m.to_usi());
        let expected = crate::types::Move::new_move(
            Square::new(File::File2, Rank::Rank4),
            Square::new(File::File1, Rank::Rank2),
            true,
        );
        assert_eq!(mv, Some(expected), "mv_usi={:?}", mv_usi);
    }
}
