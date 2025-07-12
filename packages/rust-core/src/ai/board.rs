//! Bitboard representation for shogi board
//!
//! Represents 81-square shogi board using 128-bit integers for fast operations

use std::fmt;

/// Square on shogi board (0-80)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Square(pub u8); // 0-80 (9x9)

impl Square {
    /// Create square from file and rank
    #[inline]
    pub const fn new(file: u8, rank: u8) -> Self {
        debug_assert!(file < 9 && rank < 9);
        Square(rank * 9 + file)
    }

    /// Get file (0-8, right to left)
    #[inline]
    pub const fn file(self) -> u8 {
        self.0 % 9
    }

    /// Get rank (0-8, top to bottom)
    #[inline]
    pub const fn rank(self) -> u8 {
        self.0 / 9
    }

    /// Get index
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Flip for opponent's perspective
    #[inline]
    pub const fn flip(self) -> Self {
        Square(80 - self.0)
    }
}

impl fmt::Display for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = b'9' - self.file();
        let rank = b'a' + self.rank();
        write!(f, "{}{}", file as char, rank as char)
    }
}

/// Piece types (8 types)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum PieceType {
    King = 0,   // K
    Rook = 1,   // R
    Bishop = 2, // B
    Gold = 3,   // G
    Silver = 4, // S
    Knight = 5, // N
    Lance = 6,  // L
    Pawn = 7,   // P
}

impl PieceType {
    /// Check if piece can promote
    #[inline]
    pub const fn can_promote(self) -> bool {
        matches!(
            self,
            PieceType::Rook
                | PieceType::Bishop
                | PieceType::Silver
                | PieceType::Knight
                | PieceType::Lance
                | PieceType::Pawn
        )
    }

    /// Get piece value for simple evaluation
    #[inline]
    pub const fn value(self) -> i32 {
        match self {
            PieceType::King => 0, // King has special handling
            PieceType::Rook => 1100,
            PieceType::Bishop => 950,
            PieceType::Gold => 600,
            PieceType::Silver => 550,
            PieceType::Knight => 450,
            PieceType::Lance => 350,
            PieceType::Pawn => 100,
        }
    }
}

/// Complete piece representation including promoted pieces
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub piece_type: PieceType,
    pub color: Color,
    pub promoted: bool,
}

impl Piece {
    /// Create new piece
    #[inline]
    pub const fn new(piece_type: PieceType, color: Color) -> Self {
        Piece {
            piece_type,
            color,
            promoted: false,
        }
    }

    /// Create promoted piece
    #[inline]
    pub const fn promoted(piece_type: PieceType, color: Color) -> Self {
        Piece {
            piece_type,
            color,
            promoted: true,
        }
    }

    /// Get piece value
    #[inline]
    pub fn value(self) -> i32 {
        let base_value = self.piece_type.value();
        if self.promoted {
            match self.piece_type {
                PieceType::Rook => 1500,   // Dragon
                PieceType::Bishop => 1300, // Horse
                PieceType::Silver | PieceType::Knight | PieceType::Lance | PieceType::Pawn => 600, // Same as Gold
                _ => base_value,
            }
        } else {
            base_value
        }
    }

    /// Convert to index (0-13)
    #[inline]
    pub fn to_index(self) -> usize {
        let base = self.piece_type as usize;
        if self.promoted && self.piece_type.can_promote() {
            base + 8
        } else {
            base
        }
    }
}

/// Side to move
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Color {
    Black = 0, // Sente
    White = 1, // Gote
}

impl Color {
    /// Get opposite color
    #[inline]
    pub const fn opposite(self) -> Self {
        match self {
            Color::Black => Color::White,
            Color::White => Color::Black,
        }
    }
}

/// Bitboard (81 squares)
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bitboard(pub u128); // Use lower 81 bits

impl Bitboard {
    /// Empty bitboard
    pub const EMPTY: Self = Bitboard(0);

    /// All squares set
    pub const ALL: Self = Bitboard((1u128 << 81) - 1);

    /// Create bitboard with single square set
    #[inline]
    pub fn from_square(sq: Square) -> Self {
        debug_assert!(sq.0 < 81);
        Bitboard(1u128 << sq.index())
    }

    /// Set bit at square
    #[inline]
    pub fn set(&mut self, sq: Square) {
        debug_assert!(sq.0 < 81);
        self.0 |= 1u128 << sq.index();
    }

    /// Clear bit at square
    #[inline]
    pub fn clear(&mut self, sq: Square) {
        debug_assert!(sq.0 < 81);
        self.0 &= !(1u128 << sq.index());
    }

    /// Test bit at square
    #[inline]
    pub fn test(&self, sq: Square) -> bool {
        debug_assert!(sq.0 < 81);
        (self.0 >> sq.index()) & 1 != 0
    }

    /// Pop least significant bit
    #[inline]
    pub fn pop_lsb(&mut self) -> Option<Square> {
        if self.0 == 0 {
            return None;
        }
        let lsb = self.0.trailing_zeros() as u8;
        self.0 &= self.0 - 1; // Clear LSB
        Some(Square(lsb))
    }

    /// Get least significant bit without popping
    #[inline]
    pub fn lsb(&self) -> Option<Square> {
        if self.0 == 0 {
            return None;
        }
        let lsb = self.0.trailing_zeros() as u8;
        Some(Square(lsb))
    }

    /// Count set bits
    #[inline]
    pub fn count_ones(&self) -> u32 {
        self.0.count_ones()
    }

    /// Check if empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for Bitboard {
    type Output = Self;

    #[inline]
    fn bitor(self, rhs: Self) -> Self::Output {
        Bitboard(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for Bitboard {
    type Output = Self;

    #[inline]
    fn bitand(self, rhs: Self) -> Self::Output {
        Bitboard(self.0 & rhs.0)
    }
}

impl std::ops::BitXor for Bitboard {
    type Output = Self;

    #[inline]
    fn bitxor(self, rhs: Self) -> Self::Output {
        Bitboard(self.0 ^ rhs.0)
    }
}

impl std::ops::Not for Bitboard {
    type Output = Self;

    #[inline]
    fn not(self) -> Self::Output {
        Bitboard(!self.0 & Self::ALL.0)
    }
}

impl std::ops::BitOrAssign for Bitboard {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAndAssign for Bitboard {
    #[inline]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl std::ops::BitXorAssign for Bitboard {
    #[inline]
    fn bitxor_assign(&mut self, rhs: Self) {
        self.0 ^= rhs.0;
    }
}

/// Board representation
#[derive(Clone, Debug)]
pub struct Board {
    /// Bitboards by color and piece type [color][piece_type]
    pub piece_bb: [[Bitboard; 8]; 2], // 8 piece types

    /// All pieces by color (cache)
    pub occupied_bb: [Bitboard; 2], // [color]
    pub all_bb: Bitboard,

    /// Promoted pieces bitboard
    pub promoted_bb: Bitboard,

    /// Piece on each square (fast access)
    pub squares: [Option<Piece>; 81],
}

impl Board {
    /// Create empty board
    pub fn empty() -> Self {
        Board {
            piece_bb: [[Bitboard::EMPTY; 8]; 2],
            occupied_bb: [Bitboard::EMPTY; 2],
            all_bb: Bitboard::EMPTY,
            promoted_bb: Bitboard::EMPTY,
            squares: [None; 81],
        }
    }

    /// Place piece on board
    pub fn put_piece(&mut self, sq: Square, piece: Piece) {
        let color = piece.color as usize;
        let piece_type = piece.piece_type as usize;

        // Update bitboards
        self.piece_bb[color][piece_type].set(sq);
        self.occupied_bb[color].set(sq);
        self.all_bb.set(sq);

        // Update promoted bitboard
        if piece.promoted {
            self.promoted_bb.set(sq);
        }

        // Update square info
        self.squares[sq.index()] = Some(piece);
    }

    /// Remove piece from board
    pub fn remove_piece(&mut self, sq: Square) -> Option<Piece> {
        if let Some(piece) = self.squares[sq.index()] {
            let color = piece.color as usize;
            let piece_type = piece.piece_type as usize;

            // Update bitboards
            self.piece_bb[color][piece_type].clear(sq);
            self.occupied_bb[color].clear(sq);
            self.all_bb.clear(sq);

            // Update promoted bitboard
            if piece.promoted {
                self.promoted_bb.clear(sq);
            }

            // Clear square info
            self.squares[sq.index()] = None;

            Some(piece)
        } else {
            None
        }
    }

    /// Get piece on square
    #[inline]
    pub fn piece_on(&self, sq: Square) -> Option<Piece> {
        self.squares[sq.index()]
    }

    /// Find king square
    pub fn king_square(&self, color: Color) -> Option<Square> {
        let mut bb = self.piece_bb[color as usize][PieceType::King as usize];
        bb.pop_lsb()
    }
}

/// Position structure
#[derive(Clone, Debug)]
pub struct Position {
    /// Board with bitboards (8 piece types: K,R,B,G,S,N,L,P)
    pub board: Board,

    /// Pieces in hand [color][piece_type] (excluding King)
    pub hands: [[u8; 7]; 2],

    /// Side to move
    pub side_to_move: Color,

    /// Ply count
    pub ply: u16,

    /// Zobrist hash (full 64 bits)
    pub hash: u64,

    /// History for repetition detection
    pub history: Vec<u64>,
}

impl Position {
    /// Create empty position
    pub fn empty() -> Self {
        Position {
            board: Board::empty(),
            hands: [[0; 7]; 2],
            side_to_move: Color::Black,
            ply: 0,
            hash: 0,
            history: Vec::new(),
        }
    }

    /// Create starting position
    pub fn startpos() -> Self {
        let mut pos = Self::empty();

        // Place pawns
        for file in 0..9 {
            pos.board
                .put_piece(Square::new(file, 2), Piece::new(PieceType::Pawn, Color::Black));
            pos.board
                .put_piece(Square::new(file, 6), Piece::new(PieceType::Pawn, Color::White));
        }

        // Lances
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::Lance, Color::Black));
        pos.board
            .put_piece(Square::new(8, 0), Piece::new(PieceType::Lance, Color::Black));
        pos.board
            .put_piece(Square::new(0, 8), Piece::new(PieceType::Lance, Color::White));
        pos.board
            .put_piece(Square::new(8, 8), Piece::new(PieceType::Lance, Color::White));

        // Knights
        pos.board
            .put_piece(Square::new(1, 0), Piece::new(PieceType::Knight, Color::Black));
        pos.board
            .put_piece(Square::new(7, 0), Piece::new(PieceType::Knight, Color::Black));
        pos.board
            .put_piece(Square::new(1, 8), Piece::new(PieceType::Knight, Color::White));
        pos.board
            .put_piece(Square::new(7, 8), Piece::new(PieceType::Knight, Color::White));

        // Silvers
        pos.board
            .put_piece(Square::new(2, 0), Piece::new(PieceType::Silver, Color::Black));
        pos.board
            .put_piece(Square::new(6, 0), Piece::new(PieceType::Silver, Color::Black));
        pos.board
            .put_piece(Square::new(2, 8), Piece::new(PieceType::Silver, Color::White));
        pos.board
            .put_piece(Square::new(6, 8), Piece::new(PieceType::Silver, Color::White));

        // Golds
        pos.board
            .put_piece(Square::new(3, 0), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(Square::new(5, 0), Piece::new(PieceType::Gold, Color::Black));
        pos.board
            .put_piece(Square::new(3, 8), Piece::new(PieceType::Gold, Color::White));
        pos.board
            .put_piece(Square::new(5, 8), Piece::new(PieceType::Gold, Color::White));

        // Kings
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

        // Rooks
        pos.board
            .put_piece(Square::new(7, 1), Piece::new(PieceType::Rook, Color::Black));
        pos.board
            .put_piece(Square::new(1, 7), Piece::new(PieceType::Rook, Color::White));

        // Bishops
        pos.board
            .put_piece(Square::new(1, 1), Piece::new(PieceType::Bishop, Color::Black));
        pos.board
            .put_piece(Square::new(7, 7), Piece::new(PieceType::Bishop, Color::White));

        // Calculate hash
        pos.hash = pos.compute_hash();

        pos
    }

    /// Compute Zobrist hash
    fn compute_hash(&self) -> u64 {
        use crate::ai::zobrist::ZobristHashing;
        self.zobrist_hash()
    }

    /// Check for repetition
    pub fn is_repetition(&self) -> bool {
        if self.history.len() < 4 {
            return false;
        }

        let current_hash = self.hash;
        let mut count = 0;

        // Four-fold repetition
        for &hash in self.history.iter() {
            if hash == current_hash {
                count += 1;
                if count >= 3 {
                    // Current position + 3 in history = 4 total
                    return true;
                }
            }
        }

        false
    }

    /// Make a move on the position
    pub fn do_move(&mut self, mv: super::moves::Move) {
        // Save current hash to history
        self.history.push(self.hash);

        if mv.is_drop() {
            // Handle drop move
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            let piece = Piece::new(piece_type, self.side_to_move);

            // Place piece on board
            self.board.put_piece(to, piece);

            // Remove from hand
            let hand_idx = match piece_type {
                PieceType::Rook => 0,
                PieceType::Bishop => 1,
                PieceType::Gold => 2,
                PieceType::Silver => 3,
                PieceType::Knight => 4,
                PieceType::Lance => 5,
                PieceType::Pawn => 6,
                _ => unreachable!(),
            };
            self.hands[self.side_to_move as usize][hand_idx] -= 1;

            // Update hash
            self.hash ^= self.piece_square_zobrist(piece, to);
            self.hash ^= self.hand_zobrist(
                self.side_to_move,
                piece_type,
                self.hands[self.side_to_move as usize][hand_idx] + 1,
            );
            self.hash ^= self.hand_zobrist(
                self.side_to_move,
                piece_type,
                self.hands[self.side_to_move as usize][hand_idx],
            );
        } else {
            // Handle normal move
            let from = mv.from().unwrap();
            let to = mv.to();

            // Get moving piece
            let mut piece = self.board.piece_on(from).unwrap();

            // Remove piece from source
            self.board.remove_piece(from);
            self.hash ^= self.piece_square_zobrist(piece, from);

            // Handle capture
            if let Some(captured) = self.board.piece_on(to) {
                // Debug check - should never capture king
                if captured.piece_type == PieceType::King {
                    println!("Move details: from={from}, to={to}, piece={piece:?}");
                    panic!("Illegal move: attempting to capture king at {to}");
                }

                self.board.remove_piece(to);
                self.hash ^= self.piece_square_zobrist(captured, to);

                // Add to hand (unpromoted)
                let captured_type = captured.piece_type;

                let hand_idx = match captured_type {
                    PieceType::Rook => 0,
                    PieceType::Bishop => 1,
                    PieceType::Gold => 2,
                    PieceType::Silver => 3,
                    PieceType::Knight => 4,
                    PieceType::Lance => 5,
                    PieceType::Pawn => 6,
                    PieceType::King => 0, // Should never reach here due to check above
                };

                self.hash ^= self.hand_zobrist(
                    self.side_to_move,
                    captured_type,
                    self.hands[self.side_to_move as usize][hand_idx],
                );
                self.hands[self.side_to_move as usize][hand_idx] += 1;
                self.hash ^= self.hand_zobrist(
                    self.side_to_move,
                    captured_type,
                    self.hands[self.side_to_move as usize][hand_idx],
                );
            }

            // Handle promotion
            if mv.is_promote() {
                piece.promoted = true;
            }

            // Place piece on destination
            self.board.put_piece(to, piece);
            self.hash ^= self.piece_square_zobrist(piece, to);
        }

        // Switch side to move
        self.side_to_move = self.side_to_move.opposite();
        self.hash ^= self.side_to_move_zobrist();

        // Increment ply
        self.ply += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_square_operations() {
        let sq = Square::new(4, 4); // 5e
        assert_eq!(sq.file(), 4);
        assert_eq!(sq.rank(), 4);
        assert_eq!(sq.index(), 40);
        assert_eq!(sq.to_string(), "5e");

        let flipped = sq.flip();
        assert_eq!(flipped.file(), 4);
        assert_eq!(flipped.rank(), 4);
        assert_eq!(flipped.index(), 40);
    }

    #[test]
    fn test_bitboard_operations() {
        let mut bb = Bitboard::EMPTY;
        assert!(bb.is_empty());

        let sq = Square::new(4, 4);
        bb.set(sq);
        assert!(bb.test(sq));
        assert_eq!(bb.count_ones(), 1);

        bb.clear(sq);
        assert!(!bb.test(sq));
        assert!(bb.is_empty());
    }

    #[test]
    fn test_bitboard_pop_lsb() {
        let mut bb = Bitboard::EMPTY;
        bb.set(Square::new(0, 0));
        bb.set(Square::new(4, 4));
        bb.set(Square::new(8, 8));

        assert_eq!(bb.pop_lsb(), Some(Square::new(0, 0)));
        assert_eq!(bb.pop_lsb(), Some(Square::new(4, 4)));
        assert_eq!(bb.pop_lsb(), Some(Square::new(8, 8)));
        assert_eq!(bb.pop_lsb(), None);
    }

    #[test]
    fn test_board_operations() {
        let mut board = Board::empty();
        let sq = Square::new(4, 4);
        let piece = Piece::new(PieceType::Pawn, Color::Black);

        board.put_piece(sq, piece);
        assert_eq!(board.piece_on(sq), Some(piece));
        assert!(board.all_bb.test(sq));

        board.remove_piece(sq);
        assert_eq!(board.piece_on(sq), None);
        assert!(!board.all_bb.test(sq));
    }

    #[test]
    fn test_startpos() {
        let pos = Position::startpos();

        // Check king positions
        assert_eq!(pos.board.king_square(Color::Black), Some(Square::new(4, 0)));
        assert_eq!(pos.board.king_square(Color::White), Some(Square::new(4, 8)));

        // Check pawn count
        assert_eq!(
            pos.board.piece_bb[Color::Black as usize][PieceType::Pawn as usize].count_ones(),
            9
        );
        assert_eq!(
            pos.board.piece_bb[Color::White as usize][PieceType::Pawn as usize].count_ones(),
            9
        );

        // No pieces in hand at start
        for color in 0..2 {
            for piece_type in 0..7 {
                assert_eq!(pos.hands[color][piece_type], 0);
            }
        }
    }
}
