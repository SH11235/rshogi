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
}
