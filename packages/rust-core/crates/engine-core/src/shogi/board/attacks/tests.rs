//! Tests for attack detection functionality

use crate::shogi::board::{Color, Piece, PieceType, Position, Square};
use crate::usi::parse_usi_square;

#[test]
fn test_is_attacked_with_lance() {
    // Test is_attacked method with lance attacks
    let mut pos = Position::empty();

    // Black lance at 5i (file 4, rank 8)
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::Lance, Color::Black));

    // White lance at 5a (file 4, rank 0)
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Lance, Color::White));

    // Add kings to make position valid
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.board.rebuild_occupancy_bitboards();

    // Black lance (Sente) at rank 8 attacks upward (toward rank 0)
    assert!(pos.is_attacked(parse_usi_square("5h").unwrap(), Color::Black));
    assert!(pos.is_attacked(parse_usi_square("5g").unwrap(), Color::Black));
    assert!(!pos.is_attacked(parse_usi_square("6h").unwrap(), Color::Black)); // Different file

    // White lance (Gote) at rank 0 attacks downward (toward rank 8)
    assert!(pos.is_attacked(parse_usi_square("5b").unwrap(), Color::White));
    assert!(pos.is_attacked(parse_usi_square("5c").unwrap(), Color::White));

    // Move lances to positions where they can attack
    pos.board.remove_piece(parse_usi_square("5i").unwrap());
    pos.board.remove_piece(parse_usi_square("5a").unwrap());
    pos.board
        .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Lance, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Lance, Color::White));
    pos.board.rebuild_occupancy_bitboards();

    // Now test actual attacks
    // Black lance (Sente) at rank 2 attacks toward rank 0
    assert!(pos.is_attacked(parse_usi_square("5b").unwrap(), Color::Black));
    assert!(pos.is_attacked(parse_usi_square("5a").unwrap(), Color::Black));
    assert!(!pos.is_attacked(parse_usi_square("5d").unwrap(), Color::Black)); // Cannot attack backward

    // White lance (Gote) at rank 6 attacks toward rank 8
    assert!(pos.is_attacked(parse_usi_square("5h").unwrap(), Color::White));
    assert!(pos.is_attacked(parse_usi_square("5i").unwrap(), Color::White));
    assert!(!pos.is_attacked(parse_usi_square("5f").unwrap(), Color::White)); // Cannot attack backward

    // Test with blocker
    // Place a White pawn as blocker at rank 1 (blocks Black lance)
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    pos.board.rebuild_occupancy_bitboards();

    // Black lance at rank 2 is blocked by White pawn at rank 1
    assert!(pos.is_attacked(parse_usi_square("5b").unwrap(), Color::Black)); // Lance can attack the blocker
    assert!(!pos.is_attacked(parse_usi_square("5a").unwrap(), Color::Black)); // Lance cannot attack beyond blocker

    // Remove blocker and test White lance
    pos.board.remove_piece(parse_usi_square("5b").unwrap());

    // Place a Black pawn as blocker at rank 7 (blocks White lance)
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    pos.board.rebuild_occupancy_bitboards();

    // White lance at rank 6 is blocked by Black pawn at rank 7
    assert!(pos.is_attacked(parse_usi_square("5h").unwrap(), Color::White)); // Lance can attack the blocker
    assert!(!pos.is_attacked(parse_usi_square("5i").unwrap(), Color::White));
    // Lance cannot attack beyond blocker
}

#[test]
fn test_get_lance_attackers_performance() {
    // Skip test in CI environment
    if crate::util::is_ci_environment() {
        log::debug!("Skipping performance test in CI environment");
        return;
    }

    use std::time::Instant;

    // Create a position with multiple lances
    let mut pos = Position::empty();

    // Add multiple lances on the same file to test worst case
    for rank in 0..9 {
        if rank % 3 == 0 {
            pos.board
                .put_piece(Square::new(4, rank), Piece::new(PieceType::Lance, Color::Black));
        }
    }

    // Add some blockers
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    pos.board.rebuild_occupancy_bitboards();

    // Performance test: Call get_lance_attackers_to many times
    let iterations = 100_000;
    let target = parse_usi_square("5h").unwrap();
    let lance_bb = pos.board.piece_bb[Color::Black as usize][PieceType::Lance as usize];
    let occupied = pos.board.all_bb;

    let start = Instant::now();
    for _ in 0..iterations {
        let attackers = pos.get_lance_attackers_to(target, Color::Black, lance_bb, occupied);
        // Force evaluation to prevent optimization
        std::hint::black_box(attackers);
    }
    let elapsed = start.elapsed();

    // Calculate performance metrics
    let ns_per_call = elapsed.as_nanos() / iterations as u128;
    let calls_per_sec = 1_000_000_000 / ns_per_call;

    log::debug!("Lance attackers performance:");
    log::debug!("  Time per call: {ns_per_call} ns");
    log::debug!("  Calls per second: {calls_per_sec}");

    // Assert reasonable performance
    // Note: Debug builds are much slower than release builds
    #[cfg(debug_assertions)]
    let max_ns = 500; // Allow up to 500ns in debug mode
    #[cfg(not(debug_assertions))]
    let max_ns = 100; // Expect under 100ns in release mode

    assert!(
        ns_per_call < max_ns,
        "get_lance_attackers_to is too slow: {ns_per_call} ns (max: {max_ns} ns)"
    );
}
