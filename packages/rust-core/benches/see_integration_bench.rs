//! SEE integration benchmarks
//!
//! Measures performance impact of SEE optimizations on:
//! - Basic SEE calculation time
//! - SEE with pin detection
//! - Search performance with SEE
//! - Move ordering efficiency

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use shogi_core::ai::board::{Color, Piece, PieceType, Position, Square};
use shogi_core::ai::evaluate::MaterialEvaluator;
use shogi_core::ai::movegen::MoveGen;
use shogi_core::ai::moves::{Move, MoveList};
use shogi_core::ai::search_enhanced::EnhancedSearcher;
use std::sync::Arc;
use std::time::Duration;

// Remove the static searcher - we'll create it once per benchmark function instead

/// Generate all captures for a position
fn generate_all_captures(pos: &Position) -> Vec<Move> {
    let mut moves = MoveList::new();
    let mut gen = MoveGen::new();
    gen.generate_captures(pos, &mut moves);
    moves.as_slice().to_vec()
}

/// Benchmark basic SEE calculation
fn bench_see_basic(c: &mut Criterion) {
    let mut group = c.benchmark_group("see_basic");

    // Create test positions of varying complexity
    let positions = vec![
        ("simple_capture", create_simple_capture_position()),
        ("complex_exchange", create_complex_exchange_position()),
        ("pinned_pieces", create_pinned_position()),
    ];

    // Pre-generate captures for each position
    let mut test_cases: Vec<(String, Vec<(Position, Move)>)> = Vec::new();
    for (name, pos) in positions {
        let captures = generate_all_captures(&pos);
        let cases: Vec<_> = captures.into_iter().map(|mv| (pos.clone(), mv)).collect();
        if !cases.is_empty() {
            test_cases.push((name.to_string(), cases));
        }
    }

    for (name, cases) in test_cases {
        group.bench_with_input(BenchmarkId::from_parameter(&name), &cases, |b, cases| {
            let mut idx = 0;
            b.iter(|| {
                let (pos, capture) = &cases[idx % cases.len()];
                idx += 1;

                // No clone needed - SEE is read-only
                black_box(pos.see_noinline(black_box(*capture)))
            });
        });
    }

    group.finish();
}

/// Benchmark SEE with pin detection
fn bench_see_with_pins(c: &mut Criterion) {
    let mut group = c.benchmark_group("see_with_pins");

    // Create positions with various pin configurations
    let positions = vec![
        ("single_pin", create_single_pin_position()),
        ("multiple_pins", create_multiple_pins_position()),
        ("cross_pins", create_cross_pins_position()),
    ];

    let mut test_cases: Vec<(String, Vec<(Position, Move)>)> = Vec::new();
    for (name, pos) in positions {
        let captures = generate_all_captures(&pos);
        let cases: Vec<_> = captures.into_iter().map(|mv| (pos.clone(), mv)).collect();
        if !cases.is_empty() {
            test_cases.push((name.to_string(), cases));
        }
    }

    for (name, cases) in test_cases {
        group.bench_with_input(BenchmarkId::from_parameter(&name), &cases, |b, cases| {
            let mut idx = 0;
            b.iter(|| {
                let (pos, capture) = &cases[idx % cases.len()];
                idx += 1;

                // No clone needed - SEE is read-only
                black_box(pos.see_noinline(black_box(*capture)))
            });
        });
    }

    group.finish();
}

/// Benchmark search performance with SEE
fn bench_search_with_see(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_with_see");

    // Further reduce measurement parameters for heavy benchmarks
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(2));

    let positions = vec![
        ("opening", Position::startpos()),
        ("middlegame", create_middlegame_position()),
        ("tactical", create_tactical_position()),
    ];

    // Create a single evaluator instance
    let evaluator = Arc::new(MaterialEvaluator);

    for (name, pos) in positions {
        group.bench_with_input(BenchmarkId::from_parameter(name), &pos, |b, pos| {
            // Use iter_batched to handle searcher setup
            b.iter_batched(
                || {
                    // Setup: Create new searcher and clone position
                    let searcher = EnhancedSearcher::new(2, evaluator.clone());
                    let pos_clone = pos.clone();
                    (searcher, pos_clone)
                },
                |(mut searcher, mut pos_clone)| {
                    // Measurement: Run search
                    black_box(searcher.search(
                        &mut pos_clone,
                        4, // Reduced depth for benchmarking
                        None,
                        Some(1_000), // Reduced node limit
                    ))
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

/// Benchmark move ordering efficiency
fn bench_move_ordering(c: &mut Criterion) {
    let mut group = c.benchmark_group("move_ordering");

    // Reduce parameters for move ordering benchmarks
    group.sample_size(50);
    group.measurement_time(Duration::from_secs(3));

    let evaluator = Arc::new(MaterialEvaluator);
    let positions = vec![
        ("many_captures", create_many_captures_position()),
        ("quiet_position", create_quiet_position()),
        ("forced_sequence", create_forced_sequence_position()),
    ];

    for (name, pos) in positions {
        group.bench_with_input(BenchmarkId::from_parameter(name), &pos, |b, pos| {
            // Use iter_batched for searcher setup
            b.iter_batched(
                || {
                    // Setup: Create new searcher and clone position
                    let searcher = EnhancedSearcher::new(1, evaluator.clone());
                    let pos_clone = pos.clone();
                    (searcher, pos_clone)
                },
                |(mut searcher, mut pos_clone)| {
                    // Measurement: Run search and measure cutoff efficiency
                    let result = searcher.search(&mut pos_clone, 3, None, Some(500));

                    // Return both result and mock stats for analysis
                    black_box((result, get_mock_stats()))
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

/// Create a simple capture position
fn create_simple_capture_position() -> Position {
    let mut pos = Position::empty();

    // Kings
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

    // Simple pawn capture
    pos.board
        .put_piece(Square::new(3, 3), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(Square::new(4, 4), Piece::new(PieceType::Pawn, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

/// Create a complex exchange position
fn create_complex_exchange_position() -> Position {
    let mut pos = Position::empty();

    // Kings
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));

    // Complex center with multiple pieces
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

/// Create a position with pinned pieces
fn create_pinned_position() -> Position {
    let mut pos = Position::empty();

    // Black King at 5i
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));

    // Black Gold at 5e (pinned)
    pos.board
        .put_piece(Square::new(4, 4), Piece::new(PieceType::Gold, Color::Black));

    // White Rook at 5a (pinning)
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::Rook, Color::White));

    // White Pawn at 4e (can't be captured by pinned Gold)
    pos.board
        .put_piece(Square::new(5, 4), Piece::new(PieceType::Pawn, Color::White));

    // Black Silver at 6f (can capture)
    pos.board
        .put_piece(Square::new(3, 3), Piece::new(PieceType::Silver, Color::Black));

    // White King
    pos.board
        .put_piece(Square::new(0, 8), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

/// Create positions with specific characteristics
fn create_single_pin_position() -> Position {
    create_pinned_position() // Reuse basic pinned position
}

fn create_multiple_pins_position() -> Position {
    let mut pos = create_pinned_position();

    // Add diagonal pin
    pos.board
        .put_piece(Square::new(6, 2), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(Square::new(8, 0), Piece::new(PieceType::Bishop, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos
}

fn create_cross_pins_position() -> Position {
    let mut pos = create_multiple_pins_position();

    // Add horizontal pin
    pos.board
        .put_piece(Square::new(2, 0), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(Square::new(0, 0), Piece::new(PieceType::Rook, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos
}

fn create_middlegame_position() -> Position {
    // Return a typical middlegame position
    let mut pos = Position::startpos();

    // Make some standard opening moves
    let moves = vec![
        Move::normal(Square::new(2, 2), Square::new(2, 3), false),
        Move::normal(Square::new(3, 6), Square::new(3, 5), false),
        Move::normal(Square::new(2, 3), Square::new(2, 4), false),
        Move::normal(Square::new(3, 5), Square::new(3, 4), false),
    ];

    for mv in moves {
        pos.do_move(mv);
    }

    pos
}

fn create_tactical_position() -> Position {
    // Create a position with many tactical possibilities
    create_complex_exchange_position()
}

fn create_many_captures_position() -> Position {
    let mut pos = Position::empty();

    // Set up a position where many pieces can capture on the same square
    pos.board
        .put_piece(Square::new(4, 4), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::White));

    // Target square with valuable piece
    pos.board
        .put_piece(Square::new(5, 5), Piece::new(PieceType::Gold, Color::White));

    // Multiple attackers
    pos.board
        .put_piece(Square::new(4, 5), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(Square::new(5, 4), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(Square::new(6, 6), Piece::new(PieceType::Bishop, Color::Black));
    pos.board
        .put_piece(Square::new(5, 7), Piece::new(PieceType::Rook, Color::Black));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn create_quiet_position() -> Position {
    Position::startpos() // Opening position is relatively quiet
}

fn create_forced_sequence_position() -> Position {
    // Position where there's essentially one good move
    let mut pos = Position::empty();

    // In check position
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(Square::new(4, 1), Piece::new(PieceType::Rook, Color::White));

    // Only one way to block
    pos.board
        .put_piece(Square::new(3, 0), Piece::new(PieceType::Gold, Color::Black));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

// Mock stats for benchmarking
#[derive(Default)]
struct SearchStats {
    _beta_cutoffs: u64,
    _first_move_cutoffs: u64,
}

// Helper function to simulate stats
fn get_mock_stats() -> SearchStats {
    SearchStats {
        _beta_cutoffs: 100,
        _first_move_cutoffs: 35,
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(30) // Reasonable sample size for SEE
        .measurement_time(Duration::from_millis(200)); // Short measurement time for fast operations
    targets = bench_see_basic,
              bench_see_with_pins,
              bench_search_with_see,
              bench_move_ordering
}

criterion_main!(benches);
