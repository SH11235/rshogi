//! BonaPiece for HalfKaMerged - Non-mirror + MergedPlane
//!
//! 玉位置は 81 マス直指定（ミラーなし）、両玉を 1 plane に畳む（敵玉 BonaPiece
//! を 81 引いて自玉 plane に重ねる）。
//!
//! - キングバケット: 81 (Direct)
//! - 入力平面数: 1629 (E_KING、敵玉を畳んだ後の上限)
//! - 入力次元: 131,949 (81×1629)

use super::bona_piece::BonaPiece;
use crate::types::{Color, Square};

// =============================================================================
// 定数定義
// =============================================================================

/// 後手王の開始位置（pack で -81 されると先手王 plane に重なる）
pub const E_KING: usize = 1629;

/// 駒入力数（pack 後の最大値 = E_KING、敵玉は pack で -81 される）
///
/// pack_bonapiece 適用後の範囲は 0..=1628 だが、`PIECE_INPUTS` 自体は E_KING に
/// 揃える（HalfKaHmMerged と同じ慣習）。
pub const PIECE_INPUTS: usize = E_KING; // 1629

/// HalfKaMerged 用の BonaPiece（内部は通常の BonaPiece と同じレイアウト）
pub type BonaPieceHalfKaMerged = BonaPiece;

// =============================================================================
// インデックス計算
// =============================================================================

/// 玉位置を 81 マス直指定で取得（視点に応じて反転）
#[inline]
pub fn king_index(ksq: Square, perspective: Color) -> usize {
    if perspective == Color::Black {
        ksq.index()
    } else {
        ksq.inverse().index()
    }
}

/// BonaPiece を MergedPlane 用にパック
///
/// MergedPlane では敵玉 BonaPiece (>= E_KING) を 81 引いて自玉 plane に重ねる。
/// Direct (ミラーなし) なのでマス反転は行わない。
#[inline]
pub fn pack_bonapiece(bp: BonaPieceHalfKaMerged) -> usize {
    let mut pp = bp.value() as usize;
    if pp >= E_KING {
        pp -= 81;
    }
    pp // 0..=1628
}

/// HalfKaMerged の特徴インデックスを計算
///
/// `king_idx * PIECE_INPUTS + packed_bp`
#[inline]
pub fn halfka_index(king_idx: usize, packed_bp: usize) -> usize {
    king_idx * PIECE_INPUTS + packed_bp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_constants() {
        assert_eq!(PIECE_INPUTS, 1629);
        assert_eq!(E_KING, 1629);
    }

    #[test]
    fn test_king_index_black_perspective() {
        let sq_59 = Square::new(File::File5, Rank::Rank9);
        assert_eq!(king_index(sq_59, Color::Black), sq_59.index());
    }

    #[test]
    fn test_king_index_white_perspective() {
        let sq_59 = Square::new(File::File5, Rank::Rank9);
        let sq_51 = sq_59.inverse();
        assert_eq!(king_index(sq_59, Color::White), sq_51.index());
    }

    #[test]
    fn test_pack_bonapiece_hand() {
        // 手駒はそのまま
        assert_eq!(pack_bonapiece(BonaPiece::new(50)), 50);
    }

    #[test]
    fn test_pack_bonapiece_board() {
        // 盤上駒（< E_KING）はマス反転無しでそのまま
        let bp = BonaPiece::new(100);
        assert_eq!(pack_bonapiece(bp), 100);
    }

    #[test]
    fn test_pack_bonapiece_enemy_king_fold() {
        // 敵玉は -81 で自玉 plane に重ねる
        assert_eq!(pack_bonapiece(BonaPiece::new(E_KING as u16)), E_KING - 81);
        assert_eq!(pack_bonapiece(BonaPiece::new((E_KING + 80) as u16)), E_KING + 80 - 81);
    }

    #[test]
    fn test_halfka_index() {
        assert_eq!(halfka_index(0, 0), 0);
        assert_eq!(halfka_index(1, 0), PIECE_INPUTS);
        assert_eq!(halfka_index(80, 0), 80 * PIECE_INPUTS);
    }

    /// パリティ検証: MergedPlane の pack は「マス反転なし + 敵玉 fold」であり、
    /// 検証済み HalfKaHmMerged の pack を hm_mirror=false で呼んだ結果と全 BonaPiece 値
    /// で一致するはず（HalfKaHmMerged の hm_mirror=false も「反転なし + fold」のため）。
    #[test]
    fn test_pack_parity_with_halfka_hm_no_mirror() {
        use crate::nnue::bona_piece_halfka_hm_merged::pack_bonapiece as hm_pack;
        for v in 0..1710u16 {
            let bp = BonaPiece::new(v);
            assert_eq!(pack_bonapiece(bp), hm_pack(bp, false), "mismatch at bp={v}");
        }
    }
}
