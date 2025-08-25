//! Test to diagnose MoveGen hang issue

use engine_core::{movegen::MoveGen, shogi::MoveList, usi::position_to_sfen, Position};

#[test]
fn test_movegen_startpos() {
    println!("Creating initial position...");
    let position = Position::startpos();
    println!("Position created: SFEN = {}", position_to_sfen(&position));

    println!("Creating MoveGen...");
    let mut movegen = MoveGen::new();
    println!("MoveGen created");

    println!("Creating MoveList...");
    let mut moves = MoveList::new();
    println!("MoveList created");

    println!("Calling generate_all...");
    println!("Position side_to_move: {:?}", position.side_to_move);
    println!("Position ply: {}", position.ply);

    // This is where the hang occurs
    movegen.generate_all(&position, &mut moves);

    println!("generate_all completed!");
    println!("Number of legal moves: {}", moves.len());

    // In startpos, there should be 30 legal moves
    assert_eq!(moves.len(), 30);
}

#[test]
fn test_movegen_simple_position() {
    println!("Testing with a simple position...");

    // Test with a position that has fewer pieces
    let sfen = "9/9/9/9/9/9/9/9/k1K6 b - 1"; // Only kings
    let position = Position::from_sfen(sfen).expect("Valid SFEN");

    let mut movegen = MoveGen::new();
    let mut moves = MoveList::new();

    println!("Generating moves for simple position...");
    movegen.generate_all(&position, &mut moves);

    println!("Moves generated: {}", moves.len());
    assert!(moves.len() > 0);
}
