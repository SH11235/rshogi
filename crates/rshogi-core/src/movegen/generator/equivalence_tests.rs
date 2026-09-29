//! 生成順序と駒情報を含む、新旧実装の直接比較。

use super::*;
use crate::movegen::GenType;
use crate::position::SFEN_HIRATE;
use crate::position::playout_test_support::{PERFT_MATSURI, PERFT_MIDGAME, RandomPlayout};

#[test]
fn pawn_shift_matches_effect_on_every_square() {
    for us in [Color::Black, Color::White] {
        for from in Square::all() {
            let actual = pawn_pushes(us, Bitboard::from_square(from));
            assert_eq!(actual, pawn_effect(us, from), "{us:?} {from:?}");
            for to in actual.iter() {
                assert_eq!(from.file(), to.file());
            }
        }
        // 密な配置でも筋をまたがず、p0 bit63 / p1 bit18 以降を立てない。
        for sources in [Bitboard::ALL, rank1_bb(us), !rank1_bb(us)] {
            let expected =
                sources.iter().fold(Bitboard::EMPTY, |acc, from| acc | pawn_effect(us, from));
            assert_eq!(pawn_pushes(us, sources), expected, "{us:?}");
        }
    }
}

// ExtMove の PartialEq はスコアのみを比較するので、指し手を32bitで取り出す。
fn entries(buffer: &ExtMoveBuffer) -> Vec<(u32, i32)> {
    buffer.iter().map(|ext| (ext.mv.raw32(), ext.value)).collect()
}

fn compare_type(pos: &Position, gen_type: GenType, recapture_sq: Option<Square>) {
    let mut old = ExtMoveBuffer::new();
    let mut new = ExtMoveBuffer::new();
    // 既存の要素を残して末尾に追加する契約も確認する。
    old.push(super::super::types::ExtMove::new(Move::NULL, 123));
    new.push(super::super::types::ExtMove::new(Move::NULL, 123));
    let old_len = generate_with_type_impl::<false>(pos, gen_type, &mut old, recapture_sq);
    let new_len = generate_with_type_impl::<true>(pos, gen_type, &mut new, recapture_sq);
    assert_eq!(old_len, new_len, "{gen_type:?} {:?}", pos.to_sfen());
    assert_eq!(
        entries(&old),
        entries(&new),
        "{gen_type:?} target={recapture_sq:?} {:?}",
        pos.to_sfen()
    );
}

fn compare_position(pos: &Position, recapture_sq: Square) {
    use GenType::*;
    for gen_type in [
        Quiets,
        Captures,
        QuietsAll,
        CapturesAll,
        CapturesProPlus,
        QuietsProMinus,
        CapturesProPlusAll,
        QuietsProMinusAll,
        Evasions,
        EvasionsAll,
        NonEvasions,
        NonEvasionsAll,
        Legal,
        LegalAll,
        Checks,
        ChecksAll,
        QuietChecks,
        QuietChecksAll,
        Recaptures,
        RecapturesAll,
    ] {
        // 各生成器の呼出し側と同じ、王手の有無に関する前提を守る。
        match gen_type {
            Evasions | EvasionsAll if !pos.in_check() => continue,
            Legal | LegalAll | Evasions | EvasionsAll => {}
            _ if pos.in_check() => continue,
            _ => {}
        }
        compare_type(pos, gen_type, Some(recapture_sq));
    }

    type BufferGenerator = fn(&Position, &mut ExtMoveBuffer) -> usize;
    let generators: [(BufferGenerator, BufferGenerator); 2] = [
        (generate_all_impl::<false>, generate_all_impl::<true>),
        if pos.in_check() {
            (generate_evasions_impl::<false>, generate_evasions_impl::<true>)
        } else {
            (generate_non_evasions_impl::<false>, generate_non_evasions_impl::<true>)
        },
    ];
    for (old_gen, new_gen) in generators {
        let mut old = ExtMoveBuffer::new();
        let mut new = ExtMoveBuffer::new();
        assert_eq!(old_gen(pos, &mut old), new_gen(pos, &mut new));
        assert_eq!(entries(&old), entries(&new), "{:?}", pos.to_sfen());
    }

    type ListGenerator = fn(&Position, &mut MoveList);
    let generators: [(ListGenerator, ListGenerator); 4] = [
        (generate_legal_impl::<false>, generate_legal_impl::<true>),
        (generate_legal_all_impl::<false>, generate_legal_all_impl::<true>),
        (generate_legal_with_pass_impl::<false>, generate_legal_with_pass_impl::<true>),
        (
            generate_legal_all_with_pass_impl::<false>,
            generate_legal_all_with_pass_impl::<true>,
        ),
    ];
    for (old_gen, new_gen) in generators {
        let mut old = MoveList::new();
        let mut new = MoveList::new();
        old.push(Move::NULL);
        new.push(Move::NULL);
        old_gen(pos, &mut old);
        new_gen(pos, &mut new);
        assert_eq!(old.as_slice(), new.as_slice(), "{:?}", pos.to_sfen());
    }
}

#[test]
fn fixed_positions_preserve_move_order() {
    for sfen in [
        SFEN_HIRATE,
        PERFT_MATSURI,
        PERFT_MIDGAME,
        // 既存 perft の王手回避・打ち歩詰め・歩打ちによる角筋遮断。
        "lnsgkgsnl/1r5b1/pppp1pppp/9/4R4/9/PPPP1PPPP/1B7/LNSGKGSNL w Pp 1",
        "R6sk/9/7G1/9/9/9/9/9/4K4 b P 1",
        "R8/2G1k4/9/2S6/2BN2N2/9/9/9/4K4 b P 1",
        // 金相当の全駒種・馬・龍・角・飛を升順で混在させる。
        "4k4/9/9/+P1+L1+N1+S2/1+B1+R1G1B1/8R/9/9/4K4 b - 1",
        // 両王手。
        "4k4/9/9/9/4r4/9/2b6/9/4K4 b - 1",
        // 成り・不成・端段と、7筋/8筋の語境界を含む歩。
        "4k4/PP1P1P1PP/9/9/9/9/9/9/4K4 b - 1",
    ] {
        // 盤と駒色を反転して、同じ境界条件を両手番で通す。
        let parts: Vec<_> = sfen.split_whitespace().collect();
        let board: String = parts[0]
            .chars()
            .rev()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        // 成り記号は逆順で駒の後ろに来るため、SFEN の接頭辞へ戻す。
        let mut inverted_board = String::new();
        let mut chars = board.chars().peekable();
        while let Some(c) = chars.next() {
            if chars.peek() == Some(&'+') {
                inverted_board.push(chars.next().unwrap());
            }
            inverted_board.push(c);
        }
        let hand: String = parts[2]
            .chars()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        let inverted =
            format!("{inverted_board} {} {hand} 1", if parts[1] == "b" { "w" } else { "b" });
        for sfen in [sfen, &inverted] {
            for rights in [0, 2] {
                let mut pos = Position::new();
                pos.set_sfen_with_pass_rights(sfen, rights, rights).unwrap();
                compare_position(&pos, Square::SQ_55);
                if !pos.in_check() {
                    for sq in Square::all() {
                        compare_type(&pos, GenType::Recaptures, Some(sq));
                        compare_type(&pos, GenType::RecapturesAll, Some(sq));
                    }
                }
            }
        }
    }
}

#[test]
fn random_playouts_preserve_move_order() {
    let mut checked = 0;
    let mut index = 0;
    let mut coverage = [[0; 2]; 2];
    let mut promotions = [0; 2];
    while checked < 8192 {
        let mut playout = RandomPlayout::new(0x6d6f_7665_6765_6e32, index);
        for ply in 0..256 {
            let pos = &playout.pos;
            let sq = playout.moves().last().map_or(Square::SQ_55, |mv| mv.to());
            compare_position(pos, sq);
            coverage[pos.side_to_move().index()][usize::from(pos.in_check())] += 1;
            let mut legal = MoveList::new();
            generate_legal_all_impl::<false>(pos, &mut legal);
            promotions[pos.side_to_move().index()] +=
                legal.iter().filter(|mv| mv.is_promotion()).count();
            checked += 1;
            if checked == 8192 || ply == 255 || playout.step().is_none() {
                break;
            }
        }
        index += 1;
    }
    assert!(coverage.iter().flatten().all(|&count| count >= 100), "{coverage:?}");
    assert!(promotions.iter().all(|&count| count >= 100), "{promotions:?}");
    eprintln!("比較局面数={checked} 手番別[通常, 王手]={coverage:?} 成り手数={promotions:?}");
}
