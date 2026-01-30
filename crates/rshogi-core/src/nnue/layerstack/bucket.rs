//! バケット計算
//!
//! 玉の位置に基づいて LayerStack のバケットインデックスを計算する。

use crate::position::Position;
use crate::types::{Color, Square};

/// バケット分割方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BucketDivision {
    /// 2x2 バケット（4バケット）
    ///
    /// 段を2分割（1-5段 → 1, 6-9段 → 0）
    TwoByTwo,

    /// 3x3 バケット（9バケット）
    ///
    /// 段を3分割（1-3段 → 2, 4-6段 → 1, 7-9段 → 0）
    ThreeByThree,
}

impl BucketDivision {
    /// バケット数を取得
    #[inline]
    pub const fn num_buckets(self) -> usize {
        match self {
            Self::TwoByTwo => 4,
            Self::ThreeByThree => 9,
        }
    }
}

/// 2x2 バケット用の段テーブル（0-indexed）
///
/// 1-5段 → 1, 6-9段 → 0
const TABLE_2X2: [usize; 9] = [1, 1, 1, 1, 1, 0, 0, 0, 0];

/// 3x3 バケット用の段テーブル（0-indexed）
///
/// 1-3段 → 2, 4-6段 → 1, 7-9段 → 0
const TABLE_3X3: [usize; 9] = [2, 2, 2, 1, 1, 1, 0, 0, 0];

/// 段インデックスを取得（0-indexed: 0 = 1段目, 8 = 9段目）
#[inline]
fn rank_of(sq: Square) -> usize {
    sq.rank().index()
}

/// 座標を反転（後手視点への変換用）
///
/// (file, rank) → (8 - file, 8 - rank)
/// これにより、後手視点で見たときに先手視点と同じ座標系になる
#[inline]
fn inv(sq: Square) -> Square {
    sq.inverse()
}

/// バケットインデックスを計算
///
/// 手番視点で正規化された玉位置に基づいてバケットを決定する。
///
/// # 引数
///
/// - `pos`: 現在の局面
/// - `div`: バケット分割方式
///
/// # 戻り値
///
/// バケットインデックス（0 ～ num_buckets-1）
///
/// # アルゴリズム
///
/// ```text
/// 手番視点で正規化:
/// - f_rank: 自玉の段（手番側視点で正規化）
/// - e_rank: 敵玉の段（手番側視点で正規化）
///
/// TwoByTwo:   (TABLE_2X2[e_rank] << 1) | TABLE_2X2[f_rank]
/// ThreeByThree: TABLE_3X3[e_rank] * 3 + TABLE_3X3[f_rank]
/// ```
#[inline]
pub fn bucket_index(pos: &Position, div: BucketDivision) -> usize {
    let stm = pos.side_to_move();
    let stm_king = pos.king_square(stm);
    let nstm_king = pos.king_square(!stm);

    // 手番視点で正規化
    // - 先手番: そのまま使用
    // - 後手番: 座標を反転して先手視点に変換
    let (f_rank, e_rank) = match stm {
        Color::Black => (rank_of(stm_king), rank_of(inv(nstm_king))),
        Color::White => (rank_of(inv(stm_king)), rank_of(nstm_king)),
    };

    match div {
        BucketDivision::TwoByTwo => (TABLE_2X2[e_rank] << 1) | TABLE_2X2[f_rank],
        BucketDivision::ThreeByThree => TABLE_3X3[e_rank] * 3 + TABLE_3X3[f_rank],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position::SFEN_HIRATE;

    #[test]
    fn test_bucket_division_num_buckets() {
        assert_eq!(BucketDivision::TwoByTwo.num_buckets(), 4);
        assert_eq!(BucketDivision::ThreeByThree.num_buckets(), 9);
    }

    #[test]
    fn test_rank_tables() {
        // 2x2: 1-5段 → 1, 6-9段 → 0
        assert_eq!(TABLE_2X2[0], 1); // 1段目
        assert_eq!(TABLE_2X2[4], 1); // 5段目
        assert_eq!(TABLE_2X2[5], 0); // 6段目
        assert_eq!(TABLE_2X2[8], 0); // 9段目

        // 3x3: 1-3段 → 2, 4-6段 → 1, 7-9段 → 0
        assert_eq!(TABLE_3X3[0], 2); // 1段目
        assert_eq!(TABLE_3X3[2], 2); // 3段目
        assert_eq!(TABLE_3X3[3], 1); // 4段目
        assert_eq!(TABLE_3X3[5], 1); // 6段目
        assert_eq!(TABLE_3X3[6], 0); // 7段目
        assert_eq!(TABLE_3X3[8], 0); // 9段目
    }

    #[test]
    fn test_bucket_index_hirate() {
        // 平手初期局面
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // 先手番: 先手玉は5九(8段目)、後手玉は5一(0段目)
        // f_rank = 8 (9段目, 0-indexed = 8), e_rank = inv(0) = 80-0=80 → rank=80/9=8? → 0段目の反転
        // inv(sq=4) = 80-4=76 → rank=76/9=8
        //
        // 先手視点:
        //   先手玉: 5九 = file=4, rank=8
        //   後手玉: 5一 = file=4, rank=0 → inv → file=4, rank=8
        //
        // f_rank = 8, e_rank = 8
        // 2x2: TABLE_2X2[8]=0, TABLE_2X2[8]=0 → (0 << 1) | 0 = 0
        // 3x3: TABLE_3X3[8]=0, TABLE_3X3[8]=0 → 0 * 3 + 0 = 0

        let bucket_2x2 = bucket_index(&pos, BucketDivision::TwoByTwo);
        let bucket_3x3 = bucket_index(&pos, BucketDivision::ThreeByThree);

        assert_eq!(bucket_2x2, 0);
        assert_eq!(bucket_3x3, 0);
    }

    #[test]
    fn test_bucket_index_range() {
        let mut pos = Position::new();
        pos.set_sfen(SFEN_HIRATE).unwrap();

        // 2x2 の範囲は 0-3
        let bucket_2x2 = bucket_index(&pos, BucketDivision::TwoByTwo);
        assert!(bucket_2x2 < 4);

        // 3x3 の範囲は 0-8
        let bucket_3x3 = bucket_index(&pos, BucketDivision::ThreeByThree);
        assert!(bucket_3x3 < 9);
    }

    #[test]
    fn test_inv_symmetry() {
        // inv(inv(sq)) = sq
        for i in 0..81 {
            let sq = Square::from_u8(i).unwrap();
            assert_eq!(inv(inv(sq)), sq);
        }
    }

    #[test]
    fn test_inv_center() {
        // 中央のマス（5五 = index 40）は自身に変換される
        let center = Square::SQ_55;
        assert_eq!(inv(center), center);
    }
}
