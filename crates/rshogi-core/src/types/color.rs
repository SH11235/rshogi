//! 手番（Color）

use std::ops::{Index, IndexMut};

/// 手番（先手/後手）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Color {
    Black = 0,
    White = 1,
}

impl Color {
    /// 手番の数
    pub const NUM: usize = 2;

    /// 相手番を返す
    #[inline]
    pub const fn opponent(self) -> Color {
        match self {
            Color::Black => Color::White,
            Color::White => Color::Black,
        }
    }

    /// インデックスとして使用（配列アクセス用）
    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }
}

impl std::ops::Not for Color {
    type Output = Color;

    #[inline]
    fn not(self) -> Color {
        self.opponent()
    }
}

impl<T> Index<Color> for [T] {
    type Output = T;

    #[inline]
    fn index(&self, c: Color) -> &T {
        &self[c.index()]
    }
}

impl<T> IndexMut<Color> for [T] {
    #[inline]
    fn index_mut(&mut self, c: Color) -> &mut T {
        &mut self[c.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_index_checks_length() {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        for len in 0..=Color::NUM {
            let mut storage = vec![0; len];
            let values = storage.as_mut_slice();
            for color in [Color::Black, Color::White] {
                if color.index() < len {
                    values[color] = 7;
                    assert_eq!(values[color], 7);
                } else {
                    assert!(catch_unwind(|| values[color]).is_err());
                    assert!(catch_unwind(AssertUnwindSafe(|| values[color] = 1)).is_err());
                }
            }
        }
    }

    #[test]
    fn test_color_opponent() {
        assert_eq!(Color::Black.opponent(), Color::White);
        assert_eq!(Color::White.opponent(), Color::Black);
    }

    #[test]
    fn test_color_not() {
        assert_eq!(!Color::Black, Color::White);
        assert_eq!(!Color::White, Color::Black);
    }

    #[test]
    fn test_color_index() {
        assert_eq!(Color::Black.index(), 0);
        assert_eq!(Color::White.index(), 1);
    }
}
