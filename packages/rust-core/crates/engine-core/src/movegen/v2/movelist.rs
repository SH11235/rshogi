use crate::shogi::moves::Move;
use std::mem::MaybeUninit;

/// A list of moves with fixed-size storage to avoid heap allocation
pub struct MoveList {
    // Maximum theoretical number of moves in shogi is 593, but in practice it's usually under 200
    // We use 256 for power-of-2 alignment and sufficient margin
    moves: [MaybeUninit<Move>; 256],
    len: usize,
}

impl MoveList {
    /// Create a new empty move list
    #[inline]
    pub const fn new() -> Self {
        unsafe {
            Self {
                moves: MaybeUninit::uninit().assume_init(),
                len: 0,
            }
        }
    }

    /// Add a move to the list
    #[inline]
    pub fn push(&mut self, mv: Move) {
        debug_assert!(self.len < 256, "Move list overflow");
        unsafe {
            self.moves[self.len].write(mv);
        }
        self.len += 1;
    }

    /// Get the number of moves in the list
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Check if the list is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Get a slice of all moves
    pub fn as_slice(&self) -> &[Move] {
        unsafe {
            std::slice::from_raw_parts(self.moves.as_ptr() as *const Move, self.len)
        }
    }

    /// Clear the list
    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Get an iterator over the moves
    pub fn iter(&self) -> std::slice::Iter<'_, Move> {
        self.as_slice().iter()
    }
}

// Implement IntoIterator for ergonomic use
impl IntoIterator for MoveList {
    type Item = Move;
    type IntoIter = MoveListIntoIter;

    fn into_iter(self) -> Self::IntoIter {
        MoveListIntoIter {
            list: self,
            index: 0,
        }
    }
}

pub struct MoveListIntoIter {
    list: MoveList,
    index: usize,
}

impl Iterator for MoveListIntoIter {
    type Item = Move;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.list.len {
            None
        } else {
            unsafe {
                let mv = self.list.moves[self.index].assume_init_read();
                self.index += 1;
                Some(mv)
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.list.len - self.index;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for MoveListIntoIter {}

// For compatibility with existing code that expects Vec<Move>
impl From<MoveList> for Vec<Move> {
    fn from(list: MoveList) -> Self {
        list.as_slice().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shogi::moves::{Move, NormalMove};
    use crate::shogi::{Piece, PieceType, Color, Square};

    #[test]
    fn test_movelist_basic_operations() {
        let mut list = MoveList::new();
        assert!(list.is_empty());
        assert_eq!(list.len(), 0);

        // Create a test move
        let mv = Move::Normal(NormalMove {
            from: Square::SQ_77,
            to: Square::SQ_76,
            piece: Piece {
                piece_type: PieceType::Pawn,
                owner: Color::Black,
            },
            captured: None,
            promote: false,
        });

        list.push(mv.clone());
        assert_eq!(list.len(), 1);
        assert!(!list.is_empty());

        let moves = list.as_slice();
        assert_eq!(moves.len(), 1);
        assert_eq!(moves[0], mv);
    }

    #[test]
    fn test_movelist_iterator() {
        let mut list = MoveList::new();
        
        // Add multiple moves
        for i in 0..10 {
            let mv = Move::Normal(NormalMove {
                from: Square::SQ_77,
                to: Square::from_index(i),
                piece: Piece {
                    piece_type: PieceType::Pawn,
                    owner: Color::Black,
                },
                captured: None,
                promote: false,
            });
            list.push(mv);
        }

        // Test borrowing iterator
        let mut count = 0;
        for _ in list.iter() {
            count += 1;
        }
        assert_eq!(count, 10);

        // Test consuming iterator
        count = 0;
        for _ in list {
            count += 1;
        }
        assert_eq!(count, 10);
    }

    #[test]
    fn test_movelist_to_vec() {
        let mut list = MoveList::new();
        
        let mv = Move::Normal(NormalMove {
            from: Square::SQ_77,
            to: Square::SQ_76,
            piece: Piece {
                piece_type: PieceType::Pawn,
                owner: Color::Black,
            },
            captured: None,
            promote: false,
        });
        
        list.push(mv.clone());
        list.push(mv.clone());
        
        let vec: Vec<Move> = list.into();
        assert_eq!(vec.len(), 2);
        assert_eq!(vec[0], mv);
        assert_eq!(vec[1], mv);
    }
}