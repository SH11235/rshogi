//! 玉位置に基づく入玉ドメイン分類。

use rshogi_core::position::Position;
use rshogi_core::types::Color;

/// rank (0 始まり、先手から見て一段目が 0) から tier を決める。`classify` と `classify_packed` が共有する。
fn classify_ranks(black: usize, white: usize) -> usize {
    if black <= 2 || white >= 6 {
        ENTERED_TIER
    } else if (3..=5).contains(&black) || (3..=5).contains(&white) {
        1
    } else {
        2
    }
}

/// packed sfen (PSV 先頭 32 byte) の玉のマスだけを読み、`classify` と同じ tier を返す。
///
/// packed sfen は手番 1 bit・先手玉 7 bit・後手玉 7 bit の順に下位 bit から並ぶので、局面を組み立てずに分類できる。
/// 盤面の残りは検査しない (玉のマスが 81 以上か両玉が同じマスのときだけ失敗する)。
pub fn classify_packed(sfen: &[u8]) -> Result<usize, String> {
    if sfen.len() < 2 {
        return Err(format!("packed sfen too short: {} bytes", sfen.len()));
    }
    let square = |first_bit: usize| {
        (0..7).fold(0usize, |sq, i| {
            let bit = first_bit + i;
            sq | ((((sfen[bit / 8] >> (bit % 8)) & 1) as usize) << i)
        })
    };
    let black = square(1);
    let white = square(8);
    if black >= 81 || white >= 81 {
        return Err(format!("invalid king square: black={black}, white={white}"));
    }
    if black == white {
        return Err(format!("both kings on square {black}"));
    }
    Ok(classify_ranks(black % 9, white % 9))
}

/// 入玉済みの tier 番号。
pub const ENTERED_TIER: usize = 0;

/// 玉位置から 0=entered、1=advancing、2=normal を返す。
///
/// entered は先手玉の rank が 2 以下、または後手玉の rank が 6 以上の局面。
pub fn classify(pos: &Position) -> usize {
    let black = pos.king_square(Color::Black).rank() as usize;
    let white = pos.king_square(Color::White).rank() as usize;
    classify_ranks(black, white)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packed_sfen::pack_position;
    use rshogi_core::types::{Hand, Piece};

    #[test]
    fn classify_packed_matches_classify_for_every_king_placement() {
        for side in [Color::Black, Color::White] {
            for black in 0..81 {
                for white in (0..81).filter(|&w| w != black) {
                    let mut board = [Piece::NONE; 81];
                    board[black] = Piece::B_KING;
                    board[white] = Piece::W_KING;
                    let mut pos = Position::new();
                    pos.set_from_parts(&board, &[Hand::EMPTY; 2], side).unwrap();
                    assert_eq!(
                        classify_packed(&pack_position(&pos)),
                        Ok(classify(&pos)),
                        "black={black} white={white} side={side:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn classify_packed_rejects_invalid_king_squares() {
        let mut sfen = [0u8; 32];
        // 先手玉 = 127 (bit 1..=7 を全部立てる)
        sfen[0] = 0xFE;
        assert!(classify_packed(&sfen).is_err());
        assert!(classify_packed(&[0u8; 32]).is_err(), "both kings on square 0");
        assert!(classify_packed(&[0u8; 1]).is_err());
    }
}
