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
        debug_assert!(self.len < MAX_MOVES);
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
    use crate::types::{File, PieceType, Rank, Square};

    #[test]
    fn test_movelist_new() {
        let list = MoveList::new();
        assert_eq!(list.len(), 0);
        assert!(list.is_empty());
    }

    #[test]
    fn test_movelist_push() {
        let mut list = MoveList::new();
        let sq1 = Square::new(File::File7, Rank::Rank7);
        let sq2 = Square::new(File::File7, Rank::Rank6);
        let mv = Move::new_move(sq1, sq2, false);

        list.push(mv);
        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());
        assert_eq!(list.at(0), mv);
        assert!(list.contains(mv));
    }

    #[test]
    fn test_movelist_iter() {
        let mut list = MoveList::new();
        let sq1 = Square::new(File::File7, Rank::Rank7);
        let sq2 = Square::new(File::File7, Rank::Rank6);
        let sq3 = Square::new(File::File5, Rank::Rank5);

        list.push(Move::new_move(sq1, sq2, false));
        list.push(Move::new_drop(PieceType::Pawn, sq3));

        let moves: Vec<_> = list.iter().collect();
        assert_eq!(moves.len(), 2);
    }

    #[test]
    fn test_movelist_index() {
        let mut list = MoveList::new();
        let sq = Square::new(File::File5, Rank::Rank5);
        let mv = Move::new_drop(PieceType::Gold, sq);
        list.push(mv);

        assert_eq!(list[0], mv);
    }
}
