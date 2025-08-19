//! Drop-related tests

use crate::shogi::{Board, Color, Move, Piece, PieceType, Position};
use crate::usi::parse_usi_square;

#[test]
fn test_pawn_drop_restrictions() {
    // Test nifu (double pawn) restriction
    // Start with empty position to have full control
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // Put a black pawn on file 5 (index 4)
    let sq = parse_usi_square("5f").unwrap(); // 5f
    pos.board.put_piece(
        sq,
        Piece {
            piece_type: PieceType::Pawn,
            color: Color::Black,
            promoted: false,
        },
    );

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // Pawn is index 6

    // Try to drop a pawn in the same file
    let illegal_drop = Move::drop(PieceType::Pawn, parse_usi_square("5d").unwrap()); // 5d
    assert!(!pos.is_legal_move(illegal_drop), "Should not allow double pawn");

    // Try to drop a pawn in a different file (that has no pawn)
    let legal_drop = Move::drop(PieceType::Pawn, parse_usi_square("6d").unwrap()); // 6d
    assert!(pos.is_legal_move(legal_drop), "Should allow pawn drop in different file");
}

#[test]
fn test_nifu_with_promoted_pawn() {
    // Test that promoted pawn doesn't count for nifu (double pawn)
    let mut pos = Position::empty();
    pos.board = Board::empty();
    pos.side_to_move = Color::Black;

    // Place a promoted black pawn on file 5 (index 4)
    let sq = parse_usi_square("5f").unwrap(); // 5f
    pos.board.put_piece(
        sq,
        Piece {
            piece_type: PieceType::Pawn,
            color: Color::Black,
            promoted: true,
        },
    );
    pos.board.promoted_bb.set(sq); // Mark as promoted

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop a pawn in the same file - should be legal because existing pawn is promoted
    let legal_drop = Move::drop(PieceType::Pawn, parse_usi_square("5d").unwrap()); // 5d
    assert!(
        pos.is_legal_move(legal_drop),
        "Should allow pawn drop when only promoted pawn exists in file"
    );
}

#[test]
fn test_pawn_drop_last_rank_restrictions() {
    // Test that pawns cannot be dropped on the last rank
    let mut pos = Position::empty();
    pos.board = Board::empty();

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Test Black pawn drop on rank 0 (last rank for Black)
    pos.side_to_move = Color::Black;
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
    pos.board.rebuild_occupancy_bitboards();

    let illegal_drop = Move::drop(PieceType::Pawn, parse_usi_square("5a").unwrap()); // 5a
    assert!(
        !pos.is_legal_move(illegal_drop),
        "Black should not be able to drop pawn on rank 0"
    );

    // Test White pawn drop on rank 8 (last rank for White)
    pos.side_to_move = Color::White;
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 0; // Remove Black's pawn
    pos.hands[Color::White as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    let illegal_drop = Move::drop(PieceType::Pawn, parse_usi_square("5i").unwrap()); // 5i
    assert!(
        !pos.is_legal_move(illegal_drop),
        "White should not be able to drop pawn on rank 8"
    );
}

#[test]
fn test_lance_drop_last_rank_restrictions() {
    // Test that lances cannot be dropped on the last rank
    let mut pos = Position::empty();
    pos.board = Board::empty();

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Test Black lance drop on rank 0 (last rank for Black)
    pos.side_to_move = Color::Black;
    pos.hands[Color::Black as usize][PieceType::Lance.hand_index().unwrap()] = 1; // Lance is index 5
    pos.board.rebuild_occupancy_bitboards();

    let illegal_drop = Move::drop(PieceType::Lance, parse_usi_square("5a").unwrap()); // 5a
    assert!(
        !pos.is_legal_move(illegal_drop),
        "Black should not be able to drop lance on rank 0"
    );

    // Test White lance drop on rank 8 (last rank for White)
    pos.side_to_move = Color::White;
    pos.hands[Color::Black as usize][PieceType::Lance.hand_index().unwrap()] = 0; // Remove Black's lance
    pos.hands[Color::White as usize][PieceType::Lance.hand_index().unwrap()] = 1;

    let illegal_drop = Move::drop(PieceType::Lance, parse_usi_square("5i").unwrap()); // 5i
    assert!(
        !pos.is_legal_move(illegal_drop),
        "White should not be able to drop lance on rank 8"
    );
}

#[test]
fn test_knight_drop_last_two_ranks_restrictions() {
    // Test that knights cannot be dropped on the last two ranks
    let mut pos = Position::empty();
    pos.board = Board::empty();

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Test Black knight drop
    pos.side_to_move = Color::Black;
    pos.hands[Color::Black as usize][PieceType::Knight.hand_index().unwrap()] = 1; // Knight is index 4
    pos.board.rebuild_occupancy_bitboards();

    // Cannot drop on rank 0 or 1
    let illegal_drop1 = Move::drop(PieceType::Knight, parse_usi_square("5a").unwrap()); // 5a
    assert!(
        !pos.is_legal_move(illegal_drop1),
        "Black should not be able to drop knight on rank 0"
    );

    let illegal_drop2 = Move::drop(PieceType::Knight, parse_usi_square("5b").unwrap()); // 5b
    assert!(
        !pos.is_legal_move(illegal_drop2),
        "Black should not be able to drop knight on rank 1"
    );

    // Can drop on rank 2
    let legal_drop = Move::drop(PieceType::Knight, parse_usi_square("5c").unwrap()); // 5c
    assert!(pos.is_legal_move(legal_drop), "Black should be able to drop knight on rank 2");

    // Test White knight drop
    pos.side_to_move = Color::White;
    pos.hands[Color::Black as usize][PieceType::Knight.hand_index().unwrap()] = 0; // Remove Black's knight
    pos.hands[Color::White as usize][PieceType::Knight.hand_index().unwrap()] = 1;

    // Cannot drop on rank 8 or 7
    let illegal_drop1 = Move::drop(PieceType::Knight, parse_usi_square("5i").unwrap()); // 5i
    assert!(
        !pos.is_legal_move(illegal_drop1),
        "White should not be able to drop knight on rank 8"
    );

    let illegal_drop2 = Move::drop(PieceType::Knight, parse_usi_square("5h").unwrap()); // 5h
    assert!(
        !pos.is_legal_move(illegal_drop2),
        "White should not be able to drop knight on rank 7"
    );

    // Can drop on rank 6
    let legal_drop = Move::drop(PieceType::Knight, parse_usi_square("5g").unwrap()); // 5g
    assert!(pos.is_legal_move(legal_drop), "White should be able to drop knight on rank 6");
}
