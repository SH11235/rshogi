//! seed 固定の差分検証。static の切替を使わず、並列テストから独立して各経路を呼ぶ。

use super::{attack_query::AttackQuery, mate_1ply_impl};
use crate::bitboard::{Bitboard, king_effect};
use crate::position::{
    Position,
    playout_test_support::{PERFT_MATSURI, PERFT_MIDGAME, RandomPlayout},
};
use crate::types::{Color, Square};

#[test]
fn all_modes_agree_on_forty_thousand_positions() {
    let mut positions = 0;
    let mut in_check = 0;
    let mut pinned = 0;
    let mut drops = 0;
    let mut promotions = 0;
    let mut mates = 0;
    let mut index = 0;
    while positions < 40_000 {
        let mut playout = RandomPlayout::new(0x4d41_5445_5632, index);
        if index % 3 == 1 {
            playout.pos.set_sfen(PERFT_MATSURI).unwrap();
        } else if index % 3 == 2 {
            playout.pos.set_sfen(PERFT_MIDGAME).unwrap();
        }
        for _ in 0..300 {
            let pos = &playout.pos;
            let legacy = mate_1ply_impl::<0>(pos);
            for actual in [mate_1ply_impl::<1>(pos), mate_1ply_impl::<2>(pos)] {
                assert_eq!(actual, legacy, "sfen={} {}", pos.to_sfen(), playout.describe());
            }
            mates += usize::from(legacy.is_some());
            in_check += usize::from(pos.in_check());
            for us in [Color::Black, Color::White] {
                pinned +=
                    usize::from((pos.blockers_for_king(us) & pos.pieces_c(us)).is_not_empty());
            }
            positions += 1;
            if positions == 40_000 {
                break;
            }
            let Some(mv) = playout.step() else { break };
            drops += usize::from(mv.is_drop());
            promotions += usize::from(mv.is_promotion());
        }
        index += 1;
    }
    assert!(in_check > 0 && pinned > 0 && drops > 0 && promotions > 0 && mates > 0);
    eprintln!(
        "positions={positions} checks={in_check} pinned={pinned} drops={drops} promotions={promotions} mates={mates}"
    );
}

fn assert_attacked(pos: &Position, us: Color, sq: Square, occ: Bitboard, exclude: Bitboard) {
    let expected = (pos.attackers_to_color_occ(us, sq, occ) & !exclude).is_not_empty();
    assert_eq!(
        pos.is_attacked_by_excluding(us, sq, occ, exclude),
        expected,
        "sfen={} us={us:?} sq={sq:?} occ={occ:?} exclude={exclude:?}",
        pos.to_sfen()
    );
}

#[test]
fn bool_query_matches_all_squares_and_exclusions() {
    // 全升・両色に対し、除外なし、全駒除外、全 81 升の単独除外を照合する。
    // 任意個数の除外は、その問い合わせの実際の attackers の全部分集合で網羅する。
    for index in 0..64 {
        let mut playout = RandomPlayout::new(0x4154_5441_434b, index);
        if index % 2 == 0 {
            playout.pos.set_sfen(PERFT_MATSURI).unwrap();
        }
        for _ in 0..index * 3 {
            if playout.step().is_none() {
                break;
            }
        }
        let pos = &playout.pos;
        for us in [Color::Black, Color::White] {
            for sq in Square::all() {
                for occ in [
                    pos.occupied(),
                    Bitboard::EMPTY,
                    Bitboard::ALL,
                    pos.occupied() & !Bitboard::from_square(pos.king_square(!us)),
                ] {
                    assert_attacked(pos, us, sq, occ, Bitboard::EMPTY);
                    assert_attacked(pos, us, sq, occ, Bitboard::ALL);
                    for from in Square::all() {
                        let excluded = Bitboard::from_square(from);
                        assert_attacked(pos, us, sq, occ, excluded);
                        // 移動元を抜き、移動先を置く問い合わせも同じ除外で検証する。
                        assert_attacked(
                            pos,
                            us,
                            sq,
                            (occ & !excluded) | Bitboard::from_square(sq),
                            excluded,
                        );
                    }
                    let attackers = pos.attackers_to_color_occ(us, sq, occ).as_u128();
                    let mut subset = attackers;
                    loop {
                        assert_attacked(
                            pos,
                            us,
                            sq,
                            occ,
                            Bitboard::new(subset as u64, (subset >> 64) as u64),
                        );
                        if subset == 0 {
                            break;
                        }
                        subset = (subset - 1) & attackers;
                    }
                }
            }
        }
    }
}

#[test]
fn context_matches_neighbor_queries_with_every_friendly_exclusion() {
    for index in 0..256 {
        let mut playout = RandomPlayout::new(0x4e45_4152_5632, index);
        playout
            .pos
            .set_sfen(if index % 2 == 0 {
                PERFT_MATSURI
            } else {
                PERFT_MIDGAME
            })
            .unwrap();
        for _ in 0..index % 100 {
            if playout.step().is_none() {
                break;
            }
        }
        let pos = &playout.pos;
        for us in [Color::Black, Color::White] {
            let mut query = AttackQuery::<2>::new();
            for sq in king_effect(pos.king_square(!us)).iter() {
                for from in std::iter::once(None).chain(pos.pieces_c(us).iter().map(Some)) {
                    let exclude = from.map_or(Bitboard::EMPTY, Bitboard::from_square);
                    let moved = (pos.occupied() & !exclude) | Bitboard::from_square(sq);
                    for occ in [
                        pos.occupied(),
                        moved,
                        moved & !Bitboard::from_square(pos.king_square(!us)),
                    ] {
                        let expected =
                            (pos.attackers_to_color_occ(us, sq, occ) & !exclude).is_not_empty();
                        assert_eq!(
                            query.attacked(pos, us, sq, occ, from),
                            expected,
                            "sfen={} us={us:?} sq={sq:?} from={from:?}",
                            pos.to_sfen()
                        );
                    }
                }
            }
        }
    }
}
