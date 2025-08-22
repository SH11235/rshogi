//! Performance benchmarks for board operations
//!
//! These tests measure the performance of critical board operations
//! to ensure they meet performance requirements for game engines.

use crate::shogi::board::{Color, Piece, PieceType};
use crate::shogi::moves::Move;
use crate::shogi::position::Position;
use crate::usi::parse_usi_square;
use std::time::Instant;

/// Benchmark attack detection performance
#[cfg(test)]
mod attack_performance {
    use super::*;

    #[test]
    fn benchmark_is_attacked_performance() {
        // Create a complex position with many pieces
        let pos = create_complex_position();

        let iterations = 10000;
        let target_squares = [
            parse_usi_square("5e").unwrap(),
            parse_usi_square("4d").unwrap(),
            parse_usi_square("6f").unwrap(),
            parse_usi_square("3g").unwrap(),
            parse_usi_square("7c").unwrap(),
        ];

        let start = Instant::now();

        for _ in 0..iterations {
            for &square in &target_squares {
                for color in [Color::Black, Color::White] {
                    let _ = pos.is_attacked(square, color);
                }
            }
        }

        let elapsed = start.elapsed();
        let ns_per_call = elapsed.as_nanos() / (iterations * target_squares.len() * 2) as u128;

        println!("is_attacked performance:");
        println!("  Time per call: {ns_per_call} ns");
        println!("  Calls per second: {}", 1_000_000_000 / ns_per_call);

        // Performance requirement: should be under 200ns per call in debug mode
        #[cfg(debug_assertions)]
        let max_ns = 200;
        #[cfg(not(debug_assertions))]
        let max_ns = 50;

        assert!(
            ns_per_call < max_ns,
            "is_attacked is too slow: {ns_per_call} ns (max: {max_ns} ns)"
        );
    }

    #[test]
    fn benchmark_get_attackers_to_performance() {
        let pos = create_complex_position();

        let iterations = 5000;
        let target_squares = [
            parse_usi_square("5e").unwrap(),
            parse_usi_square("4d").unwrap(),
            parse_usi_square("6f").unwrap(),
        ];

        let start = Instant::now();

        for _ in 0..iterations {
            for &square in &target_squares {
                for color in [Color::Black, Color::White] {
                    let _ = pos.get_attackers_to(square, color);
                }
            }
        }

        let elapsed = start.elapsed();
        let ns_per_call = elapsed.as_nanos() / (iterations * target_squares.len() * 2) as u128;

        println!("get_attackers_to performance:");
        println!("  Time per call: {ns_per_call} ns");
        println!("  Calls per second: {}", 1_000_000_000 / ns_per_call);

        #[cfg(debug_assertions)]
        let max_ns = 500;
        #[cfg(not(debug_assertions))]
        let max_ns = 100;

        assert!(
            ns_per_call < max_ns,
            "get_attackers_to is too slow: {ns_per_call} ns (max: {max_ns} ns)"
        );
    }
}

/// Benchmark SEE (Static Exchange Evaluation) performance
#[cfg(test)]
mod see_performance {
    use super::*;

    #[test]
    fn benchmark_see_calculation_performance() {
        let pos = create_tactical_position();

        let iterations = 1000;
        let test_moves = create_test_capture_moves(&pos);

        let start = Instant::now();

        for _ in 0..iterations {
            for &mv in &test_moves {
                let _ = pos.see(mv);
            }
        }

        let elapsed = start.elapsed();
        let ns_per_call = elapsed.as_nanos() / (iterations * test_moves.len()) as u128;

        println!("SEE calculation performance:");
        println!("  Time per call: {ns_per_call} ns");
        println!("  Calls per second: {}", 1_000_000_000 / ns_per_call);

        #[cfg(debug_assertions)]
        let max_ns = 2000; // SEE is more complex
        #[cfg(not(debug_assertions))]
        let max_ns = 500;

        assert!(
            ns_per_call < max_ns,
            "SEE calculation is too slow: {ns_per_call} ns (max: {max_ns} ns)"
        );
    }

    #[test]
    fn benchmark_see_ge_performance() {
        let pos = create_tactical_position();

        let iterations = 2000;
        let test_moves = create_test_capture_moves(&pos);
        let thresholds = [0, 100, 200, 500];

        let start = Instant::now();

        for _ in 0..iterations {
            for &mv in &test_moves {
                for &threshold in &thresholds {
                    let _ = pos.see_ge(mv, threshold);
                }
            }
        }

        let elapsed = start.elapsed();
        let ns_per_call =
            elapsed.as_nanos() / (iterations * test_moves.len() * thresholds.len()) as u128;

        println!("SEE >= threshold performance:");
        println!("  Time per call: {ns_per_call} ns");
        println!("  Calls per second: {}", 1_000_000_000 / ns_per_call);

        #[cfg(debug_assertions)]
        let max_ns = 1500;
        #[cfg(not(debug_assertions))]
        let max_ns = 300;

        assert!(
            ns_per_call < max_ns,
            "SEE >= calculation is too slow: {ns_per_call} ns (max: {max_ns} ns)"
        );
    }
}

/// Benchmark move execution and validation performance
#[cfg(test)]
mod move_performance {
    use super::*;

    #[test]
    fn benchmark_make_move_performance() {
        let pos = create_complex_position();
        let test_moves = create_test_normal_moves(&pos);

        let iterations = 1000;

        let start = Instant::now();

        for _ in 0..iterations {
            for &mv in &test_moves {
                let mut pos_copy = pos.clone();
                pos_copy.do_move(mv);
            }
        }

        let elapsed = start.elapsed();
        let ns_per_call = elapsed.as_nanos() / (iterations * test_moves.len()) as u128;

        println!("make_move performance:");
        println!("  Time per call: {ns_per_call} ns");
        println!("  Calls per second: {}", 1_000_000_000 / ns_per_call);

        #[cfg(debug_assertions)]
        let max_ns = 3000;
        #[cfg(not(debug_assertions))]
        let max_ns = 1000;

        assert!(
            ns_per_call < max_ns,
            "make_move is too slow: {ns_per_call} ns (max: {max_ns} ns)"
        );
    }

    #[test]
    fn benchmark_is_legal_move_performance() {
        let pos = create_complex_position();
        let test_moves = create_test_moves_mixed(&pos);

        let iterations = 2000;

        let start = Instant::now();

        for _ in 0..iterations {
            for &mv in &test_moves {
                let _ = pos.is_legal_move(mv);
            }
        }

        let elapsed = start.elapsed();
        let ns_per_call = elapsed.as_nanos() / (iterations * test_moves.len()) as u128;

        println!("is_legal_move performance:");
        println!("  Time per call: {ns_per_call} ns");
        println!("  Calls per second: {}", 1_000_000_000 / ns_per_call);

        #[cfg(debug_assertions)]
        let max_ns = 1000;
        #[cfg(not(debug_assertions))]
        let max_ns = 200;

        assert!(
            ns_per_call < max_ns,
            "is_legal_move is too slow: {ns_per_call} ns (max: {max_ns} ns)"
        );
    }
}

/// Helper functions to create test positions and moves
fn create_complex_position() -> Position {
    let mut pos = Position::empty();

    // Add kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Add multiple pieces of each type for complex interactions
    let black_pieces = [
        (parse_usi_square("1i").unwrap(), PieceType::Lance),
        (parse_usi_square("9i").unwrap(), PieceType::Lance),
        (parse_usi_square("2i").unwrap(), PieceType::Knight),
        (parse_usi_square("8i").unwrap(), PieceType::Knight),
        (parse_usi_square("3i").unwrap(), PieceType::Silver),
        (parse_usi_square("7i").unwrap(), PieceType::Silver),
        (parse_usi_square("4i").unwrap(), PieceType::Gold),
        (parse_usi_square("6i").unwrap(), PieceType::Gold),
        (parse_usi_square("2h").unwrap(), PieceType::Bishop),
        (parse_usi_square("8h").unwrap(), PieceType::Rook),
        (parse_usi_square("5g").unwrap(), PieceType::Pawn),
        (parse_usi_square("4g").unwrap(), PieceType::Pawn),
        (parse_usi_square("6g").unwrap(), PieceType::Pawn),
    ];

    let white_pieces = [
        (parse_usi_square("1a").unwrap(), PieceType::Lance),
        (parse_usi_square("9a").unwrap(), PieceType::Lance),
        (parse_usi_square("2a").unwrap(), PieceType::Knight),
        (parse_usi_square("8a").unwrap(), PieceType::Knight),
        (parse_usi_square("3a").unwrap(), PieceType::Silver),
        (parse_usi_square("7a").unwrap(), PieceType::Silver),
        (parse_usi_square("4a").unwrap(), PieceType::Gold),
        (parse_usi_square("6a").unwrap(), PieceType::Gold),
        (parse_usi_square("8b").unwrap(), PieceType::Bishop),
        (parse_usi_square("2b").unwrap(), PieceType::Rook),
        (parse_usi_square("5c").unwrap(), PieceType::Pawn),
        (parse_usi_square("4c").unwrap(), PieceType::Pawn),
        (parse_usi_square("6c").unwrap(), PieceType::Pawn),
    ];

    for (square, piece_type) in black_pieces {
        pos.board.put_piece(square, Piece::new(piece_type, Color::Black));
    }

    for (square, piece_type) in white_pieces {
        pos.board.put_piece(square, Piece::new(piece_type, Color::White));
    }

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn create_tactical_position() -> Position {
    let mut pos = Position::empty();

    // Create a position with many possible captures for SEE testing
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Central tension with multiple attackers and defenders
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(parse_usi_square("4f").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(parse_usi_square("6f").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(parse_usi_square("4d").unwrap(), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(parse_usi_square("6d").unwrap(), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn create_test_capture_moves(_pos: &Position) -> Vec<Move> {
    vec![
        Move::normal(parse_usi_square("4f").unwrap(), parse_usi_square("5e").unwrap(), false),
        Move::normal(parse_usi_square("6f").unwrap(), parse_usi_square("5e").unwrap(), false),
        Move::normal(parse_usi_square("5h").unwrap(), parse_usi_square("5e").unwrap(), false),
    ]
}

fn create_test_normal_moves(_pos: &Position) -> Vec<Move> {
    vec![
        Move::normal(parse_usi_square("5g").unwrap(), parse_usi_square("5f").unwrap(), false),
        Move::normal(parse_usi_square("4g").unwrap(), parse_usi_square("4f").unwrap(), false),
        Move::normal(parse_usi_square("6g").unwrap(), parse_usi_square("6f").unwrap(), false),
        Move::normal(parse_usi_square("3i").unwrap(), parse_usi_square("4h").unwrap(), false),
        Move::normal(parse_usi_square("7i").unwrap(), parse_usi_square("6h").unwrap(), false),
    ]
}

fn create_test_moves_mixed(pos: &Position) -> Vec<Move> {
    let mut moves = create_test_normal_moves(pos);
    moves.extend(create_test_capture_moves(pos));

    // Add some drop moves
    moves.push(Move::drop(PieceType::Pawn, parse_usi_square("5f").unwrap()));
    moves.push(Move::drop(PieceType::Silver, parse_usi_square("4f").unwrap()));

    moves
}
