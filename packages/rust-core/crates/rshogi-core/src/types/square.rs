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
    // 方向定数（縦型Bitboardのやねうら王と同じ符号）
    pub const DELTA_D: i8 = 1; // 下(段+1)
    pub const DELTA_R: i8 = -9; // 右(筋-1)
    pub const DELTA_U: i8 = -1; // 上(段-1)
    pub const DELTA_L: i8 = 9; // 左(筋+1)
    pub const DELTA_RU: i8 = Self::DELTA_R + Self::DELTA_U;
    pub const DELTA_RD: i8 = Self::DELTA_R + Self::DELTA_D;
    pub const DELTA_LU: i8 = Self::DELTA_L + Self::DELTA_U;
    pub const DELTA_LD: i8 = Self::DELTA_L + Self::DELTA_D;
    pub const DELTA_RUU: i8 = Self::DELTA_RU + Self::DELTA_U;
    pub const DELTA_LUU: i8 = Self::DELTA_LU + Self::DELTA_U;
    pub const DELTA_RDD: i8 = Self::DELTA_RD + Self::DELTA_D;
    pub const DELTA_LDD: i8 = Self::DELTA_LD + Self::DELTA_D;

    /// FileとRankからSquareを生成
    #[inline]
    pub const fn new(file: File, rank: Rank) -> Square {
        Square(file as u8 * 9 + rank as u8)
    }

    /// 盤内かどうか
    #[inline]
    pub const fn is_ok(self) -> bool {
        self.0 < Self::NUM as u8
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

    /// 方向オフセットを足したSquareを返す（盤外ならNone）
    ///
    /// YaneuraOuのSQ_U/SQ_D/SQ_L/SQ_R等に対応するオフセットをそのまま扱える。
    #[inline]
    pub const fn offset(self, delta: i8) -> Option<Square> {
        let (df, dr) = match delta {
            Self::DELTA_U => (0, -1),
            Self::DELTA_D => (0, 1),
            Self::DELTA_L => (1, 0),
            Self::DELTA_R => (-1, 0),
            Self::DELTA_RU => (-1, -1),
            Self::DELTA_RD => (-1, 1),
            Self::DELTA_LU => (1, -1),
            Self::DELTA_LD => (1, 1),
            Self::DELTA_RUU => (-1, -2),
            Self::DELTA_LUU => (1, -2),
            Self::DELTA_RDD => (-1, 2),
            Self::DELTA_LDD => (1, 2),
            _ => {
                let value = self.0 as i16 + delta as i16;
                if value >= 0 && value < 81 {
                    return Some(Square(value as u8));
                } else {
                    return None;
                }
            }
        };

        let file = self.file().index() as i16 + df as i16;
        let rank = self.rank().index() as i16 + dr as i16;
        if file >= 0 && file < 9 && rank >= 0 && rank < 9 {
            if let (Some(f), Some(r)) = (File::from_u8(file as u8), Rank::from_u8(rank as u8)) {
                return Some(Square::new(f, r));
            }
        }
        None
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
    fn test_square_offset() {
        let sq = Square::new(File::File5, Rank::Rank5);
        assert_eq!(sq.offset(Square::DELTA_U), Some(Square::new(File::File5, Rank::Rank4)));
        assert_eq!(sq.offset(Square::DELTA_LD), Some(Square::new(File::File6, Rank::Rank6)));

        let edge = Square::new(File::File5, Rank::Rank1);
        assert_eq!(edge.offset(Square::DELTA_U), None);
    }

    #[test]
    fn test_square_all() {
        let all: Vec<_> = Square::all().collect();
        assert_eq!(all.len(), 81);
        assert_eq!(all[0], Square::SQ_11);
        assert_eq!(all[80], Square::SQ_99);
    }
}
