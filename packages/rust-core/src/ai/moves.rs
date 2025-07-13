//! Move representation and utilities
//!
//! Defines move types and basic move operations for shogi

use super::board::{PieceType, Square};

/// Move representation
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Move {
    /// Encoded move data:
    /// - bits 0-6: destination square (0-80)
    /// - bits 7-13: source square (0-80) or piece type for drops (81-87)
    /// - bit 14: promotion flag
    /// - bit 15: drop flag
    data: u16,
}

impl Move {
    /// Null move constant
    pub const NULL: Self = Move { data: 0 };

    /// Create null move (for compatibility)
    #[inline]
    pub const fn null() -> Self {
        Self::NULL
    }

    /// Create normal move (convenience method)
    #[inline]
    pub fn make_normal(from: Square, to: Square) -> Self {
        Self::normal(from, to, false)
    }

    /// Create drop move (convenience method)
    #[inline]
    pub fn make_drop(piece_type: PieceType, to: Square) -> Self {
        Self::drop(piece_type, to)
    }

    /// Create a normal move (piece moving on board)
    #[inline]
    pub fn normal(from: Square, to: Square, promote: bool) -> Self {
        debug_assert!(from.0 < 81 && to.0 < 81);
        let mut data = to.0 as u16;
        data |= (from.0 as u16) << 7;
        if promote {
            data |= 1 << 14;
        }
        Move { data }
    }

    /// Create a drop move (placing piece from hand)
    #[inline]
    pub fn drop(piece_type: PieceType, to: Square) -> Self {
        debug_assert!(to.0 < 81);
        debug_assert!(!matches!(piece_type, PieceType::King));

        let mut data = to.0 as u16;
        // Encode piece type in source field (81-87)
        data |= (81 + piece_type as u16 - 1) << 7; // -1 to skip King
        data |= 1 << 15; // Set drop flag
        Move { data }
    }

    /// Check if this is a null move
    #[inline]
    pub fn is_null(self) -> bool {
        self.data == 0
    }

    /// Get source square (None for drops)
    #[inline]
    pub fn from(self) -> Option<Square> {
        if self.is_drop() {
            None
        } else {
            Some(Square(((self.data >> 7) & 0x7F) as u8))
        }
    }

    /// Get destination square
    #[inline]
    pub fn to(self) -> Square {
        Square((self.data & 0x7F) as u8)
    }

    /// Check if this is a drop move
    #[inline]
    pub fn is_drop(self) -> bool {
        (self.data & (1 << 15)) != 0
    }

    /// Check if this is a promotion
    #[inline]
    pub fn is_promote(self) -> bool {
        (self.data & (1 << 14)) != 0
    }

    /// Get dropped piece type (only valid for drops)
    #[inline]
    pub fn drop_piece_type(self) -> PieceType {
        debug_assert!(self.is_drop());
        let encoded = ((self.data >> 7) & 0x7F) as u8;
        match encoded - 81 {
            0 => PieceType::Rook,
            1 => PieceType::Bishop,
            2 => PieceType::Gold,
            3 => PieceType::Silver,
            4 => PieceType::Knight,
            5 => PieceType::Lance,
            6 => PieceType::Pawn,
            _ => unreachable!(),
        }
    }

    /// Convert to u16 for compact storage
    #[inline]
    pub fn to_u16(self) -> u16 {
        self.data
    }

    /// Create from u16
    #[inline]
    pub fn from_u16(data: u16) -> Self {
        Move { data }
    }

    /// Check if move is pseudo-legal capture (requires board state for accuracy)
    #[inline]
    pub fn is_capture_hint(self) -> bool {
        // This is just a hint - actual capture detection needs board state
        // Used for move ordering
        false
    }
}

impl std::fmt::Display for Move {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_null() {
            write!(f, "null")
        } else if self.is_drop() {
            let piece_type = self.drop_piece_type();
            let to = self.to();
            write!(f, "{piece_type:?}*{to}")
        } else {
            let from = self.from().unwrap();
            let to = self.to();
            if self.is_promote() {
                write!(f, "{from}{to}+")
            } else {
                write!(f, "{from}{to}")
            }
        }
    }
}

/// List of moves with pre-allocated capacity
#[derive(Clone, Debug, Default)]
pub struct MoveList {
    moves: Vec<Move>,
}

impl MoveList {
    /// Create new move list with default capacity
    pub fn new() -> Self {
        // Average number of legal moves in shogi is around 80-100
        MoveList {
            moves: Vec::with_capacity(128),
        }
    }

    /// Add a move to the list
    #[inline]
    pub fn push(&mut self, m: Move) {
        self.moves.push(m);
    }

    /// Get number of moves
    #[inline]
    pub fn len(&self) -> usize {
        self.moves.len()
    }

    /// Check if empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.moves.is_empty()
    }

    /// Clear the list
    #[inline]
    pub fn clear(&mut self) {
        self.moves.clear();
    }

    /// Get slice of moves
    #[inline]
    pub fn as_slice(&self) -> &[Move] {
        &self.moves
    }

    /// Get mutable slice of moves
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut Vec<Move> {
        &mut self.moves
    }

    /// Convert to vector
    #[inline]
    pub fn into_vec(self) -> Vec<Move> {
        self.moves
    }
}

impl std::ops::Index<usize> for MoveList {
    type Output = Move;

    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        &self.moves[index]
    }
}

impl IntoIterator for MoveList {
    type Item = Move;
    type IntoIter = std::vec::IntoIter<Move>;

    fn into_iter(self) -> Self::IntoIter {
        self.moves.into_iter()
    }
}

impl<'a> IntoIterator for &'a MoveList {
    type Item = &'a Move;
    type IntoIter = std::slice::Iter<'a, Move>;

    fn into_iter(self) -> Self::IntoIter {
        self.moves.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_move() {
        let from = Square::new(2, 6);
        let to = Square::new(2, 5);
        let m = Move::normal(from, to, false);

        assert_eq!(m.from(), Some(from));
        assert_eq!(m.to(), to);
        assert!(!m.is_drop());
        assert!(!m.is_promote());
    }

    #[test]
    fn test_promotion_move() {
        let from = Square::new(2, 2);
        let to = Square::new(2, 1);
        let m = Move::normal(from, to, true);

        assert_eq!(m.from(), Some(from));
        assert_eq!(m.to(), to);
        assert!(!m.is_drop());
        assert!(m.is_promote());
    }

    #[test]
    fn test_drop_move() {
        let to = Square::new(4, 4);
        let m = Move::drop(PieceType::Pawn, to);

        assert_eq!(m.from(), None);
        assert_eq!(m.to(), to);
        assert!(m.is_drop());
        assert!(!m.is_promote());
        assert_eq!(m.drop_piece_type(), PieceType::Pawn);
    }

    #[test]
    fn test_move_display() {
        let m1 = Move::normal(Square::new(2, 6), Square::new(2, 5), false);
        assert_eq!(m1.to_string(), "7g7f");

        let m2 = Move::normal(Square::new(2, 2), Square::new(2, 1), true);
        assert_eq!(m2.to_string(), "7c7b+");

        let m3 = Move::drop(PieceType::Pawn, Square::new(4, 4));
        assert_eq!(m3.to_string(), "Pawn*5e");
    }

    #[test]
    fn test_move_list() {
        let mut list = MoveList::new();
        assert!(list.is_empty());

        list.push(Move::normal(Square::new(2, 6), Square::new(2, 5), false));
        list.push(Move::drop(PieceType::Pawn, Square::new(4, 4)));

        assert_eq!(list.len(), 2);
        assert!(!list.is_empty());

        // Test indexing
        let m0 = list[0];
        assert_eq!(m0.to(), Square::new(2, 5));

        // Test iteration
        let moves: Vec<Move> = list.into_iter().collect();
        assert_eq!(moves.len(), 2);
    }

    #[test]
    fn test_move_encoding() {
        // 16ビットエンコーディングの全パターンテスト

        // 通常の移動（成りなし）
        let m1 = Move::normal(Square::new(0, 0), Square::new(8, 8), false);
        assert_eq!(m1.from(), Some(Square::new(0, 0)));
        assert_eq!(m1.to(), Square::new(8, 8));
        assert!(!m1.is_promote());
        assert!(!m1.is_drop());

        // 通常の移動（成りあり）
        let m2 = Move::normal(Square::new(4, 2), Square::new(4, 6), true);
        assert_eq!(m2.from(), Some(Square::new(4, 2)));
        assert_eq!(m2.to(), Square::new(4, 6));
        assert!(m2.is_promote());
        assert!(!m2.is_drop());

        // 持ち駒を打つ（各駒種）
        let piece_types = [
            PieceType::Rook,
            PieceType::Bishop,
            PieceType::Gold,
            PieceType::Silver,
            PieceType::Knight,
            PieceType::Lance,
            PieceType::Pawn,
        ];

        for pt in &piece_types {
            let m = Move::drop(*pt, Square::new(4, 4));
            assert_eq!(m.from(), None);
            assert_eq!(m.to(), Square::new(4, 4));
            assert!(m.is_drop());
            assert!(!m.is_promote());
            assert_eq!(m.drop_piece_type(), *pt);
        }
    }

    #[test]
    fn test_move_to_u16_from_u16() {
        // to_u16() → from_u16() のラウンドトリップテスト

        // 全ての升目の組み合わせをテスト（サンプリング）
        for from_file in 0..9 {
            for from_rank in 0..9 {
                for to_file in 0..9 {
                    for to_rank in 0..9 {
                        let from = Square::new(from_file, from_rank);
                        let to = Square::new(to_file, to_rank);

                        // 成りなし
                        let m1 = Move::normal(from, to, false);
                        let encoded1 = m1.to_u16();
                        let decoded1 = Move::from_u16(encoded1);
                        assert_eq!(m1, decoded1);

                        // 成りあり
                        let m2 = Move::normal(from, to, true);
                        let encoded2 = m2.to_u16();
                        let decoded2 = Move::from_u16(encoded2);
                        assert_eq!(m2, decoded2);
                    }
                }
            }
        }

        // 持ち駒打ちのテスト
        for pt in &[
            PieceType::Rook,
            PieceType::Bishop,
            PieceType::Gold,
            PieceType::Silver,
            PieceType::Knight,
            PieceType::Lance,
            PieceType::Pawn,
        ] {
            for file in 0..9 {
                for rank in 0..9 {
                    let to = Square::new(file, rank);
                    let m = Move::drop(*pt, to);
                    let encoded = m.to_u16();
                    let decoded = Move::from_u16(encoded);
                    assert_eq!(m, decoded);
                }
            }
        }
    }

    #[test]
    fn test_move_null() {
        // NULL moveのテスト
        assert!(Move::NULL.is_null());
        assert_eq!(Move::NULL.to_u16(), 0);

        let normal_move = Move::normal(Square::new(0, 0), Square::new(0, 1), false);
        assert!(!normal_move.is_null());
    }

    #[test]
    fn test_move_is_capture_hint() {
        // キャプチャヒントのテスト
        let m1 = Move::normal(Square::new(0, 0), Square::new(0, 1), false);
        assert!(!m1.is_capture_hint());

        // キャプチャヒントを設定（実装がある場合）
        // 注: 現在の実装にis_capture_hintメソッドがない場合はこのテストはスキップ
    }

    #[test]
    fn test_move_list_operations() {
        // MoveListの各種操作テスト
        let mut list = MoveList::new();

        // 初期状態
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);

        // 要素の追加
        for i in 0..10 {
            list.push(Move::normal(
                Square::new((i % 9) as u8, 0),
                Square::new((i % 9) as u8, 1),
                false,
            ));
        }

        assert!(!list.is_empty());
        assert_eq!(list.len(), 10);

        // スライスへのアクセス
        let slice = list.as_slice();
        assert_eq!(slice.len(), 10);

        // インデックスアクセス
        for i in 0..10 {
            let m = list[i];
            assert_eq!(m.from(), Some(Square::new((i % 9) as u8, 0)));
        }

        // clear操作
        list.clear();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn test_move_list_capacity() {
        // MoveListの容量テスト（256手まで）
        let mut list = MoveList::new();

        // 最大容量までの追加をテスト
        for i in 0..256 {
            list.push(Move::normal(
                Square::new((i % 9) as u8, (i / 9 % 9) as u8),
                Square::new(((i + 1) % 9) as u8, ((i + 1) / 9 % 9) as u8),
                false,
            ));
        }

        assert_eq!(list.len(), 256);

        // 全ての要素が正しく保存されているか確認
        for i in 0..256 {
            let m = list[i];
            assert_eq!(m.from(), Some(Square::new((i % 9) as u8, (i / 9 % 9) as u8)));
            assert_eq!(m.to(), Square::new(((i + 1) % 9) as u8, ((i + 1) / 9 % 9) as u8));
        }
    }

    #[test]
    fn test_move_list_iterator() {
        // イテレータの正確性テスト
        let mut list = MoveList::new();
        let moves_data = vec![
            Move::normal(Square::new(0, 0), Square::new(0, 1), false),
            Move::normal(Square::new(1, 1), Square::new(1, 2), true),
            Move::drop(PieceType::Pawn, Square::new(4, 4)),
        ];

        for m in &moves_data {
            list.push(*m);
        }

        // 参照イテレータ
        let collected: Vec<_> = list.as_slice().to_vec();
        assert_eq!(collected, moves_data);

        // into_iterイテレータ
        let collected2: Vec<_> = list.into_iter().collect();
        assert_eq!(collected2, moves_data);
    }

    #[test]
    fn test_move_boundary_cases() {
        // 境界値のテスト

        // 角の升目
        let corners = [
            Square::new(0, 0), // 9九
            Square::new(8, 0), // 1九
            Square::new(0, 8), // 9一
            Square::new(8, 8), // 1一
        ];

        for &from in &corners {
            for &to in &corners {
                if from.index() != to.index() {
                    let m = Move::normal(from, to, false);
                    assert_eq!(m.from(), Some(from));
                    assert_eq!(m.to(), to);

                    // エンコード/デコードのテスト
                    let encoded = m.to_u16();
                    let decoded = Move::from_u16(encoded);
                    assert_eq!(m, decoded);
                }
            }
        }
    }
}
