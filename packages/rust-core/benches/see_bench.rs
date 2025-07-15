use criterion::{black_box, criterion_group, criterion_main, Criterion};
use shogi_core::{Color, Move, Piece, PieceType, Position, Square};

fn setup_complex_position() -> Position {
    let mut pos = Position::empty();

    // Complex position with multiple pieces
    // Black pieces
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(3, 7), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(Square::new(5, 7), Piece::new(PieceType::Bishop, Color::Black));
    pos.board
        .put_piece(Square::new(4, 7), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(Square::new(3, 6), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(Square::new(5, 6), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(Square::new(4, 6), Piece::new(PieceType::Pawn, Color::Black));

    // White pieces
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(Square::new(3, 1), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(Square::new(5, 1), Piece::new(PieceType::Bishop, Color::White));
    pos.board
        .put_piece(Square::new(4, 1), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(Square::new(3, 2), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(Square::new(5, 2), Piece::new(PieceType::Silver, Color::White));
    pos.board
        .put_piece(Square::new(4, 2), Piece::new(PieceType::Pawn, Color::White));

    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

fn bench_see_simple_capture(c: &mut Criterion) {
    let mut pos = Position::empty();

    // Simple capture: pawn takes pawn
    pos.board
        .put_piece(Square::new(4, 4), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(Square::new(4, 3), Piece::new(PieceType::Pawn, Color::White));
    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    let mv = Move::normal(Square::new(4, 4), Square::new(4, 3), false);

    c.bench_function("see_simple_capture", |b| b.iter(|| black_box(pos.see(black_box(mv)))));
}

fn bench_see_complex_exchange(c: &mut Criterion) {
    let pos = setup_complex_position();

    // Complex exchange with multiple pieces
    let mv = Move::normal(Square::new(4, 6), Square::new(4, 2), false);

    c.bench_function("see_complex_exchange", |b| b.iter(|| black_box(pos.see(black_box(mv)))));
}

fn bench_see_with_xray(c: &mut Criterion) {
    let mut pos = Position::empty();

    // Position with X-ray attacks
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(Square::new(4, 5), Piece::new(PieceType::Rook, Color::Black));
    pos.board
        .put_piece(Square::new(4, 3), Piece::new(PieceType::Pawn, Color::White));
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::Rook, Color::White));
    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;

    let mv = Move::normal(Square::new(4, 5), Square::new(4, 3), false);

    c.bench_function("see_with_xray", |b| b.iter(|| black_box(pos.see(black_box(mv)))));
}

fn bench_see_ge_threshold(c: &mut Criterion) {
    let pos = setup_complex_position();

    // Various moves to test see_ge with different thresholds
    let moves = vec![
        Move::normal(Square::new(4, 6), Square::new(4, 2), false),
        Move::normal(Square::new(3, 6), Square::new(4, 5), false),
        Move::normal(Square::new(5, 6), Square::new(4, 5), false),
    ];

    c.bench_function("see_ge_threshold_0", |b| {
        b.iter(|| {
            for &mv in &moves {
                black_box(pos.see_ge(black_box(mv), 0));
            }
        })
    });

    c.bench_function("see_ge_threshold_100", |b| {
        b.iter(|| {
            for &mv in &moves {
                black_box(pos.see_ge(black_box(mv), 100));
            }
        })
    });
}

criterion_group!(
    benches,
    bench_see_simple_capture,
    bench_see_complex_exchange,
    bench_see_with_xray,
    bench_see_ge_threshold
);
criterion_main!(benches);
