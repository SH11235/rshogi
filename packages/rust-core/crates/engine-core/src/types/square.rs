//! 升目（Square）

use super::{File, Rank};

/// 升目（0-80）
///
/// 配置: 縦型Bitboard対応
/// SQ_11(1一)=0, SQ_12(1二)=1, ..., SQ_19(1九)=8, SQ_21(2一)=9, ...
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Square(u8);

impl Square {
    /// 升目の数
    pub const NUM: usize = 81;

    // 定数定義（主要なもの）
    /// 1一
    pub const SQ_11: Square = Square(0);
    /// 5五（中央）
    pub const SQ_55: Square = Square(40);
    /// 9九
    pub const SQ_99: Square = Square(80);

    /// FileとRankからSquareを生成
    #[inline]
    pub const fn new(file: File, rank: Rank) -> Square {
        Square(file as u8 * 9 + rank as u8)
    }

    /// 筋を取得
    #[inline]
    pub const fn file(self) -> File {
        // SAFETY: self.0 / 9 は 0..=8 なので有効なFile値
        unsafe { std::mem::transmute(self.0 / 9) }
    }

    /// 段を取得
    #[inline]
    pub const fn rank(self) -> Rank {
        // SAFETY: self.0 % 9 は 0..=8 なので有効なRank値
        unsafe { std::mem::transmute(self.0 % 9) }
    }

    /// インデックスとして使用
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// 内部値を取得
    #[inline]
    pub const fn raw(self) -> u8 {
        self.0
    }

    /// u8から生成（範囲チェックあり）
    #[inline]
    pub const fn from_u8(n: u8) -> Option<Square> {
        if n < 81 {
            Some(Square(n))
        } else {
            None
        }
    }

    /// u8から生成（範囲チェックなし）
    ///
    /// # Safety
    /// n < 81 でなければならない
    #[inline]
    pub const unsafe fn from_u8_unchecked(n: u8) -> Square {
        debug_assert!(n < 81);
        Square(n)
    }

    /// 180度回転
    #[inline]
    pub const fn inverse(self) -> Square {
        Square(80 - self.0)
    }

    /// 左右反転（5筋軸）
    #[inline]
    pub const fn mirror(self) -> Square {
        // fn new から self.0 = file * 9 + rank
        // self.0 / 9 = file
        // self.0 % 9 = rank
        let file = 8 - self.0 / 9; // file を左右反転
        let rank = self.0 % 9; // rank はそのまま
        Square(file * 9 + rank)
    }

    /// USI形式の文字列（"7g"等）に変換
    pub fn to_usi(self) -> String {
        let file = self.file().to_usi_char();
        let rank = self.rank().to_usi_char();
        format!("{file}{rank}")
    }

    /// USI形式の文字列からSquareに変換
    pub fn from_usi(s: &str) -> Option<Square> {
        let mut chars = s.chars();
        let file = File::from_usi_char(chars.next()?)?;
        let rank = Rank::from_usi_char(chars.next()?)?;
        Some(Square::new(file, rank))
    }

    /// 全ての升を返すイテレータ
    pub fn all() -> impl Iterator<Item = Square> {
        (0..81).map(Square)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_square_new() {
        let sq = Square::new(File::File1, Rank::Rank1);
        assert_eq!(sq, Square::SQ_11);

        let sq = Square::new(File::File5, Rank::Rank5);
        assert_eq!(sq, Square::SQ_55);

        let sq = Square::new(File::File9, Rank::Rank9);
        assert_eq!(sq, Square::SQ_99);
    }

    #[test]
    fn test_square_file_rank() {
        let sq = Square::new(File::File3, Rank::Rank7);
        assert_eq!(sq.file(), File::File3);
        assert_eq!(sq.rank(), Rank::Rank7);
    }

    #[test]
    fn test_square_from_u8() {
        assert_eq!(Square::from_u8(0), Some(Square::SQ_11));
        assert_eq!(Square::from_u8(80), Some(Square::SQ_99));
        assert_eq!(Square::from_u8(81), None);
    }

    #[test]
    fn test_square_inverse() {
        assert_eq!(Square::SQ_11.inverse(), Square::SQ_99);
        assert_eq!(Square::SQ_99.inverse(), Square::SQ_11);
        assert_eq!(Square::SQ_55.inverse(), Square::SQ_55);
    }

    #[test]
    fn test_square_mirror() {
        // 1筋 <-> 9筋
        let sq1 = Square::new(File::File1, Rank::Rank5);
        let sq9 = Square::new(File::File9, Rank::Rank5);
        assert_eq!(sq1.mirror(), sq9);
        assert_eq!(sq9.mirror(), sq1);

        // 5筋は不変
        assert_eq!(Square::SQ_55.mirror(), Square::SQ_55);
    }

    #[test]
    fn test_square_usi() {
        assert_eq!(Square::new(File::File7, Rank::Rank7).to_usi(), "7g");
        assert_eq!(Square::from_usi("7g"), Some(Square::new(File::File7, Rank::Rank7)));
        assert_eq!(Square::from_usi("1a"), Some(Square::SQ_11));
        assert_eq!(Square::from_usi("9i"), Some(Square::SQ_99));
        assert_eq!(Square::from_usi(""), None);
        assert_eq!(Square::from_usi("0a"), None);
    }

    #[test]
    fn test_square_all() {
        let all: Vec<_> = Square::all().collect();
        assert_eq!(all.len(), 81);
        assert_eq!(all[0], Square::SQ_11);
        assert_eq!(all[80], Square::SQ_99);
    }
}
