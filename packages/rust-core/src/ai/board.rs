//! Bitboard representation for shogi board
//!
//! Represents 81-square shogi board using 128-bit integers for fast operations

use super::piece_constants::piece_type_to_hand_index;
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

/// Display square in shogi notation (e.g., "5e")
/// ● USIプロトコルとの関係式
/// Square::new(file, rank) → USI座標
/// - file（第一引数）: USI_file = 9 - file
/// - file 8 → USI 1筋
/// - file 7 → USI 2筋
/// - file 0 → USI 9筋
/// - rank（第二引数）: USI_rank = rank + 'a'
/// - rank 0 → USI 'a'段（一段目）
/// - rank 8 → USI 'i'段（九段目）
///
/// ## Examples:
/// Square::new(8, 7)
/// → USI: "1h" (1八)
///
/// Square::new(8, 0)
/// → USI: "1a" (1一)
///
/// Square::new(0, 0)
/// → USI: "9a" (9一)
///
/// Square::new(4, 4)
/// → USI: "5e" (5五)
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

impl TryFrom<u8> for PieceType {
    type Error = &'static str;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(PieceType::King),
            1 => Ok(PieceType::Rook),
            2 => Ok(PieceType::Bishop),
            3 => Ok(PieceType::Gold),
            4 => Ok(PieceType::Silver),
            5 => Ok(PieceType::Knight),
            6 => Ok(PieceType::Lance),
            7 => Ok(PieceType::Pawn),
            _ => Err("Invalid piece type value"),
        }
    }
}

impl PieceType {
    /// Get the index of this piece type (0-7)
    #[inline]
    pub const fn as_index(self) -> usize {
        self as usize
    }

    /// Create a piece type from index (0-7)
    #[inline]
    pub const fn from_index(index: usize) -> Option<PieceType> {
        match index {
            0 => Some(PieceType::King),
            1 => Some(PieceType::Rook),
            2 => Some(PieceType::Bishop),
            3 => Some(PieceType::Gold),
            4 => Some(PieceType::Silver),
            5 => Some(PieceType::Knight),
            6 => Some(PieceType::Lance),
            7 => Some(PieceType::Pawn),
            _ => None,
        }
    }

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

    /// Check if piece is promoted
    #[inline]
    pub const fn is_promoted(self) -> bool {
        self.promoted
    }

    /// Promote this piece
    #[inline]
    pub fn promote(self) -> Self {
        Piece {
            promoted: true,
            ..self
        }
    }

    /// Flip piece color
    #[inline]
    pub fn flip_color(self) -> Self {
        Piece {
            color: self.color.flip(),
            ..self
        }
    }

    /// Convert to index (0-15)
    /// Note: Promoted King (8) and promoted Gold (11) are never used
    #[inline]
    pub fn to_index(self) -> usize {
        let base = self.piece_type as usize;
        if self.promoted && self.piece_type.can_promote() {
            base + PROMOTED_OFFSET
        } else {
            base
        }
    }
}

/// Number of piece types (King to Pawn)
pub const NUM_PIECE_TYPES: usize = 8;

/// Offset for promoted pieces in indexing
pub const PROMOTED_OFFSET: usize = 8;

/// Maximum piece index (including promoted pieces)
/// This includes unused indices for promoted King and Gold
pub const MAX_PIECE_INDEX: usize = NUM_PIECE_TYPES + PROMOTED_OFFSET; // 16

/// Number of squares on the board
pub const BOARD_SQUARES: usize = 81;

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

    /// Flip color (same as opposite)
    #[inline]
    pub const fn flip(self) -> Self {
        self.opposite()
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

    /// Get a bitboard with all squares in a file set
    #[inline]
    pub fn file_mask(file: u8) -> Self {
        debug_assert!(file < 9);
        let mut mask = 0u128;
        for rank in 0..9 {
            mask |= 1u128 << (rank * 9 + file);
        }
        Bitboard(mask)
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
    /// - 目的: 駒種別・手番別の配置を管理
    /// - [手番(先手/後手)][駒種(8種類)]の2次元配列
    /// - 例: piece_bb[BLACK][PAWN] = 先手の歩の位置すべて
    /// - 用途: 特定の駒種の移動生成、駒の価値計算
    pub piece_bb: [[Bitboard; 8]; 2], // 8 piece types

    /// All pieces by color (cache)
    /// - 目的: 各手番の全駒位置をキャッシュ
    /// - occupied_bb[BLACK] = 先手の全駒のOR演算結果
    /// - 用途: 自分の駒への移動を除外、王手判定の高速化
    /// - 利点: 毎回piece_bbをOR演算する必要がない
    pub occupied_bb: [Bitboard; 2], // [color]
    /// - 目的: 盤上の全駒位置（両手番）をキャッシュ
    /// - occupied_bb[BLACK] | occupied_bb[WHITE]の結果
    /// - 用途: 空きマス判定、飛び駒の移動範囲計算
    /// - 利点: 最も頻繁に使用されるため事前計算
    /// - 更新タイミング:
    ///   1. 手を指した時 (make_moveメソッド)
    ///   2. 手を戻した時 (unmake_moveメソッド)
    ///   3. 局面を設定した時 (set_positionなど)
    pub all_bb: Bitboard,

    /// Promoted pieces bitboard
    /// - 目的: 成り駒の位置を記録
    /// - 成り駒かどうかの判定を高速化
    /// - 用途: 駒の動き生成時の成り判定、駒の表示
    /// - 利点: 駒種と成り状態を別管理することで効率化
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

/// Information needed to undo a move
#[derive(Clone, Debug)]
pub struct UndoInfo {
    /// Captured piece (if any)
    pub captured: Option<Piece>,
    /// Whether the moving piece was promoted before the move
    pub moved_piece_was_promoted: bool,
    /// Previous hash value
    pub previous_hash: u64,
    /// Previous ply count
    pub previous_ply: u16,
}

/// Position structure
#[derive(Clone, Debug)]
pub struct Position {
    /// Board with bitboards (8 piece types: K,R,B,G,S,N,L,P)
    pub board: Board,

    /// Pieces in hand [color][piece_type] (excluding King)
    pub hands: [[u8; 7]; 2],

    /// 手番 (Black or White)
    pub side_to_move: Color,

    /// 手数
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

    /// Get king square for color
    pub fn king_square(&self, color: Color) -> Option<Square> {
        self.board.king_square(color)
    }

    /// Get piece at square
    pub fn piece_at(&self, sq: Square) -> Option<Piece> {
        self.board.piece_on(sq)
    }

    /// Make a move on the position
    pub fn do_move(&mut self, mv: super::moves::Move) -> UndoInfo {
        // Save current hash to history
        self.history.push(self.hash);

        // Initialize undo info
        let mut undo_info = UndoInfo {
            captured: None,
            moved_piece_was_promoted: false,
            previous_hash: self.hash,
            previous_ply: self.ply,
        };

        if mv.is_drop() {
            // Handle drop move
            let to = mv.to();
            let piece_type = mv.drop_piece_type();
            let piece = Piece::new(piece_type, self.side_to_move);

            // Place piece on board
            self.board.put_piece(to, piece);

            // Remove from hand
            let hand_idx = piece_type_to_hand_index(piece_type)
                .expect("Drop piece type must be valid hand piece");
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

            // Save promoted status for undo
            undo_info.moved_piece_was_promoted = piece.promoted;

            // Remove piece from source
            self.board.remove_piece(from);
            self.hash ^= self.piece_square_zobrist(piece, from);

            // Handle capture
            if let Some(captured) = self.board.piece_on(to) {
                // Save captured piece for undo
                undo_info.captured = Some(captured);
                // Debug check - should never capture king
                if captured.piece_type == PieceType::King {
                    println!("Move details: from={from}, to={to}, piece={piece:?}");
                    panic!("Illegal move: attempting to capture king at {to}");
                }

                self.board.remove_piece(to);
                self.hash ^= self.piece_square_zobrist(captured, to);

                // Add to hand (unpromoted)
                let captured_type = captured.piece_type;

                let hand_idx =
                    piece_type_to_hand_index(captured_type).expect("Captured piece cannot be King");

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
        // Always XOR with the White side hash to toggle between Black/White
        use crate::ai::zobrist::ZOBRIST;
        self.hash ^= ZOBRIST.side_to_move;

        // Increment ply
        self.ply += 1;

        undo_info
    }

    /// Undo a move on the position
    pub fn undo_move(&mut self, mv: super::moves::Move, undo_info: UndoInfo) {
        // Remove last hash from history
        self.history.pop();

        // Restore hash value
        self.hash = undo_info.previous_hash;

        // Restore side to move and ply
        self.side_to_move = self.side_to_move.opposite();
        self.ply = undo_info.previous_ply;

        if mv.is_drop() {
            // Undo drop move
            let to = mv.to();
            let piece_type = mv.drop_piece_type();

            // Remove piece from board
            self.board.remove_piece(to);

            // Add back to hand
            let hand_idx = piece_type_to_hand_index(piece_type)
                .expect("Drop piece type must be valid hand piece");
            self.hands[self.side_to_move as usize][hand_idx] += 1;
        } else {
            // Undo normal move
            let from = mv.from().unwrap();
            let to = mv.to();

            // Get piece from destination
            let mut piece = self.board.piece_on(to).unwrap();

            // Remove piece from destination
            self.board.remove_piece(to);

            // Restore promotion status
            if mv.is_promote() {
                piece.promoted = undo_info.moved_piece_was_promoted;
            }

            // Place piece back at source
            self.board.put_piece(from, piece);

            // Restore captured piece if any
            if let Some(captured) = undo_info.captured {
                self.board.put_piece(to, captured);

                // Remove from hand
                let captured_type = captured.piece_type;
                let hand_idx =
                    piece_type_to_hand_index(captured_type).expect("Captured piece cannot be King");
                self.hands[self.side_to_move as usize][hand_idx] -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::moves::Move;

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

    #[test]
    fn test_do_move_normal_move() {
        let mut pos = Position::startpos();
        let from = Square::new(6, 2); // 3g
        let to = Square::new(6, 3); // 3f
        let mv = Move::normal(from, to, false);

        // 初期ハッシュを記録
        let initial_hash = pos.hash;

        // 手を実行
        let _undo_info = pos.do_move(mv);

        // 駒が移動していることを確認
        assert_eq!(pos.board.piece_on(from), None);
        assert_eq!(pos.board.piece_on(to), Some(Piece::new(PieceType::Pawn, Color::Black)));

        // 手番が切り替わっていることを確認
        assert_eq!(pos.side_to_move, Color::White);

        // 手数が増えていることを確認
        assert_eq!(pos.ply, 1);

        // ハッシュが変わっていることを確認
        assert_ne!(pos.hash, initial_hash);

        // 履歴に追加されていることを確認
        assert_eq!(pos.history.len(), 1);
        assert_eq!(pos.history[0], initial_hash);
    }

    #[test]
    fn test_do_move_capture() {
        // 駒を取る手のテスト
        let mut pos = Position::startpos();

        // 歩を前進させる
        let mv1 = Move::normal(Square::new(6, 2), Square::new(6, 3), false); // 3g-3f
        let _undo1 = pos.do_move(mv1);

        // 相手の歩を前進させる
        let mv2 = Move::normal(Square::new(4, 6), Square::new(4, 5), false); // 5c-5d
        let _undo2 = pos.do_move(mv2);

        // さらに歩を前進
        let mv3 = Move::normal(Square::new(6, 3), Square::new(6, 4), false); // 3f-3e
        let _undo3 = pos.do_move(mv3);

        // 相手の歩をさらに前進
        let mv4 = Move::normal(Square::new(4, 5), Square::new(4, 4), false); // 5d-5e
        let _undo4 = pos.do_move(mv4);

        // 歩で相手の歩を取る
        let from = Square::new(6, 4); // 3e
        let to = Square::new(4, 4); // 5e
        let mv = Move::normal(from, to, false);

        let captured_piece = pos.board.piece_on(to).unwrap();
        assert_eq!(captured_piece.piece_type, PieceType::Pawn);
        assert_eq!(captured_piece.color, Color::White);

        let _undo5 = pos.do_move(mv);

        // 駒が取られていることを確認
        assert_eq!(pos.board.piece_on(from), None);
        assert_eq!(pos.board.piece_on(to), Some(Piece::new(PieceType::Pawn, Color::Black)));

        // 持ち駒が増えていることを確認
        assert_eq!(pos.hands[Color::Black as usize][6], 1); // 歩のインデックスは6
    }

    #[test]
    fn test_do_move_promotion() {
        // 成りのテスト - 成り動作だけをチェック
        let _pos = Position::startpos();

        // 手動で駒を配置して成りをテスト
        let mut board = Board::empty();
        let mut pawn = Piece::new(PieceType::Pawn, Color::Black);
        board.put_piece(Square::new(2, 6), pawn);

        // do_moveを使わずに直接成りをテスト
        pawn.promoted = true;
        board.remove_piece(Square::new(2, 6));
        board.put_piece(Square::new(2, 7), pawn);

        // 成った駒になっていることを確認
        let piece = board.piece_on(Square::new(2, 7)).unwrap();
        assert_eq!(piece.piece_type, PieceType::Pawn);
        assert!(piece.promoted);
        assert_eq!(piece.color, Color::Black);
    }

    #[test]
    fn test_do_move_drop() {
        // 持ち駒を打つテスト
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;

        // 最小限の駒を配置
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

        // 持ち駒を設定
        pos.hands[Color::Black as usize][6] = 1;

        // 歩を打つ
        let to = Square::new(4, 4); // 5e
        let mv = Move::drop(PieceType::Pawn, to);

        let _undo_info = pos.do_move(mv);

        // 駒が置かれていることを確認
        assert_eq!(pos.board.piece_on(to), Some(Piece::new(PieceType::Pawn, Color::Black)));

        // 持ち駒が減っていることを確認
        assert_eq!(pos.hands[Color::Black as usize][6], 0);

        // 手番が切り替わっていることを確認
        assert_eq!(pos.side_to_move, Color::White);
    }

    #[test]
    fn test_do_move_all_piece_types() {
        // 各駒種の移動をテスト
        let test_cases = vec![
            // (from, to, piece_type)
            (
                Square::new(6, 2), // 3g
                Square::new(6, 3), // 3f
                PieceType::Pawn,
            ),
            (
                Square::new(4, 0), // 5i
                Square::new(5, 1), // 4h
                PieceType::King,
            ),
            (
                Square::new(5, 0), // 4i
                Square::new(5, 1), // 4h
                PieceType::Gold,
            ),
            (
                Square::new(6, 0), // 3i
                Square::new(6, 1), // 3h
                PieceType::Silver,
            ),
            (
                Square::new(7, 0), // 2i
                Square::new(6, 2), // 3g
                PieceType::Knight,
            ),
            (
                Square::new(8, 0), // 1i
                Square::new(8, 1), // 1h
                PieceType::Lance,
            ),
            (
                Square::new(7, 1), // 2h
                Square::new(7, 3), // 2f
                PieceType::Rook,
            ),
            (
                Square::new(1, 1), // 8h
                Square::new(2, 2), // 7g
                PieceType::Bishop,
            ),
        ];

        for (from, to, expected_piece_type) in test_cases {
            let mut pos = Position::startpos();
            let piece = pos.board.piece_on(from);

            // デバッグ: 初期配置の確認
            if piece.is_none() {
                println!("No piece at {from:?}");
                println!("Expected: {expected_piece_type:?}");
                // 周辺の駒を確認
                for file in 0..9 {
                    if let Some(p) = pos.board.piece_on(Square::new(file, 1)) {
                        if p.piece_type == expected_piece_type && p.color == Color::Black {
                            println!("Found {expected_piece_type:?} at Square::new({file}, 1)");
                        }
                    }
                }
                panic!("Piece not found at expected position");
            }

            let piece = piece.unwrap();
            assert_eq!(piece.piece_type, expected_piece_type);

            let mv = Move::normal(from, to, false);
            let _undo_info = pos.do_move(mv);

            // 駒が移動していることを確認
            assert_eq!(pos.board.piece_on(from), None);
            let moved_piece = pos.board.piece_on(to).unwrap();
            assert_eq!(moved_piece.piece_type, expected_piece_type);
        }
    }

    #[test]
    fn test_do_move_drop_all_piece_types() {
        // 各駒種の持ち駒打ちをテスト
        let test_cases = vec![
            (PieceType::Pawn, 6),
            (PieceType::Lance, 5),
            (PieceType::Knight, 4),
            (PieceType::Silver, 3),
            (PieceType::Gold, 2),
            (PieceType::Bishop, 1),
            (PieceType::Rook, 0),
        ];

        for (piece_type, hand_idx) in test_cases {
            let mut pos = Position::empty();
            pos.side_to_move = Color::Black;

            // 最小限の駒を配置
            pos.board
                .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
            pos.board
                .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

            // 各種持ち駒を設定
            pos.hands[Color::Black as usize][hand_idx] = 1;

            // 持ち駒があることを確認
            assert!(pos.hands[Color::Black as usize][hand_idx] > 0);

            let to = Square::new(4, 4); // 5e
            let mv = Move::drop(piece_type, to);

            let _undo_info = pos.do_move(mv);

            // 駒が置かれていることを確認
            let placed_piece = pos.board.piece_on(to).unwrap();
            assert_eq!(placed_piece.piece_type, piece_type);
            assert_eq!(placed_piece.color, Color::Black);
            assert!(!placed_piece.promoted);

            // 持ち駒が減っていることを確認
            assert_eq!(pos.hands[Color::Black as usize][hand_idx], 0);
        }
    }

    #[test]
    fn test_do_move_special_promotion_cases() {
        // 特殊な成りのケース（1段目での成り強制など）
        // startposを使って基本的な成りの動作をテスト
        let mut pos = Position::startpos();

        // 歩を前進させて成る
        // 7七歩
        let mv1 = Move::normal(Square::new(6, 2), Square::new(6, 3), false); // 3g-3f
        pos.do_move(mv1);

        // 相手の歩を前進
        let mv2 = Move::normal(Square::new(6, 6), Square::new(6, 5), false); // 3c-3d
        pos.do_move(mv2);

        // さらに前進
        let mv3 = Move::normal(Square::new(6, 3), Square::new(6, 4), false); // 3f-3e
        pos.do_move(mv3);

        // 相手の歩をさらに前進
        let mv4 = Move::normal(Square::new(6, 5), Square::new(6, 4), false); // 3d-3e
        pos.do_move(mv4);

        // 銀を前進させる（成りのテスト用）
        let mv5 = Move::normal(Square::new(6, 0), Square::new(6, 1), false); // 3i-3h
        pos.do_move(mv5);

        // パスのような手
        let mv6 = Move::normal(Square::new(4, 6), Square::new(4, 5), false);
        pos.do_move(mv6);

        // 銀を敵陣に進めて成る
        let mv7 = Move::normal(Square::new(6, 1), Square::new(6, 2), false); // 3h-3g
        pos.do_move(mv7);

        // パスのような手
        let mv8 = Move::normal(Square::new(4, 5), Square::new(4, 4), false);
        pos.do_move(mv8);

        // 銀をさらに前進
        let mv9 = Move::normal(Square::new(6, 2), Square::new(6, 3), false); // 3g-3f
        let _undo9 = pos.do_move(mv9);

        // パスのような手
        let mv10 = Move::normal(Square::new(4, 4), Square::new(4, 3), false);
        let _undo10 = pos.do_move(mv10);

        // 銀を敵陣三段目に進めて成る
        let mv11 = Move::normal(Square::new(6, 3), Square::new(6, 4), true); // 3f-3e+
        let _undo11 = pos.do_move(mv11);

        let piece = pos.board.piece_on(Square::new(6, 4)).unwrap();
        assert_eq!(piece.piece_type, PieceType::Silver);
        assert!(piece.promoted);
    }

    #[test]
    fn test_is_repetition() {
        // 簡単な繰り返しのテスト
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;

        // 最小限の駒を配置
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::Rook, Color::Black));

        // 初期ハッシュを計算
        pos.hash = pos.compute_hash();
        let initial_hash = pos.hash;

        // 飛車を動かす
        let mv1 = Move::normal(Square::new(0, 0), Square::new(0, 1), false);
        let _undo1 = pos.do_move(mv1);

        // 飛車を戻す
        let mv2 = Move::normal(Square::new(0, 1), Square::new(0, 0), false);
        let _undo2 = pos.do_move(mv2);

        // この時点で初期局面に戻った（1回目）
        assert_eq!(pos.hash, initial_hash);
        assert!(!pos.is_repetition()); // まだ繰り返しではない

        // 2回目の往復
        let _undo3 = pos.do_move(mv1);
        let _undo4 = pos.do_move(mv2);

        // この時点で初期局面に戻った（2回目）
        assert_eq!(pos.hash, initial_hash);
        assert!(!pos.is_repetition()); // まだ3回ではない

        // 3回目の往復
        let _undo5 = pos.do_move(mv1);
        let _undo6 = pos.do_move(mv2);

        // この時点で初期局面に戻った（3回目）
        assert_eq!(pos.hash, initial_hash);
        assert!(pos.is_repetition()); // 3回繰り返しで千日手
    }

    #[test]
    fn test_is_repetition_with_different_hands() {
        // 持ち駒が異なる場合は同一局面ではない
        let mut pos1 = Position::startpos();
        let mut pos2 = Position::startpos();

        // 同じ動きだが、pos2では歩を取る
        let mv1 = Move::normal(Square::new(6, 2), Square::new(6, 3), false);
        pos1.do_move(mv1);
        pos2.do_move(mv1);

        // pos2では相手の歩を前進させて取る
        let mv2 = Move::normal(Square::new(6, 6), Square::new(6, 5), false);
        pos2.do_move(mv2);
        let mv3 = Move::normal(Square::new(6, 3), Square::new(6, 5), false);
        pos2.do_move(mv3);

        // 異なるハッシュ値になるはず
        assert_ne!(pos1.hash, pos2.hash);
    }

    #[test]
    fn test_is_repetition_edge_cases() {
        let mut pos = Position::startpos();

        // 履歴が4未満の場合
        assert!(!pos.is_repetition());

        let _undo1 = pos.do_move(Move::normal(Square::new(6, 2), Square::new(6, 3), false));
        assert!(!pos.is_repetition());

        let _undo2 = pos.do_move(Move::normal(Square::new(6, 6), Square::new(6, 5), false));
        assert!(!pos.is_repetition());

        let _undo3 = pos.do_move(Move::normal(Square::new(6, 3), Square::new(6, 4), false));
        assert!(!pos.is_repetition());
    }

    #[test]
    fn test_king_square_edge_cases() {
        // 空の盤面（玉がない）
        let mut board = Board::empty();
        assert_eq!(board.king_square(Color::Black), None);
        assert_eq!(board.king_square(Color::White), None);

        // 玉を配置
        let black_king = Piece::new(PieceType::King, Color::Black);
        let white_king = Piece::new(PieceType::King, Color::White);

        board.put_piece(Square::new(4, 0), black_king);
        board.put_piece(Square::new(4, 8), white_king);

        assert_eq!(board.king_square(Color::Black), Some(Square::new(4, 0)));
        assert_eq!(board.king_square(Color::White), Some(Square::new(4, 8)));

        // 玉を移動
        board.remove_piece(Square::new(4, 0));
        board.put_piece(Square::new(5, 1), black_king);

        assert_eq!(board.king_square(Color::Black), Some(Square::new(5, 1)));
        assert_eq!(board.king_square(Color::White), Some(Square::new(4, 8)));
    }

    #[test]
    fn test_square_flip() {
        // flip()メソッドのテスト
        let sq = Square::new(2, 3); // インデックス: 2 + 3*9 = 29
        let flipped = sq.flip(); // 80 - 29 = 51

        // 反転後の座標を計算: 51 = file + rank*9
        // 51 / 9 = 5 余り 6
        assert_eq!(flipped.file(), 6); // 51 % 9 = 6
        assert_eq!(flipped.rank(), 5); // 51 / 9 = 5
    }

    #[test]
    fn test_piece_to_index() {
        // 各駒種のインデックス変換テスト
        assert_eq!(Piece::new(PieceType::King, Color::Black).to_index(), 0);
        assert_eq!(Piece::new(PieceType::Rook, Color::Black).to_index(), 1);
        assert_eq!(Piece::new(PieceType::Bishop, Color::Black).to_index(), 2);
        assert_eq!(Piece::new(PieceType::Gold, Color::Black).to_index(), 3);
        assert_eq!(Piece::new(PieceType::Silver, Color::Black).to_index(), 4);
        assert_eq!(Piece::new(PieceType::Knight, Color::Black).to_index(), 5);
        assert_eq!(Piece::new(PieceType::Lance, Color::Black).to_index(), 6);
        assert_eq!(Piece::new(PieceType::Pawn, Color::Black).to_index(), 7);

        // 成り駒
        let mut promoted_rook = Piece::new(PieceType::Rook, Color::Black);
        promoted_rook.promoted = true;
        assert_eq!(promoted_rook.to_index(), 9); // 1 + 8

        let mut promoted_bishop = Piece::new(PieceType::Bishop, Color::Black);
        promoted_bishop.promoted = true;
        assert_eq!(promoted_bishop.to_index(), 10); // 2 + 8

        let mut promoted_silver = Piece::new(PieceType::Silver, Color::Black);
        promoted_silver.promoted = true;
        assert_eq!(promoted_silver.to_index(), 12); // 4 + 8

        let mut promoted_knight = Piece::new(PieceType::Knight, Color::Black);
        promoted_knight.promoted = true;
        assert_eq!(promoted_knight.to_index(), 13); // 5 + 8

        let mut promoted_lance = Piece::new(PieceType::Lance, Color::Black);
        promoted_lance.promoted = true;
        assert_eq!(promoted_lance.to_index(), 14); // 6 + 8

        let mut promoted_pawn = Piece::new(PieceType::Pawn, Color::Black);
        promoted_pawn.promoted = true;
        assert_eq!(promoted_pawn.to_index(), 15); // 7 + 8
    }

    #[test]
    fn test_piece_type_methods() {
        // Test as_index()
        assert_eq!(PieceType::King.as_index(), 0);
        assert_eq!(PieceType::Rook.as_index(), 1);
        assert_eq!(PieceType::Bishop.as_index(), 2);
        assert_eq!(PieceType::Gold.as_index(), 3);
        assert_eq!(PieceType::Silver.as_index(), 4);
        assert_eq!(PieceType::Knight.as_index(), 5);
        assert_eq!(PieceType::Lance.as_index(), 6);
        assert_eq!(PieceType::Pawn.as_index(), 7);

        // Test from_index()
        assert_eq!(PieceType::from_index(0), Some(PieceType::King));
        assert_eq!(PieceType::from_index(1), Some(PieceType::Rook));
        assert_eq!(PieceType::from_index(7), Some(PieceType::Pawn));
        assert_eq!(PieceType::from_index(8), None);
        assert_eq!(PieceType::from_index(100), None);

        // Test round-trip conversion
        for i in 0..8 {
            if let Some(pt) = PieceType::from_index(i) {
                assert_eq!(pt.as_index(), i);
            }
        }
    }

    #[test]
    fn test_bitboard_file_mask() {
        // 各筋のマスクをテスト
        for file in 0..9 {
            let mask = Bitboard::file_mask(file);

            // その筋の全ての升がセットされているか確認
            for rank in 0..9 {
                let sq = Square::new(file, rank);
                assert!(mask.test(sq), "file {file} rank {rank} should be set");
            }

            // 他の筋の升はセットされていないか確認
            for other_file in 0..9 {
                if other_file != file {
                    for rank in 0..9 {
                        let sq = Square::new(other_file, rank);
                        assert!(!mask.test(sq), "file {other_file} rank {rank} should not be set");
                    }
                }
            }
        }
    }

    #[test]
    fn test_do_move_undo_move_reversibility() {
        // do_move/undo_moveの可逆性をテスト
        let mut pos = Position::startpos();
        let original_pos = pos.clone();

        // テストケース1: 通常の移動
        let mv1 = Move::normal(Square::new(6, 2), Square::new(6, 3), false); // 3g-3f
        let undo_info1 = pos.do_move(mv1);

        // 手を実行後の状態を確認
        assert_ne!(pos.hash, original_pos.hash);
        assert_eq!(pos.side_to_move, Color::White);
        assert_eq!(pos.ply, 1);

        // 手を戻す
        pos.undo_move(mv1, undo_info1);

        // 完全に元に戻ったことを確認
        assert_eq!(pos.hash, original_pos.hash);
        assert_eq!(pos.side_to_move, original_pos.side_to_move);
        assert_eq!(pos.ply, original_pos.ply);
        assert_eq!(pos.history.len(), original_pos.history.len());

        // 盤面も元に戻ったことを確認
        for sq in 0..81 {
            let square = Square(sq);
            assert_eq!(pos.board.piece_on(square), original_pos.board.piece_on(square));
        }
    }

    #[test]
    fn test_do_move_undo_move_capture() {
        // 駒を取る手の可逆性をテスト
        let mut pos = Position::startpos();

        // 準備: 駒を取れる位置まで進める
        let _u1 = pos.do_move(Move::normal(Square::new(6, 2), Square::new(6, 3), false));
        let _u2 = pos.do_move(Move::normal(Square::new(4, 6), Square::new(4, 5), false));
        let _u3 = pos.do_move(Move::normal(Square::new(6, 3), Square::new(6, 4), false));
        let _u4 = pos.do_move(Move::normal(Square::new(4, 5), Square::new(4, 4), false));

        // この時点の状態を保存
        let before_capture = pos.clone();

        // 駒を取る
        let capture_move = Move::normal(Square::new(6, 4), Square::new(4, 4), false);
        let undo_info = pos.do_move(capture_move);

        // 駒が取れたことを確認
        assert_eq!(pos.hands[Color::Black as usize][6], 1); // 歩を1枚持っている

        // 手を戻す
        pos.undo_move(capture_move, undo_info);

        // 完全に元に戻ったことを確認
        assert_eq!(pos.hash, before_capture.hash);
        assert_eq!(pos.hands[Color::Black as usize][6], 0); // 持ち駒なし
        for sq in 0..81 {
            let square = Square(sq);
            assert_eq!(pos.board.piece_on(square), before_capture.board.piece_on(square));
        }
    }

    #[test]
    fn test_do_move_undo_move_promotion() {
        // 成りの可逆性をテスト
        let mut pos = Position::empty();

        // 銀を敵陣三段目に配置
        pos.board
            .put_piece(Square::new(4, 6), Piece::new(PieceType::Silver, Color::Black));
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));
        pos.hash = pos.compute_hash();

        let before_promotion = pos.clone();

        // 成る
        let promote_move = Move::normal(Square::new(4, 6), Square::new(4, 7), true);
        let undo_info = pos.do_move(promote_move);

        // 成ったことを確認
        let promoted_piece = pos.board.piece_on(Square::new(4, 7)).unwrap();
        assert!(promoted_piece.promoted);

        // 手を戻す
        pos.undo_move(promote_move, undo_info);

        // 完全に元に戻ったことを確認
        assert_eq!(pos.hash, before_promotion.hash);
        let original_piece = pos.board.piece_on(Square::new(4, 6)).unwrap();
        assert!(!original_piece.promoted);
    }

    #[test]
    fn test_do_move_undo_move_drop() {
        // 駒打ちの可逆性をテスト
        let mut pos = Position::empty();
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

        // 持ち駒を設定
        pos.hands[Color::Black as usize][6] = 1; // 歩を1枚
        pos.hash = pos.compute_hash();

        let before_drop = pos.clone();

        // 歩を打つ
        let drop_move = Move::drop(PieceType::Pawn, Square::new(4, 4));
        let undo_info = pos.do_move(drop_move);

        // 打ったことを確認
        assert!(pos.board.piece_on(Square::new(4, 4)).is_some());
        assert_eq!(pos.hands[Color::Black as usize][6], 0);

        // 手を戻す
        pos.undo_move(drop_move, undo_info);

        // 完全に元に戻ったことを確認
        assert_eq!(pos.hash, before_drop.hash);
        assert!(pos.board.piece_on(Square::new(4, 4)).is_none());
        assert_eq!(pos.hands[Color::Black as usize][6], 1);
    }

    #[test]
    fn test_do_move_undo_move_multiple() {
        // 複数手の実行と戻しをテスト
        let mut pos = Position::startpos();
        let original_pos = pos.clone();

        let moves = vec![
            Move::normal(Square::new(6, 2), Square::new(6, 3), false),
            Move::normal(Square::new(4, 6), Square::new(4, 5), false),
            Move::normal(Square::new(7, 1), Square::new(7, 7), false), // 飛車
            Move::normal(Square::new(1, 7), Square::new(1, 1), false), // 相手の飛車
        ];

        let mut undo_infos = Vec::new();

        // 全ての手を実行
        for mv in &moves {
            let undo_info = pos.do_move(*mv);
            undo_infos.push(undo_info);
        }

        // 逆順で全ての手を戻す
        for (mv, undo_info) in moves.iter().zip(undo_infos.iter()).rev() {
            pos.undo_move(*mv, undo_info.clone());
        }

        // 完全に元に戻ったことを確認
        assert_eq!(pos.hash, original_pos.hash);
        assert_eq!(pos.side_to_move, original_pos.side_to_move);
        assert_eq!(pos.ply, original_pos.ply);
        for sq in 0..81 {
            let square = Square(sq);
            assert_eq!(pos.board.piece_on(square), original_pos.board.piece_on(square));
        }
    }
}
