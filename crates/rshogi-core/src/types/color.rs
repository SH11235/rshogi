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

// SAFETY: Color は #[repr(u8)] で値域 0..=1。Color::NUM (2) 以上の配列に対して安全。
impl<T> Index<Color> for [T] {
    type Output = T;

    #[inline]
    fn index(&self, c: Color) -> &T {
        debug_assert!((c as usize) < self.len());
        unsafe { self.get_unchecked(c as usize) }
    }
}

impl<T> IndexMut<Color> for [T] {
    #[inline]
    fn index_mut(&mut self, c: Color) -> &mut T {
        debug_assert!((c as usize) < self.len());
        unsafe { self.get_unchecked_mut(c as usize) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
