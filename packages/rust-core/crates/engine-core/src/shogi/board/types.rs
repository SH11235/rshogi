//! Basic types for shogi board representation
//!
//! This module contains fundamental types like Square, PieceType, Piece, and Color.

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
    /// use engine_core::usi::parse_usi_square;
    /// use std::str::FromStr;
    ///
    /// let sq: Square = "7g".parse().unwrap();
    /// assert_eq!(sq, parse_usi_square("7g").unwrap());
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

// 手駒配列 (Position.hands) の並び順を一元管理（King を除く 7 種）
pub const HAND_ORDER: [PieceType; 7] = [
    PieceType::Rook,
    PieceType::Bishop,
    PieceType::Gold,
    PieceType::Silver,
    PieceType::Knight,
    PieceType::Lance,
    PieceType::Pawn,
];

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

    /// Position.hands に対応するインデックス（King は None）
    #[inline]
    pub const fn hand_index(self) -> Option<usize> {
        match self {
            PieceType::King => None,
            _ => Some(self as usize - 1),
        }
    }

    /// hands のインデックスから PieceType を取得（範囲外は None）
    #[inline]
    pub const fn from_hand_index(index: usize) -> Option<PieceType> {
        if index < 7 { Some(HAND_ORDER[index]) } else { None }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usi::parse_usi_square;

    #[test]
    fn test_promotion_zones() {
        // Test USI rank mapping and promotion zones
        println!("\nUSI rank mapping test:");

        let test_squares = vec!["2a", "2b", "2c", "2d", "2e", "2f", "2g", "2h", "2i"];

        for sq_str in &test_squares {
            let sq = parse_usi_square(sq_str).unwrap();
            println!("{} -> rank = {}", sq_str, sq.rank());
        }

        println!("\nPromotion zone test:");
        let from = parse_usi_square("2b").unwrap();
        let to = parse_usi_square("8h").unwrap();

        println!("2b rank = {}", from.rank());
        println!("8h rank = {}", to.rank());

        // Check if promotion is possible from 2b to 8h
        println!("\nFor Black piece moving from 2b to 8h:");
        println!("from.rank() = {} (is <= 2? {})", from.rank(), from.rank() <= 2);
        println!("to.rank() = {} (is <= 2? {})", to.rank(), to.rank() <= 2);

        let black_can_promote = from.rank() <= 2 || to.rank() <= 2;
        println!("Can Black promote? {}", black_can_promote);
        assert!(black_can_promote, "Black should be able to promote when leaving promotion zone");

        println!("\nFor White piece moving from 2b to 8h:");
        println!("from.rank() = {} (is >= 6? {})", from.rank(), from.rank() >= 6);
        println!("to.rank() = {} (is >= 6? {})", to.rank(), to.rank() >= 6);

        let white_can_promote = from.rank() >= 6 || to.rank() >= 6;
        println!("Can White promote? {}", white_can_promote);
        assert!(
            white_can_promote,
            "White should be able to promote when entering promotion zone"
        );
    }

    #[test]
    fn test_square_operations() {
        let sq = parse_usi_square("5e").unwrap(); // 5e
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
        assert_eq!(sq, parse_usi_square("7g").unwrap()); // 7g
        assert_eq!(sq.to_string(), "7g");

        let sq = Square::from_usi_chars('1', 'a').unwrap();
        assert_eq!(sq, parse_usi_square("1a").unwrap()); // 1a
        assert_eq!(sq.to_string(), "1a");

        let sq = Square::from_usi_chars('9', 'i').unwrap();
        assert_eq!(sq, parse_usi_square("9i").unwrap()); // 9i
        assert_eq!(sq.to_string(), "9i");

        let sq = Square::from_usi_chars('5', 'e').unwrap();
        assert_eq!(sq, parse_usi_square("5e").unwrap()); // 5e
        assert_eq!(sq.to_string(), "5e");

        // Invalid file
        assert!(Square::from_usi_chars('0', 'e').is_err());
        assert!(Square::from_usi_chars('a', 'e').is_err());

        // Invalid rank
        assert!(Square::from_usi_chars('5', 'j').is_err());
        assert!(Square::from_usi_chars('5', '1').is_err());

        // Test FromStr implementation
        let sq: Square = "7g".parse().unwrap();
        assert_eq!(sq, parse_usi_square("7g").unwrap());
        assert_eq!(sq.to_string(), "7g");

        let sq: Square = "1a".parse().unwrap();
        assert_eq!(sq, parse_usi_square("1a").unwrap());

        let sq: Square = "9i".parse().unwrap();
        assert_eq!(sq, parse_usi_square("9i").unwrap());

        // Invalid formats for parse
        assert!("5".parse::<Square>().is_err());
        assert!("5ee".parse::<Square>().is_err());
        assert!("".parse::<Square>().is_err());
        assert!("0a".parse::<Square>().is_err());
        assert!("5j".parse::<Square>().is_err());
    }

    #[test]
    fn test_square_flip() {
        // flip()メソッドのテスト
        let sq = parse_usi_square("7d").unwrap(); // インデックス: 2 + 3*9 = 29
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
}
