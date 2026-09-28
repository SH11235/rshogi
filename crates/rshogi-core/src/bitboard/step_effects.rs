//! 占有に依存しない遠方駒の利き。

use super::Bitboard;
use crate::types::Square;

/// 盤上の駒を考慮しない飛車の利き。
pub static ROOK_STEP: [Bitboard; Square::NUM] =
    build_step_effects([(1, 0), (-1, 0), (0, 1), (0, -1)]);

/// 盤上の駒を考慮しない角の利き。
pub static BISHOP_STEP: [Bitboard; Square::NUM] =
    build_step_effects([(1, 1), (1, -1), (-1, 1), (-1, -1)]);

const fn build_step_effects(directions: [(i32, i32); 4]) -> [Bitboard; Square::NUM] {
    let mut table = [Bitboard::EMPTY; Square::NUM];
    let mut sq = 0;
    while sq < Square::NUM {
        let mut words = [0u64; 2];
        let mut direction = 0;
        while direction < directions.len() {
            let (df, dr) = directions[direction];
            let mut file = sq as i32 / 9 + df;
            let mut rank = sq as i32 % 9 + dr;
            while file >= 0 && file < 9 && rank >= 0 && rank < 9 {
                let target = (file * 9 + rank) as usize;
                // 1〜7筋は下位ワード、8〜9筋は上位ワードの先頭から格納する。
                words[target / 63] |= 1u64 << (target % 63);
                file += df;
                rank += dr;
            }
            direction += 1;
        }
        table[sq] = Bitboard::new(words[0], words[1]);
        sq += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitboard::{bishop_effect, lance_effect, lance_step_effect, rook_effect};
    use crate::types::Color;

    #[test]
    fn step_tables_match_empty_occupancy_effects() {
        for sq in Square::all() {
            assert_eq!(ROOK_STEP[sq.index()], rook_effect(sq, Bitboard::EMPTY));
            assert_eq!(BISHOP_STEP[sq.index()], bishop_effect(sq, Bitboard::EMPTY));
            for color in [Color::Black, Color::White] {
                assert_eq!(lance_step_effect(color, sq), lance_effect(color, sq, Bitboard::EMPTY));
            }
        }
    }

    #[test]
    fn lance_check_squares_match_rook_intersection() {
        // 各筋の全占有パターンを両手番で調べ、ワード境界の両側を含める。
        for sq in Square::all() {
            for mask in 0..512u64 {
                let file = sq.index() / 9;
                let occupied = if file < 7 {
                    Bitboard::new(mask << (file * 9), 0)
                } else {
                    Bitboard::new(0, mask << ((file - 7) * 9))
                };
                let rook = rook_effect(sq, occupied);
                for color in [Color::Black, Color::White] {
                    assert_eq!(
                        rook & lance_step_effect(color, sq),
                        lance_effect(color, sq, occupied),
                        "sq={sq:?} color={color:?} mask={mask:#x}"
                    );
                }
            }
        }
    }
}
