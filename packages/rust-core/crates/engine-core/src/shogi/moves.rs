//! Move representation and utilities
//!
//! Defines move types and basic move operations for shogi

use super::board::{PieceType, Square};
use smallvec::SmallVec;

/// Type alias for move lists using SmallVec
/// Most shogi positions have < 128 legal moves, so this avoids heap allocation
pub type MoveVec = SmallVec<[Move; 128]>;

/// Type alias for tracking tried moves in history updates
/// Limited to 16 moves to minimize stack usage (MAX_MOVES_TO_UPDATE)
pub type TriedMoves = SmallVec<[Move; 16]>;

/// Type alias for capture move lists
/// Most positions have < 32 capture moves
pub type CaptureBuf = SmallVec<[Move; 32]>;

/// Type alias for large move buffers when needed
/// Use sparingly due to stack size (512 bytes)
pub type BigMoveBuf = SmallVec<[Move; 128]>;

/// Move representation
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Move {
    /// Encoded move data (32-bit):
    /// - bits 0-6: destination square (0-80)
    /// - bits 7-13: source square (0-80) or piece type for drops (81-87)
    /// - bit 14: promotion flag
    /// - bit 15: drop flag
    /// - bits 16-19: piece type (0-13)
    /// - bits 20-23: captured piece type (0-13, 15 for no capture)
    /// - bits 24-31: reserved for future use
    data: u32,
}

impl Default for Move {
    /// Returns a null move (no-op move)
    ///
    /// This ensures that the default move is consistently a null move,
    /// which is semantically meaningful in the context of chess/shogi engines.
    #[inline]
    fn default() -> Self {
        Self::null()
    }
}

impl Move {
    /// Null move constant
    ///
    /// Represents a no-op move, used in various contexts:
    /// - Default/uninitialized move value
    /// - Null move pruning in search algorithms
    /// - Placeholder when no valid move exists
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
    /// Note: This is a temporary API. Use normal_with_piece for full functionality.
    #[inline]
    pub fn normal(from: Square, to: Square, promote: bool) -> Self {
        debug_assert!(from.0 < 81 && to.0 < 81);
        let mut data = to.0 as u32;
        data |= (from.0 as u32) << 7;
        if promote {
            data |= 1 << 14;
        }
        // Piece type will be 0 (unknown) for backward compatibility
        Move { data }
    }

    /// Create a normal move with piece type information
    #[inline]
    pub fn normal_with_piece(
        from: Square,
        to: Square,
        promote: bool,
        piece_type: PieceType,
        captured_type: Option<PieceType>,
    ) -> Self {
        debug_assert!(from.0 < 81 && to.0 < 81);
        let mut data = to.0 as u32;
        data |= (from.0 as u32) << 7;
        if promote {
            data |= 1 << 14;
        }
        // Encode piece type (add 1 to distinguish from 0 = unknown)
        data |= ((piece_type as u32) + 1) << 16;
        // Encode captured piece type (15 for no capture, add 1 to distinguish from 0)
        let captured_bits = captured_type.map(|t| (t as u32) + 1).unwrap_or(15);
        data |= captured_bits << 20;
        Move { data }
    }

    /// Create a drop move (placing piece from hand)
    #[inline]
    pub fn drop(piece_type: PieceType, to: Square) -> Self {
        debug_assert!(to.0 < 81);
        debug_assert!(!matches!(piece_type, PieceType::King));

        let mut data = to.0 as u32;
        // Encode piece type in source field (81-87)
        data |= (81 + piece_type as u32 - 1) << 7; // -1 to skip King
        data |= 1 << 15; // Set drop flag
                         // Also store piece type in the dedicated field (add 1 to distinguish from 0 = unknown)
        data |= ((piece_type as u32) + 1) << 16;
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

    /// Convert to u32 for storage
    #[inline]
    pub fn to_u32(self) -> u32 {
        self.data
    }

    /// Create from u32
    #[inline]
    pub fn from_u32(data: u32) -> Self {
        Move { data }
    }

    /// Convert to u16 for backward compatibility (loses piece type info)
    #[inline]
    pub fn to_u16(self) -> u16 {
        (self.data & 0xFFFF) as u16
    }

    /// Create from u16 (for backward compatibility)
    #[inline]
    pub fn from_u16(data: u16) -> Self {
        Move { data: data as u32 }
    }

    /// Get the piece type being moved
    #[inline]
    pub fn piece_type(self) -> Option<PieceType> {
        if self.is_null() {
            return None;
        }
        if self.is_drop() {
            return Some(self.drop_piece_type());
        }
        let piece_bits = ((self.data >> 16) & 0xF) as u8;
        if piece_bits == 0 {
            None // Unknown piece type (backward compatibility)
        } else {
            // PieceType enum values: King=0, Rook=1, ..., Pawn=7
            // We stored piece_type + 1, so subtract 1 to get original value
            Some(unsafe { std::mem::transmute::<u8, PieceType>(piece_bits - 1) })
        }
    }

    /// Get the captured piece type
    #[inline]
    pub fn captured_piece_type(self) -> Option<PieceType> {
        let captured_bits = ((self.data >> 20) & 0xF) as u8;
        if captured_bits == 15 || captured_bits == 0 {
            None // No capture (15) or unknown/old format (0)
        } else {
            // Values 1-14 represent actual piece types
            Some(unsafe { std::mem::transmute::<u8, PieceType>(captured_bits - 1) })
        }
    }

    /// Check if move is pseudo-legal capture (requires board state for accuracy)
    #[inline]
    pub fn is_capture_hint(self) -> bool {
        // Now we can check if there's a captured piece
        self.captured_piece_type().is_some()
    }

    /// Create move from USI string
    pub fn from_usi(usi: &str) -> Result<Self, String> {
        crate::usi::parse_usi_move(usi).map_err(|e| e.to_string())
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

    /// Iterator over moves
    #[inline]
    pub fn iter(&self) -> std::slice::Iter<'_, Move> {
        self.moves.iter()
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
    use crate::usi::parse_usi_square;

    use super::*;

    #[test]
    fn test_normal_move() {
        let from = parse_usi_square("7c").unwrap();
        let to = parse_usi_square("7d").unwrap();
        let m = Move::normal(from, to, false);

        assert_eq!(m.from(), Some(from));
        assert_eq!(m.to(), to);
        assert!(!m.is_drop());
        assert!(!m.is_promote());
    }

    #[test]
    fn test_promotion_move() {
        let from = parse_usi_square("7c").unwrap();
        let to = parse_usi_square("7b").unwrap();
        let m = Move::normal(from, to, true);

        assert_eq!(m.from(), Some(from));
        assert_eq!(m.to(), to);
        assert!(!m.is_drop());
        assert!(m.is_promote());
    }

    #[test]
    fn test_drop_move() {
        let to = parse_usi_square("5e").unwrap();
        let m = Move::drop(PieceType::Pawn, to);

        assert_eq!(m.from(), None);
        assert_eq!(m.to(), to);
        assert!(m.is_drop());
        assert!(!m.is_promote());
        assert_eq!(m.drop_piece_type(), PieceType::Pawn);
    }

    #[test]
    fn test_move_display() {
        // With Black=top coordinate system:
        // Black pieces at ranks 0-2, White at ranks 6-8
        let m1 =
            Move::normal(parse_usi_square("7c").unwrap(), parse_usi_square("7d").unwrap(), false);
        assert_eq!(m1.to_string(), "7c7d");

        let m2 =
            Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7h").unwrap(), true);
        assert_eq!(m2.to_string(), "7g7h+");

        let m3 = Move::drop(PieceType::Pawn, parse_usi_square("5e").unwrap());
        assert_eq!(m3.to_string(), "Pawn*5e");
    }

    #[test]
    fn test_move_list() {
        let mut list = MoveList::new();
        assert!(list.is_empty());

        list.push(Move::normal(
            parse_usi_square("7c").unwrap(),
            parse_usi_square("7d").unwrap(),
            false,
        ));
        list.push(Move::drop(PieceType::Pawn, parse_usi_square("5e").unwrap()));

        assert_eq!(list.len(), 2);
        assert!(!list.is_empty());

        // Test indexing
        let m0 = list[0];
        assert_eq!(m0.to(), parse_usi_square("7d").unwrap());

        // Test iteration
        let moves: Vec<Move> = list.into_iter().collect();
        assert_eq!(moves.len(), 2);
    }

    #[test]
    fn test_move_encoding() {
        // 32ビットエンコーディングの全パターンテスト

        // 通常の移動（成りなし）
        let m1 =
            Move::normal(parse_usi_square("9a").unwrap(), parse_usi_square("1i").unwrap(), false);
        assert_eq!(m1.from(), Some(parse_usi_square("9a").unwrap()));
        assert_eq!(m1.to(), parse_usi_square("1i").unwrap());
        assert!(!m1.is_promote());
        assert!(!m1.is_drop());

        // 通常の移動（成りあり）
        let m2 =
            Move::normal(parse_usi_square("5c").unwrap(), parse_usi_square("5g").unwrap(), true);
        assert_eq!(m2.from(), Some(parse_usi_square("5c").unwrap()));
        assert_eq!(m2.to(), parse_usi_square("5g").unwrap());
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
            let m = Move::drop(*pt, parse_usi_square("5e").unwrap());
            assert_eq!(m.from(), None);
            assert_eq!(m.to(), parse_usi_square("5e").unwrap());
            assert!(m.is_drop());
            assert!(!m.is_promote());
            assert_eq!(m.drop_piece_type(), *pt);
        }
    }

    #[test]
    fn test_move_to_u32_from_u32() {
        // to_u32() → from_u32() のラウンドトリップテスト

        // 全ての升目の組み合わせをテスト（サンプリング）
        for from_file in 0..9 {
            for from_rank in 0..9 {
                for to_file in 0..9 {
                    for to_rank in 0..9 {
                        let from = Square::new(from_file, from_rank);
                        let to = Square::new(to_file, to_rank);

                        // 成りなし
                        let m1 = Move::normal(from, to, false);
                        let encoded1 = m1.to_u32();
                        let decoded1 = Move::from_u32(encoded1);
                        assert_eq!(m1, decoded1);

                        // 成りあり
                        let m2 = Move::normal(from, to, true);
                        let encoded2 = m2.to_u32();
                        let decoded2 = Move::from_u32(encoded2);
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
                    let encoded = m.to_u32();
                    let decoded = Move::from_u32(encoded);
                    assert_eq!(m, decoded);
                    // Drop moves should store piece type
                    assert_eq!(m.piece_type(), Some(*pt));
                }
            }
        }
    }

    #[test]
    fn test_move_null() {
        // NULL moveのテスト
        assert!(Move::NULL.is_null());
        assert_eq!(Move::NULL.to_u16(), 0);

        let normal_move =
            Move::normal(parse_usi_square("9a").unwrap(), parse_usi_square("9b").unwrap(), false);
        assert!(!normal_move.is_null());
    }

    #[test]
    fn test_move_is_capture_hint() {
        // キャプチャヒントのテスト
        let m1 =
            Move::normal(parse_usi_square("9a").unwrap(), parse_usi_square("9b").unwrap(), false);
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
            Move::normal(parse_usi_square("9a").unwrap(), parse_usi_square("9b").unwrap(), false),
            Move::normal(parse_usi_square("8b").unwrap(), parse_usi_square("8c").unwrap(), true),
            Move::drop(PieceType::Pawn, parse_usi_square("5e").unwrap()),
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
            parse_usi_square("9a").unwrap(), // 9九
            parse_usi_square("1a").unwrap(), // 1九
            parse_usi_square("9i").unwrap(), // 9一
            parse_usi_square("1i").unwrap(), // 1一
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

    #[test]
    fn test_move_with_piece_type() {
        // Test normal_with_piece API
        let from = parse_usi_square("7c").unwrap();
        let to = parse_usi_square("7d").unwrap();

        // Test without capture
        let m1 = Move::normal_with_piece(from, to, false, PieceType::Pawn, None);
        assert_eq!(m1.from(), Some(from));
        assert_eq!(m1.to(), to);
        assert!(!m1.is_promote());
        assert_eq!(m1.piece_type(), Some(PieceType::Pawn));
        assert_eq!(m1.captured_piece_type(), None);
        assert!(!m1.is_capture_hint());

        // Test with capture
        let m2 = Move::normal_with_piece(from, to, true, PieceType::Silver, Some(PieceType::Gold));
        assert_eq!(m2.from(), Some(from));
        assert_eq!(m2.to(), to);
        assert!(m2.is_promote());
        assert_eq!(m2.piece_type(), Some(PieceType::Silver));
        assert_eq!(m2.captured_piece_type(), Some(PieceType::Gold));
        assert!(m2.is_capture_hint());

        // Test encoding/decoding preserves all information
        let encoded = m2.to_u32();
        let decoded = Move::from_u32(encoded);
        assert_eq!(m2, decoded);
        assert_eq!(decoded.piece_type(), Some(PieceType::Silver));
        assert_eq!(decoded.captured_piece_type(), Some(PieceType::Gold));
    }

    #[test]
    fn test_backward_compatibility() {
        // Test that old Move::normal creates moves with unknown piece type
        let m =
            Move::normal(parse_usi_square("7c").unwrap(), parse_usi_square("7d").unwrap(), false);
        assert_eq!(m.piece_type(), None); // Unknown piece type
        assert_eq!(m.captured_piece_type(), None);

        // Test u16 compatibility (loses piece type info)
        let m_with_type = Move::normal_with_piece(
            parse_usi_square("7c").unwrap(),
            parse_usi_square("7d").unwrap(),
            false,
            PieceType::Pawn,
            Some(PieceType::Gold),
        );
        let u16_encoded = m_with_type.to_u16();
        let u16_decoded = Move::from_u16(u16_encoded);

        // Basic move info preserved
        assert_eq!(u16_decoded.from(), Some(parse_usi_square("7c").unwrap()));
        assert_eq!(u16_decoded.to(), parse_usi_square("7d").unwrap());
        assert!(!u16_decoded.is_promote());

        // But piece type info is lost
        assert_eq!(u16_decoded.piece_type(), None);
        assert_eq!(u16_decoded.captured_piece_type(), None);
    }
}
