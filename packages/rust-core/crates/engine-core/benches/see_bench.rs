use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::movegen::MoveGen;
use engine_core::shogi::{Color, Move, MoveList, Piece, PieceType, Position, Square};
use rand::{Rng, SeedableRng};
use rand_xoshiro::Xoshiro256PlusPlus;
use std::hint::black_box;

/// Create various test positions for benchmarking
fn create_test_positions() -> Vec<(String, Position)> {
    let mut positions = Vec::new();

    // Position 1: Simple pawn exchange
    {
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
        positions.push(("simple_pawn".to_string(), pos));
    }

    // Position 2: Multiple attackers
    {
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
        positions.push(("complex_exchange".to_string(), pos));
    }

    // Position 3: X-ray attacks
    {
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
        positions.push(("xray_attack".to_string(), pos));
    }

    // Position 4: Pinned pieces
    {
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
        positions.push(("pinned_pieces".to_string(), pos));
    }

    // Position 5: Deep exchange sequence
    {
        let mut pos = Position::empty();
        pos.board
            .put_piece(Square::new(0, 0), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(8, 8), Piece::new(PieceType::King, Color::White));

        // Central target
        pos.board
            .put_piece(Square::new(4, 4), Piece::new(PieceType::Gold, Color::White));

        // Multiple attackers
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
        positions.push(("deep_exchange".to_string(), pos));
    }

    positions
}

/// Generate all captures for a position
fn generate_captures(pos: &Position) -> Vec<Move> {
    let mut moves = MoveList::new();
    let mut gen = MoveGen::new();
    gen.generate_captures(pos, &mut moves);
    moves.as_slice().to_vec()
}

fn bench_see_simple_capture(c: &mut Criterion) {
    let positions = create_test_positions();

    // Pre-generate capture moves for each position
    let mut position_captures: Vec<(Position, Vec<Move>)> = Vec::new();
    for (_, pos) in positions {
        let captures = generate_captures(&pos);
        if !captures.is_empty() {
            position_captures.push((pos, captures));
        }
    }

    // Pre-generate random test cases to avoid RNG in measurement loop
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0x1234_5678_9ABC_DEF0);
    let test_count = 1000;
    let mut test_cases: Vec<(&Position, Move)> = Vec::new();
    for _ in 0..test_count {
        let (pos, captures) = &position_captures[rng.random_range(0..position_captures.len())];
        let capture = captures[rng.random_range(0..captures.len())];
        test_cases.push((pos, capture));
    }

    c.bench_function("see_simple_capture", |b| {
        let mut idx = 0;
        b.iter(|| {
            let (pos, capture) = test_cases[idx % test_cases.len()];
            idx += 1;

            // No clone needed - SEE is read-only
            black_box(pos.see_noinline(black_box(capture)))
        })
    });
}

fn bench_see_complex_exchange(c: &mut Criterion) {
    let positions = create_test_positions();

    // Select positions with complex exchanges
    let complex_positions: Vec<_> = positions
        .into_iter()
        .filter(|(name, _)| name.contains("complex") || name.contains("deep"))
        .collect();

    let mut position_captures: Vec<(Position, Vec<Move>)> = Vec::new();
    for (_, pos) in complex_positions {
        let captures = generate_captures(&pos);
        if !captures.is_empty() {
            position_captures.push((pos, captures));
        }
    }

    // Pre-generate test cases
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0xDEAD_BEEF_CAFE_BABE);
    let test_count = 1000;
    let mut test_cases: Vec<(&Position, Move)> = Vec::new();
    for _ in 0..test_count {
        let (pos, captures) = &position_captures[rng.random_range(0..position_captures.len())];
        let capture = captures[rng.random_range(0..captures.len())];
        test_cases.push((pos, capture));
    }

    c.bench_function("see_complex_exchange", |b| {
        let mut idx = 0;
        b.iter(|| {
            let (pos, capture) = test_cases[idx % test_cases.len()];
            idx += 1;

            // No clone needed - SEE is read-only
            black_box(pos.see_noinline(black_box(capture)))
        })
    });
}

fn bench_see_with_xray(c: &mut Criterion) {
    let positions = create_test_positions();

    // Select positions with x-ray potential
    let xray_positions: Vec<_> = positions
        .into_iter()
        .filter(|(name, _)| name.contains("xray") || name.contains("deep"))
        .collect();

    let mut position_captures: Vec<(Position, Vec<Move>)> = Vec::new();
    for (_, pos) in xray_positions {
        let captures = generate_captures(&pos);
        if !captures.is_empty() {
            position_captures.push((pos, captures));
        }
    }

    // Pre-generate test cases
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(0xFEED_FACE_DEAD_C0DE);
    let test_count = 1000;
    let mut test_cases: Vec<(&Position, Move)> = Vec::new();
    for _ in 0..test_count {
        let (pos, captures) = &position_captures[rng.random_range(0..position_captures.len())];
        let capture = captures[rng.random_range(0..captures.len())];
        test_cases.push((pos, capture));
    }

    c.bench_function("see_with_xray", |b| {
        let mut idx = 0;
        b.iter(|| {
            let (pos, capture) = test_cases[idx % test_cases.len()];
            idx += 1;

            // No clone needed - SEE is read-only
            black_box(pos.see_noinline(black_box(capture)))
        })
    });
}

fn bench_see_ge_threshold(c: &mut Criterion) {
    let positions = create_test_positions();

    let mut all_captures: Vec<(Position, Move)> = Vec::new();
    for (_, pos) in positions {
        let captures = generate_captures(&pos);
        for capture in captures {
            all_captures.push((pos.clone(), capture));
        }
    }

    let thresholds = vec![0, 100, 200, -100];
    let mut group = c.benchmark_group("see_ge_threshold");

    // Adjust group parameters
    group.sample_size(50);
    group.measurement_time(std::time::Duration::from_millis(500));

    for threshold in thresholds {
        group.bench_with_input(
            BenchmarkId::from_parameter(threshold),
            &threshold,
            |b, &threshold| {
                let mut idx = 0;
                b.iter(|| {
                    let (pos, capture) = &all_captures[idx % all_captures.len()];
                    idx += 1;

                    // No clone needed - see_ge is also read-only
                    black_box(pos.see_ge(black_box(*capture), black_box(threshold)))
                })
            },
        );
    }

    group.finish();
}

fn bench_see_batch(c: &mut Criterion) {
    let positions = create_test_positions();

    // Prepare a batch of different positions and moves
    let mut test_cases: Vec<(Position, Move)> = Vec::new();
    for (_, pos) in positions {
        let captures = generate_captures(&pos);
        for capture in captures.into_iter().take(3) {
            test_cases.push((pos.clone(), capture));
        }
    }

    c.bench_function("see_batch_mixed", |b| {
        b.iter(|| {
            let mut total = 0i32;
            for (pos, capture) in test_cases.iter() {
                // No clone needed - SEE is read-only
                total += pos.see_noinline(*capture);
            }
            black_box(total)
        })
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(100)  // Increased sample size for more stable measurements
        .measurement_time(std::time::Duration::from_secs(5)) // Longer measurement time for accuracy
        .warm_up_time(std::time::Duration::from_secs(2)); // Adequate warm-up time
    targets = bench_see_simple_capture,
              bench_see_complex_exchange,
              bench_see_with_xray,
              bench_see_ge_threshold,
              bench_see_batch
}
criterion_main!(benches);
