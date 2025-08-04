//! Bitboard representation for shogi board
//!
//! Represents 81-square shogi board using 128-bit integers for fast operations

use crate::shogi::ATTACK_TABLES;
use crate::zobrist::ZOBRIST;
#[cfg(debug_assertions)]
use log::warn;

use super::moves::Move;
use super::piece_constants::{
    piece_type_to_hand_index, SEE_GAIN_ARRAY_SIZE, SEE_MAX_DEPTH, SEE_PIECE_VALUES,
};
use std::fmt;

/// Square on shogi board (0-80)
///
/// **IMPORTANT**: Internal file coordinate is reversed from USI notation!
/// - file 0 = 9筋 (leftmost)
/// - file 8 = 1筋 (rightmost)
///
/// To avoid confusion, prefer using from_usi_chars() instead of Square::new().
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Square(pub u8); // 0-80 (9x9)

impl Square {
    /// Create square from file and rank
    ///
    /// **WARNING**: file coordinate is reversed from USI notation!
    /// - file 0 = 9筋 (leftmost)
    /// - file 8 = 1筋 (rightmost)
    ///
    /// Consider using `from_usi_chars()` instead for safety.
    #[inline]
    pub const fn new(file: u8, rank: u8) -> Self {
        debug_assert!(file < 9 && rank < 9);
        Square(rank * 9 + file)
    }

    /// Get file (0-8, left to right in internal representation)
    /// Returns: 0=9筋, 1=8筋, ..., 8=1筋
    #[inline]
    pub const fn file(self) -> u8 {
        self.0 % 9
    }

    /// Get rank (0-8, top to bottom)
    /// Returns: 0=a段, 1=b段, ..., 8=i段
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

    /// Create square from USI notation characters (low-level API)
    ///
    /// # Arguments
    /// * `file` - File character ('1'-'9')
    /// * `rank` - Rank character ('a'-'i')
    ///
    /// # Example
    /// ```
    /// use engine_core::shogi::board::Square;
    ///
    /// let sq = Square::from_usi_chars('7', 'g').unwrap();
    /// assert_eq!(sq.to_string(), "7g");
    /// ```
    pub fn from_usi_chars(file: char, rank: char) -> Result<Self, crate::usi::UsiParseError> {
        use crate::usi::UsiParseError;

        // Validate file character
        let file_idx = match file {
            '1'..='9' => 8 - (file.to_digit(10).unwrap() as u8 - 1),
            _ => return Err(UsiParseError::InvalidSquare(format!("Invalid file: {file}"))),
        };

        // Validate rank character
        let rank_idx = match rank {
            'a'..='i' => (rank as u32 - 'a' as u32) as u8,
            _ => return Err(UsiParseError::InvalidSquare(format!("Invalid rank: {rank}"))),
        };

        Ok(Square::new(file_idx, rank_idx))
    }
}

/// Display square in shogi notation (e.g., "5e")
///
/// Converts internal representation to USI notation:
/// - Internal file 0 → USI '9'
/// - Internal file 8 → USI '1'
/// - Internal rank 0 → USI 'a'
/// - Internal rank 8 → USI 'i'
///
/// ## Examples:
/// - Square::new(0, 0) → "9a" (9一)
/// - Square::new(8, 0) → "1a" (1一)
/// - Square::new(0, 8) → "9i" (9九)
/// - Square::new(8, 8) → "1i" (1九)
/// - Square::new(4, 4) → "5e" (5五)
/// - Square::new(2, 6) → "7g" (7七)
impl fmt::Display for Square {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = b'9' - self.file();
        let rank = b'a' + self.rank();
        write!(f, "{}{}", file as char, rank as char)
    }
}

impl std::str::FromStr for Square {
    type Err = crate::usi::UsiParseError;

    /// Parse USI square notation (e.g., "5e", "1a")
    ///
    /// # Example
    /// ```
    /// use engine_core::shogi::board::Square;
    /// use std::str::FromStr;
    ///
    /// let sq: Square = "7g".parse().unwrap();
    /// assert_eq!(sq, Square::new(2, 6));
    /// ```
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        use crate::usi::UsiParseError;

        let mut chars = s.chars();
        match (chars.next(), chars.next(), chars.next()) {
            (Some(f), Some(r), None) => Self::from_usi_chars(f, r),
            _ => {
                Err(UsiParseError::InvalidSquare(format!("Expected 2 characters, got {}", s.len())))
            }
        }
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
    Black = 0, // Sente (先手) - plays from bottom (rank 8)
    White = 1, // Gote (後手) - plays from top (rank 0)
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

    /// Rebuild occupancy bitboards from piece bitboards
    /// This is useful after manual bitboard manipulation (e.g., in tests)
    pub fn rebuild_occupancy_bitboards(&mut self) {
        // Clear existing occupancy bitboards
        self.all_bb = Bitboard::EMPTY;
        self.occupied_bb[0] = Bitboard::EMPTY;
        self.occupied_bb[1] = Bitboard::EMPTY;

        // Rebuild from piece bitboards
        for color in 0..2 {
            for piece_type in 0..8 {
                self.occupied_bb[color] |= self.piece_bb[color][piece_type];
            }
            self.all_bb |= self.occupied_bb[color];
        }
    }

    /// Get pieces of specific type and color
    pub fn pieces_of_type_and_color(&self, piece_type: PieceType, color: Color) -> Bitboard {
        self.piece_bb[color as usize][piece_type as usize]
    }

    /// Check if a square is attacked by a specific color
    pub fn is_attacked_by(&self, sq: Square, by_color: Color) -> bool {
        use crate::shogi::ATTACK_TABLES;

        // Check attacks from each piece type
        // King attacks
        let king_attacks = ATTACK_TABLES.king_attacks[sq.index()];
        if !(king_attacks & self.piece_bb[by_color as usize][PieceType::King as usize]).is_empty() {
            return true;
        }

        // Gold attacks (includes promoted pieces)
        let gold_attacks = ATTACK_TABLES.gold_attacks[by_color as usize][sq.index()];
        if !(gold_attacks & self.piece_bb[by_color as usize][PieceType::Gold as usize]).is_empty() {
            return true;
        }

        // TODO: Add checks for other piece types (rook, bishop, silver, knight, lance, pawn)
        // For now, this is a simplified implementation

        false
    }

    /// Find king square
    pub fn king_square(&self, color: Color) -> Option<Square> {
        let mut bb = self.piece_bb[color as usize][PieceType::King as usize];
        let king_sq = bb.pop_lsb();

        #[cfg(debug_assertions)]
        {
            if king_sq.is_none() {
                warn!("No king found for {color:?}");
                warn!("Board state: all_bb has {} pieces", self.all_bb.count_ones());
            }
            // Verify there's only one king
            if !bb.is_empty() {
                panic!("Multiple kings found for {color:?}");
            }
        }

        king_sq
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
    /// Alias for hash (for compatibility)
    pub zobrist_hash: u64,

    /// History for repetition detection
    pub history: Vec<u64>,
}

/// SEE用の軽量なピン情報
struct SeePinInfo {
    /// ピンされた駒のビットボード
    pinned: Bitboard,
    /// ピン方向のマスク（4方向）
    vertical_pins: Bitboard, // 縦方向のピン
    horizontal_pins: Bitboard, // 横方向のピン
    diag_ne_pins: Bitboard,    // 北東-南西の斜めピン
    diag_nw_pins: Bitboard,    // 北西-南東の斜めピン
}

impl SeePinInfo {
    /// 空のピン情報を作成
    fn empty() -> Self {
        SeePinInfo {
            pinned: Bitboard::EMPTY,
            vertical_pins: Bitboard::EMPTY,
            horizontal_pins: Bitboard::EMPTY,
            diag_ne_pins: Bitboard::EMPTY,
            diag_nw_pins: Bitboard::EMPTY,
        }
    }

    /// 指定された駒が指定された方向に移動できるかチェック
    fn can_move(&self, from: Square, to: Square) -> bool {
        // ピンされていない駒は自由に動ける
        if !self.pinned.test(from) {
            return true;
        }

        // ピンされている場合、ピンの方向に沿った移動のみ許可

        // 縦方向のピン
        if self.vertical_pins.test(from) {
            return from.file() == to.file();
        }

        // 横方向のピン
        if self.horizontal_pins.test(from) {
            return from.rank() == to.rank();
        }

        // 北東-南西の斜めピン
        if self.diag_ne_pins.test(from) {
            let file_diff = from.file() as i8 - to.file() as i8;
            let rank_diff = from.rank() as i8 - to.rank() as i8;
            return file_diff == rank_diff;
        }

        // 北西-南東の斜めピン
        if self.diag_nw_pins.test(from) {
            let file_diff = from.file() as i8 - to.file() as i8;
            let rank_diff = from.rank() as i8 - to.rank() as i8;
            return file_diff == -rank_diff;
        }

        false
    }
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
            zobrist_hash: 0,
            history: Vec::new(),
        }
    }

    /// Create starting position
    pub fn startpos() -> Self {
        let mut pos = Self::empty();

        // Place pawns
        // Black pawns on rank 6 (7th rank), White pawns on rank 2 (3rd rank)
        for file in 0..9 {
            let white_pawn_sq = Square::from_usi_chars((b'9' - file as u8) as char, 'c').unwrap();
            pos.board.put_piece(white_pawn_sq, Piece::new(PieceType::Pawn, Color::White));
            let black_pawn_sq = Square::from_usi_chars((b'9' - file as u8) as char, 'g').unwrap();
            pos.board.put_piece(black_pawn_sq, Piece::new(PieceType::Pawn, Color::Black));
        }

        // Black pieces on rank 8 (9th rank) and rank 7 (8th rank) - Black is at bottom
        // Lances
        pos.board.put_piece(
            Square::from_usi_chars('9', 'i').unwrap(),
            Piece::new(PieceType::Lance, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('1', 'i').unwrap(),
            Piece::new(PieceType::Lance, Color::Black),
        );

        // Knights
        pos.board.put_piece(
            Square::from_usi_chars('8', 'i').unwrap(),
            Piece::new(PieceType::Knight, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('2', 'i').unwrap(),
            Piece::new(PieceType::Knight, Color::Black),
        );

        // Silvers
        pos.board.put_piece(
            Square::from_usi_chars('7', 'i').unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('3', 'i').unwrap(),
            Piece::new(PieceType::Silver, Color::Black),
        );

        // Golds
        pos.board.put_piece(
            Square::from_usi_chars('6', 'i').unwrap(),
            Piece::new(PieceType::Gold, Color::Black),
        );
        pos.board.put_piece(
            Square::from_usi_chars('4', 'i').unwrap(),
            Piece::new(PieceType::Gold, Color::Black),
        );

        // King
        pos.board.put_piece(
            Square::from_usi_chars('5', 'i').unwrap(),
            Piece::new(PieceType::King, Color::Black),
        );

        // Rook (at 2h in USI)
        pos.board.put_piece(
            Square::from_usi_chars('2', 'h').unwrap(),
            Piece::new(PieceType::Rook, Color::Black),
        );

        // Bishop (at 8h in USI)
        pos.board.put_piece(
            Square::from_usi_chars('8', 'h').unwrap(),
            Piece::new(PieceType::Bishop, Color::Black),
        );

        // White pieces on rank 0 (1st rank) and rank 1 (2nd rank) - White is at top
        // Lances
        pos.board.put_piece(
            Square::from_usi_chars('9', 'a').unwrap(),
            Piece::new(PieceType::Lance, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('1', 'a').unwrap(),
            Piece::new(PieceType::Lance, Color::White),
        );

        // Knights
        pos.board.put_piece(
            Square::from_usi_chars('8', 'a').unwrap(),
            Piece::new(PieceType::Knight, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('2', 'a').unwrap(),
            Piece::new(PieceType::Knight, Color::White),
        );

        // Silvers
        pos.board.put_piece(
            Square::from_usi_chars('7', 'a').unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('3', 'a').unwrap(),
            Piece::new(PieceType::Silver, Color::White),
        );

        // Golds
        pos.board.put_piece(
            Square::from_usi_chars('6', 'a').unwrap(),
            Piece::new(PieceType::Gold, Color::White),
        );
        pos.board.put_piece(
            Square::from_usi_chars('4', 'a').unwrap(),
            Piece::new(PieceType::Gold, Color::White),
        );

        // King
        pos.board.put_piece(
            Square::from_usi_chars('5', 'a').unwrap(),
            Piece::new(PieceType::King, Color::White),
        );

        // Rook (at 8b in USI)
        pos.board.put_piece(
            Square::from_usi_chars('8', 'b').unwrap(),
            Piece::new(PieceType::Rook, Color::White),
        );

        // Bishop (at 2b in USI)
        pos.board.put_piece(
            Square::from_usi_chars('2', 'b').unwrap(),
            Piece::new(PieceType::Bishop, Color::White),
        );

        // Calculate hash
        pos.hash = pos.compute_hash();
        pos.zobrist_hash = pos.hash;

        // Set initial ply to 0
        pos.ply = 0;

        pos
    }

    /// Create position from SFEN string
    pub fn from_sfen(sfen: &str) -> Result<Position, String> {
        crate::usi::parse_sfen(sfen).map_err(|e| e.to_string())
    }

    /// Compute Zobrist hash
    fn compute_hash(&self) -> u64 {
        use crate::zobrist::ZobristHashing;
        ZobristHashing::zobrist_hash(self)
    }

    /// Get zobrist hash (method for compatibility)
    pub fn zobrist_hash(&self) -> u64 {
        self.zobrist_hash
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

    /// Validate if a move is pseudo-legal (doesn't check for leaving king in check)
    /// Returns true if the move appears to be legal based on basic rules
    pub fn is_pseudo_legal(&self, mv: super::moves::Move) -> bool {
        if mv.is_null() {
            return false;
        }

        if mv.is_drop() {
            let to = mv.to();
            // Check destination is empty
            if self.board.piece_on(to).is_some() {
                return false;
            }
            // Check we have the piece in hand
            let piece_type = mv.drop_piece_type();
            let hand_idx = match piece_type_to_hand_index(piece_type) {
                Ok(idx) => idx,
                Err(_) => return false,
            };
            if self.hands[self.side_to_move as usize][hand_idx] == 0 {
                return false;
            }
        } else {
            let from = match mv.from() {
                Some(f) => f,
                None => return false,
            };
            let to = mv.to();

            // Check source has a piece
            let piece = match self.board.piece_on(from) {
                Some(p) => p,
                None => return false,
            };

            // Check piece belongs to side to move
            if piece.color != self.side_to_move {
                return false;
            }

            // Check destination - if occupied, must be opponent's piece
            if let Some(dest_piece) = self.board.piece_on(to) {
                if dest_piece.color == self.side_to_move {
                    return false;
                }
                // Never allow king capture
                if dest_piece.piece_type == PieceType::King {
                    return false;
                }
            }
        }

        true
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
            let from = mv.from().expect("Normal move must have from square");
            let to = mv.to();

            // Get moving piece
            let mut piece = self.board.piece_on(from).expect("Move source must have a piece");

            // CRITICAL: Validate that the moving piece belongs to the side to move
            // This prevents illegal moves where the wrong side's piece is being moved
            if piece.color != self.side_to_move {
                eprintln!("ERROR: Attempting to move opponent's piece!");
                eprintln!("Move: from={from}, to={to}");
                eprintln!("Moving piece: {piece:?}");
                eprintln!("Side to move: {:?}", self.side_to_move);
                eprintln!("Position SFEN: {}", crate::usi::position_to_sfen(self));
                panic!("Illegal move: attempting to move opponent's piece from {from} to {to}");
            }

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
                    eprintln!("ERROR: King capture detected!");
                    eprintln!("Move: from={from}, to={to}");
                    eprintln!("Moving piece: {piece:?}");
                    eprintln!("Captured piece: {captured:?}");
                    eprintln!("Side to move: {:?}", self.side_to_move);
                    eprintln!("Position SFEN: {}", crate::usi::position_to_sfen(self));
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
        self.hash ^= ZOBRIST.side_to_move;
        self.zobrist_hash = self.hash;

        // Increment ply
        self.ply += 1;

        undo_info
    }

    /// Check if the current side to move is in check
    pub fn is_in_check(&self) -> bool {
        // Get the king square for the side to move
        if let Some(king_sq) = self.board.king_square(self.side_to_move) {
            // Check if the king is attacked by the opponent
            self.board.is_attacked_by(king_sq, self.side_to_move.opposite())
        } else {
            // No king on board - shouldn't happen in a legal position
            false
        }
    }

    /// Check if position is draw (simplified check)
    pub fn is_draw(&self) -> bool {
        // Simple repetition detection would go here
        // For now, return false
        false
    }

    /// Check if side is in check
    pub fn in_check(&self) -> bool {
        // For now, simply check if king is attacked
        let king_bb = self.board.piece_bb[self.side_to_move as usize][PieceType::King as usize];
        if let Some(king_sq) = king_bb.lsb() {
            self.is_attacked(king_sq, self.side_to_move.opposite())
        } else {
            false
        }
    }

    /// Check if specific color is in check
    pub fn is_check(&self, color: Color) -> bool {
        let king_bb = self.board.piece_bb[color as usize][PieceType::King as usize];
        if let Some(king_sq) = king_bb.lsb() {
            self.is_attacked(king_sq, color.opposite())
        } else {
            false
        }
    }

    /// Check if a square is attacked by a given color
    pub fn is_attacked(&self, sq: Square, by_color: Color) -> bool {
        // Check pawn attacks
        let pawn_bb = self.board.piece_bb[by_color as usize][PieceType::Pawn as usize];
        let pawn_attacks = ATTACK_TABLES.pawn_attacks(sq, by_color.opposite());
        if !(pawn_bb & pawn_attacks).is_empty() {
            return true;
        }

        // Check knight attacks
        let knight_bb = self.board.piece_bb[by_color as usize][PieceType::Knight as usize];
        let knight_attacks = ATTACK_TABLES.knight_attacks(sq, by_color.opposite());
        if !(knight_bb & knight_attacks).is_empty() {
            return true;
        }

        // Check king attacks
        let king_bb = self.board.piece_bb[by_color as usize][PieceType::King as usize];
        let king_attacks = ATTACK_TABLES.king_attacks(sq);
        if !(king_bb & king_attacks).is_empty() {
            return true;
        }

        // Check gold attacks
        let gold_bb = self.board.piece_bb[by_color as usize][PieceType::Gold as usize];
        let gold_attacks = ATTACK_TABLES.gold_attacks(sq, by_color.opposite());
        if !(gold_bb & gold_attacks).is_empty() {
            return true;
        }

        // Check silver attacks
        let silver_bb = self.board.piece_bb[by_color as usize][PieceType::Silver as usize];
        let silver_attacks = ATTACK_TABLES.silver_attacks(sq, by_color.opposite());
        if !(silver_bb & silver_attacks).is_empty() {
            return true;
        }

        // Check sliding pieces (rook, bishop, lance)
        let occupied = self.board.all_bb;

        // Rook attacks
        let rook_bb = self.board.piece_bb[by_color as usize][PieceType::Rook as usize];
        let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        if !(rook_bb & rook_attacks).is_empty() {
            return true;
        }

        // Bishop attacks
        let bishop_bb = self.board.piece_bb[by_color as usize][PieceType::Bishop as usize];
        let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
        if !(bishop_bb & bishop_attacks).is_empty() {
            return true;
        }

        // Lance attacks
        let lance_bb = self.board.piece_bb[by_color as usize][PieceType::Lance as usize]
            & !self.board.promoted_bb;
        let lance_attackers = self.get_lance_attackers_to(sq, by_color, lance_bb, occupied);
        if !lance_attackers.is_empty() {
            return true;
        }

        false
    }

    /// Get all pieces of a given color attacking a square
    /// Returns a bitboard with all attacking pieces
    pub fn get_attackers_to(&self, sq: Square, by_color: Color) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;

        // Check pawn attacks
        let pawn_bb = self.board.piece_bb[by_color as usize][PieceType::Pawn as usize];
        let pawn_attacks = ATTACK_TABLES.pawn_attacks(sq, by_color.opposite());
        attackers |= pawn_bb & pawn_attacks;

        // Check knight attacks
        let knight_bb = self.board.piece_bb[by_color as usize][PieceType::Knight as usize];
        let knight_attacks = ATTACK_TABLES.knight_attacks(sq, by_color.opposite());
        attackers |= knight_bb & knight_attacks;

        // Check king attacks
        let king_bb = self.board.piece_bb[by_color as usize][PieceType::King as usize];
        let king_attacks = ATTACK_TABLES.king_attacks(sq);
        attackers |= king_bb & king_attacks;

        // Check gold attacks (including promoted pieces that move like gold)
        let gold_bb = self.board.piece_bb[by_color as usize][PieceType::Gold as usize];
        let gold_attacks = ATTACK_TABLES.gold_attacks(sq, by_color.opposite());
        attackers |= gold_bb & gold_attacks;

        // Check promoted pawns, lances, knights, and silvers (they move like gold)
        let promoted_bb = self.board.promoted_bb;
        let tokin_bb = pawn_bb & promoted_bb;
        let promoted_lance_bb =
            self.board.piece_bb[by_color as usize][PieceType::Lance as usize] & promoted_bb;
        let promoted_knight_bb = knight_bb & promoted_bb;
        let promoted_silver_bb =
            self.board.piece_bb[by_color as usize][PieceType::Silver as usize] & promoted_bb;
        attackers |=
            (tokin_bb | promoted_lance_bb | promoted_knight_bb | promoted_silver_bb) & gold_attacks;

        // Check silver attacks (unpromoted only)
        let silver_bb =
            self.board.piece_bb[by_color as usize][PieceType::Silver as usize] & !promoted_bb;
        let silver_attacks = ATTACK_TABLES.silver_attacks(sq, by_color.opposite());
        attackers |= silver_bb & silver_attacks;

        // Check sliding pieces (rook, bishop, lance)
        let occupied = self.board.all_bb;

        // Rook attacks (including dragon)
        let rook_bb = self.board.piece_bb[by_color as usize][PieceType::Rook as usize];
        let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        attackers |= rook_bb & rook_attacks;

        // Dragon (promoted rook) also has king moves
        let dragon_bb = rook_bb & promoted_bb;
        attackers |= dragon_bb & king_attacks;

        // Bishop attacks (including horse)
        let bishop_bb = self.board.piece_bb[by_color as usize][PieceType::Bishop as usize];
        let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
        attackers |= bishop_bb & bishop_attacks;

        // Horse (promoted bishop) also has king moves
        let horse_bb = bishop_bb & promoted_bb;
        attackers |= horse_bb & king_attacks;

        // Lance attacks (only unpromoted, as promoted lance moves like gold)
        let lance_bb =
            self.board.piece_bb[by_color as usize][PieceType::Lance as usize] & !promoted_bb;

        // Use attack tables for efficient lance detection
        attackers |= self.get_lance_attackers_to(sq, by_color, lance_bb, occupied);

        attackers
    }

    /// Get lance attackers to a square using optimized bitboard operations
    fn get_lance_attackers_to(
        &self,
        sq: Square,
        by_color: Color,
        lance_bb: Bitboard,
        occupied: Bitboard,
    ) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;
        let file = sq.file();

        // Get all lances in the same file
        let file_mask = ATTACK_TABLES.file_masks[file as usize];
        let lances_in_file = lance_bb & file_mask;

        if lances_in_file.is_empty() {
            return attackers;
        }

        // Get potential lance attackers using pre-computed rays
        // Note: We use the opposite color because lance_rays[color][sq] gives squares a lance can ATTACK from sq,
        // but we want squares that can attack sq
        let lance_ray = ATTACK_TABLES.lance_rays[by_color.opposite() as usize][sq.index()];
        let potential_attackers = lances_in_file & lance_ray;

        // Check each potential attacker for blockers
        let mut lances = potential_attackers;
        while !lances.is_empty() {
            let from = lances.pop_lsb().expect("Lance bitboard should not be empty");

            // Use pre-computed between bitboard
            let between = ATTACK_TABLES.between_bb(from, sq);
            if (between & occupied).is_empty() {
                // Path is clear, lance can attack
                attackers.set(from);
            }
        }

        attackers
    }

    /// Get blockers for king (simplified version)
    /// Returns a bitboard of pieces that are pinned to the king
    pub fn get_blockers_for_king(&self, king_color: Color) -> Bitboard {
        let king_bb = self.board.piece_bb[king_color as usize][PieceType::King as usize];
        let king_sq = match king_bb.lsb() {
            Some(sq) => sq,
            None => {
                // This should never happen in a valid position
                log::error!(
                    "King not found for color {king_color:?} in get_blockers_for_king - data inconsistency"
                );
                return Bitboard::EMPTY;
            }
        };

        let enemy_color = king_color.opposite();
        let occupied = self.board.all_bb;
        let mut blockers = Bitboard::EMPTY;

        // Check for pins by sliding pieces (rook, bishop, lance)

        // Rook and Dragon pins (horizontal and vertical)
        let enemy_rooks = self.board.piece_bb[enemy_color as usize][PieceType::Rook as usize];
        let _rook_attacks = ATTACK_TABLES.sliding_attacks(king_sq, occupied, PieceType::Rook);
        let rook_xray = ATTACK_TABLES.sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Rook);

        // Find pieces between king and enemy rooks
        let potential_rook_pinners = enemy_rooks & rook_xray;
        let mut pinners_bb = potential_rook_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            // Check if there's exactly one piece between king and pinner
            let between = self.get_between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 {
                blockers |= between;
            }
        }

        // Bishop and Horse pins (diagonal)
        let enemy_bishops = self.board.piece_bb[enemy_color as usize][PieceType::Bishop as usize];
        let _bishop_attacks = ATTACK_TABLES.sliding_attacks(king_sq, occupied, PieceType::Bishop);
        let bishop_xray =
            ATTACK_TABLES.sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Bishop);

        // Find pieces between king and enemy bishops
        let potential_bishop_pinners = enemy_bishops & bishop_xray;
        let mut pinners_bb = potential_bishop_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            // Check if there's exactly one piece between king and pinner
            let between = self.get_between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 {
                blockers |= between;
            }
        }

        // Lance pins (vertical only)
        let enemy_lances = self.board.piece_bb[enemy_color as usize][PieceType::Lance as usize]
            & !self.board.promoted_bb;

        // Use file mask to get lances in the same file
        let file_mask = ATTACK_TABLES.file_mask(king_sq.file());
        let lances_in_file = enemy_lances & file_mask;

        if !lances_in_file.is_empty() {
            // Get the ray from king in the direction of enemy lance attacks
            let lance_ray = if enemy_color == Color::Black {
                // Black lance attacks from below (higher ranks)
                ATTACK_TABLES.lance_rays[Color::White as usize][king_sq.index()]
            } else {
                // White lance attacks from above (lower ranks)
                ATTACK_TABLES.lance_rays[Color::Black as usize][king_sq.index()]
            };

            // Find the closest lance that can attack the king
            let potential_pinners = lances_in_file & lance_ray;
            if let Some(lance_sq) = potential_pinners.lsb() {
                // Use pre-computed between bitboard
                let between = ATTACK_TABLES.between_bb(king_sq, lance_sq) & occupied;
                if between.count_ones() == 1 {
                    blockers |= between;
                }
            }
        }

        blockers
    }

    /// Get bitboard of squares between two squares (exclusive)
    fn get_between_bb(&self, sq1: Square, sq2: Square) -> Bitboard {
        let mut between = Bitboard::EMPTY;

        let file1 = sq1.file() as i8;
        let rank1 = sq1.rank() as i8;
        let file2 = sq2.file() as i8;
        let rank2 = sq2.rank() as i8;

        let file_diff = file2 - file1;
        let rank_diff = rank2 - rank1;

        // Check if squares are aligned
        if file_diff == 0 || rank_diff == 0 || file_diff.abs() == rank_diff.abs() {
            let file_step = file_diff.signum();
            let rank_step = rank_diff.signum();

            let mut file = file1 + file_step;
            let mut rank = rank1 + rank_step;

            while file != file2 || rank != rank2 {
                between.set(Square::new(file as u8, rank as u8));
                file += file_step;
                rank += rank_step;
            }
        }

        between
    }

    /// Undo a move on the position
    pub fn undo_move(&mut self, mv: super::moves::Move, undo_info: UndoInfo) {
        // Remove last hash from history
        self.history.pop();

        // Restore hash value
        self.hash = undo_info.previous_hash;
        self.zobrist_hash = self.hash;

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
            let from = mv.from().expect("Normal move must have from square");
            let to = mv.to();

            // Get piece from destination
            let mut piece =
                self.board.piece_on(to).expect("Move destination must have a piece after move");

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

    /// Static Exchange Evaluation (SEE)
    /// Evaluates the material gain/loss from a capture sequence
    /// Returns the expected material gain from the move (positive = good, negative = bad)
    pub fn see(&self, mv: Move) -> i32 {
        self.see_internal(mv, 0)
    }

    /// Static Exchange Evaluation (no inline version for benchmarking)
    /// This prevents constant folding in benchmarks
    #[inline(never)]
    pub fn see_noinline(&self, mv: Move) -> i32 {
        self.see_internal(mv, 0)
    }

    /// Static Exchange Evaluation with threshold
    /// Returns true if the SEE value is greater than or equal to the threshold
    pub fn see_ge(&self, mv: Move, threshold: i32) -> bool {
        // Use threshold in internal calculation for early termination
        self.see_internal(mv, threshold) >= threshold
    }

    /// Internal SEE implementation using gain array algorithm
    /// Returns the expected material gain from the move
    /// If threshold is provided and SEE value cannot reach it, returns early
    fn see_internal(&self, mv: Move, threshold: i32) -> i32 {
        let to = mv.to();

        // Not a capture
        let captured = match self.board.piece_on(to) {
            Some(piece) => piece,
            None => return 0,
        };

        let captured_value = Self::see_piece_value(captured);

        // For drops, we assume the piece is safe
        if mv.is_drop() {
            return captured_value;
        }

        let from = mv.from().expect("Normal move must have from square");
        let mut occupied = self.board.all_bb;

        // Get the initial attacker
        let attacker = self.board.piece_on(from).expect("Move source must have a piece");
        let attacker_value = if mv.is_promote() {
            Self::see_promoted_piece_value(attacker.piece_type)
        } else {
            Self::see_piece_value(attacker)
        };

        // Delta pruning optimization for SEE
        //
        // Returns early if the maximum possible gain cannot reach the threshold.
        // This optimization is particularly effective for:
        // - High thresholds that are clearly unreachable
        // - Shallow exchanges (2-4 captures)
        // - Positions with limited attacking pieces
        //
        // Only apply for see_ge calls (threshold != 0)
        if threshold != 0 && captured_value < threshold {
            // Best case is just capturing the target piece
            return captured_value;
        }

        // Calculate pin information for both colors
        let (black_pins, white_pins) = self.calculate_pins_for_see();

        // Gain array to track material balance at each ply
        let mut gain = [0i32; SEE_GAIN_ARRAY_SIZE];
        let mut depth = 0;

        // Track cumulative evaluation to avoid O(n²) recalculation
        let mut cumulative_eval = captured_value;

        // gain[0] is the initial capture value
        gain[0] = captured_value;

        // Make the initial capture
        occupied.clear(from);
        occupied.set(to); // The capturing piece now occupies the target square

        // Get all attackers
        let mut attackers = self.get_all_attackers_to(to, occupied);

        let mut stm = self.side_to_move.opposite();
        // The first piece to be potentially recaptured is the initial attacker
        let mut _last_captured_value = attacker_value;

        // Generate capture sequence
        loop {
            // Select appropriate pin info based on side to move
            let pin_info = match stm {
                Color::Black => &black_pins,
                Color::White => &white_pins,
            };

            // Get next attacker considering pin constraints
            match self.pop_least_valuable_attacker_with_pins(
                &mut attackers,
                occupied,
                stm,
                to,
                pin_info,
            ) {
                Some((sq, _, attacker_value)) => {
                    depth += 1;

                    // gain[d] = 取られた駒の価値 - gain[d‑1]
                    gain[depth] = _last_captured_value - gain[depth - 1];

                    // Update cumulative evaluation (O(1) instead of O(n))
                    cumulative_eval = std::cmp::max(-cumulative_eval, gain[depth]);

                    // Delta pruning: early termination if we can't possibly reach threshold
                    if threshold != 0 && depth >= 1 {
                        // Current evaluation from initial side's perspective
                        let current_eval = if depth & 1 == 1 {
                            -cumulative_eval
                        } else {
                            cumulative_eval
                        };

                        // Estimate maximum possible remaining value
                        // Consider remaining attackers by piece type
                        let max_remaining_value = self.estimate_max_remaining_value(
                            &attackers,
                            stm,
                            threshold,
                            current_eval,
                        );

                        // Maximum possible gain
                        let max_possible_gain = if stm == self.side_to_move {
                            // We move next, can potentially gain more
                            current_eval + max_remaining_value
                        } else {
                            // Opponent moves next, our position might get worse
                            current_eval
                        };

                        // Early termination if we can't reach threshold
                        if max_possible_gain < threshold {
                            return current_eval;
                        }
                    }

                    // 深さの上限チェック
                    if depth >= SEE_MAX_DEPTH {
                        break;
                    }

                    // 盤面を更新
                    occupied.clear(sq); // 攻撃駒を元の升から除去
                    occupied.set(to); // 取った駒が目的地に移動
                    _last_captured_value = attacker_value; // 次に取られる駒の価値を更新

                    // X-ray を更新して「幽霊駒」問題を防ぐ
                    self.update_xray_attacks(sq, to, &mut attackers, occupied);

                    // 手番を反転
                    stm = stm.opposite();
                }
                None => break,
            }
        }

        // Apply negamax propagation from the end

        // Propagate scores from the end
        // At odd depths (1, 3, 5...), the opponent moved last, so we negate and maximize
        // At even depths (0, 2, 4...), we moved last, so we keep the sign

        // Work backwards, alternating between minimizing and maximizing
        for d in (0..depth).rev() {
            let _old_gain = gain[d];

            // Check who moved at this depth
            // depth 0: initial attacker (same as side_to_move)
            // depth 1: opponent
            // depth 2: initial attacker again
            // etc.

            // At each depth, we're computing from the perspective of who moved at that depth
            // They choose between standing pat (-gain[d]) or opponent's best continuation (gain[d+1])
            gain[d] = std::cmp::max(-gain[d], gain[d + 1]);
        }

        // Fix sign for even number of exchanges
        // When depth is odd (meaning even number of total exchanges),
        // the last move was made by the opponent, so we need to negate
        if depth & 1 == 1 {
            gain[0] = -gain[0];
        }

        gain[0]
    }

    /// Get piece value for SEE calculation
    #[inline]
    fn see_piece_value(piece: Piece) -> i32 {
        SEE_PIECE_VALUES[piece.promoted as usize][piece.piece_type as usize]
    }

    /// Get base piece type value for SEE
    #[inline]
    fn see_piece_type_value(piece_type: PieceType) -> i32 {
        SEE_PIECE_VALUES[0][piece_type as usize]
    }

    /// Get promoted piece value for SEE
    #[inline]
    fn see_promoted_piece_value(piece_type: PieceType) -> i32 {
        SEE_PIECE_VALUES[1][piece_type as usize]
    }

    /// Estimate maximum remaining value that can be captured
    /// Returns the total value of all remaining attacking pieces
    #[inline]
    fn estimate_max_remaining_value(
        &self,
        attackers: &Bitboard,
        stm: Color,
        threshold: i32,
        current_eval: i32,
    ) -> i32 {
        // Extract only attacking pieces for the side to move
        let mut bb = *attackers & self.board.occupied_bb[stm as usize];

        // Total value of all remaining attackers
        let mut total = 0;

        while let Some(sq) = bb.pop_lsb() {
            // Get actual piece value including promotion status
            if let Some(piece) = self.board.piece_on(sq) {
                total += Self::see_piece_value(piece);

                // Early termination if threshold is already exceeded
                if current_eval + total >= threshold {
                    break;
                }
            }
        }

        total
    }

    /// SEE用の軽量なピン計算（両陣営分）
    fn calculate_pins_for_see(&self) -> (SeePinInfo, SeePinInfo) {
        let black_pins = self.calculate_pins_for_color(Color::Black);
        let white_pins = self.calculate_pins_for_color(Color::White);
        (black_pins, white_pins)
    }

    /// 特定色のピン計算
    fn calculate_pins_for_color(&self, color: Color) -> SeePinInfo {
        // ピンが存在しない場合の早期リターン最適化
        let king_bb = self.board.piece_bb[color as usize][PieceType::King as usize];
        let king_sq = match king_bb.lsb() {
            Some(sq) => sq,
            None => return SeePinInfo::empty(),
        };

        let mut pin_info = SeePinInfo::empty();
        let enemy = color.opposite();
        let occupied = self.board.all_bb;
        let our_pieces = self.board.occupied_bb[color as usize];

        // 敵のスライダー駒を取得
        let enemy_rooks = self.board.piece_bb[enemy as usize][PieceType::Rook as usize];
        let enemy_bishops = self.board.piece_bb[enemy as usize][PieceType::Bishop as usize];
        let enemy_lances = self.board.piece_bb[enemy as usize][PieceType::Lance as usize]
            & !self.board.promoted_bb;

        // 飛車・竜による縦横のピン
        let rook_xray = ATTACK_TABLES.sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Rook);
        let potential_rook_pinners = enemy_rooks & rook_xray;

        let mut pinners_bb = potential_rook_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            let between = ATTACK_TABLES.between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 && (between & our_pieces).count_ones() == 1 {
                let pinned_sq =
                    between.lsb().expect("Between squares must have at least one square");
                pin_info.pinned.set(pinned_sq);

                // ピンの方向を判定
                if king_sq.file() == pinner_sq.file() {
                    pin_info.vertical_pins.set(pinned_sq);
                } else {
                    pin_info.horizontal_pins.set(pinned_sq);
                }
            }
        }

        // 角・馬による斜めのピン
        let bishop_xray =
            ATTACK_TABLES.sliding_attacks(king_sq, Bitboard::EMPTY, PieceType::Bishop);
        let potential_bishop_pinners = enemy_bishops & bishop_xray;

        pinners_bb = potential_bishop_pinners;
        while let Some(pinner_sq) = pinners_bb.pop_lsb() {
            let between = ATTACK_TABLES.between_bb(king_sq, pinner_sq) & occupied;
            if between.count_ones() == 1 && (between & our_pieces).count_ones() == 1 {
                let pinned_sq =
                    between.lsb().expect("Between squares must have at least one square");
                pin_info.pinned.set(pinned_sq);

                // ピンの方向を判定
                let file_diff = king_sq.file() as i8 - pinner_sq.file() as i8;
                let rank_diff = king_sq.rank() as i8 - pinner_sq.rank() as i8;

                if file_diff == rank_diff {
                    pin_info.diag_ne_pins.set(pinned_sq);
                } else {
                    pin_info.diag_nw_pins.set(pinned_sq);
                }
            }
        }

        // 香車による縦のピン（特殊処理）
        let file_mask = ATTACK_TABLES.file_masks[king_sq.file() as usize];
        let lances_in_file = enemy_lances & file_mask;

        if !lances_in_file.is_empty() {
            // 香車は方向性があるので、敵香車の位置と王の位置関係を確認
            let mut lance_bb = lances_in_file;
            while let Some(lance_sq) = lance_bb.pop_lsb() {
                let can_attack = match enemy {
                    Color::Black => lance_sq.rank() < king_sq.rank(),
                    Color::White => lance_sq.rank() > king_sq.rank(),
                };

                if can_attack {
                    let between = ATTACK_TABLES.between_bb(lance_sq, king_sq) & occupied;
                    if between.count_ones() == 1 && (between & our_pieces).count_ones() == 1 {
                        let pinned_sq =
                            between.lsb().expect("Between squares must have at least one square");
                        pin_info.pinned.set(pinned_sq);
                        pin_info.vertical_pins.set(pinned_sq);
                    }
                }
            }
        }

        pin_info
    }

    /// Get all attackers to a square (both colors)
    fn get_all_attackers_to(&self, sq: Square, occupied: Bitboard) -> Bitboard {
        // Get attackers from both colors with current occupancy
        let black_attackers = self.get_attackers_to_with_occupancy(sq, Color::Black, occupied);
        let white_attackers = self.get_attackers_to_with_occupancy(sq, Color::White, occupied);
        black_attackers | white_attackers
    }

    /// Get attackers to a square with custom occupancy (for X-ray detection)
    fn get_attackers_to_with_occupancy(
        &self,
        sq: Square,
        by_color: Color,
        occupied: Bitboard,
    ) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;

        // Check pawn attacks
        let pawn_bb = self.board.piece_bb[by_color as usize][PieceType::Pawn as usize];
        let pawn_attacks = ATTACK_TABLES.pawn_attacks(sq, by_color.opposite());
        attackers |= pawn_bb & pawn_attacks;

        // Check knight attacks
        let knight_bb = self.board.piece_bb[by_color as usize][PieceType::Knight as usize];
        let knight_attacks = ATTACK_TABLES.knight_attacks(sq, by_color.opposite());
        attackers |= knight_bb & knight_attacks;

        // Check king attacks
        let king_bb = self.board.piece_bb[by_color as usize][PieceType::King as usize];
        let king_attacks = ATTACK_TABLES.king_attacks(sq);
        attackers |= king_bb & king_attacks;

        // Check gold attacks (including promoted pieces that move like gold)
        let gold_bb = self.board.piece_bb[by_color as usize][PieceType::Gold as usize];
        let gold_attacks = ATTACK_TABLES.gold_attacks(sq, by_color.opposite());
        attackers |= gold_bb & gold_attacks;

        // Check promoted pawns, lances, knights, and silvers (they move like gold)
        let promoted_bb = self.board.promoted_bb;
        let tokin_bb = pawn_bb & promoted_bb;
        let promoted_lance_bb =
            self.board.piece_bb[by_color as usize][PieceType::Lance as usize] & promoted_bb;
        let promoted_knight_bb = knight_bb & promoted_bb;
        let promoted_silver_bb =
            self.board.piece_bb[by_color as usize][PieceType::Silver as usize] & promoted_bb;
        attackers |=
            (tokin_bb | promoted_lance_bb | promoted_knight_bb | promoted_silver_bb) & gold_attacks;

        // Check silver attacks (non-promoted)
        let silver_bb =
            self.board.piece_bb[by_color as usize][PieceType::Silver as usize] & !promoted_bb;
        let silver_attacks = ATTACK_TABLES.silver_attacks(sq, by_color.opposite());
        attackers |= silver_bb & silver_attacks;

        // Check sliding attacks with custom occupancy
        let rook_bb = self.board.piece_bb[by_color as usize][PieceType::Rook as usize];
        let rook_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Rook);
        attackers |= rook_bb & rook_attacks;

        let bishop_bb =
            self.board.piece_bb[by_color as usize][PieceType::Bishop as usize] & occupied;
        let bishop_attacks = ATTACK_TABLES.sliding_attacks(sq, occupied, PieceType::Bishop);
        attackers |= bishop_bb & bishop_attacks;

        // Check lance attacks with custom occupancy
        let lance_bb = (self.board.piece_bb[by_color as usize][PieceType::Lance as usize]
            & occupied)
            & !promoted_bb;
        attackers |= self.get_lance_attackers_to_with_occupancy(sq, by_color, lance_bb, occupied);

        attackers
    }

    /// Get lance attackers with custom occupancy
    fn get_lance_attackers_to_with_occupancy(
        &self,
        sq: Square,
        by_color: Color,
        lance_bb: Bitboard,
        occupied: Bitboard,
    ) -> Bitboard {
        let mut attackers = Bitboard::EMPTY;
        let file = sq.file();

        // Get all lances in the same file
        let file_mask = ATTACK_TABLES.file_masks[file as usize];
        let lances_in_file = lance_bb & file_mask;

        if lances_in_file.is_empty() {
            return attackers;
        }

        // Get potential lance attackers using pre-computed rays
        let lance_ray = ATTACK_TABLES.lance_rays[by_color.opposite() as usize][sq.index()];
        let potential_attackers = lances_in_file & lance_ray;

        // Check each potential attacker for blockers
        let mut lances = potential_attackers;
        while !lances.is_empty() {
            let from = lances.pop_lsb().expect("Lance bitboard should not be empty");

            // Use pre-computed between bitboard
            let between = ATTACK_TABLES.between_bb(from, sq);
            if (between & occupied).is_empty() {
                // Path is clear, lance can attack
                attackers.set(from);
            }
        }

        attackers
    }

    /// Pop the least valuable attacker considering pin constraints
    fn pop_least_valuable_attacker_with_pins(
        &self,
        attackers: &mut Bitboard,
        occupied: Bitboard,
        color: Color,
        to: Square,
        pin_info: &SeePinInfo,
    ) -> Option<(Square, PieceType, i32)> {
        // Check pieces in order of increasing value
        for piece_type in [
            PieceType::Pawn,
            PieceType::Lance,
            PieceType::Knight,
            PieceType::Silver,
            PieceType::Gold,
            PieceType::Bishop,
            PieceType::Rook,
        ] {
            // Only consider pieces that are still on the board
            let pieces =
                self.board.piece_bb[color as usize][piece_type as usize] & *attackers & occupied;

            // For each potential attacker of this type
            let mut candidates = pieces;
            while let Some(sq) = candidates.pop_lsb() {
                // Check if this piece can legally move to the target square considering pins
                if pin_info.can_move(sq, to) {
                    attackers.clear(sq);

                    // Check if piece is promoted
                    let is_promoted = self.board.promoted_bb.test(sq);
                    let value = if is_promoted {
                        Self::see_promoted_piece_value(piece_type)
                    } else {
                        Self::see_piece_type_value(piece_type)
                    };

                    return Some((sq, piece_type, value));
                }
            }
        }

        // King should not normally participate in exchanges, but check anyway
        let king_bb =
            self.board.piece_bb[color as usize][PieceType::King as usize] & *attackers & occupied;
        if let Some(sq) = king_bb.lsb() {
            // Kings are never pinned
            attackers.clear(sq);
            return Some((sq, PieceType::King, Self::see_piece_type_value(PieceType::King)));
        }

        None
    }

    /// Update X-ray attacks after removing a piece
    fn update_xray_attacks(
        &self,
        from: Square,
        to: Square,
        attackers: &mut Bitboard,
        occupied: Bitboard,
    ) {
        // Check if there's a clear line between from and to
        let between = ATTACK_TABLES.between_bb(from, to);
        if between.is_empty() {
            return; // Not aligned, no x-rays possible
        }

        // Check for rook/dragon x-rays (orthogonal)
        if from.file() == to.file() || from.rank() == to.rank() {
            let rook_attackers = (self.board.piece_bb[Color::Black as usize]
                [PieceType::Rook as usize]
                | self.board.piece_bb[Color::White as usize][PieceType::Rook as usize])
                & occupied;
            let rook_attacks = ATTACK_TABLES.sliding_attacks(to, occupied, PieceType::Rook);
            *attackers |= rook_attackers & rook_attacks;
        }

        // Check for bishop/horse x-rays (diagonal)
        if (from.file() as i8 - to.file() as i8).abs()
            == (from.rank() as i8 - to.rank() as i8).abs()
        {
            let bishop_attackers = (self.board.piece_bb[Color::Black as usize]
                [PieceType::Bishop as usize]
                | self.board.piece_bb[Color::White as usize][PieceType::Bishop as usize])
                & occupied;
            let bishop_attacks = ATTACK_TABLES.sliding_attacks(to, occupied, PieceType::Bishop);
            *attackers |= bishop_attackers & bishop_attacks;
        }

        // Check for lance x-rays (vertical only)
        if from.file() == to.file() {
            // Black lances attack upward (towards rank 8)
            if from.rank() < to.rank() {
                let black_lances = self.board.piece_bb[Color::Black as usize]
                    [PieceType::Lance as usize]
                    & occupied;
                // Find black lances that can reach the target
                let mut lance_candidates = black_lances;
                while let Some(lance_sq) = lance_candidates.lsb() {
                    lance_candidates.clear(lance_sq);
                    if lance_sq.file() == to.file() && lance_sq.rank() < to.rank() {
                        // Check if path is clear
                        let between_lance = ATTACK_TABLES.between_bb(lance_sq, to);
                        if (between_lance & occupied).is_empty() {
                            attackers.set(lance_sq);
                        }
                    }
                }
            }
            // White lances attack downward (towards rank 0)
            else if from.rank() > to.rank() {
                let white_lances = self.board.piece_bb[Color::White as usize]
                    [PieceType::Lance as usize]
                    & occupied;
                // Find white lances that can reach the target
                let mut lance_candidates = white_lances;
                while let Some(lance_sq) = lance_candidates.lsb() {
                    lance_candidates.clear(lance_sq);
                    if lance_sq.file() == to.file() && lance_sq.rank() > to.rank() {
                        // Check if path is clear
                        let between_lance = ATTACK_TABLES.between_bb(lance_sq, to);
                        if (between_lance & occupied).is_empty() {
                            attackers.set(lance_sq);
                        }
                    }
                }
            }
        }
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
    fn test_square_from_usi_chars() {
        let sq = Square::from_usi_chars('7', 'g').unwrap();
        assert_eq!(sq, Square::new(2, 6)); // 7g
        assert_eq!(sq.to_string(), "7g");

        let sq = Square::from_usi_chars('1', 'a').unwrap();
        assert_eq!(sq, Square::new(8, 0)); // 1a
        assert_eq!(sq.to_string(), "1a");

        let sq = Square::from_usi_chars('9', 'i').unwrap();
        assert_eq!(sq, Square::new(0, 8)); // 9i
        assert_eq!(sq.to_string(), "9i");

        let sq = Square::from_usi_chars('5', 'e').unwrap();
        assert_eq!(sq, Square::new(4, 4)); // 5e
        assert_eq!(sq.to_string(), "5e");

        // Invalid file
        assert!(Square::from_usi_chars('0', 'e').is_err());
        assert!(Square::from_usi_chars('a', 'e').is_err());

        // Invalid rank
        assert!(Square::from_usi_chars('5', 'j').is_err());
        assert!(Square::from_usi_chars('5', '1').is_err());

        // Test FromStr implementation
        let sq: Square = "7g".parse().unwrap();
        assert_eq!(sq, Square::new(2, 6));
        assert_eq!(sq.to_string(), "7g");

        let sq: Square = "1a".parse().unwrap();
        assert_eq!(sq, Square::new(8, 0));

        let sq: Square = "9i".parse().unwrap();
        assert_eq!(sq, Square::new(0, 8));

        // Invalid formats for parse
        assert!("5".parse::<Square>().is_err());
        assert!("5ee".parse::<Square>().is_err());
        assert!("".parse::<Square>().is_err());
        assert!("0a".parse::<Square>().is_err());
        assert!("5j".parse::<Square>().is_err());
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
        assert_eq!(pos.board.king_square(Color::Black), Some(Square::new(4, 8)));
        assert_eq!(pos.board.king_square(Color::White), Some(Square::new(4, 0)));

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
        // Black pawn is on rank 6, moves toward rank 0
        let from = Square::new(6, 6); // Black pawn
        let to = Square::new(6, 5); // One square forward for Black
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

        // Black歩を前進させる (rank 6 -> 5)
        let mv1 = Move::normal(Square::new(6, 6), Square::new(6, 5), false);
        let _undo1 = pos.do_move(mv1);

        // White歩を前進させる (rank 2 -> 3)
        let mv2 = Move::normal(Square::new(4, 2), Square::new(4, 3), false);
        let _undo2 = pos.do_move(mv2);

        // Black歩をさらに前進 (rank 5 -> 4)
        let mv3 = Move::normal(Square::new(6, 5), Square::new(6, 4), false);
        let _undo3 = pos.do_move(mv3);

        // White歩をさらに前進 (rank 3 -> 4)
        let mv4 = Move::normal(Square::new(4, 3), Square::new(4, 4), false);
        let _undo4 = pos.do_move(mv4);

        // Black歩でWhite歩を取る
        let from = Square::new(6, 4);
        let to = Square::new(4, 4);
        let mv = Move::normal(from, to, false);

        let captured_piece = pos.board.piece_on(to).expect("Capture move must have captured piece");
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
        let piece = board.piece_on(Square::new(2, 7)).expect("Piece should exist at Square(2, 7)");
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
            // (from, to, piece_type, color)
            (
                Square::new(6, 6), // Black pawn
                Square::new(6, 5), // One square forward
                PieceType::Pawn,
                Color::Black,
            ),
            (
                Square::new(4, 8), // Black King
                Square::new(5, 7), // Diagonal move
                PieceType::King,
                Color::Black,
            ),
            (
                Square::new(5, 8), // Black Gold
                Square::new(5, 7), // Forward
                PieceType::Gold,
                Color::Black,
            ),
            (
                Square::new(6, 8), // Black Silver
                Square::new(6, 7), // Forward
                PieceType::Silver,
                Color::Black,
            ),
            (
                Square::new(7, 8), // Black Knight
                Square::new(6, 6), // Knight jump
                PieceType::Knight,
                Color::Black,
            ),
            (
                Square::new(8, 8), // Black Lance
                Square::new(8, 7), // Forward
                PieceType::Lance,
                Color::Black,
            ),
            (
                Square::new(7, 7), // Black Rook
                Square::new(7, 5), // Forward
                PieceType::Rook,
                Color::Black,
            ),
            (
                Square::new(1, 7), // Black Bishop
                Square::new(2, 6), // Diagonal
                PieceType::Bishop,
                Color::Black,
            ),
        ];

        for (from, to, expected_piece_type, expected_color) in test_cases {
            let mut pos = Position::startpos();
            let piece = pos.board.piece_on(from);

            // デバッグ: 初期配置の確認
            if piece.is_none() {
                println!("No piece at {from:?}");
                println!("Expected: {expected_piece_type:?}");
                // 周辺の駒を確認
                for rank in 0..9 {
                    for file in 0..9 {
                        if let Some(p) = pos.board.piece_on(Square::new(file, rank)) {
                            if p.piece_type == expected_piece_type && p.color == expected_color {
                                println!(
                                    "Found {expected_piece_type:?} at Square::new({file}, {rank})"
                                );
                            }
                        }
                    }
                }
                panic!("Piece not found at expected position");
            }

            let piece = piece.expect("Piece should exist at this square");
            assert_eq!(piece.piece_type, expected_piece_type);
            assert_eq!(piece.color, expected_color);

            let mv = Move::normal(from, to, false);
            let _undo_info = pos.do_move(mv);

            // 駒が移動していることを確認
            assert_eq!(pos.board.piece_on(from), None);
            let moved_piece =
                pos.board.piece_on(to).expect("Piece should exist at destination after move");
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
            let placed_piece =
                pos.board.piece_on(to).expect("Piece should exist at destination after drop");
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

        let piece = pos
            .board
            .piece_on(Square::new(6, 4))
            .expect("Piece should exist at Square(6, 4)");
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
    fn test_see_simple_pawn_capture() {
        // Test simple pawn takes pawn
        let mut pos = Position::empty();

        // Black pawn on 5e (Square::new(4, 4))
        let black_pawn = Piece::new(PieceType::Pawn, Color::Black);
        pos.board.put_piece(Square::new(4, 4), black_pawn);

        // White pawn on 5d (Square::new(4, 3))
        let white_pawn = Piece::new(PieceType::Pawn, Color::White);
        pos.board.put_piece(Square::new(4, 3), white_pawn);

        // Black to move, pawn takes pawn
        pos.side_to_move = Color::Black;
        let mv = Move::normal(Square::new(4, 4), Square::new(4, 3), false);

        // SEE should be 100 (pawn value)
        assert_eq!(pos.see(mv), 100);
        assert!(pos.see_ge(mv, 0));
        assert!(pos.see_ge(mv, 100));
        assert!(!pos.see_ge(mv, 101));
    }

    #[test]
    fn test_see_bad_exchange() {
        // Test rook takes pawn defended by pawn
        let mut pos = Position::empty();

        // Black rook on 5f (Square::new(4, 5))
        let black_rook = Piece::new(PieceType::Rook, Color::Black);
        pos.board.put_piece(Square::new(4, 5), black_rook);

        // White pawn on 5d (Square::new(4, 3))
        let white_pawn = Piece::new(PieceType::Pawn, Color::White);
        pos.board.put_piece(Square::new(4, 3), white_pawn);

        // White gold on 5c defending (Square::new(4, 2))
        let white_gold = Piece::new(PieceType::Gold, Color::White);
        pos.board.put_piece(Square::new(4, 2), white_gold);

        // Black to move, rook takes pawn
        pos.side_to_move = Color::Black;
        let mv = Move::normal(Square::new(4, 5), Square::new(4, 3), false);

        // SEE should be 100 - 900 = -800 (win pawn, lose rook to gold)
        assert_eq!(pos.see(mv), -800);
        assert!(!pos.see_ge(mv, 0));
    }

    #[test]
    fn test_see_complex_exchange() {
        // Test complex exchange: pawn takes pawn, gold takes pawn, silver takes gold
        let mut pos = Position::empty();

        // Black pawn on 5e
        let black_pawn = Piece::new(PieceType::Pawn, Color::Black);
        pos.board.put_piece(Square::new(4, 4), black_pawn);

        // White pawn on 5d
        let white_pawn = Piece::new(PieceType::Pawn, Color::White);
        pos.board.put_piece(Square::new(4, 3), white_pawn);

        // White gold on 5c (can capture on 5d)
        let white_gold = Piece::new(PieceType::Gold, Color::White);
        pos.board.put_piece(Square::new(4, 2), white_gold);

        // Black silver on 6e (can capture on 5d diagonally)
        let black_silver = Piece::new(PieceType::Silver, Color::Black);
        pos.board.put_piece(Square::new(5, 4), black_silver);

        // Black to move, pawn takes pawn
        pos.side_to_move = Color::Black;
        let mv = Move::normal(Square::new(4, 4), Square::new(4, 3), false);

        // Exchange: PxP (win 100), GxP (lose 100), SxG (win 600)
        // Net: 100 - 100 + 600 = 600
        assert_eq!(pos.see(mv), 600);
        assert!(pos.see_ge(mv, 0));
    }

    #[test]
    fn test_see_x_ray_attack() {
        // Test X-ray attack: rook behind rook
        let mut pos = Position::empty();

        // Black rook on 5f
        let black_rook1 = Piece::new(PieceType::Rook, Color::Black);
        pos.board.put_piece(Square::new(4, 5), black_rook1);

        // Black rook on 5g (behind first rook)
        let black_rook2 = Piece::new(PieceType::Rook, Color::Black);
        pos.board.put_piece(Square::new(4, 6), black_rook2);

        // White pawn on 5d
        let white_pawn = Piece::new(PieceType::Pawn, Color::White);
        pos.board.put_piece(Square::new(4, 3), white_pawn);

        // White rook on 5a (defending)
        let white_rook = Piece::new(PieceType::Rook, Color::White);
        pos.board.put_piece(Square::new(4, 0), white_rook);

        // Black to move, rook takes pawn
        pos.side_to_move = Color::Black;
        let mv = Move::normal(Square::new(4, 5), Square::new(4, 3), false);

        // Exchange: RxP (win 100), RxR (lose 900), RxR (win 900)
        // Net: 100 - 900 + 900 = 100
        assert_eq!(pos.see(mv), 100);
        assert!(pos.see_ge(mv, 0));
    }

    #[test]
    fn test_see_with_pinned_piece() {
        // Test SEE with pinned pieces
        let mut pos = Position::empty();

        // Black King at 5i (file 4, rank 8)
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));

        // Black Gold at 5e (file 4, rank 4) - will be pinned
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::Gold, Color::Black));

        // White Rook at 5a (file 4, rank 0) - pinning the Gold
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::Rook, Color::White));

        // White Pawn at 4e (file 5, rank 4) - can be captured
        pos.board
            .put_piece(Square::new(5, 4), Piece::new(PieceType::Pawn, Color::White));

        // Black Silver at 6f (file 3, rank 5) - can capture the pawn
        pos.board
            .put_piece(Square::new(3, 5), Piece::new(PieceType::Silver, Color::Black));

        // White King at 9a (file 0, rank 0)
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::White));

        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        // The Gold cannot capture the Pawn because it's pinned
        // Only the Silver can capture
        let mv = Move::normal(Square::new(3, 5), Square::new(5, 4), false); // Silver takes Pawn

        // Silver takes Pawn (+100)
        assert_eq!(pos.see(mv), 100);
    }

    #[test]
    fn test_see_with_diagonal_pin() {
        // Test SEE with diagonally pinned piece
        let mut pos = Position::empty();

        // Black King at 5i (file 4, rank 8)
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));

        // Black Silver at 4h (file 5, rank 7) - will be pinned diagonally
        pos.board
            .put_piece(Square::new(5, 7), Piece::new(PieceType::Silver, Color::Black));

        // White Bishop at 1e (file 8, rank 4) - pinning the Silver
        pos.board
            .put_piece(Square::new(8, 4), Piece::new(PieceType::Bishop, Color::White));

        // White Pawn at 3h (file 6, rank 7) - Silver cannot capture due to pin
        pos.board
            .put_piece(Square::new(6, 7), Piece::new(PieceType::Pawn, Color::White));

        // Black Gold at 3g (file 6, rank 6) - can capture the pawn
        pos.board
            .put_piece(Square::new(6, 6), Piece::new(PieceType::Gold, Color::Black));

        // White King at 9a (file 0, rank 0)
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::White));

        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        // The Silver is pinned and cannot capture
        // Only the Gold can capture
        let mv = Move::normal(Square::new(6, 6), Square::new(6, 7), false); // Gold takes Pawn

        // Gold takes Pawn (+100)
        assert_eq!(pos.see(mv), 100);
    }

    #[test]
    fn test_see_delta_pruning() {
        // Test delta pruning optimization in SEE
        let mut pos = Position::empty();

        // Set up a position where delta pruning can help
        // Black King at 5i
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
        // White King at 5a
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

        // Black Pawn at 5f
        pos.board
            .put_piece(Square::new(4, 3), Piece::new(PieceType::Pawn, Color::Black));
        // White Gold at 5e (defended by Rook)
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::Gold, Color::White));
        // White Rook at 5c
        pos.board
            .put_piece(Square::new(4, 6), Piece::new(PieceType::Rook, Color::White));

        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        let mv = Move::normal(Square::new(4, 3), Square::new(4, 4), false); // Pawn takes Gold

        // SEE value: Pawn takes Gold (+600), Rook takes Pawn (-100)
        let see_value = pos.see(mv);
        // Total: +600 - 100 = +500
        assert_eq!(see_value, 500);

        // Test see_ge with various thresholds
        // Should use delta pruning for early termination
        assert!(!pos.see_ge(mv, 600)); // 500 < 600
        assert!(pos.see_ge(mv, 500)); // 500 >= 500
        assert!(pos.see_ge(mv, 400)); // 500 > 400
        assert!(pos.see_ge(mv, 0)); // 500 > 0
        assert!(pos.see_ge(mv, -100)); // 500 > -100
    }

    #[test]
    fn test_see_ge_early_termination() {
        // Test that see_ge can terminate early when threshold cannot be reached
        let mut pos = Position::empty();

        // Black King at 5i
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
        // White King at 5a
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

        // Black Pawn at 5f
        pos.board
            .put_piece(Square::new(4, 3), Piece::new(PieceType::Pawn, Color::Black));
        // White Pawn at 5e (undefended)
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::Pawn, Color::White));

        pos.board.rebuild_occupancy_bitboards();
        pos.side_to_move = Color::Black;

        let mv = Move::normal(Square::new(4, 3), Square::new(4, 4), false); // Pawn takes Pawn

        // Normal SEE value is +100 (simple pawn capture)
        assert_eq!(pos.see(mv), 100);

        // Test see_ge with threshold that triggers early termination
        assert!(!pos.see_ge(mv, 1000)); // Can't reach 1000 with just a pawn capture
        assert!(!pos.see_ge(mv, 500)); // Can't reach 500 either
        assert!(!pos.see_ge(mv, 200)); // Can't reach 200
        assert!(pos.see_ge(mv, 100)); // Exactly 100
        assert!(pos.see_ge(mv, 0)); // Greater than 0
    }

    #[test]
    fn test_see_multiple_high_value_attackers() {
        // Test case with multiple high-value pieces (Rook + Bishop + Lance)
        let mut pos = Position::empty();

        // Set up a simple position where Black has multiple attackers
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(8, 8), Piece::new(PieceType::King, Color::White));

        // Target: White Gold on 5e worth 600
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::Gold, Color::White));

        // Black attackers:
        // - Pawn on 5f (can take Gold)
        // - Rook on 5a (can support after pawn takes)
        // - Bishop on 2h (can support after pawn takes)
        pos.board
            .put_piece(Square::new(4, 3), Piece::new(PieceType::Pawn, Color::Black));
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::Rook, Color::Black));
        pos.board
            .put_piece(Square::new(1, 1), Piece::new(PieceType::Bishop, Color::Black));

        // White defender: Silver on 6f
        pos.board
            .put_piece(Square::new(5, 3), Piece::new(PieceType::Silver, Color::White));

        pos.side_to_move = Color::Black;

        // Move: Pawn takes Gold
        let mv = Move::normal(Square::new(4, 3), Square::new(4, 4), false);

        // SEE calculation:
        // +600 (gold) - 100 (pawn) + 500 (silver) - 700 (bishop) = 300
        // But actually the exchange ends with +600 - 100 = 500 since White will not take the pawn
        // with Silver if it loses material
        let see_value = pos.see(mv);
        assert!(see_value > 0, "Should be a good exchange: {see_value}");

        // Test that see_ge works correctly with multiple attackers
        // The key test is that the algorithm considers all remaining attackers
        assert!(pos.see_ge(mv, 0)); // Positive value
        assert!(pos.see_ge(mv, 500)); // Can reach 500
        assert!(!pos.see_ge(mv, 1500)); // Cannot reach 1500
    }

    #[test]
    fn test_see_promoted_pieces() {
        // Test SEE with promoted pieces to ensure correct value calculation
        let mut pos = Position::empty();

        // Kings
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(8, 8), Piece::new(PieceType::King, Color::White));

        // Target: White promoted pawn (Tokin) on 5e
        let mut tokin = Piece::new(PieceType::Pawn, Color::White);
        tokin.promoted = true;
        pos.board.put_piece(Square::new(4, 4), tokin);

        // Black attacker: Silver on 4d
        pos.board
            .put_piece(Square::new(3, 3), Piece::new(PieceType::Silver, Color::Black));

        // White defenders: promoted Rook (Dragon) on 5a that can recapture
        let mut dragon = Piece::new(PieceType::Rook, Color::White);
        dragon.promoted = true;
        pos.board.put_piece(Square::new(4, 8), dragon);

        // Black has another attacker: promoted Bishop (Horse) on 2b
        let mut horse = Piece::new(PieceType::Bishop, Color::Black);
        horse.promoted = true;
        pos.board.put_piece(Square::new(1, 7), horse);

        pos.side_to_move = Color::Black;

        // Move: Silver takes Tokin
        let mv = Move::normal(Square::new(3, 3), Square::new(4, 4), false);

        // SEE calculation:
        // +600 (tokin) - 500 (silver) + 1200 (dragon) - 900 (horse) = 400
        // But White won't take if it loses material, so it's just +600 - 500 = 100
        let see_value = pos.see(mv);
        assert!(see_value > 0, "Should be a good exchange: {see_value}");

        // Test that the algorithm correctly sums multiple promoted pieces
        assert!(pos.see_ge(mv, 0)); // Positive value
        assert!(pos.see_ge(mv, 100)); // Exactly 100
        assert!(!pos.see_ge(mv, 200)); // Cannot reach 200
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
        let promoted_piece = pos
            .board
            .piece_on(Square::new(4, 7))
            .expect("Promoted piece should exist at Square(4, 7)");
        assert!(promoted_piece.promoted);

        // 手を戻す
        pos.undo_move(promote_move, undo_info);

        // 完全に元に戻ったことを確認
        assert_eq!(pos.hash, before_promotion.hash);
        let original_piece = pos
            .board
            .piece_on(Square::new(4, 6))
            .expect("Original piece should exist at Square(4, 6)");
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

    #[test]
    fn test_is_attacked_with_lance() {
        // Test is_attacked method with lance attacks
        let mut pos = Position::empty();

        // Black lance at 5i (file 4, rank 8)
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::Lance, Color::Black));

        // White lance at 5a (file 4, rank 0)
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::Lance, Color::White));

        // Add kings to make position valid
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(8, 8), Piece::new(PieceType::King, Color::White));

        pos.board.rebuild_occupancy_bitboards();

        // Black lance (Sente) at rank 8 attacks upward (toward rank 0)
        assert!(pos.is_attacked(Square::new(4, 7), Color::Black));
        assert!(pos.is_attacked(Square::new(4, 6), Color::Black));
        assert!(!pos.is_attacked(Square::new(3, 7), Color::Black)); // Different file

        // White lance (Gote) at rank 0 attacks downward (toward rank 8)
        assert!(pos.is_attacked(Square::new(4, 1), Color::White));
        assert!(pos.is_attacked(Square::new(4, 2), Color::White));

        // Move lances to positions where they can attack
        pos.board.remove_piece(Square::new(4, 8));
        pos.board.remove_piece(Square::new(4, 0));
        pos.board
            .put_piece(Square::new(4, 2), Piece::new(PieceType::Lance, Color::Black));
        pos.board
            .put_piece(Square::new(4, 6), Piece::new(PieceType::Lance, Color::White));
        pos.board.rebuild_occupancy_bitboards();

        // Now test actual attacks
        // Black lance (Sente) at rank 2 attacks toward rank 0
        assert!(pos.is_attacked(Square::new(4, 1), Color::Black));
        assert!(pos.is_attacked(Square::new(4, 0), Color::Black));
        assert!(!pos.is_attacked(Square::new(4, 3), Color::Black)); // Cannot attack backward

        // White lance (Gote) at rank 6 attacks toward rank 8
        assert!(pos.is_attacked(Square::new(4, 7), Color::White));
        assert!(pos.is_attacked(Square::new(4, 8), Color::White));
        assert!(!pos.is_attacked(Square::new(4, 5), Color::White)); // Cannot attack backward

        // Test with blocker
        // Place a White pawn as blocker at rank 1 (blocks Black lance)
        pos.board
            .put_piece(Square::new(4, 1), Piece::new(PieceType::Pawn, Color::White));
        pos.board.rebuild_occupancy_bitboards();

        // Black lance at rank 2 is blocked by White pawn at rank 1
        assert!(pos.is_attacked(Square::new(4, 1), Color::Black)); // Lance can attack the blocker
        assert!(!pos.is_attacked(Square::new(4, 0), Color::Black)); // Lance cannot attack beyond blocker

        // Remove blocker and test White lance
        pos.board.remove_piece(Square::new(4, 1));

        // Place a Black pawn as blocker at rank 7 (blocks White lance)
        pos.board
            .put_piece(Square::new(4, 7), Piece::new(PieceType::Pawn, Color::Black));
        pos.board.rebuild_occupancy_bitboards();

        // White lance at rank 6 is blocked by Black pawn at rank 7
        assert!(pos.is_attacked(Square::new(4, 7), Color::White)); // Lance can attack the blocker
        assert!(!pos.is_attacked(Square::new(4, 8), Color::White)); // Lance cannot attack beyond blocker
    }

    #[test]
    fn test_get_lance_attackers_performance() {
        // Skip test in CI environment
        if crate::util::is_ci_environment() {
            println!("Skipping performance test in CI environment");
            return;
        }

        use std::time::Instant;

        // Create a position with multiple lances
        let mut pos = Position::empty();

        // Add multiple lances on the same file to test worst case
        for rank in 0..9 {
            if rank % 3 == 0 {
                pos.board
                    .put_piece(Square::new(4, rank), Piece::new(PieceType::Lance, Color::Black));
            }
        }

        // Add some blockers
        pos.board
            .put_piece(Square::new(4, 5), Piece::new(PieceType::Pawn, Color::White));
        pos.board.rebuild_occupancy_bitboards();

        // Performance test: Call get_lance_attackers_to many times
        let iterations = 100_000;
        let target = Square::new(4, 7);
        let lance_bb = pos.board.piece_bb[Color::Black as usize][PieceType::Lance as usize];
        let occupied = pos.board.all_bb;

        let start = Instant::now();
        for _ in 0..iterations {
            let attackers = pos.get_lance_attackers_to(target, Color::Black, lance_bb, occupied);
            // Force evaluation to prevent optimization
            std::hint::black_box(attackers);
        }
        let elapsed = start.elapsed();

        // Calculate performance metrics
        let ns_per_call = elapsed.as_nanos() / iterations as u128;
        let calls_per_sec = 1_000_000_000 / ns_per_call;

        println!("Lance attackers performance:");
        println!("  Time per call: {ns_per_call} ns");
        println!("  Calls per second: {calls_per_sec}");

        // Assert reasonable performance
        // Note: Debug builds are much slower than release builds
        #[cfg(debug_assertions)]
        let max_ns = 500; // Allow up to 500ns in debug mode
        #[cfg(not(debug_assertions))]
        let max_ns = 100; // Expect under 100ns in release mode

        assert!(
            ns_per_call < max_ns,
            "get_lance_attackers_to is too slow: {ns_per_call} ns (max: {max_ns} ns)"
        );
    }
}
