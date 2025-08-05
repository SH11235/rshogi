use engine_core::shogi::{Color, MoveList, Piece, PieceType};
use engine_core::usi::parse_usi_square;
use engine_core::{MoveGen, Position};

#[test]
fn test_lance_movement_direction() {
    // Test Black Lance (should move towards rank 0, not rank 8)
    let mut pos = Position::empty();

    // Place Black Lance at rank 7, file 4
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Lance, Color::Black));
    // Place kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.side_to_move = Color::Black;

    let mut movegen = MoveGen::new();
    let mut moves = MoveList::new();
    movegen.generate_all(&pos, &mut moves);

    // Find moves from the Lance
    let lance_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5h").unwrap()))
        .collect();

    // Black Lance should be able to move to ranks 6, 5, 4, 3, 2, 1 (not to rank 8)
    let valid_targets = vec![
        parse_usi_square("5g").unwrap(),
        parse_usi_square("5f").unwrap(),
        parse_usi_square("5e").unwrap(),
        parse_usi_square("5d").unwrap(),
        parse_usi_square("5c").unwrap(),
        parse_usi_square("5b").unwrap(),
    ];

    for target in valid_targets {
        assert!(
            lance_moves.iter().any(|m| m.to() == target),
            "Black Lance at 4,7 should be able to move to {target:?}"
        );
    }

    // Should NOT be able to move to rank 8 (backward)
    assert!(
        !lance_moves.iter().any(|m| m.to() == parse_usi_square("5i").unwrap()),
        "Black Lance should NOT be able to move backward to rank 8"
    );
}

#[test]
fn test_white_lance_movement_direction() {
    // Test White Lance (should move towards rank 8, not rank 0)
    let mut pos = Position::empty();

    // Place White Lance at rank 1, file 4
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Lance, Color::White));
    // Place kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.side_to_move = Color::White;

    let mut movegen = MoveGen::new();
    let mut moves = MoveList::new();
    movegen.generate_all(&pos, &mut moves);

    // Find moves from the Lance
    let lance_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5b").unwrap()))
        .collect();

    // White Lance should be able to move to ranks 2, 3, 4, 5, 6, 7 (not to rank 0)
    let valid_targets = vec![
        parse_usi_square("5c").unwrap(),
        parse_usi_square("5d").unwrap(),
        parse_usi_square("5e").unwrap(),
        parse_usi_square("5f").unwrap(),
        parse_usi_square("5g").unwrap(),
        parse_usi_square("5h").unwrap(),
    ];

    for target in valid_targets {
        assert!(
            lance_moves.iter().any(|m| m.to() == target),
            "White Lance at 4,1 should be able to move to {target:?}"
        );
    }

    // Should NOT be able to move to rank 0 (backward)
    assert!(
        !lance_moves.iter().any(|m| m.to() == parse_usi_square("5a").unwrap()),
        "White Lance should NOT be able to move backward to rank 0"
    );
}

#[test]
fn test_lance_attack_direction() {
    // Test that Lance attack checks are consistent with movement
    let mut pos = Position::empty();

    // Place Black king at rank 4
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    // Place White Lance at rank 6 (below the Black king)
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Lance, Color::White));
    // Place White king somewhere
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    pos.side_to_move = Color::Black;

    let mut movegen = MoveGen::new();
    let mut moves = MoveList::new();
    movegen.generate_all(&pos, &mut moves);

    // Black should be in check from the White Lance
    // So Black king must move or block/capture the Lance
    let king_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5e").unwrap()))
        .collect();

    // King should have escape moves (not on file 4)
    assert!(
        !king_moves.is_empty(),
        "Black king should have escape moves when in check from White Lance"
    );

    // All moves should be king moves or moves that block/capture the Lance
    for mv in moves.as_slice() {
        if !mv.is_drop() {
            let from = mv.from().unwrap();
            assert!(
                from == parse_usi_square("5e").unwrap() || // King move
                mv.to() == parse_usi_square("5g").unwrap() || // Capture Lance
                (mv.to().file() == 4 && mv.to().rank() == 5), // Block on rank 5
                "In check, only king moves, captures, or blocks should be legal"
            );
        }
    }
}
