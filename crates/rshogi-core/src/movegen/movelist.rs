//! 指し手リスト

use crate::types::Move;

use super::types::MAX_MOVES;

/// 指し手生成バッファ
pub struct MoveList {
    moves: [Move; MAX_MOVES],
    len: usize,
}

impl MoveList {
    /// 空のMoveListを作成
    #[inline]
    pub const fn new() -> Self {
        Self {
            moves: [Move::NONE; MAX_MOVES],
            len: 0,
        }
    }

    /// 指し手の数
    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// 空かどうか
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// イテレータを取得
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &Move> {
        self.moves[..self.len].iter()
    }

    /// 指定された指し手が含まれているか
    pub fn contains(&self, mv: Move) -> bool {
        self.moves[..self.len].contains(&mv)
    }

    /// i番目の指し手を取得
    #[inline]
    pub fn at(&self, i: usize) -> Move {
        debug_assert!(i < self.len);
        self.moves[i]
    }

    /// 指し手を追加
    #[inline]
    pub fn push(&mut self, mv: Move) {
        if self.len >= MAX_MOVES {
            // バッファ溢れ時はそれ以上追加しない（releaseでも安全に）
            return;
        }
        self.moves[self.len] = mv;
        self.len += 1;
    }

    /// 内部バッファへの可変参照を取得
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [Move; MAX_MOVES] {
        &mut self.moves
    }

    /// 長さを設定（生成後に呼び出す）
    #[inline]
    pub fn set_len(&mut self, len: usize) {
        debug_assert!(len <= MAX_MOVES);
        self.len = len;
    }

    /// スライスとして取得
    #[inline]
    pub fn as_slice(&self) -> &[Move] {
        &self.moves[..self.len]
    }
}

impl Default for MoveList {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Index<usize> for MoveList {
    type Output = Move;

    fn index(&self, index: usize) -> &Self::Output {
        &self.moves[index]
    }
}

impl<'a> IntoIterator for &'a MoveList {
    type Item = &'a Move;
    type IntoIter = std::slice::Iter<'a, Move>;

    fn into_iter(self) -> Self::IntoIter {
        self.moves[..self.len].iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_movelist_lifecycle() {
        let mut list = MoveList::new();
        assert!(list.is_empty());
        let moves = [
            Move::from_usi("7g7f").unwrap(),
            Move::from_usi("P*5e").unwrap(),
        ];
        for mv in moves {
            list.push(mv);
        }
        assert!(!list.is_empty());
        assert_eq!(list.len(), moves.len());
        assert_eq!(list.iter().copied().collect::<Vec<_>>(), moves);
        for (i, mv) in moves.into_iter().enumerate() {
            assert_eq!(list.at(i), mv);
            assert_eq!(list[i], mv);
            assert!(list.contains(mv));
        }
    }

    #[test]
    fn test_movelist_push_overflow_is_safe() {
        let mut list = MoveList::new();
        for _ in 0..MAX_MOVES {
            list.push(Move::NONE);
        }
        let len_before = list.len();
        list.push(Move::NONE);
        assert_eq!(list.len(), len_before);
    }
}
