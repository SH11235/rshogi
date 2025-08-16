//! Tests for check evasion move generation

use crate::{
    movegen::generator::MoveGenImpl, usi::parse_usi_square, Color, Piece, PieceType, Position,
};

#[test]
fn test_single_check_non_sliding_piece() {
    // Single check by knight - can only capture, no blocks possible
    let mut pos = Position::empty();

    // Black king at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White knight at 6g giving check
    pos.board
        .put_piece(parse_usi_square("6g").unwrap(), Piece::new(PieceType::Knight, Color::White));
    // Black gold at 7h (can capture knight at 6g)
    pos.board
        .put_piece(parse_usi_square("7h").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    // Black silver at 4h (cannot help - knight can't be blocked)
    pos.board
        .put_piece(parse_usi_square("4h").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    // White king far away
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black has a rook in hand
    pos.hands[Color::Black as usize][PieceType::Rook.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards after manual piece placement
    pos.board.rebuild_occupancy_bitboards();

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // Gold can capture knight
    let gold_capture = moves.as_slice().iter().any(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("7h").unwrap())
            && m.to() == parse_usi_square("6g").unwrap()
    });
    assert!(gold_capture, "Gold should be able to capture checking knight");

    // Silver cannot move (except to squares that don't help)
    let silver_to_6g = moves.as_slice().iter().any(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("4h").unwrap())
            && m.to() == parse_usi_square("6g").unwrap()
    });
    assert!(!silver_to_6g, "Silver cannot capture knight from 4h");

    // No drops can block a knight check
    let drop_count = moves.as_slice().iter().filter(|m| m.is_drop()).count();
    assert_eq!(drop_count, 0, "No drops should be possible to block knight check");
}

#[test]
fn test_single_check_sliding_piece() {
    // Single check by rook - can capture or block
    let mut pos = Position::empty();

    // Black king at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White rook at 5a giving check
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    // Black gold at 6h (can interpose at 5h)
    pos.board
        .put_piece(parse_usi_square("6h").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    // Black silver at 4g (can interpose at 5h diagonally)
    pos.board
        .put_piece(parse_usi_square("4g").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    // White king far away
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black has a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards after manual piece placement
    pos.board.rebuild_occupancy_bitboards();

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // Gold can block at 5h
    let gold_block = moves.as_slice().iter().any(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("6h").unwrap())
            && m.to() == parse_usi_square("5h").unwrap()
    });
    assert!(gold_block, "Gold should be able to block rook check at 5h");

    // Silver can block at 5h
    let silver_block = moves.as_slice().iter().any(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("4g").unwrap())
            && m.to() == parse_usi_square("5h").unwrap()
    });
    assert!(silver_block, "Silver should be able to block rook check at 5h");

    // Pawn can be dropped to block
    let pawn_drop_5e = moves.as_slice().iter().any(|m| {
        m.is_drop()
            && m.drop_piece_type() == PieceType::Pawn
            && m.to() == parse_usi_square("5e").unwrap()
    });
    assert!(pawn_drop_5e, "Pawn should be droppable at 5e to block check");
}

#[test]
fn test_double_check_only_king_moves() {
    // Double check - only king can move
    let mut pos = Position::empty();

    // Black king at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White rook at 5a giving check
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    // White bishop at 2f giving check (double check)
    pos.board
        .put_piece(parse_usi_square("2f").unwrap(), Piece::new(PieceType::Bishop, Color::White));
    // Black gold at 7i (cannot move in double check)
    pos.board
        .put_piece(parse_usi_square("7i").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    // Black silver at 3i (cannot move in double check)
    pos.board
        .put_piece(parse_usi_square("3i").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    // White king far away
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black has pieces in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
    pos.hands[Color::Black as usize][PieceType::Gold.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards after manual piece placement
    pos.board.rebuild_occupancy_bitboards();

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // Only king moves should be generated
    for mv in moves.as_slice() {
        if !mv.is_drop() {
            assert_eq!(
                mv.from(),
                Some(parse_usi_square("5i").unwrap()),
                "Only king should move in double check"
            );
        } else {
            panic!("No drops should be allowed in double check");
        }
    }

    // King should have some escape squares
    assert!(moves.len() > 0, "King should have escape moves");
}

#[test]
fn test_pinned_piece_can_capture_checker() {
    // Test that a pinned piece can capture the checking piece if it's on the pin ray
    let mut pos = Position::empty();

    // Black king at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // Black silver at 5h (will be pinned)
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    // White rook at 5a (pinning silver through 5h)
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    // White gold at 5g giving check (on the pin ray, so silver can capture)
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Gold, Color::White));
    // White king far away
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Rebuild occupancy bitboards after manual piece placement
    pos.board.rebuild_occupancy_bitboards();

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // Silver can capture gold (along pin ray)
    let silver_capture = moves.as_slice().iter().any(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("5h").unwrap())
            && m.to() == parse_usi_square("5g").unwrap()
    });
    assert!(
        silver_capture,
        "Pinned silver should be able to capture checking gold along pin ray"
    );

    // Silver cannot move to other squares (off the pin ray)
    let silver_off_ray = moves.as_slice().iter().any(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("5h").unwrap())
            && m.to() != parse_usi_square("5g").unwrap()
    });
    assert!(!silver_off_ray, "Pinned silver should not be able to move off pin ray");
}

#[test]
fn test_promotion_required_to_escape_check() {
    // Test case where promotion is required to escape check
    let mut pos = Position::empty();

    // Black king at 2i
    pos.board
        .put_piece(parse_usi_square("2i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // Black pawn at 2b (must promote when moving to 2a)
    pos.board
        .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    // White rook at 2a giving check (pawn can capture it with promotion)
    pos.board
        .put_piece(parse_usi_square("2a").unwrap(), Piece::new(PieceType::Rook, Color::White));
    // White king far away
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Rebuild occupancy bitboards after manual piece placement
    pos.board.rebuild_occupancy_bitboards();

    let mut gen = MoveGenImpl::new(&pos);

    // Debug
    println!("\ntest_promotion_required_to_escape_check:");
    println!("Checkers: {}", gen.checkers.count_ones());
    println!("King at 2i, Pawn at 2b, Gold at 2a, Rook at 1i giving check");

    let moves = gen.generate_all();
    println!("Generated {} moves", moves.len());
    for m in moves.as_slice() {
        if !m.is_drop() {
            println!(
                "  Move from {} to {} (promoted: {})",
                m.from().map(|sq| sq.to_string()).unwrap_or("?".to_string()),
                m.to().to_string(),
                m.is_promote()
            );
        }
    }

    // Pawn must capture gold with promotion
    let pawn_capture = moves.as_slice().iter().any(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("2b").unwrap())
            && m.to() == parse_usi_square("2a").unwrap()
            && m.is_promote()
    });
    assert!(pawn_capture, "Pawn must promote when capturing gold at 2a");

    // No non-promotion move should exist
    let pawn_no_promote = moves.as_slice().iter().any(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("2b").unwrap())
            && m.to() == parse_usi_square("2a").unwrap()
            && !m.is_promote()
    });
    assert!(!pawn_no_promote, "Pawn cannot move to 2a without promotion");
}
