//! SEE profiling binary for flamegraph analysis
//!
//! This binary is designed to run SEE calculations intensively for 3-5 seconds
//! to generate meaningful flamegraph data.

use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;
use shogi_core::ai::movegen::MoveGen;
use shogi_core::ai::moves::MoveList;
use shogi_core::{Color, Move, Piece, PieceType, Position, Square};
use std::hint::black_box;
use std::time::{Duration, Instant};

/// Create various test positions for profiling
fn create_random_positions(count: usize) -> Vec<(Position, Vec<Move>)> {
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1234_5678_9ABC_DEF0);
    let mut positions = Vec::new();

    // Create base position templates
    let templates = vec![
        create_simple_exchange_position(),
        create_complex_exchange_position(),
        create_xray_position(),
        create_pinned_position(),
        create_deep_exchange_position(),
    ];

    // Generate variations of each template
    for _ in 0..count {
        let base_pos = &templates[rng.random_range(0..templates.len())];
        let mut pos = base_pos.clone();

        // Add some random pieces to create variations
        for _ in 0..rng.random_range(0..3) {
            let sq = Square::new(rng.random_range(0..9) as u8, rng.random_range(0..9) as u8);
            if pos.board.piece_on(sq).is_none() {
                let piece_type = match rng.random_range(0..5) {
                    0 => PieceType::Pawn,
                    1 => PieceType::Lance,
                    2 => PieceType::Knight,
                    3 => PieceType::Silver,
                    _ => PieceType::Gold,
                };
                let color = if rng.random_bool(0.5) {
                    Color::Black
                } else {
                    Color::White
                };
                pos.board.put_piece(sq, Piece::new(piece_type, color));
            }
        }

        pos.board.rebuild_occupancy_bitboards();

        // Generate captures for this position
        let mut moves = MoveList::new();
        let mut gen = MoveGen::new();
        gen.generate_captures(&pos, &mut moves);

        if !moves.is_empty() {
            let captures = moves.as_slice().to_vec();
            positions.push((pos, captures));
        }
    }

    positions
}

fn create_simple_exchange_position() -> Position {
    let mut pos = Position::empty();
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(Square::new(4, 4), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(Square::new(4, 3), Piece::new(PieceType::Pawn, Color::White));
    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn create_complex_exchange_position() -> Position {
    let mut pos = Position::empty();
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(Square::new(4, 4), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(Square::new(3, 3), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(Square::new(5, 5), Piece::new(PieceType::Bishop, Color::Black));
    pos.board
        .put_piece(Square::new(4, 6), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(Square::new(4, 2), Piece::new(PieceType::Rook, Color::Black));
    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn create_xray_position() -> Position {
    let mut pos = Position::empty();
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(Square::new(4, 7), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(Square::new(4, 5), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(Square::new(4, 3), Piece::new(PieceType::Pawn, Color::White));
    pos.board
        .put_piece(Square::new(4, 1), Piece::new(PieceType::Rook, Color::White));
    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn create_pinned_position() -> Position {
    let mut pos = Position::empty();
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(Square::new(4, 4), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(Square::new(4, 7), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(Square::new(5, 4), Piece::new(PieceType::Pawn, Color::White));
    pos.board
        .put_piece(Square::new(3, 3), Piece::new(PieceType::Silver, Color::Black));
    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn create_deep_exchange_position() -> Position {
    let mut pos = Position::empty();
    pos.board
        .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(8, 8), Piece::new(PieceType::King, Color::White));

    // Central target
    pos.board
        .put_piece(Square::new(4, 4), Piece::new(PieceType::Gold, Color::White));

    // Multiple attackers and defenders
    pos.board
        .put_piece(Square::new(3, 3), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(Square::new(2, 2), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(Square::new(5, 5), Piece::new(PieceType::Pawn, Color::White));
    pos.board
        .put_piece(Square::new(6, 6), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(Square::new(4, 1), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(Square::new(4, 7), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(Square::new(1, 1), Piece::new(PieceType::Bishop, Color::Black));
    pos.board
        .put_piece(Square::new(7, 7), Piece::new(PieceType::Bishop, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn main() {
    println!("Generating test positions...");
    let test_positions = create_random_positions(100);

    println!("Starting SEE profiling...");
    println!("Target duration: 3-5 seconds");

    let start = Instant::now();
    let target_duration = Duration::from_secs(4);
    let mut iterations = 0u64;
    let mut see_calculations = 0u64;

    // Run SEE calculations until we reach the target duration
    while start.elapsed() < target_duration {
        for (pos, captures) in test_positions.iter() {
            for capture in captures.iter() {
                // Use black_box to prevent optimization
                let _result = black_box(pos.see(black_box(*capture)));
                see_calculations += 1;
            }
        }
        iterations += 1;
    }

    let elapsed = start.elapsed();
    let see_per_sec = see_calculations as f64 / elapsed.as_secs_f64();

    println!("\nProfiling complete!");
    println!("Duration: {:.2} seconds", elapsed.as_secs_f64());
    println!("Iterations: {iterations}");
    println!("Total SEE calculations: {see_calculations}");
    println!("SEE calculations/second: {see_per_sec:.0}");
    println!("\nFlamegraph data has been collected.");
}
