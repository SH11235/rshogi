//! USI notation parsing and formatting

use crate::shogi::{
    piece_type_to_hand_index, Color, Move, PieceType, Position, Square, MAX_HAND_PIECES,
};
use std::fmt;

/// Error type for USI parsing
#[derive(Debug, Clone, PartialEq)]
pub enum UsiParseError {
    InvalidSquare(String),
    InvalidPiece(char),
    InvalidMoveFormat(String),
    InvalidSfen(String),
    InvalidRankCount(usize),
    UnknownPieceChar(char),
    InvalidHandsFormat(String),
    InvalidMoveCount(String),
    InvalidSideToMove(String),
}

impl fmt::Display for UsiParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UsiParseError::InvalidSquare(s) => write!(f, "Invalid square notation: {s}"),
            UsiParseError::InvalidPiece(c) => write!(f, "Invalid piece character: {c}"),
            UsiParseError::InvalidMoveFormat(s) => write!(f, "Invalid move format: {s}"),
            UsiParseError::InvalidSfen(s) => write!(f, "Invalid SFEN: {s}"),
            UsiParseError::InvalidRankCount(n) => {
                write!(f, "Invalid rank count: {n} (expected 9)")
            }
            UsiParseError::UnknownPieceChar(c) => write!(f, "Unknown piece character: {c}"),
            UsiParseError::InvalidHandsFormat(s) => write!(f, "Invalid hands format: {s}"),
            UsiParseError::InvalidMoveCount(s) => write!(f, "Invalid move count: {s}"),
            UsiParseError::InvalidSideToMove(s) => {
                write!(f, "Invalid side to move: {s} (expected 'b' or 'w')")
            }
        }
    }
}

impl std::error::Error for UsiParseError {}

/// Parse a USI square notation (e.g., "5e", "1a") to Square
pub fn parse_usi_square(s: &str) -> Result<Square, UsiParseError> {
    if s.len() != 2 {
        return Err(UsiParseError::InvalidSquare(s.to_string()));
    }

    let chars: Vec<char> = s.chars().collect();
    let file = chars[0];
    let rank = chars[1];

    // File: '1'-'9' (right to left in Shogi)
    let file_idx = match file {
        '1'..='9' => 8 - (file.to_digit(10).unwrap() as u8 - 1),
        _ => return Err(UsiParseError::InvalidSquare(s.to_string())),
    };

    // Rank: 'a'-'i' (top to bottom)
    let rank_idx = match rank {
        'a'..='i' => (rank as u32 - 'a' as u32) as u8,
        _ => return Err(UsiParseError::InvalidSquare(s.to_string())),
    };

    Ok(Square::new(file_idx, rank_idx))
}

/// Parse a USI piece character to PieceType
fn parse_usi_piece_type(c: char) -> Result<PieceType, UsiParseError> {
    match c.to_uppercase().next().unwrap() {
        'P' => Ok(PieceType::Pawn),
        'L' => Ok(PieceType::Lance),
        'N' => Ok(PieceType::Knight),
        'S' => Ok(PieceType::Silver),
        'G' => Ok(PieceType::Gold),
        'B' => Ok(PieceType::Bishop),
        'R' => Ok(PieceType::Rook),
        'K' => Ok(PieceType::King),
        _ => Err(UsiParseError::InvalidPiece(c)),
    }
}

/// Parse a USI move notation (e.g., "7g7f", "7g7f+", "P*5e")
pub fn parse_usi_move(s: &str) -> Result<Move, UsiParseError> {
    if s.len() < 4 {
        return Err(UsiParseError::InvalidMoveFormat(s.to_string()));
    }

    // Check for drop move (e.g., "P*5e")
    if s.contains('*') {
        let parts: Vec<&str> = s.split('*').collect();
        if parts.len() != 2 || parts[0].len() != 1 {
            return Err(UsiParseError::InvalidMoveFormat(s.to_string()));
        }

        let piece_type = parse_usi_piece_type(parts[0].chars().next().unwrap())?;
        let to = parse_usi_square(parts[1])?;

        return Ok(Move::drop(piece_type, to));
    }

    // Normal move or promotion
    let (move_str, promote) = if let Some(stripped) = s.strip_suffix('+') {
        (stripped, true)
    } else {
        (s, false)
    };

    if move_str.len() != 4 {
        return Err(UsiParseError::InvalidMoveFormat(s.to_string()));
    }

    let from = parse_usi_square(&move_str[0..2])?;
    let to = parse_usi_square(&move_str[2..4])?;

    Ok(Move::normal(from, to, promote))
}

/// Convert a Move to USI notation
pub fn move_to_usi(mv: &Move) -> String {
    if mv.is_drop() {
        let piece_char = match mv.drop_piece_type() {
            PieceType::Pawn => 'P',
            PieceType::Lance => 'L',
            PieceType::Knight => 'N',
            PieceType::Silver => 'S',
            PieceType::Gold => 'G',
            PieceType::Bishop => 'B',
            PieceType::Rook => 'R',
            _ => unreachable!("Invalid drop piece"),
        };
        format!("{}*{}", piece_char, mv.to())
    } else {
        let from = mv.from().unwrap();
        let to = mv.to();
        if mv.is_promote() {
            format!("{from}{to}+")
        } else {
            format!("{from}{to}")
        }
    }
}

/// Parse hands from SFEN format (e.g., "2P3l4n" or "-")
fn parse_hands(hands_str: &str) -> Result<[[u8; 7]; 2], UsiParseError> {
    let mut hands = [[0u8; 7]; 2];

    if hands_str == "-" {
        return Ok(hands);
    }

    let chars: Vec<char> = hands_str.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        // Parse count (default is 1)
        let mut count = 1u8;
        if i < chars.len() && chars[i].is_ascii_digit() {
            count = 0;
            while i < chars.len() && chars[i].is_ascii_digit() {
                count =
                    count.saturating_mul(10).saturating_add(chars[i].to_digit(10).unwrap() as u8);
                i += 1;
            }
        }

        if i >= chars.len() {
            return Err(UsiParseError::InvalidHandsFormat(hands_str.to_string()));
        }

        // Parse piece type
        let piece_char = chars[i];
        let color = if piece_char.is_uppercase() { 0 } else { 1 }; // 0=Black, 1=White
        let piece_type = parse_usi_piece_type(piece_char).map_err(|_| {
            UsiParseError::InvalidHandsFormat(format!("Unknown piece in hands: {piece_char}"))
        })?;

        let hand_idx = piece_type_to_hand_index(piece_type).map_err(|_| {
            UsiParseError::InvalidHandsFormat(format!(
                "Invalid piece type for hand: {piece_type:?}"
            ))
        })?;

        // Accumulate pieces and clip to maximum possible count
        let max_pieces = MAX_HAND_PIECES[hand_idx];
        hands[color][hand_idx] = (hands[color][hand_idx] + count).min(max_pieces);
        i += 1;
    }

    Ok(hands)
}

/// Parse a SFEN string to create a Position
pub fn parse_sfen(sfen: &str) -> Result<Position, UsiParseError> {
    let parts: Vec<&str> = sfen.split_whitespace().collect();
    if parts.len() < 4 {
        return Err(UsiParseError::InvalidSfen("Too few parts".to_string()));
    }

    let board_str = parts[0];
    let side_to_move = match parts[1] {
        "b" => Color::Black,
        "w" => Color::White,
        _ => return Err(UsiParseError::InvalidSideToMove(parts[1].to_string())),
    };
    let hands_str = parts[2];
    let move_count_str = parts[3];

    // Create empty position (important: starts from clean state)
    let mut pos = Position::empty();

    // Parse board
    let ranks: Vec<&str> = board_str.split('/').collect();
    if ranks.len() != 9 {
        return Err(UsiParseError::InvalidRankCount(ranks.len()));
    }

    // Parse from 9th rank (index 0) to 1st rank (index 8)
    for (rank_idx, rank_str) in ranks.iter().enumerate() {
        let mut file_idx = 0;
        let chars: Vec<char> = rank_str.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let c = chars[i];

            // Check if we've already filled the rank
            if file_idx >= 9 {
                return Err(UsiParseError::InvalidSfen(format!(
                    "Rank {} has too many characters: extra characters after 9 squares",
                    rank_idx + 1
                )));
            }

            if c.is_ascii_digit() {
                // Empty squares - only single digits are valid in SFEN
                let count = c.to_digit(10).unwrap() as u8;
                if count == 0 {
                    return Err(UsiParseError::InvalidSfen(format!(
                        "Rank {} has invalid empty square count: 0 is not allowed",
                        rank_idx + 1
                    )));
                }
                if file_idx + count > 9 {
                    return Err(UsiParseError::InvalidSfen(format!(
                        "Rank {} has too many squares: position {} + {} empty squares exceeds 9",
                        rank_idx + 1,
                        file_idx + 1,
                        count
                    )));
                }
                file_idx += count;
            } else if c == '+' {
                // Promoted piece
                i += 1;
                if i >= chars.len() {
                    return Err(UsiParseError::InvalidSfen(
                        "Incomplete promoted piece".to_string(),
                    ));
                }
                let piece_char = chars[i];
                let color = if piece_char.is_uppercase() {
                    Color::Black
                } else {
                    Color::White
                };
                let piece_type = parse_usi_piece_type(piece_char)?;
                let square = Square::new(file_idx, rank_idx as u8);

                // Create promoted piece
                let mut piece = crate::shogi::Piece::new(piece_type, color);
                piece.promoted = true;
                pos.board.put_piece(square, piece);

                file_idx += 1;
            } else {
                // Normal piece
                let color = if c.is_uppercase() {
                    Color::Black
                } else {
                    Color::White
                };
                let piece_type = parse_usi_piece_type(c)?;
                let square = Square::new(file_idx, rank_idx as u8);

                let piece = crate::shogi::Piece::new(piece_type, color);
                pos.board.put_piece(square, piece);

                file_idx += 1;
            }
            i += 1;
        }

        if file_idx != 9 {
            return Err(UsiParseError::InvalidSfen(format!(
                "Rank {} has wrong number of squares: expected 9, got {}",
                rank_idx + 1,
                file_idx
            )));
        }
    }

    // Parse hands
    pos.hands = parse_hands(hands_str)?;

    // Set side to move
    pos.side_to_move = side_to_move;

    // Parse and set move count (ply)
    let move_count: u16 = move_count_str
        .parse()
        .map_err(|_| UsiParseError::InvalidMoveCount(move_count_str.to_string()))?;
    if move_count == 0 {
        return Err(UsiParseError::InvalidMoveCount("Move count must be at least 1".to_string()));
    }
    // Convert from move number to ply (multiply by 2 and adjust for side)
    pos.ply = (move_count - 1) * 2 + if side_to_move == Color::White { 1 } else { 0 };

    Ok(pos)
}

/// Convert piece to USI character
fn piece_to_usi_char(piece: &crate::shogi::Piece) -> String {
    let piece_char = match piece.piece_type {
        PieceType::King => 'K',
        PieceType::Rook => 'R',
        PieceType::Bishop => 'B',
        PieceType::Gold => 'G',
        PieceType::Silver => 'S',
        PieceType::Knight => 'N',
        PieceType::Lance => 'L',
        PieceType::Pawn => 'P',
    };

    let piece_char = if piece.color == Color::White {
        piece_char.to_lowercase().next().unwrap()
    } else {
        piece_char
    };

    if piece.promoted {
        // NOTE: This outputs lowercase for white promoted pieces (e.g., "+p").
        // This is correct for SFEN notation, but some GUIs may expect uppercase
        // in USI "info pv" output. Currently not an issue since move_to_usi()
        // outputs uppercase, but needs attention if more output paths are added.
        format!("+{piece_char}")
    } else {
        piece_char.to_string()
    }
}

/// Convert a Position to SFEN notation
pub fn position_to_sfen(pos: &Position) -> String {
    let mut sfen_parts = Vec::new();

    // 1. Board
    let mut board_str = String::new();
    for rank in 0..9 {
        if rank > 0 {
            board_str.push('/');
        }

        let mut empty_count = 0;
        for file in 0..9 {
            let square = Square::new(file, rank);
            if let Some(piece) = pos.board.piece_on(square) {
                if empty_count > 0 {
                    board_str.push_str(&empty_count.to_string());
                    empty_count = 0;
                }
                board_str.push_str(&piece_to_usi_char(&piece));
            } else {
                empty_count += 1;
            }
        }

        if empty_count > 0 {
            board_str.push_str(&empty_count.to_string());
        }
    }
    sfen_parts.push(board_str);

    // 2. Side to move
    sfen_parts.push(
        if pos.side_to_move == Color::Black {
            "b"
        } else {
            "w"
        }
        .to_string(),
    );

    // 3. Hands
    let mut hands_str = String::new();

    // Process both Black and White pieces in order
    for color in [Color::Black, Color::White] {
        let color_idx = color as usize;
        // Order: RBGSNLP (using HAND_PIECE_TYPES constant)
        use crate::shogi::HAND_PIECE_TYPES;

        for (idx, &piece_type) in HAND_PIECE_TYPES.iter().enumerate() {
            let count = pos.hands[color_idx][idx];
            if count > 0 {
                if count > 1 {
                    hands_str.push_str(&count.to_string());
                }
                let ch = match piece_type {
                    PieceType::Rook => 'R',
                    PieceType::Bishop => 'B',
                    PieceType::Gold => 'G',
                    PieceType::Silver => 'S',
                    PieceType::Knight => 'N',
                    PieceType::Lance => 'L',
                    PieceType::Pawn => 'P',
                    _ => unreachable!(),
                };
                let ch = if color == Color::Black {
                    ch
                } else {
                    ch.to_lowercase().next().unwrap()
                };
                hands_str.push(ch);
            }
        }
    }

    // After processing all pieces, check if any were found
    if hands_str.is_empty() {
        sfen_parts.push("-".to_string());
    } else {
        sfen_parts.push(hands_str);
    }

    // 4. Move count
    let move_count = (pos.ply / 2) + 1;
    sfen_parts.push(move_count.to_string());

    sfen_parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_usi_square() {
        assert_eq!(parse_usi_square("5e").unwrap(), Square::new(4, 4));
        assert_eq!(parse_usi_square("1a").unwrap(), Square::new(8, 0));
        assert_eq!(parse_usi_square("9i").unwrap(), Square::new(0, 8));

        // Verify round-trip conversion
        let sq = parse_usi_square("7g").unwrap();
        assert_eq!(sq.to_string(), "7g");

        let sq = parse_usi_square("1a").unwrap();
        assert_eq!(sq.to_string(), "1a");

        let sq = parse_usi_square("9i").unwrap();
        assert_eq!(sq.to_string(), "9i");

        assert!(parse_usi_square("").is_err());
        assert!(parse_usi_square("5").is_err());
        assert!(parse_usi_square("5ee").is_err());
        assert!(parse_usi_square("0a").is_err());
        assert!(parse_usi_square("5j").is_err());
    }

    #[test]
    fn test_parse_usi_move() {
        // Normal moves
        let mv = parse_usi_move("7g7f").unwrap();
        assert_eq!(mv.from(), Some(Square::new(2, 6)));
        assert_eq!(mv.to(), Square::new(2, 5));
        assert!(!mv.is_promote());
        assert!(!mv.is_drop());

        // Promotion
        let mv = parse_usi_move("8h2b+").unwrap();
        assert_eq!(mv.from(), Some(Square::new(1, 7)));
        assert_eq!(mv.to(), Square::new(7, 1));
        assert!(mv.is_promote());

        // Drop
        let mv = parse_usi_move("P*5e").unwrap();
        assert_eq!(mv.to(), Square::new(4, 4));
        assert!(mv.is_drop());
        assert_eq!(mv.drop_piece_type(), PieceType::Pawn);
    }

    #[test]
    fn test_move_to_usi() {
        let mv = Move::normal(Square::new(2, 6), Square::new(2, 5), false);
        assert_eq!(move_to_usi(&mv), "7g7f");

        let mv = Move::normal(Square::new(1, 7), Square::new(7, 1), true);
        assert_eq!(move_to_usi(&mv), "8h2b+");

        let mv = Move::drop(PieceType::Pawn, Square::new(4, 4));
        assert_eq!(move_to_usi(&mv), "P*5e");
    }

    #[test]
    fn test_parse_hands() {
        // Empty hands
        let hands = parse_hands("-").unwrap();
        assert_eq!(hands, [[0; 7]; 2]);

        // Single piece
        let hands = parse_hands("P").unwrap();
        assert_eq!(hands[0][6], 1); // Black pawn

        // Multiple pieces
        let hands = parse_hands("2P3l").unwrap();
        assert_eq!(hands[0][6], 2); // 2 Black pawns
        assert_eq!(hands[1][5], 3); // 3 White lances

        // Complex hands
        let hands = parse_hands("RBG2S2N2L9P2b2s2n2l9p").unwrap();
        assert_eq!(hands[0][0], 1); // Black rook
        assert_eq!(hands[0][1], 1); // Black bishop
        assert_eq!(hands[0][2], 1); // Black gold
        assert_eq!(hands[0][3], 2); // Black silver
        assert_eq!(hands[0][6], 9); // Black pawns
        assert_eq!(hands[1][1], 2); // White bishops
        assert_eq!(hands[1][6], 9); // White pawns

        // Test accumulation of duplicate pieces
        let hands = parse_hands("2P3P").unwrap();
        assert_eq!(hands[0][6], 5); // 2 + 3 = 5 Black pawns

        // Test clipping to maximum
        let hands = parse_hands("10P10P").unwrap();
        assert_eq!(hands[0][6], 18); // Clipped to max 18 pawns

        let hands = parse_hands("3R2R").unwrap();
        assert_eq!(hands[0][0], 2); // Clipped to max 2 rooks
    }

    #[test]
    fn test_parse_sfen_startpos() {
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
        let pos = parse_sfen(sfen).unwrap();

        assert_eq!(pos.side_to_move, Color::Black);
        assert_eq!(pos.ply, 0);

        // Check some pieces
        assert!(pos.board.piece_on(Square::new(0, 0)).is_some());
        assert!(pos.board.piece_on(Square::new(4, 0)).is_some()); // King

        // Check empty hands
        assert_eq!(pos.hands, [[0; 7]; 2]);
    }

    #[test]
    fn test_parse_sfen_promoted() {
        let sfen = "lnsgkgsnl/1r5+B1/ppppppppp/9/9/9/PPPPPPPPP/1+b5R1/LNSGKGSNL w - 10";
        let pos = parse_sfen(sfen).unwrap();

        assert_eq!(pos.side_to_move, Color::White);
        assert_eq!(pos.ply, 19); // Move 10, white to move

        // Check promoted bishop on rank 2 (1r5+B1)
        // File 7, Rank 2 has the Black promoted bishop
        let piece = pos.board.piece_on(Square::new(7, 1)).unwrap();
        assert_eq!(piece.piece_type, PieceType::Bishop);
        assert_eq!(piece.color, Color::Black);
        assert!(piece.promoted);

        // Check promoted bishop on rank 8 (1+b5R1)
        // File 1, Rank 8 has the White promoted bishop
        let piece = pos.board.piece_on(Square::new(1, 7)).unwrap();
        assert_eq!(piece.piece_type, PieceType::Bishop);
        assert_eq!(piece.color, Color::White);
        assert!(piece.promoted);
    }

    #[test]
    fn test_parse_sfen_with_hands() {
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b 2P 5";
        let pos = parse_sfen(sfen).unwrap();

        assert_eq!(pos.hands[0][6], 2); // Black has 2 pawns
        assert_eq!(pos.ply, 8); // Move 5, black to move
    }

    #[test]
    fn test_parse_sfen_handicap() {
        // 6-piece handicap
        let sfen = "lnsgkgsnl/9/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1";
        let pos = parse_sfen(sfen).unwrap();

        assert_eq!(pos.side_to_move, Color::White);
        // Check that rook and bishop are missing
        assert!(pos.board.piece_on(Square::new(1, 1)).is_none());
        assert!(pos.board.piece_on(Square::new(7, 1)).is_none());
    }

    #[test]
    fn test_parse_sfen_errors() {
        // Too few parts
        assert!(parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL").is_err());

        // Wrong number of ranks
        assert!(parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1 b - 1").is_err());

        // Invalid side to move
        assert!(
            parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL x - 1").is_err()
        );

        // Invalid move count
        assert!(
            parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 0").is_err()
        );
        assert!(parse_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - abc")
            .is_err());

        // Invalid piece
        assert!(
            parse_sfen("Xnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1").is_err()
        );

        // Invalid empty square count 0
        let err = parse_sfen("0/9/9/9/9/9/9/9/9 b - 1").unwrap_err();
        if let UsiParseError::InvalidSfen(msg) = &err {
            assert!(msg.contains("Rank 1"), "Error message should mention Rank 1, got: {msg}");
            assert!(
                msg.contains("0 is not allowed"),
                "Error message should mention 0 not allowed, got: {msg}"
            );
        } else {
            panic!("Expected InvalidSfen error, got: {err:?}");
        }

        // File index overflow from too many pieces - 10 pawns
        let err = parse_sfen("pppppppppp/9/9/9/9/9/9/9/9 b - 1").unwrap_err();
        if let UsiParseError::InvalidSfen(msg) = &err {
            assert!(msg.contains("Rank 1"), "Error message should mention Rank 1, got: {msg}");
            assert!(
                msg.contains("too many characters"),
                "Error message should mention too many characters, got: {msg}"
            );
        } else {
            panic!("Expected InvalidSfen error, got: {err:?}");
        }

        // Test the new overflow check for large empty squares
        let err = parse_sfen("5p5/9/9/9/9/9/9/9/9 b - 1").unwrap_err();
        if let UsiParseError::InvalidSfen(msg) = &err {
            assert!(msg.contains("Rank 1"), "Error message should mention Rank 1, got: {msg}");
            assert!(
                msg.contains("7 + 5 empty squares exceeds 9"),
                "Error message should mention specific counts, got: {msg}"
            );
        } else {
            panic!("Expected InvalidSfen error, got: {err:?}");
        }

        // Too few squares in rank
        let err = parse_sfen("8/9/9/9/9/9/9/9/9 b - 1").unwrap_err();
        if let UsiParseError::InvalidSfen(msg) = &err {
            assert!(msg.contains("Rank 1"), "Error message should mention Rank 1, got: {msg}");
            assert!(
                msg.contains("expected 9, got 8"),
                "Error message should mention expected vs actual, got: {msg}"
            );
        } else {
            panic!("Expected InvalidSfen error, got: {err:?}");
        }
    }

    #[test]
    fn test_position_to_sfen_roundtrip() {
        let test_sfens = vec![
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w 2P 5",
            "lnsgkgsnl/1r5+B1/ppppppppp/9/9/9/PPPPPPPPP/1+b5R1/LNSGKGSNL b - 10",
            "lnsgkgsnl/9/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1", // Handicap
        ];

        for sfen in test_sfens {
            let pos = parse_sfen(sfen).unwrap();
            let generated = position_to_sfen(&pos);
            assert_eq!(sfen, generated, "Round-trip failed for: {sfen}");
        }
    }

    #[test]
    fn test_parse_sfen_numeric_compression() {
        // Test with 7th and 8th ranks having numbers
        let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/7PP/1B5R1/LNSGKGSNL b - 1";
        let pos = parse_sfen(sfen).unwrap();

        // Check that pawns are in correct positions
        let piece = pos.board.piece_on(Square::new(7, 6)).unwrap();
        assert_eq!(piece.piece_type, PieceType::Pawn);
        assert_eq!(piece.color, Color::Black);

        let piece = pos.board.piece_on(Square::new(8, 6)).unwrap();
        assert_eq!(piece.piece_type, PieceType::Pawn);
        assert_eq!(piece.color, Color::Black);
    }
}
