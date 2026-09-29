//! 1 手詰め候補表のコンパイル時構築。

use crate::bitboard::{
    BISHOP_STEP, Bitboard, GOLD_EFFECT, KING_EFFECT, KNIGHT_EFFECT, PAWN_EFFECT, RANK_BB,
    ROOK_STEP, SILVER_EFFECT,
};
use crate::types::{PieceType, Square};

use super::tables::PieceTypeCheck;

// const 評価では演算子 trait を呼べないため、構築中だけ u128 で集合を扱う。
const fn bb(bits: u128) -> Bitboard {
    Bitboard::new(bits as u64, (bits >> 64) as u64)
}

const fn bit(sq: usize) -> u128 {
    Bitboard::from_square(Square::from_u8(sq as u8).unwrap()).as_u128()
}

const fn lance(color: usize, sq: usize) -> u128 {
    let mut result = 0;
    let mut rank = 0;
    while rank < 9 {
        if (color == 0 && rank < sq % 9) || (color == 1 && rank > sq % 9) {
            result |= bit(sq / 9 * 9 + rank);
        }
        rank += 1;
    }
    result
}

const fn two_hops(targets: u128, effects: &[Bitboard; 81]) -> u128 {
    let mut result = 0;
    let mut remaining = bb(targets);
    while remaining.is_not_empty() {
        let sq = remaining.lsb_unchecked();
        result |= effects[sq.index()].as_u128();
        remaining = Bitboard::from_square(sq).andnot(remaining);
    }
    result
}

pub(super) const fn check_candidates() -> [[[Bitboard; 2]; PieceTypeCheck::NUM]; 81] {
    let mut table = [[[Bitboard::EMPTY; 2]; PieceTypeCheck::NUM]; 81];
    let mut sq = 0;
    while sq < 81 {
        let mut us = 0;
        while us < 2 {
            let them = 1 - us;
            let first_rank = if us == 0 { 0 } else { 6 };
            let enemy = RANK_BB[first_rank].as_u128()
                | RANK_BB[first_rank + 1].as_u128()
                | RANK_BB[first_rank + 2].as_u128();
            let promo = GOLD_EFFECT[them][sq].as_u128() & enemy;
            let pawn_no_pro =
                two_hops(PAWN_EFFECT[them][sq].as_u128() & !enemy, &PAWN_EFFECT[them]);
            let pawn_pro = two_hops(promo, &PAWN_EFFECT[them]);
            let mut lance_bb = lance(them, sq);
            if enemy & bit(sq) != 0 {
                if sq / 9 > 0 {
                    lance_bb |= lance(them, sq - 9);
                }
                if sq / 9 < 8 {
                    lance_bb |= lance(them, sq + 9);
                }
            }
            let knight = two_hops(KNIGHT_EFFECT[them][sq].as_u128() | promo, &KNIGHT_EFFECT[them]);
            let mut silver =
                two_hops(SILVER_EFFECT[them][sq].as_u128() | promo, &SILVER_EFFECT[them]);
            if sq % 9 == (if us == 0 { 3 } else { 5 }) {
                let base = sq / 9 * 9 + if us == 0 { 2 } else { 6 };
                silver |= bit(base) | (BISHOP_STEP[base].as_u128() & KING_EFFECT[base].as_u128());
                if sq / 9 < 7 {
                    silver |= bit(base + 18);
                }
                if sq / 9 >= 2 {
                    silver |= bit(base - 18);
                }
            }
            if sq % 9 == 4 {
                silver |= KNIGHT_EFFECT[us][sq].as_u128();
            }
            let gold = two_hops(GOLD_EFFECT[them][sq].as_u128(), &GOLD_EFFECT[them]) & !bit(sq);
            table[sq][PieceTypeCheck::PawnWithNoPro as usize][us] = bb(pawn_no_pro);
            table[sq][PieceTypeCheck::PawnWithPro as usize][us] = bb(pawn_pro);
            table[sq][PieceTypeCheck::Lance as usize][us] = bb(lance_bb);
            table[sq][PieceTypeCheck::Knight as usize][us] = bb(knight);
            table[sq][PieceTypeCheck::Silver as usize][us] = bb(silver);
            table[sq][PieceTypeCheck::Gold as usize][us] = bb(gold);
            table[sq][PieceTypeCheck::Bishop as usize][us] = BISHOP_STEP[sq];
            table[sq][PieceTypeCheck::Rook as usize][us] = ROOK_STEP[sq];
            table[sq][PieceTypeCheck::ProBishop as usize][us] =
                bb(BISHOP_STEP[sq].as_u128() | KING_EFFECT[sq].as_u128());
            table[sq][PieceTypeCheck::ProRook as usize][us] =
                bb(ROOK_STEP[sq].as_u128() | KING_EFFECT[sq].as_u128());
            table[sq][PieceTypeCheck::NonSlider as usize][us] =
                bb(pawn_no_pro | pawn_pro | knight | silver | gold);
            us += 1;
        }
        sq += 1;
    }
    table
}

pub(super) const fn check_around() -> [[[Bitboard; 2]; PieceType::NUM + 1]; 81] {
    let mut table = [[[Bitboard::EMPTY; 2]; PieceType::NUM + 1]; 81];
    let mut sq = 0;
    while sq < 81 {
        let mut us = 0;
        while us < 2 {
            let them = 1 - us;
            let mut pt = 1;
            while pt <= PieceType::NUM {
                let mut result = 0;
                let mut near = KING_EFFECT[sq];
                while near.is_not_empty() {
                    let dest = near.lsb_unchecked();
                    near = Bitboard::from_square(dest).andnot(near);
                    let idx = dest.index();
                    result |= match PieceType::from_u8(pt as u8).unwrap() {
                        PieceType::Pawn => PAWN_EFFECT[them][idx].as_u128(),
                        PieceType::Lance => lance(them, idx),
                        PieceType::Knight => KNIGHT_EFFECT[them][idx].as_u128(),
                        PieceType::Silver => SILVER_EFFECT[them][idx].as_u128(),
                        PieceType::Bishop => BISHOP_STEP[idx].as_u128(),
                        PieceType::Rook => ROOK_STEP[idx].as_u128(),
                        PieceType::Horse => BISHOP_STEP[idx].as_u128() | KING_EFFECT[idx].as_u128(),
                        PieceType::Dragon => ROOK_STEP[idx].as_u128() | KING_EFFECT[idx].as_u128(),
                        PieceType::King => KING_EFFECT[idx].as_u128(),
                        _ => GOLD_EFFECT[them][idx].as_u128(),
                    };
                }
                table[sq][pt][us] = bb(result & !bit(sq));
                pt += 1;
            }
            us += 1;
        }
        sq += 1;
    }
    table
}
