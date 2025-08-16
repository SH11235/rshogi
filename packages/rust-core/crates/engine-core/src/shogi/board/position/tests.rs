//! Tests for Position functionality

use crate::shogi::board::{Color, PieceType, Position};
use crate::usi::parse_usi_square;

#[test]
fn test_startpos() {
    let pos = Position::startpos();

    // Check king positions
    assert_eq!(pos.board.king_square(Color::Black), Some(parse_usi_square("5i").unwrap()));
    assert_eq!(pos.board.king_square(Color::White), Some(parse_usi_square("5a").unwrap()));

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
fn test_count_piece_on_board() {
    // Test with starting position
    let pos = Position::startpos();

    // Check piece counts
    assert_eq!(pos.count_piece_on_board(PieceType::King), 2);
    assert_eq!(pos.count_piece_on_board(PieceType::Rook), 2);
    assert_eq!(pos.count_piece_on_board(PieceType::Bishop), 2);
    assert_eq!(pos.count_piece_on_board(PieceType::Gold), 4);
    assert_eq!(pos.count_piece_on_board(PieceType::Silver), 4);
    assert_eq!(pos.count_piece_on_board(PieceType::Knight), 4);
    assert_eq!(pos.count_piece_on_board(PieceType::Lance), 4);
    assert_eq!(pos.count_piece_on_board(PieceType::Pawn), 18);

    // Test with empty position
    let empty_pos = Position::empty();
    assert_eq!(empty_pos.count_piece_on_board(PieceType::Rook), 0);
    assert_eq!(empty_pos.count_piece_on_board(PieceType::Pawn), 0);
}

#[test]
fn test_count_piece_in_hand() {
    let mut pos = Position::empty();

    // Add some pieces to hands
    pos.hands[Color::Black as usize][PieceType::Rook.hand_index().unwrap()] = 1; // Rook
    pos.hands[Color::Black as usize][PieceType::Bishop.hand_index().unwrap()] = 2; // Bishop
    pos.hands[Color::White as usize][PieceType::Pawn.hand_index().unwrap()] = 5; // Pawn

    // Test counts
    assert_eq!(pos.count_piece_in_hand(Color::Black, PieceType::Rook), 1);
    assert_eq!(pos.count_piece_in_hand(Color::Black, PieceType::Bishop), 2);
    assert_eq!(pos.count_piece_in_hand(Color::Black, PieceType::Pawn), 0);
    assert_eq!(pos.count_piece_in_hand(Color::White, PieceType::Pawn), 5);

    // King should always return 0
    assert_eq!(pos.count_piece_in_hand(Color::Black, PieceType::King), 0);
    assert_eq!(pos.count_piece_in_hand(Color::White, PieceType::King), 0);
}
