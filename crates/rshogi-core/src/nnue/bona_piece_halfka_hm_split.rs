//! BonaPiece for HalfKaHmSplit - Half-Mirror + SplitPlane
//!
//! 玉位置は Half-Mirror（45 バケット）、両玉を別 plane で持つ（畳まない）。
//!
//! - キングバケット: 45 (Half-Mirror)
//! - 入力平面数: 1710 (1548 + 81 × 2、両玉 plane 別々)
//! - 入力次元: 76,950 (45×1710)

use super::bona_piece::BonaPiece;
use crate::types::{Color, Square};

// =============================================================================
// 定数定義
// =============================================================================

/// 手駒領域の終端（盤上駒のミラー対象判定に使用）
pub const FE_HAND_END: usize = 90;

/// 駒入力数（両玉 plane を含む split 形式: 1548 + 81 × 2 = 1710）
pub const PIECE_INPUTS: usize = 1548 + 81 * 2;

/// HalfKaHmSplit 用の BonaPiece（内部は通常の BonaPiece と同じレイアウト）
#[allow(non_camel_case_types)]
pub type BonaPieceHalfKaHmSplit = BonaPiece;

// =============================================================================
// インデックス計算
// =============================================================================

/// キングバケットを計算（Half-Mirror、45 バケット）
///
/// 玉位置を 45 バケット（9 段 × 5 筋）に圧縮。ファイル 5-8（0-indexed）は
/// 4-1 にミラーリング。bucket index は `file_m * 9 + rank`、範囲 0..=44。
#[inline]
pub fn king_bucket(ksq: Square, perspective: Color) -> usize {
    let sq = if perspective == Color::Black {
        ksq
    } else {
        ksq.inverse()
    };

    let file = sq.file() as usize; // 0..=8
    let rank = sq.rank() as usize; // 0..=8

    let file_m = if file >= 5 { 8 - file } else { file }; // 0..=4

    file_m * 9 + rank // 0..=44
}

/// Half-Mirror が必要かどうかを判定
///
/// 玉のファイルが 5 以上（6 筋-9 筋）の場合に true。
#[inline]
pub fn is_hm_mirror(ksq: Square, perspective: Color) -> bool {
    let sq = if perspective == Color::Black {
        ksq
    } else {
        ksq.inverse()
    };

    sq.file() as usize >= 5
}

/// BonaPiece を SplitPlane + Half-Mirror 用にパック
///
/// SplitPlane では敵玉を畳まない（fold なし）。Half-Mirror が必要な場合は
/// 盤上駒のマス目を筋反転する。手駒（< `FE_HAND_END`）と玉 plane (>= `FE_OLD_END`)
/// については、手駒は反転対象外、玉 plane は plane 内のマス座標を反転する。
#[inline]
pub fn pack_bonapiece(bp: BonaPieceHalfKaHmSplit, hm_mirror: bool) -> usize {
    let pp = bp.value() as usize;

    if hm_mirror && pp >= FE_HAND_END {
        // 盤上駒 or 玉 plane: `FE_HAND_END + piece_index * 81 + sq` 形式。
        // sq の筋（file）を反転して同じ piece_index 内に書き戻す。
        let rel = pp - FE_HAND_END;
        let piece_index = rel / 81;
        let sq = rel % 81;

        // Square index: file * 9 + rank
        let file = sq / 9;
        let rank = sq % 9;
        let mirrored_file = 8 - file;
        let mirrored_sq = mirrored_file * 9 + rank;

        FE_HAND_END + piece_index * 81 + mirrored_sq
    } else {
        // 手駒、または mirror 不要: そのまま
        pp
    }
}

/// HalfKaHmSplit の特徴インデックスを計算
///
/// `king_bucket * PIECE_INPUTS + packed_bp`
#[inline]
pub fn halfka_index(kb: usize, packed_bp: usize) -> usize {
    kb * PIECE_INPUTS + packed_bp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_constants() {
        assert_eq!(PIECE_INPUTS, 1710);
    }

    #[test]
    fn test_king_bucket_black_perspective() {
        // 5九（file=4, rank=8）: bucket = 4*9 + 8 = 44
        let sq_59 = Square::new(File::File5, Rank::Rank9);
        assert_eq!(king_bucket(sq_59, Color::Black), 44);

        // 1九（file=0, rank=8）: bucket = 0*9 + 8 = 8
        let sq_19 = Square::new(File::File1, Rank::Rank9);
        assert_eq!(king_bucket(sq_19, Color::Black), 8);

        // 9九（file=8, mirror to 0, rank=8）: bucket = 0*9 + 8 = 8
        let sq_99 = Square::new(File::File9, Rank::Rank9);
        assert_eq!(king_bucket(sq_99, Color::Black), 8);

        // 1一（file=0, rank=0）: bucket = 0
        let sq_11 = Square::new(File::File1, Rank::Rank1);
        assert_eq!(king_bucket(sq_11, Color::Black), 0);
    }

    #[test]
    fn test_is_hm_mirror() {
        assert!(!is_hm_mirror(Square::new(File::File1, Rank::Rank1), Color::Black));
        assert!(!is_hm_mirror(Square::new(File::File5, Rank::Rank9), Color::Black));
        assert!(is_hm_mirror(Square::new(File::File6, Rank::Rank1), Color::Black));
        assert!(is_hm_mirror(Square::new(File::File9, Rank::Rank9), Color::Black));
    }

    #[test]
    fn test_pack_bonapiece_hand_no_mirror() {
        // 手駒はミラーしない
        let bp = BonaPiece::new(50);
        assert_eq!(pack_bonapiece(bp, true), 50);
        assert_eq!(pack_bonapiece(bp, false), 50);
    }

    #[test]
    fn test_pack_bonapiece_board_mirror() {
        // 盤上駒 (f_pawn=90, sq=0 -> 1一): file=0, rank=0
        // mirror: file=8 (9筋), rank=0 -> sq = 8*9+0 = 72
        let bp = BonaPiece::new(90);
        assert_eq!(pack_bonapiece(bp, false), 90);
        assert_eq!(pack_bonapiece(bp, true), 90 + 72);
    }

    #[test]
    fn test_pack_bonapiece_enemy_king_not_folded() {
        // SplitPlane: 敵玉は畳まれない（>= E_KING でも値はそのまま、ただし mirror は適用される）
        // E_KING(1629) は piece_index = (1629-90)/81 = 19、sq = (1629-90)%81 = 0 (1一)
        // mirror(file=0->8): pp = 90 + 19*81 + 72 = 1701
        let bp = BonaPiece::new(1629);
        // mirror 無し: そのまま
        assert_eq!(pack_bonapiece(bp, false), 1629);
        // mirror あり: 1一 -> 9一
        assert_eq!(pack_bonapiece(bp, true), 90 + 19 * 81 + 72);
    }

    #[test]
    fn test_halfka_index() {
        assert_eq!(halfka_index(0, 0), 0);
        assert_eq!(halfka_index(1, 0), PIECE_INPUTS);
        assert_eq!(halfka_index(44, 0), 44 * PIECE_INPUTS);
    }

    /// パリティ検証: SplitPlane の pack は「マス反転のみ（fold なし）」。
    /// bp < E_KING(1629) の範囲では fold が発火しないため、検証済み HalfKaHmMerged の
    /// pack（反転 + fold）と一致するはず。
    #[test]
    fn test_pack_parity_with_halfka_hm_below_eking() {
        use crate::nnue::bona_piece_halfka_hm_merged::pack_bonapiece as hm_pack;
        for v in 0..1629u16 {
            let bp = BonaPiece::new(v);
            for &m in &[false, true] {
                assert_eq!(pack_bonapiece(bp, m), hm_pack(bp, m), "mismatch at bp={v} mirror={m}");
            }
        }
    }

    /// パリティ検証: SplitPlane は敵玉を fold しない。bp >= E_KING の玉 plane では
    /// HalfKaHmMerged（fold あり）と異なり、値が 81 引かれない。
    #[test]
    fn test_pack_no_fold_above_eking() {
        // E_KING(1629) は piece_index=(1629-90)/81=19, sq=0。mirror 無しでそのまま。
        assert_eq!(pack_bonapiece(BonaPiece::new(1629), false), 1629);
        // 玉 plane 上限付近も fold されない（split は 1710 入力を維持）。
        assert_eq!(pack_bonapiece(BonaPiece::new(1709), false), 1709);
    }

    /// king_bucket は検証済み HalfKaHmMerged と同一ロジック（45 バケット Half-Mirror）。
    #[test]
    fn test_king_bucket_parity_with_halfka_hm() {
        use crate::nnue::bona_piece_halfka_hm_merged::king_bucket as hm_kb;
        for f in [
            File::File1,
            File::File3,
            File::File5,
            File::File7,
            File::File9,
        ] {
            for r in [Rank::Rank1, Rank::Rank5, Rank::Rank9] {
                let sq = Square::new(f, r);
                for &c in &[Color::Black, Color::White] {
                    assert_eq!(king_bucket(sq, c), hm_kb(sq, c));
                }
            }
        }
    }
}
