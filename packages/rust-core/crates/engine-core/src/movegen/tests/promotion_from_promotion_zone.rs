//! Test for promotion moves from pieces already in promotion zone

use crate::{
    movegen::MoveGenerator,
    shogi::{Color, PieceType, Position},
    usi::parse_usi_square,
};

#[test]
fn test_bishop_promotion_from_promotion_zone() {
    // Create position with White Bishop on 2b that can move and promote
    // Clear some squares so the bishop can move diagonally
    let sfen = "lnsgkgsnl/1r5b1/pppppp1p1/7p1/9/7P1/PPPPPP1P1/1R7/LNSGKGSNL w - 1";
    let pos = Position::from_sfen(sfen).unwrap();

    // Verify bishop is on 2b
    let from = parse_usi_square("2b").unwrap();
    let piece = pos.board.piece_on(from).unwrap();
    assert_eq!(piece.piece_type, PieceType::Bishop);
    assert_eq!(piece.color, Color::White);
    assert!(!piece.promoted);

    // Generate all legal moves
    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // Find move 2b7g+ (a move that should exist)
    let to = parse_usi_square("7g").unwrap();

    let promotion_move =
        moves.iter().find(|m| m.from() == Some(from) && m.to() == to && m.is_promote());

    assert!(promotion_move.is_some(), "Should be able to promote bishop from 2b to 7g");

    // Also check that non-promotion move exists
    let non_promotion_move =
        moves.iter().find(|m| m.from() == Some(from) && m.to() == to && !m.is_promote());

    assert!(
        non_promotion_move.is_some(),
        "Should also have non-promotion option from 2b to 7g"
    );

    // Verify that promotion from promotion zone works
    // Bishop at 2b is already in Black's promotion zone (ranks 0-2)
    // When moving to 7g (rank 6), it's entering White's promotion zone
    assert!(from.rank() <= 2 || to.rank() >= 6, "Move should qualify for promotion");
}

#[test]
fn test_rook_promotion_from_promotion_zone() {
    // Create position with Black Rook on 7b (in White's promotion zone)
    // Black's turn, clear path for rook to move
    let sfen = "lnsgkgsnl/r1R4b1/ppppppppp/9/9/9/PPPPPPPPP/1B7/LNSGKGSNL b - 1";
    let pos = Position::from_sfen(sfen).unwrap();

    // Verify rook is on 7b
    let from = parse_usi_square("7b").unwrap();
    let piece = pos.board.piece_on(from).unwrap();
    assert_eq!(piece.piece_type, PieceType::Rook);
    assert_eq!(piece.color, Color::Black);
    assert!(!piece.promoted);

    // Generate all legal moves
    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // Find move 7b7a+ (moving to opponent's back rank)
    let to = parse_usi_square("7a").unwrap();
    let promotion_move =
        moves.iter().find(|m| m.from() == Some(from) && m.to() == to && m.is_promote());

    assert!(promotion_move.is_some(), "Should be able to promote rook from 7b to 7a");
}

#[test]
fn test_no_promotion_for_promoted_pieces() {
    // Create position with promoted bishop (horse) on 2b
    // Lowercase '+b' means promoted White bishop
    let sfen = "lnsgkgsnl/1r5+b1/pppppp1p1/7p1/9/7P1/PPPPPP1P1/1R7/LNSGKGSNL w - 1";
    let pos = Position::from_sfen(sfen).unwrap();

    // Verify promoted bishop is on 2b
    let from = parse_usi_square("2b").unwrap();
    let piece = pos.board.piece_on(from).unwrap();
    assert_eq!(piece.piece_type, PieceType::Bishop);
    assert_eq!(piece.color, Color::White);
    assert!(piece.promoted);

    // Generate all legal moves
    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // Check that no promotion moves exist for the promoted bishop
    // Find any move from the promoted bishop
    let bishop_moves: Vec<_> = moves.iter().filter(|m| m.from() == Some(from)).collect();

    // Check none of them are promotion moves
    let has_promotion = bishop_moves.iter().any(|m| m.is_promote());
    assert!(!has_promotion, "Promoted pieces should not be able to promote again");

    // But normal moves should exist
    assert!(
        !bishop_moves.is_empty(),
        "Promoted bishop should still be able to move normally"
    );
}
