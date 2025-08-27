//! Tests for has_any_legal_move() optimization

use crate::{movegen::MoveGenerator, usi, Position};

#[test]
fn test_has_any_legal_move_matches_generate_all() {
    // Test various positions to ensure has_any_legal_move() returns the same result as generate_all()
    let test_positions = vec![
        // Initial position
        Position::startpos(),
        // Various game positions from benchmark_positions.txt
        Position::from_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .unwrap(),
        Position::from_sfen(
            "ln1gkg1nl/1r2s2b1/p1pppp1pp/1p4p2/9/2P4P1/PP1PPPP1P/1B2S2R1/LN1GKG1NL w - 1",
        )
        .unwrap(),
        Position::from_sfen(
            "8l/1l+R2P3/p2pBG1pp/kps1p4/Nn1P2G2/P1P1P2PP/1PS6/1KSG3+r1/LN2+p3L w Sbgn3p 124",
        )
        .unwrap(),
        // King in check position
        Position::from_sfen("lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LN1GKGSNL b - 1")
            .unwrap(),
        // Complex middle game
        Position::from_sfen(
            "ln1g1g1nl/1ks2r3/1pppp2pp/p3spp2/9/P3SPP2/1PPPP2PP/1KS2R3/LN1G1G1NL b Bb 1",
        )
        .unwrap(),
    ];

    for pos in test_positions {
        let sfen = usi::position_to_sfen(&pos);

        // Test with MoveGenerator
        let movegen = MoveGenerator::new();
        let has_any = movegen.has_legal_moves(&pos).unwrap();

        let all_moves = movegen.generate_all(&pos).unwrap();
        let has_moves = !all_moves.is_empty();

        assert_eq!(
            has_any, has_moves,
            "has_legal_moves() and generate_all() disagree for position: {sfen}"
        );
    }
}

// TODO: Add double check and checkmate position tests with verified positions

#[test]
fn test_has_any_legal_move_block_check_with_drop() {
    // Position where check can only be blocked by dropping a piece
    let pos = Position::from_sfen("4k4/9/9/9/4r4/9/9/9/4K4 b G 1").unwrap();

    assert!(pos.is_in_check(), "King should be in check");

    let movegen = MoveGenerator::new();
    assert!(
        movegen.has_legal_moves(&pos).unwrap(),
        "Should be able to block check with gold drop"
    );

    // Verify at least one drop move exists
    let all_moves = movegen.generate_all(&pos).unwrap();
    let has_drop = all_moves.as_slice().iter().any(|m| m.is_drop());
    assert!(has_drop, "Should have at least one drop move to block check");
}

#[test]
fn test_has_any_legal_move_king_moves_first() {
    // Test that king moves are checked first for early exit
    // Position where king has many moves
    let pos = Position::from_sfen("9/9/9/9/4k4/9/9/9/4K4 b - 1").unwrap();

    let movegen = MoveGenerator::new();
    assert!(movegen.has_legal_moves(&pos).unwrap(), "King should have legal moves");

    // The implementation should return true quickly after checking king moves
    // This is more of a performance characteristic than a correctness test
}

#[test]
fn test_has_any_legal_move_promoted_pieces() {
    // Test with promoted pieces to ensure they are handled correctly
    let pos = Position::from_sfen("9/9/9/4+R4/4k4/9/9/9/4K4 w - 1").unwrap();

    let movegen = MoveGenerator::new();
    let has_any = movegen.has_legal_moves(&pos).unwrap();

    let all_moves = movegen.generate_all(&pos).unwrap();
    assert_eq!(has_any, !all_moves.is_empty(), "Results should match for promoted pieces");
}

#[test]
fn test_has_any_legal_move_various_piece_types() {
    // Test that all piece types are checked properly
    let positions = vec![
        // Position with only rook moves
        "9/9/9/4R4/4k4/9/9/9/4K4 w - 1",
        // Position with only bishop moves
        "9/9/9/4B4/3k5/9/9/9/4K4 b - 1",
        // Position with only gold moves
        "9/9/9/4G4/3k5/9/9/9/4K4 b - 1",
        // Position with only silver moves
        "9/9/9/4S4/3k5/9/9/9/4K4 b - 1",
        // Position with only knight moves
        "9/9/9/9/3kN4/9/9/9/4K4 b - 1",
        // Position with only lance moves
        "9/9/9/9/3kL4/9/9/9/4K4 b - 1",
        // Position with only pawn moves
        "9/9/9/9/3kP4/9/9/9/4K4 b - 1",
    ];

    for sfen in positions {
        let pos = Position::from_sfen(sfen).unwrap();
        let movegen = MoveGenerator::new();
        let has_any = movegen.has_legal_moves(&pos).unwrap();

        let all_moves = movegen.generate_all(&pos).unwrap();
        assert_eq!(has_any, !all_moves.is_empty(), "Results should match for position: {sfen}");
    }
}
