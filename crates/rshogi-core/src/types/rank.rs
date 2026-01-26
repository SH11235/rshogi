//! 段（Rank）

use super::Color;

/// 段（1段〜9段）
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Rank {
    Rank1 = 0,
    Rank2 = 1,
    Rank3 = 2,
    Rank4 = 3,
    Rank5 = 4,
    Rank6 = 5,
    Rank7 = 6,
    Rank8 = 7,
    Rank9 = 8,
}

impl Rank {
    /// 段の数
    pub const NUM: usize = 9;

    /// 全ての段
    pub const ALL: [Rank; 9] = [
        Rank::Rank1,
        Rank::Rank2,
        Rank::Rank3,
        Rank::Rank4,
        Rank::Rank5,
        Rank::Rank6,
        Rank::Rank7,
        Rank::Rank8,
        Rank::Rank9,
    ];

    /// 成れる段かどうか（先手視点で1-3段、後手視点で7-9段）
    #[inline]
    pub const fn can_promote(self, color: Color) -> bool {
        match color {
            Color::Black => (self as u8) <= (Rank::Rank3 as u8),
            Color::White => (self as u8) >= (Rank::Rank7 as u8),
        }
    }

    /// 相対段（先手から見た段）
    #[inline]
    pub const fn relative(self, color: Color) -> Rank {
        match color {
            Color::Black => self,
            // SAFETY: 8 - n where n is 0..=8, so result is 0..=8
            Color::White => unsafe { std::mem::transmute::<u8, Rank>(8 - self as u8) },
        }
    }

    /// u8からRankに変換
    #[inline]
    pub const fn from_u8(n: u8) -> Option<Rank> {
        if n < 9 {
            // SAFETY: n < 9 なので有効なRank値
            Some(unsafe { std::mem::transmute::<u8, Rank>(n) })
        } else {
            None
        }
    }

    /// インデックスとして使用
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    /// USI形式の文字（'a'-'i'）に変換
    #[inline]
    pub const fn to_usi_char(self) -> char {
        // b'a' は u8 型のバイトリテラル で、ASCII 'a' の数値 97（0x61）
        // self as u8 は 0〜8 の範囲
        (b'a' + self as u8) as char
    }

    /// USI形式の文字からRankに変換
    #[inline]
    pub const fn from_usi_char(c: char) -> Option<Rank> {
        let n = (c as u8).wrapping_sub(b'a');
        Rank::from_u8(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rank_from_u8() {
        assert_eq!(Rank::from_u8(0), Some(Rank::Rank1));
        assert_eq!(Rank::from_u8(8), Some(Rank::Rank9));
        assert_eq!(Rank::from_u8(9), None);
    }

    #[test]
    fn test_rank_index() {
        assert_eq!(Rank::Rank1.index(), 0);
        assert_eq!(Rank::Rank9.index(), 8);
    }

    #[test]
    fn test_rank_usi() {
        assert_eq!(Rank::Rank1.to_usi_char(), 'a');
        assert_eq!(Rank::Rank9.to_usi_char(), 'i');
        assert_eq!(Rank::from_usi_char('a'), Some(Rank::Rank1));
        assert_eq!(Rank::from_usi_char('i'), Some(Rank::Rank9));
        assert_eq!(Rank::from_usi_char('j'), None);
    }

    #[test]
    fn test_rank_can_promote() {
        // 先手: 1-3段で成れる
        assert!(Rank::Rank1.can_promote(Color::Black));
        assert!(Rank::Rank3.can_promote(Color::Black));
        assert!(!Rank::Rank4.can_promote(Color::Black));

        // 後手: 7-9段で成れる
        assert!(!Rank::Rank6.can_promote(Color::White));
        assert!(Rank::Rank7.can_promote(Color::White));
        assert!(Rank::Rank9.can_promote(Color::White));
    }

    #[test]
    fn test_rank_relative() {
        // 先手視点はそのまま
        assert_eq!(Rank::Rank1.relative(Color::Black), Rank::Rank1);
        assert_eq!(Rank::Rank9.relative(Color::Black), Rank::Rank9);

        // 後手視点は反転
        assert_eq!(Rank::Rank1.relative(Color::White), Rank::Rank9);
        assert_eq!(Rank::Rank9.relative(Color::White), Rank::Rank1);
        assert_eq!(Rank::Rank5.relative(Color::White), Rank::Rank5); // 中央は同じ
    }
}
