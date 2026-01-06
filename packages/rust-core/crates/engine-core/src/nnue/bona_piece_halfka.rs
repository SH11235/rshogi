//! BonaPiece for HalfKA_hm^ - Half-Mirror & Factorization版
//!
//! HalfKA_hm^アーキテクチャ用のBonaPiece定義。
//! nnue-pytorchの実装に準拠し、以下の特徴を持つ:
//! - キングバケット: 45バケット (Half-Mirror: 9段 × 5筋)
//! - Factorization: 駒のみの特徴量を追加
//!
//! 注意: nnue-pytorchではDISTINGUISH_GOLDSは無効（成金は金と同じ扱い）。
//! これはHalfKPと同じBonaPieceレイアウトを使用する。

use crate::types::{Color, Square};

// =============================================================================
// 定数定義（YaneuraOu evaluate.h準拠、DISTINGUISH_GOLDS無効）
// =============================================================================

/// 手駒領域の終端
pub const FE_HAND_END: usize = 90;

/// 盤上駒領域の終端（玉を除く）
/// e_dragon + 81 = 1467 + 81 = 1548
pub const FE_OLD_END: usize = 1548;

/// 先手王の開始位置
pub const F_KING: usize = 1548;

/// 後手王の開始位置
pub const E_KING: usize = 1629;

/// 駒入力数（pack後の最大値 = e_king - 81 = 1548、e_kingはpackで-81される）
/// ※ pack_bonapiece適用後の範囲は 0..1548 だが、PIECE_INPUTSは1629
pub const PIECE_INPUTS: usize = E_KING; // 1629

// =============================================================================
// BonaPieceレイアウト（参考）
// =============================================================================
//
// 手駒 (0-89):
//   f_hand_pawn:   1..19   e_hand_pawn:  20..38
//   f_hand_lance: 39..43   e_hand_lance: 44..48
//   f_hand_knight:49..53   e_hand_knight:54..58
//   f_hand_silver:59..63   e_hand_silver:64..68
//   f_hand_gold:  69..73   e_hand_gold:  74..78
//   f_hand_bishop:79..81   e_hand_bishop:82..84
//   f_hand_rook:  85..87   e_hand_rook:  88..89
//   fe_hand_end = 90
//
// 盤上駒 (90-1547):
//   f_pawn:    90..171    e_pawn:   171..252
//   f_lance:  252..333    e_lance:  333..414
//   f_knight: 414..495    e_knight: 495..576
//   f_silver: 576..657    e_silver: 657..738
//   f_gold:   738..819    e_gold:   819..900
//   f_bishop: 900..981    e_bishop: 981..1062
//   f_horse: 1062..1143   e_horse: 1143..1224
//   f_rook:  1224..1305   e_rook:  1305..1386
//   f_dragon:1386..1467   e_dragon:1467..1548
//   fe_old_end = 1548
//
// 王 (1548-1710):
//   f_king: 1548..1629
//   e_king: 1629..1710

// =============================================================================
// BonaPieceHalfKA (既存BonaPieceのラッパー)
// =============================================================================

use super::bona_piece::BonaPiece;

/// HalfKA_hm^用のBonaPiece
///
/// 内部的にはHalfKPと同じBonaPieceを使用するが、
/// pack_bonapiece関数で適切に変換する。
pub type BonaPieceHalfKA = BonaPiece;

// =============================================================================
// pack_bonapiece - HalfKA_hm^用パッキング
// =============================================================================

/// BonaPieceをHalfKA_hm^用にパック
///
/// C++のpack_bonapiece関数を移植:
/// 1. 手駒（<90）: そのまま
/// 2. 盤上駒（>=90）: hm_mirrorが必要な場合はマス目を反転
/// 3. 敵王（>=e_king）: -81してf_king平面に揃える
#[inline]
pub fn pack_bonapiece(bp: BonaPieceHalfKA, hm_mirror: bool) -> usize {
    let mut pp = bp.value() as usize;

    // 手駒はミラー不要
    if hm_mirror && pp >= FE_HAND_END {
        // 盤上駒: layout is fe_hand_end + piece_index*81 + sq
        let rel = pp - FE_HAND_END;
        let piece_index = rel / 81;
        let sq = rel % 81;

        // マス目をミラー（ファイルのみ: 1筋 ↔ 9筋）
        // Square index: sq = file * 9 + rank (file: 0-8, rank: 0-8)
        // file = sq / 9, rank = sq % 9
        let file = sq / 9;
        let rank = sq % 9;
        let mirrored_file = 8 - file;
        let mirrored_sq = mirrored_file * 9 + rank;

        pp = FE_HAND_END + piece_index * 81 + mirrored_sq;
    }

    // 敵王を先手王平面にパック
    if pp >= E_KING {
        pp -= 81;
    }

    pp // 0..(e_king-1) = 0..1628
}

/// キングバケットを計算（Half-Mirror）
///
/// 玉位置を45バケット（9段 × 5筋）に圧縮。
/// ファイル5-8（0-indexed）は0-3にミラーリング。
///
/// C++実装:
/// ```cpp
/// int file = file_of(ksq);  // 0..8
/// int rank = rank_of(ksq);  // 0..8
/// int file_m = (file >= 5) ? (8 - file) : file;  // 0..4
/// return rank * 5 + file_m;  // 0..44
/// ```
#[inline]
pub fn king_bucket(ksq: Square, perspective: Color) -> usize {
    // 視点に応じてマスを変換
    let sq = if perspective == Color::Black {
        ksq
    } else {
        ksq.inverse()
    };

    let file = sq.file() as usize; // 0..8
    let rank = sq.rank() as usize; // 0..8

    // Half-mirror: file >= 5 なら反転（5,6,7,8 → 3,2,1,0）
    let file_m = if file >= 5 { 8 - file } else { file }; // 0..4

    rank * 5 + file_m // 0..44
}

/// Half-Mirrorが必要かどうかを判定
///
/// 玉のファイルが5以上（6筋-9筋）の場合にtrue。
#[inline]
pub fn is_hm_mirror(ksq: Square, perspective: Color) -> bool {
    // 視点に応じてマスを変換
    let sq = if perspective == Color::Black {
        ksq
    } else {
        ksq.inverse()
    };

    sq.file() as usize >= 5
}

/// HalfKA_hm^の特徴インデックスを計算
///
/// C++実装:
/// ```cpp
/// int kb = king_bucket(perspective, ksq_persp);
/// int pp = pack_bonapiece(p, hm_mirror);
/// return kb * PIECE_INPUTS + pp;
/// ```
#[inline]
pub fn halfka_index(kb: usize, packed_bp: usize) -> usize {
    kb * PIECE_INPUTS + packed_bp
}

/// Factorization特徴インデックスを計算
///
/// BASE_INPUTS + packed_bonapiece
#[inline]
pub fn factorized_index(packed_bp: usize) -> usize {
    use super::constants::BASE_INPUTS_HALFKA;
    BASE_INPUTS_HALFKA + packed_bp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{File, Rank};

    #[test]
    fn test_constants() {
        assert_eq!(FE_HAND_END, 90);
        assert_eq!(FE_OLD_END, 1548);
        assert_eq!(F_KING, 1548);
        assert_eq!(E_KING, 1629);
        assert_eq!(PIECE_INPUTS, 1629);
    }

    #[test]
    fn test_king_bucket_black_perspective() {
        // 先手視点でのキングバケット計算

        // 5九（file=4, rank=8）: bucket = 8*5 + 4 = 44
        let sq_59 = Square::new(File::File5, Rank::Rank9);
        assert_eq!(king_bucket(sq_59, Color::Black), 44);

        // 1九（file=0, rank=8）: bucket = 8*5 + 0 = 40
        let sq_19 = Square::new(File::File1, Rank::Rank9);
        assert_eq!(king_bucket(sq_19, Color::Black), 40);

        // 9九（file=8, mirror to 0, rank=8）: bucket = 8*5 + 0 = 40
        let sq_99 = Square::new(File::File9, Rank::Rank9);
        assert_eq!(king_bucket(sq_99, Color::Black), 40);

        // 6九（file=5, mirror to 3, rank=8）: bucket = 8*5 + 3 = 43
        let sq_69 = Square::new(File::File6, Rank::Rank9);
        assert_eq!(king_bucket(sq_69, Color::Black), 43);

        // 5一（file=4, rank=0）: bucket = 0*5 + 4 = 4
        let sq_51 = Square::new(File::File5, Rank::Rank1);
        assert_eq!(king_bucket(sq_51, Color::Black), 4);

        // 1一（file=0, rank=0）: bucket = 0*5 + 0 = 0
        let sq_11 = Square::new(File::File1, Rank::Rank1);
        assert_eq!(king_bucket(sq_11, Color::Black), 0);
    }

    #[test]
    fn test_is_hm_mirror() {
        // ファイル1-5（index 0-4）: ミラー不要
        assert!(!is_hm_mirror(Square::new(File::File1, Rank::Rank1), Color::Black));
        assert!(!is_hm_mirror(Square::new(File::File5, Rank::Rank9), Color::Black));

        // ファイル6-9（index 5-8）: ミラー必要
        assert!(is_hm_mirror(Square::new(File::File6, Rank::Rank1), Color::Black));
        assert!(is_hm_mirror(Square::new(File::File9, Rank::Rank9), Color::Black));
    }

    #[test]
    fn test_pack_bonapiece_hand_no_mirror() {
        // 手駒はミラーしない
        let bp = BonaPiece::new(50); // 手駒領域内
        assert_eq!(pack_bonapiece(bp, true), 50);
        assert_eq!(pack_bonapiece(bp, false), 50);
    }

    #[test]
    fn test_pack_bonapiece_board_mirror() {
        // 盤上駒のミラー
        // f_pawn (90) + sq の場合
        // sq = file * 9 + rank

        // sq=0 (1一): file=0, rank=0
        // ミラー後: file=8 (9筋), rank=0 → sq = 8*9+0 = 72
        let sq = 0;
        let bp = BonaPiece::new((90 + sq) as u16);
        assert_eq!(pack_bonapiece(bp, false), 90 + sq);
        assert_eq!(pack_bonapiece(bp, true), 90 + 72);

        // sq=9 (2一): file=1, rank=0
        // ミラー後: file=7 (8筋), rank=0 → sq = 7*9+0 = 63
        let sq = 9;
        let bp = BonaPiece::new((90 + sq) as u16);
        assert_eq!(pack_bonapiece(bp, false), 90 + sq);
        assert_eq!(pack_bonapiece(bp, true), 90 + 63);

        // sq=40 (5五): file=4, rank=4
        // ミラー後: file=4 (5筋), rank=4 → sq = 4*9+4 = 40 (中央なので変わらない)
        let sq = 40;
        let bp = BonaPiece::new((90 + sq) as u16);
        assert_eq!(pack_bonapiece(bp, true), 90 + 40);
    }

    #[test]
    fn test_pack_bonapiece_enemy_king() {
        // 敵王のパック: e_king - 81
        let bp = BonaPiece::new(E_KING as u16);
        assert_eq!(pack_bonapiece(bp, false), E_KING - 81);
    }

    #[test]
    fn test_halfka_index() {
        // kb=0, bp=0 → index=0
        assert_eq!(halfka_index(0, 0), 0);

        // kb=1, bp=0 → index=1629
        assert_eq!(halfka_index(1, 0), PIECE_INPUTS);

        // kb=44, bp=0 → index=44*1629=71676
        assert_eq!(halfka_index(44, 0), 44 * PIECE_INPUTS);
    }

    #[test]
    fn test_factorized_index() {
        use super::super::constants::BASE_INPUTS_HALFKA;

        // bp=0 → index=73305
        assert_eq!(factorized_index(0), BASE_INPUTS_HALFKA);

        // bp=100 → index=73405
        assert_eq!(factorized_index(100), BASE_INPUTS_HALFKA + 100);
    }
}
