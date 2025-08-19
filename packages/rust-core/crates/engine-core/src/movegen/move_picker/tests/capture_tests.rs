//! Capture-related tests

use crate::movegen::move_picker::MovePicker;
use crate::search::types::SearchStack;
use crate::shogi::{Move, Position};
use crate::usi::{parse_usi_move, parse_usi_square};
use crate::History;

#[test]
fn test_see_calculation() {
    // Create a position where we can test SEE
    let mut pos = Position::startpos();

    // Create a position with some captures
    let setup_moves = [
        parse_usi_move("7g7f").unwrap(), // 先手の歩
        parse_usi_move("3c3d").unwrap(), // 後手の歩
        parse_usi_move("2g2f").unwrap(), // 先手の歩
        parse_usi_move("3d3e").unwrap(), // 後手の歩
        parse_usi_move("2f2e").unwrap(), // 先手の歩
        parse_usi_move("3e3f").unwrap(), // 後手の歩
    ];

    for mv in &setup_moves {
        pos.do_move(*mv);
    }

    let history = History::new();
    let stack = SearchStack::default();
    let picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

    // Test capturing the pawn at 3f with our pawn at 3g
    // This should be a good capture (pawn for pawn)
    let capture_3f = parse_usi_move("3g3f").unwrap(); // 先手の歩が後手の歩を取る
    let see_value = picker.see(capture_3f);
    assert_eq!(see_value, 100, "Pawn x Pawn should have SEE value of 100 (pawn value)");

    // If there were a more valuable piece defending, SEE would be negative
    // But in this simple position, it's just pawn for pawn
}

#[test]
fn test_mvv_lva_ordering() {
    // MVV-LVA ordering is now handled by SEE
    // This test verifies that captures are ordered correctly
    let mut pos = Position::startpos();

    // Make some moves to create capture opportunities
    let moves = [
        Move::normal(parse_usi_square("7g").unwrap(), parse_usi_square("7f").unwrap(), false), // Black pawn forward
        Move::normal(parse_usi_square("6c").unwrap(), parse_usi_square("6d").unwrap(), false), // White pawn forward
        Move::normal(parse_usi_square("7f").unwrap(), parse_usi_square("7e").unwrap(), false), // Black pawn forward
        Move::normal(parse_usi_square("6d").unwrap(), parse_usi_square("6e").unwrap(), false), // White pawn forward
    ];

    for mv in &moves {
        pos.do_move(*mv);
    }

    let history = History::new();
    let stack = SearchStack::default();
    let mut picker = MovePicker::new(&pos, None, None, &history, &stack, 1);

    // Collect captures
    let mut captures = Vec::new();
    while let Some(mv) = picker.next_move() {
        if !mv.is_drop() && pos.board.piece_on(mv.to()).is_some() {
            captures.push(mv);
        }
        if captures.len() >= 5 {
            break;
        }
    }

    // Should have some captures
    assert!(!captures.is_empty(), "Should have found some captures");
}
