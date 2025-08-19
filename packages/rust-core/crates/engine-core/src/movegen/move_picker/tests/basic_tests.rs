//! Basic move picker tests

use crate::movegen::move_picker::MovePicker;
use crate::search::types::SearchStack;
use crate::shogi::{Move, Position};
use crate::usi::parse_usi_square;
use crate::{Color, History, Piece, PieceType};

#[test]
fn test_move_picker_stages() {
    let pos = Position::startpos();
    let history = History::new();
    let stack = SearchStack::default();

    // Use a known legal move from starting position
    // Black pawn at rank 6 moves toward rank 0
    let tt_move = Some(Move::normal(
        parse_usi_square("2g").unwrap(), // Black pawn
        parse_usi_square("2f").unwrap(), // One square forward
        false,
    ));

    let mut picker = MovePicker::new(&pos, tt_move, None, &history, &stack, 1);

    // First move should be TT move
    let first_move = picker.next_move();
    assert_eq!(first_move, tt_move);

    // Subsequent moves should not include TT move
    let mut moves = Vec::new();
    while let Some(mv) = picker.next_move() {
        assert_ne!(Some(mv), tt_move);
        moves.push(mv);
    }

    // Should have generated all legal moves except TT move
    assert!(!moves.is_empty());
}

#[test]
fn test_quiescence_picker() {
    // Create a position with no possible captures
    let mut pos = Position::empty();

    // Add kings (required)
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Add some pieces that cannot capture each other
    pos.board
        .put_piece(parse_usi_square("7g").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(parse_usi_square("3c").unwrap(), Piece::new(PieceType::Pawn, Color::White));

    pos.board.rebuild_occupancy_bitboards();

    let history = History::new();
    let stack = SearchStack::default();

    let mut picker = MovePicker::new_quiescence(&pos, None, &history, &stack, 1);

    // In this position, there should be no captures
    assert!(
        picker.next_move().is_none(),
        "Quiescence search should return no moves when no captures are available"
    );
}

#[test]
fn test_pv_move_no_duplication() {
    // Test that PV move is not returned multiple times
    let pos = Position::startpos();
    let history = History::new();
    let stack = SearchStack::default();

    // Create a legal move as PV move
    let pv_move = Some(Move::normal(
        parse_usi_square("7g").unwrap(),
        parse_usi_square("7f").unwrap(),
        false,
    ));

    // Create move picker with PV move at root (ply 0)
    let mut picker = MovePicker::new(&pos, None, pv_move, &history, &stack, 0);

    // Collect all moves and check for duplicates
    let mut moves = Vec::new();
    let mut pv_count = 0;
    while let Some(mv) = picker.next_move() {
        if Some(mv) == pv_move {
            pv_count += 1;
        }
        moves.push(mv);
    }

    // PV move should appear exactly once
    assert_eq!(pv_count, 1, "PV move should be returned exactly once");

    // Check that we got a reasonable number of moves (should be all legal moves)
    assert!(moves.len() > 20, "Should generate many moves from starting position");
    assert!(moves.len() < 40, "Should not have duplicate moves");
}

#[test]
fn test_killer_moves() {
    let pos = Position::startpos();
    let history = History::new();
    let mut stack = SearchStack::default();

    // Set killer moves (using legal moves from starting position)
    // Black pawns are at rank 6, move toward rank 0
    // 2g2f: file 7, rank 6 -> rank 5
    let killer1 =
        Move::normal(parse_usi_square("2g").unwrap(), parse_usi_square("2f").unwrap(), false);
    // 7g7f: file 2, rank 6 -> rank 5
    let killer2 =
        Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false);
    stack.killers[0] = Some(killer1);
    stack.killers[1] = Some(killer2);

    let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

    // Collect moves and track when killers appear
    let mut move_count = 0;
    let mut killer_positions = vec![];
    while let Some(mv) = picker.next_move() {
        if mv == killer1 || mv == killer2 {
            killer_positions.push(move_count);
        }
        move_count += 1;
    }

    // Killers should be generated
    assert!(!killer_positions.is_empty(), "Killer moves should be generated");

    // Killers should appear relatively early (after captures)
    for &pos in &killer_positions {
        assert!(pos < 10, "Killer move at position {pos} is too late");
    }
}
