//! Test for king capture issue

use crate::ai::board::{Color, Piece, PieceType, Position, Square};
use crate::ai::movegen::MoveGen;

#[test]
fn test_no_king_capture() {
    let mut pos = Position::empty();

    // Setup a position where silver can capture king
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 1), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(Square::new(3, 2), Piece::new(PieceType::King, Color::White));

    let mut gen = MoveGen::new(&pos);
    let moves = gen.generate_all();

    // Check that no move captures the king
    for m in moves.as_slice() {
        if !m.is_drop() {
            if let Some(from) = m.from() {
                let to = m.to();
                if from == Square::new(4, 1) && to == Square::new(3, 2) {
                    panic!("Generated illegal move: silver captures king!");
                }
            }
        }
    }

    println!("OK: No king capture moves generated");
}
