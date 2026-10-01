// 駒移動による1手詰め判定（近接王手のみを探索）
//
// YaneuraOu mate1ply_without_effect.cpp の移植（離し角・飛車は未対応）

use crate::bitboard::{
    Bitboard, bishop_effect, dragon_effect, gold_effect, horse_effect, king_effect, knight_effect,
    lance_effect, rook_effect, silver_effect,
};
use crate::mate::helpers::{can_king_escape_with_from, can_piece_capture};
use crate::mate::tables::{PieceTypeCheck, check_cand_bb};
use crate::mate::{
    aligned, bishop_step_effect, can_promote, cross45_step_effect, lance_step_effect,
    rook_step_effect,
};
use crate::position::Position;
use crate::types::{Color, Move, PieceType, Rank, Square};

/// 同じ駒の候補升・成り不成りで共有する pin 情報。
struct PinCache<const MODE: u8> {
    value: Bitboard,
    ready: bool,
}

impl<const MODE: u8> PinCache<MODE> {
    #[inline(always)]
    fn new(pos: &Position, them: Color, from: Square) -> Self {
        let value = match MODE {
            0 => pos.pinned_pieces_excluding(them, from),
            1 => pinned_pieces_excluding_inline(pos, them, from),
            _ => Bitboard::EMPTY,
        };
        Self {
            value,
            ready: MODE != 2,
        }
    }

    #[inline(always)]
    fn get(&mut self, pos: &Position, them: Color, from: Square) -> Bitboard {
        if MODE == 2 && !self.ready {
            self.value = pinned_pieces_excluding_inline(pos, them, from);
            self.ready = true;
        }
        self.value
    }
}

/// `Position::pinned_pieces_excluding` と同じ計算を mate 内に展開する。
/// 他の呼出し元の codegen を維持するため、元の関数には inline 属性を付けない。
#[inline(always)]
fn pinned_pieces_excluding_inline(pos: &Position, them: Color, avoid: Square) -> Bitboard {
    let avoid_not = !Bitboard::from_square(avoid);
    let ksq = pos.king_square(them);
    let enemy = !them;

    let lance_bb = pos.pieces(enemy, PieceType::Lance) & avoid_not;
    let bishop_bb = (pos.bishop_horse() & pos.pieces_c(enemy)) & avoid_not;
    let rook_bb = (pos.rook_dragon() & pos.pieces_c(enemy)) & avoid_not;
    let pinners = (lance_step_effect(them, ksq) & lance_bb)
        | (bishop_step_effect(ksq) & bishop_bb)
        | (rook_step_effect(ksq) & rook_bb);

    // avoid が pinner 自身の場合も、占有と pinner 候補の両方から除く。
    let pieces_without_avoid = pos.occupied() & avoid_not;
    let mut result = Bitboard::EMPTY;
    for pinner_sq in pinners.iter() {
        let between = crate::bitboard::between_bb(ksq, pinner_sq) & pieces_without_avoid;
        if !between.more_than_one() {
            result |= between & pos.pieces_c(them);
        }
    }
    result
}

/// 駒移動による1手詰めを判定（非打ち手のみ対象）。
///
/// `PIN_MODE` は 0: 従来の関数呼出し、1: inline、2: inline + 必要時に計算。
pub fn check_move_mate<const PIN_MODE: u8>(pos: &Position, us: Color) -> Option<Move> {
    const { assert!(PIN_MODE <= 2) };
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
        let mut new_pin = PinCache::<PIN_MODE>::new(pos, them, from);

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
            if can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide) {
                continue;
            }
            return Some(Move::new_move(from, to, false));
        }
    }

    // ROOK
    for from in pos.pieces(us, PieceType::Rook).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = rook_effect(from, slide) & bb_move & king_effect(sq_king); // 近接のみ
        let mut new_pin = PinCache::<PIN_MODE>::new(pos, them, from);

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
            } else if can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide) {
                continue;
            }

            return Some(Move::new_move(from, to, promote));
        }
    }

    // HORSE
    for from in pos.pieces(us, PieceType::Horse).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = horse_effect(from, slide) & bb_move & king_effect(sq_king); // 近接のみ
        let mut new_pin = PinCache::<PIN_MODE>::new(pos, them, from);

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
            } else if can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide) {
                continue;
            }

            return Some(Move::new_move(from, to, false));
        }
    }

    // BISHOP
    for from in pos.pieces(us, PieceType::Bishop).iter() {
        let slide = occupied ^ Bitboard::from_square(from);
        let mut bb_check = bishop_effect(from, slide) & bb_move & king_effect(sq_king); // 近接のみ
        let mut new_pin = PinCache::<PIN_MODE>::new(pos, them, from);

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
            } else if can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide) {
                continue;
            }

            return Some(Move::new_move(from, to, promote));
        }
    }

    // LANCE（成りで詰まない場合、不成り串刺しも試す）
    let mut bb =
        check_cand_bb(us, PieceTypeCheck::Lance, sq_king) & pos.pieces(us, PieceType::Lance);
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

            // 成り（または不成り）の利きが王を含むか
            if !bb_attacks.contains(sq_king) {
                // 成りでは王手にならない場合、直接 LANCE_NO_PRO へ
            } else {
                // toに味方の利きがfrom以外にない場合はスキップ
                // （toの駒が取られると王手が残らない）
                let attackers_to_us =
                    pos.attackers_to_color_occ(us, to, slide) ^ Bitboard::from_square(from);
                if attackers_to_us.is_empty() {
                    // toが味方利きで守られていない → LANCE_NO_PRO へ
                } else if pos.discovered(from, to, our_king, our_pinned) {
                    // 自玉が素抜かれる → LANCE_NO_PRO へ
                } else if can_king_escape_with_from(pos, them, from, to, bb_attacks, slide) {
                    // 玉が逃げられる → LANCE_NO_PRO へ
                } else if dc_candidates.contains(from) {
                    // 両王手なので合い利かず → 詰み
                    return Some(Move::new_move(from, to, can_promote(us, from, to)));
                } else if can_piece_capture(pos, them, to, pinned, slide) {
                    // toの駒が取れる → LANCE_NO_PRO へ
                } else {
                    // 成りで詰み
                    return Some(Move::new_move(from, to, can_promote(us, from, to)));
                }
            }

            // LANCE_NO_PRO: 敵陣で不成りで串刺しにする王手
            if (us == Color::Black && to.rank() == Rank::Rank3)
                || (us == Color::White && to.rank() == Rank::Rank7)
            {
                let bb_skewer = lance_step_effect(us, to);
                if !bb_skewer.contains(sq_king) {
                    continue;
                }
                let attackers_to_us =
                    pos.attackers_to_color_occ(us, to, slide) ^ Bitboard::from_square(from);
                if attackers_to_us.is_empty() {
                    continue;
                }
                if pos.discovered(from, to, our_king, our_pinned) {
                    continue;
                }
                if can_king_escape_with_from(pos, them, from, to, bb_skewer, slide) {
                    continue;
                }
                // 串刺しでの両王手はありえない
                if can_piece_capture(pos, them, to, pinned, slide) {
                    continue;
                }
                return Some(Move::new_move(from, to, false));
            }
        }
    }

    // GOLD相当（Gold/ProPawn/ProLance/ProKnight/ProSilver）
    let gold_like = pos.golds_c(us);
    let mut bb = check_cand_bb(us, PieceTypeCheck::Gold, sq_king) & gold_like;
    while bb.is_not_empty() {
        let from = bb.pop();
        let mut bb_check = gold_effect(us, from) & gold_effect(them, sq_king) & bb_move; // 近接のみ
        if bb_check.is_empty() {
            continue;
        }
        let slide = occupied ^ Bitboard::from_square(from);
        let mut new_pin = PinCache::<PIN_MODE>::new(pos, them, from);

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
            } else if can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide) {
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
        let mut new_pin = PinCache::<PIN_MODE>::new(pos, them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            let bb_attacks_s = silver_effect(us, to);
            if bb_attacks_s.contains(sq_king)
                && has_other_attacker(pos, us, from, to, slide)
                && !pos.discovered(from, to, our_king, our_pinned)
                && !can_king_escape_with_from(pos, them, from, to, bb_attacks_s, slide)
                && (dc_candidates.contains(from) && !aligned(from, to, sq_king)
                    || !can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide))
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
                        || !can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide))
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
        let mut new_pin = PinCache::<PIN_MODE>::new(pos, them, from);

        while bb_check.is_not_empty() {
            let to = bb_check.pop();
            let bb_attacks = knight_effect(us, to);
            if bb_attacks.contains(sq_king)
                && !pos.discovered(from, to, our_king, our_pinned)
                && !can_king_escape_with_from(pos, them, from, to, bb_attacks, slide)
                && (dc_candidates.contains(from)
                    || !can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide))
            {
                return Some(Move::new_move(from, to, false));
            }

            if can_promote(us, from, to) {
                let bb_attacks_g = gold_effect(us, to);
                if bb_attacks_g.contains(sq_king)
                    && has_other_attacker(pos, us, from, to, slide)
                    && !pos.discovered(from, to, our_king, our_pinned)
                    && !can_king_escape_with_from(pos, them, from, to, bb_attacks_g, slide)
                    && (dc_candidates.contains(from)
                        || !can_piece_capture(pos, them, to, new_pin.get(pos, them, from), slide))
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
    let attackers = pos.attackers_to_color_occ(us, to, slide);
    let attackers_wo_from = attackers & !Bitboard::from_square(from);
    attackers_wo_from.is_not_empty()
}

#[cfg(test)]
mod tests {
    use super::{PinCache, check_move_mate, pinned_pieces_excluding_inline};
    use crate::position::Position;
    use crate::types::{Color, PieceType, Square};

    #[test]
    fn pin_modes_agree_on_random_legal_positions() {
        use crate::position::playout_test_support::RandomPlayout;

        let mut positions = 0;
        let mut coverage = [[0; 2]; 2];
        let mut found = [0; 2];
        let mut piece_types = [false; 15];
        for index in 0..40 {
            let mut playout = RandomPlayout::new(0x4D41_5445_2026, index);
            for _ in 0..300 {
                let pos = &playout.pos;
                let us = pos.side_to_move();
                let baseline = check_move_mate::<0>(pos, us);
                assert_eq!(baseline, check_move_mate::<1>(pos, us), "{}", playout.describe());
                assert_eq!(baseline, check_move_mate::<2>(pos, us), "{}", playout.describe());
                coverage[us.index()][usize::from(pos.in_check())] += 1;
                found[us.index()] += usize::from(baseline.is_some());
                for from in pos.pieces_c(us).iter() {
                    piece_types[pos.piece_on(from).piece_type() as usize] = true;
                    // mate が pin 計算に達しない局面でも、inline 版の同値性を検証する。
                    assert_eq!(
                        pos.pinned_pieces_excluding(!us, from),
                        pinned_pieces_excluding_inline(pos, !us, from),
                        "from={from:?} {}",
                        playout.describe()
                    );
                }
                positions += 1;
                if playout.step().is_none() {
                    break;
                }
            }
        }
        assert!(positions >= 5000, "局面数不足: {positions}");
        assert!(coverage.iter().flatten().all(|&count| count > 0), "{coverage:?}");
        assert!(found.iter().all(|&count| count > 0), "移動による詰みが不足: {found:?}");
        for pt in [
            PieceType::Rook,
            PieceType::Bishop,
            PieceType::Dragon,
            PieceType::Horse,
            PieceType::ProPawn,
            PieceType::ProLance,
            PieceType::ProKnight,
            PieceType::ProSilver,
        ] {
            assert!(piece_types[pt as usize], "標本に {pt:?} が無い");
        }
    }

    #[test]
    fn lazy_pin_is_computed_on_first_use_and_cached() {
        let mut pos = Position::new();
        pos.set_sfen("4k4/4g4/4+R1S2/9/9/9/9/9/K8 b - 1").unwrap();
        let them = Color::White;
        let from = Square::new(crate::types::File::File3, crate::types::Rank::Rank3);
        let expected = pos.pinned_pieces_excluding(them, from);
        assert!(expected.is_not_empty());
        let mut cache = PinCache::<2>::new(&pos, them, from);
        assert!(!cache.ready);
        assert_eq!(cache.get(&pos, them, from), expected);
        assert!(cache.ready);
        // 計算済みの値（空の bitboard も含む）は次の候補で再計算しない。
        cache.value = crate::bitboard::Bitboard::EMPTY;
        assert!(cache.get(&pos, them, from).is_empty());
    }

    #[test]
    fn test_lance_promo_mate_6f6g() {
        // 後手番: 6fの後手香が6g成で詰み (成金が7gの先手玉に王手)
        // RS の mate_1ply がこの詰みを検出できないバグの再現テスト
        let sfen =
            "1+B1g3nl/3r1kg2/1s2pp1p1/2p2bs1p/p4Np2/1SPl1P1PP/1GK1PS3/7R1/1N3G2L w NL3P3p 56";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();

        let mv = super::check_move_mate::<2>(&pos, crate::types::Color::White);
        assert!(mv.is_some(), "mate_1ply should find 6f6g+ (lance promotion to gold)");
        let mv = mv.unwrap();
        assert_eq!(mv.to_usi(), "6f6g+", "expected move 6f6g+");
    }

    #[test]
    fn test_knight_promo_gold_check_from_candidate_table() {
        // 先手: 桂2四・銀3二・金2三・玉5九 / 後手: 玉1一
        // 桂2四→1二成（金）と金2三→1二の両方が有効な詰みとして検出される
        // CHECK_CAND_BB由来で金相当の駒が候補として列挙されることの確認
        let sfen = "8k/6S2/7G1/7N1/9/9/9/9/4K4 b - 1";
        let mut pos = Position::new();
        pos.set_sfen(sfen).unwrap();

        let mv = super::check_move_mate::<2>(&pos, crate::types::Color::Black);
        assert!(mv.is_some(), "mate should be found");
    }
}
