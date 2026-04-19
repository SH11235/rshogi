//! HandCount Dense Input — L1 層に concat される 14 元の持ち駒ベクトル
//!
//! bullet-shogi `ShogiHalfKaHmHandCount` と 1:1 で対応する dense 補助入力。
//! `[stm 持ち駒 7 種, nstm 持ち駒 7 種] = 14 元` で、配置順は
//! `pawn, lance, knight, silver, gold, bishop, rook`。
//!
//! NNUE 推論時に FT 出力 (1536) の後ろに concat して L1 層へ入力する。
//!
//! 現状は helper のみ実装済み（nnue-hand-count-dense feature 有効時のみ build される）。
//! LayerStack の重み読み出し・評価パスへの配線は未実装で、ロード時に
//! `NetworkLayerStacks::read` が HandCountDense モデルを拒否する。

#![allow(dead_code)]

use crate::position::Position;
use crate::types::Color;

/// HandCount Dense 補助入力の次元数（`[stm 7 種 + nstm 7 種] = 14`）
pub const HAND_COUNT_DIMS: usize = 14;

/// 片視点あたりの駒種数（pawn..rook）
const HAND_PIECE_TYPES: usize = 7;

/// 現局面から HandCount Dense 補助入力を抽出する。
///
/// 値は bullet 側と同じスケール（本数をそのまま i16 化）。i8 量子化後の重みと
/// 組み合わせて L1 層の内部スケールに適合する。
///
/// レイアウト:
///
/// ```text
/// out[0..7]  : stm  視点の持ち駒本数 (pawn, lance, knight, silver, gold, bishop, rook)
/// out[7..14] : nstm 視点の持ち駒本数 (同上)
/// ```
#[inline]
pub fn extract_hand_count(pos: &Position) -> [i16; HAND_COUNT_DIMS] {
    let stm = pos.side_to_move();
    let nstm = !stm;
    let mut out = [0i16; HAND_COUNT_DIMS];
    write_side(pos, stm, &mut out[0..HAND_PIECE_TYPES]);
    write_side(pos, nstm, &mut out[HAND_PIECE_TYPES..HAND_COUNT_DIMS]);
    out
}

#[inline]
fn write_side(pos: &Position, color: Color, out: &mut [i16]) {
    use crate::types::PieceType;

    // bullet 側と一致する並び: pawn, lance, knight, silver, gold, bishop, rook
    const HAND_ORDER: [PieceType; 7] = [
        PieceType::Pawn,
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Rook,
    ];

    let hand = pos.hand(color);
    for (i, pt) in HAND_ORDER.iter().enumerate() {
        // `Hand::count` は u32 を返すが、手駒の最大は歩 18 枚で i16 の範囲に十分収まる。
        out[i] = hand.count(*pt) as i16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants() {
        // 片視点 7 種 × 2 視点 = 14
        assert_eq!(HAND_COUNT_DIMS, 14);
        assert_eq!(HAND_PIECE_TYPES, 7);
    }

    #[test]
    fn extract_from_startpos() {
        // 平手初期局面: 両側手駒ゼロ
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap();
        let hc = extract_hand_count(&pos);
        assert_eq!(hc, [0i16; HAND_COUNT_DIMS]);
    }

    #[test]
    fn extract_reflects_hand_from_sfen() {
        // 盤上 2 枚の歩を抜いて先手の手駒へ、盤上の後手角を抜いて後手の手駒へ。
        // 盤上歩 16 (9 先 + 7 後)、盤上角は後手のみ 0 (手駒 1 枚)。
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r7/2ppppppp/9/9/9/PPPPPPPPP/7R1/LNSGKGSNL b 2Pb 1")
            .unwrap();
        let hc = extract_hand_count(&pos);
        // stm (Black) 視点: index 0 = pawn
        assert_eq!(hc[0], 2, "stm pawn count");
        // nstm (White) 視点: index 7+5 = bishop
        assert_eq!(hc[7 + 5], 1, "nstm bishop count");
        // それ以外は 0
        for (i, v) in hc.iter().enumerate() {
            if i != 0 && i != 7 + 5 {
                assert_eq!(*v, 0, "index {i} should be 0 but got {v}");
            }
        }
    }
}
