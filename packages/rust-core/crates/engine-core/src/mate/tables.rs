// 1手詰め探索用の初期化テーブル

use crate::bitboard::{
    bishop_effect, gold_effect, king_effect, knight_effect, lance_effect, pawn_effect, rook_effect,
    silver_effect, Bitboard,
};
use crate::types::{Color, PieceType, Square};
use std::sync::LazyLock;

/// 王手になる駒の種類（PieceTypeCheckの列挙）
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceTypeCheck {
    /// 不成りのまま王手になるところ（成れる場合は含まず）
    PawnWithNoPro = 0,
    /// 成りで王手になるところ
    PawnWithPro = 1,
    /// 香での王手
    Lance = 2,
    /// 桂での王手
    Knight = 3,
    /// 銀での王手
    Silver = 4,
    /// 金での王手
    Gold = 5,
    /// 角での王手
    Bishop = 6,
    /// 飛での王手
    Rook = 7,
    /// 馬での王手
    ProBishop = 8,
    /// 龍での王手
    ProRook = 9,
    /// 非遠方駒の合体bitboard
    NonSlider = 10,
}

impl PieceTypeCheck {
    pub const NUM: usize = 11;

    #[inline]
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::PawnWithNoPro),
            1 => Some(Self::PawnWithPro),
            2 => Some(Self::Lance),
            3 => Some(Self::Knight),
            4 => Some(Self::Silver),
            5 => Some(Self::Gold),
            6 => Some(Self::Bishop),
            7 => Some(Self::Rook),
            8 => Some(Self::ProBishop),
            9 => Some(Self::ProRook),
            10 => Some(Self::NonSlider),
            _ => None,
        }
    }
}

/// 王手になる候補の駒の位置を示すBitboard
/// [玉の位置][PieceTypeCheck][攻撃側の色]
pub static CHECK_CAND_BB: LazyLock<[[[Bitboard; 2]; PieceTypeCheck::NUM]; 81]> =
    LazyLock::new(init_check_cand_bb);

/// 玉周辺の利きを求めるときに使う、玉周辺に利きをつける候補の駒を表すBB
/// [玉の位置][駒の種類(PAWN-KING)][攻撃側の色]
pub static CHECK_AROUND_BB: LazyLock<[[[Bitboard; 2]; PieceType::NUM + 1]; 81]> =
    LazyLock::new(init_check_around_bb);

/// sq1に対してsq2の延長上にある次の升
/// [sq1][sq2] -> 次の升（盤外ならNone）
pub static NEXT_SQUARE: LazyLock<[[Option<Square>; 81]; 81]> = LazyLock::new(init_next_square);

/// CHECK_CAND_BBの初期化
fn init_check_cand_bb() -> [[[Bitboard; 2]; PieceTypeCheck::NUM]; 81] {
    let mut table = [[[Bitboard::EMPTY; 2]; PieceTypeCheck::NUM]; 81];

    for sq_king in Square::all() {
        for &us in &[Color::Black, Color::White] {
            let idx = sq_king.index();
            let c = us.index();

            table[idx][PieceTypeCheck::PawnWithNoPro as usize][c] = pawn_effect(!us, sq_king);
            table[idx][PieceTypeCheck::PawnWithPro as usize][c] = gold_effect(!us, sq_king);
            table[idx][PieceTypeCheck::Lance as usize][c] =
                lance_effect(!us, sq_king, Bitboard::EMPTY);
            table[idx][PieceTypeCheck::Knight as usize][c] = knight_effect(!us, sq_king);
            table[idx][PieceTypeCheck::Silver as usize][c] = silver_effect(!us, sq_king);
            table[idx][PieceTypeCheck::Gold as usize][c] = gold_effect(!us, sq_king);
            table[idx][PieceTypeCheck::Bishop as usize][c] =
                bishop_effect(sq_king, Bitboard::EMPTY);
            table[idx][PieceTypeCheck::Rook as usize][c] = rook_effect(sq_king, Bitboard::EMPTY);
            table[idx][PieceTypeCheck::ProBishop as usize][c] =
                bishop_effect(sq_king, Bitboard::EMPTY) | king_effect(sq_king);
            table[idx][PieceTypeCheck::ProRook as usize][c] =
                rook_effect(sq_king, Bitboard::EMPTY) | king_effect(sq_king);

            let mut non_slider = Bitboard::EMPTY;
            non_slider |= table[idx][PieceTypeCheck::PawnWithNoPro as usize][c];
            non_slider |= table[idx][PieceTypeCheck::PawnWithPro as usize][c];
            non_slider |= table[idx][PieceTypeCheck::Knight as usize][c];
            non_slider |= table[idx][PieceTypeCheck::Silver as usize][c];
            non_slider |= table[idx][PieceTypeCheck::Gold as usize][c];
            table[idx][PieceTypeCheck::NonSlider as usize][c] = non_slider;
        }
    }

    table
}

/// CHECK_AROUND_BBの初期化
fn init_check_around_bb() -> [[[Bitboard; 2]; PieceType::NUM + 1]; 81] {
    let mut table = [[[Bitboard::EMPTY; 2]; PieceType::NUM + 1]; 81];

    for sq_king in Square::all() {
        let around = king_effect(sq_king);
        for &us in &[Color::Black, Color::White] {
            let c = us.index();
            for pt_idx in 1..=PieceType::NUM {
                let pt = PieceType::from_u8(pt_idx as u8).unwrap();
                let mut bb = Bitboard::EMPTY;

                for near in around.iter() {
                    let cand = match pt {
                        PieceType::Pawn => pawn_effect(!us, near),
                        PieceType::Lance => lance_effect(!us, near, Bitboard::EMPTY),
                        PieceType::Knight => knight_effect(!us, near),
                        PieceType::Silver => silver_effect(!us, near),
                        PieceType::Bishop => bishop_effect(near, Bitboard::EMPTY),
                        PieceType::Rook => rook_effect(near, Bitboard::EMPTY),
                        PieceType::ProPawn
                        | PieceType::ProLance
                        | PieceType::ProKnight
                        | PieceType::ProSilver
                        | PieceType::Gold => gold_effect(!us, near),
                        PieceType::Horse => {
                            bishop_effect(near, Bitboard::EMPTY) | king_effect(near)
                        }
                        PieceType::Dragon => rook_effect(near, Bitboard::EMPTY) | king_effect(near),
                        PieceType::King => king_effect(near),
                    };
                    bb |= cand;
                }

                // 玉自身の升は除外
                bb &= !Bitboard::from_square(sq_king);
                table[sq_king.index()][pt.index()][c] = bb;
            }
        }
    }

    table
}

/// NEXT_SQUAREの初期化
fn init_next_square() -> [[Option<Square>; 81]; 81] {
    let mut table = [[None; 81]; 81];

    for s1 in Square::all() {
        for s2 in Square::all() {
            let f1 = s1.file().index() as i32;
            let r1 = s1.rank().index() as i32;
            let f2 = s2.file().index() as i32;
            let r2 = s2.rank().index() as i32;

            let df = (f2 - f1).signum();
            let dr = (r2 - r1).signum();

            // 同一マスや非直線の場合はNone
            if (df == 0 && dr == 0) || !(df == 0 || dr == 0 || df.abs() == dr.abs()) {
                table[s1.index()][s2.index()] = None;
                continue;
            }

            let nf = f2 + df;
            let nr = r2 + dr;
            if (0..=8).contains(&nf) && (0..=8).contains(&nr) {
                if let (Some(file), Some(rank)) =
                    (crate::types::File::from_u8(nf as u8), crate::types::Rank::from_u8(nr as u8))
                {
                    table[s1.index()][s2.index()] = Some(Square::new(file, rank));
                }
            }
        }
    }

    table
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_piece_type_check_enum() {
        assert_eq!(PieceTypeCheck::NUM, 11);
        assert_eq!(PieceTypeCheck::from_u8(0), Some(PieceTypeCheck::PawnWithNoPro));
        assert_eq!(PieceTypeCheck::from_u8(10), Some(PieceTypeCheck::NonSlider));
        assert_eq!(PieceTypeCheck::from_u8(11), None);
    }

    #[test]
    fn test_tables_initialization() {
        // テーブルが初期化されることを確認
        let _ = &*CHECK_CAND_BB;
        let _ = &*CHECK_AROUND_BB;
        let _ = &*NEXT_SQUARE;
    }
}
