//! Tests for pawn forward capture moves
//!
//! This test ensures that pawns can capture pieces directly in front of them.
//! In shogi, pawns move and capture only one square forward (not diagonally like chess).

use engine_core::{
    movegen::MoveGen,
    shogi::{Color, MoveList, Piece, PieceType, Square},
    Position,
};

#[test]
fn test_black_pawn_forward_capture() {
    // Test all files for Black pawns capturing White pieces
    for file in 0..9 {
        // Set up a position with Black pawn on rank 6 and White piece on rank 5
        let mut pos = Position::empty();
        let black_sq = Square::new(file, 6); // Black pawn at rank 6
        let white_sq = Square::new(file, 5); // White piece at rank 5

        pos.board.put_piece(black_sq, Piece::new(PieceType::Pawn, Color::Black));
        pos.board.put_piece(white_sq, Piece::new(PieceType::Pawn, Color::White));
        // Kings are required for move generation
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
        pos.side_to_move = Color::Black;

        let mut move_gen = MoveGen::new();
        let mut moves = MoveList::new();
        move_gen.generate_all(&pos, &mut moves);

        // Check that forward capture is generated
        let forward_capture = moves
            .as_slice()
            .iter()
            .any(|&mv| !mv.is_drop() && mv.from() == Some(black_sq) && mv.to() == white_sq);

        assert!(
            forward_capture,
            "Black pawn at {black_sq} should be able to capture White piece at {white_sq}"
        );
    }
}

#[test]
fn test_white_pawn_forward_capture() {
    // Test all files for White pawns capturing Black pieces
    for file in 0..9 {
        // Set up a position with White pawn on rank 2 and Black piece on rank 3
        let mut pos = Position::empty();
        let white_sq = Square::new(file, 2); // White pawn at rank 2
        let black_sq = Square::new(file, 3); // Black piece at rank 3

        pos.board.put_piece(white_sq, Piece::new(PieceType::Pawn, Color::White));
        pos.board.put_piece(black_sq, Piece::new(PieceType::Pawn, Color::Black));
        // Kings are required for move generation
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
        pos.side_to_move = Color::White;

        let mut move_gen = MoveGen::new();
        let mut moves = MoveList::new();
        move_gen.generate_all(&pos, &mut moves);

        // Check that forward capture is generated
        let forward_capture = moves
            .as_slice()
            .iter()
            .any(|&mv| !mv.is_drop() && mv.from() == Some(white_sq) && mv.to() == black_sq);

        assert!(
            forward_capture,
            "White pawn at {white_sq} should be able to capture Black piece at {black_sq}"
        );
    }
}

#[test]
fn test_pawn_blocked_by_own_piece() {
    // Test that pawns cannot move forward when blocked by their own pieces
    let mut pos = Position::empty();

    // Black pawn blocked by Black piece
    let black_pawn_sq = Square::new(4, 6);
    let black_blocker_sq = Square::new(4, 5);
    pos.board.put_piece(black_pawn_sq, Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(black_blocker_sq, Piece::new(PieceType::Lance, Color::Black));
    // Kings are required for move generation
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
    pos.side_to_move = Color::Black;

    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check that pawn cannot move forward
    let blocked_move = moves.as_slice().iter().any(|&mv| {
        !mv.is_drop() && mv.from() == Some(black_pawn_sq) && mv.to() == black_blocker_sq
    });

    assert!(
        !blocked_move,
        "Black pawn at {black_pawn_sq} should NOT be able to move to {black_blocker_sq} (blocked by own piece)"
    );
}

#[test]
fn test_pawn_forward_capture_various_pieces() {
    // Test that pawns can capture various enemy piece types
    let piece_types = [
        PieceType::Pawn,
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Rook,
    ];

    for piece_type in &piece_types {
        let mut pos = Position::empty();
        let black_pawn_sq = Square::new(4, 6);
        let target_sq = Square::new(4, 5);

        pos.board.put_piece(black_pawn_sq, Piece::new(PieceType::Pawn, Color::Black));
        pos.board.put_piece(target_sq, Piece::new(*piece_type, Color::White));
        // Kings are required for move generation
        pos.board
            .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
        pos.side_to_move = Color::Black;

        let mut move_gen = MoveGen::new();
        let mut moves = MoveList::new();
        move_gen.generate_all(&pos, &mut moves);

        let can_capture = moves
            .as_slice()
            .iter()
            .any(|&mv| !mv.is_drop() && mv.from() == Some(black_pawn_sq) && mv.to() == target_sq);

        assert!(
            can_capture,
            "Black pawn should be able to capture White {piece_type:?} at {target_sq}"
        );
    }
}

#[test]
fn test_pawn_promotion_on_capture() {
    // Test that pawns can promote when capturing in the promotion zone

    // Black pawn capturing on rank 2 (enemy territory)
    let mut pos = Position::empty();
    let black_pawn_sq = Square::new(4, 3);
    let target_sq = Square::new(4, 2);

    pos.board.put_piece(black_pawn_sq, Piece::new(PieceType::Pawn, Color::Black));
    pos.board.put_piece(target_sq, Piece::new(PieceType::Pawn, Color::White));
    // Kings are required for move generation
    pos.board
        .put_piece(Square::new(4, 8), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(4, 0), Piece::new(PieceType::King, Color::White));
    pos.side_to_move = Color::Black;

    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check for both promoting and non-promoting capture moves
    let promote_capture = moves.as_slice().iter().any(|&mv| {
        !mv.is_drop() && mv.from() == Some(black_pawn_sq) && mv.to() == target_sq && mv.is_promote()
    });

    let normal_capture = moves.as_slice().iter().any(|&mv| {
        !mv.is_drop()
            && mv.from() == Some(black_pawn_sq)
            && mv.to() == target_sq
            && !mv.is_promote()
    });

    assert!(
        promote_capture,
        "Black pawn should be able to capture with promotion at {target_sq}"
    );
    assert!(
        normal_capture,
        "Black pawn should be able to capture without promotion at {target_sq}"
    );
}

#[test]
fn test_pawn_must_promote_on_last_rank() {
    // Test that pawns must promote when capturing to the last rank

    // Black pawn capturing on rank 0 (must promote)
    let mut pos = Position::empty();
    let black_pawn_sq = Square::new(4, 1);
    let target_sq = Square::new(4, 0);

    pos.board.put_piece(black_pawn_sq, Piece::new(PieceType::Pawn, Color::Black));
    pos.board.put_piece(target_sq, Piece::new(PieceType::Pawn, Color::White));
    // Kings are required for move generation (place kings away from the action)
    pos.board
        .put_piece(Square::new(0, 8), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(8, 1), Piece::new(PieceType::King, Color::White));
    pos.side_to_move = Color::Black;

    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check that only promoting capture is generated (not non-promoting)
    let promote_capture = moves.as_slice().iter().any(|&mv| {
        !mv.is_drop() && mv.from() == Some(black_pawn_sq) && mv.to() == target_sq && mv.is_promote()
    });

    let normal_capture = moves.as_slice().iter().any(|&mv| {
        !mv.is_drop()
            && mv.from() == Some(black_pawn_sq)
            && mv.to() == target_sq
            && !mv.is_promote()
    });

    assert!(
        promote_capture,
        "Black pawn must be able to capture with promotion to last rank"
    );
    assert!(
        !normal_capture,
        "Black pawn should NOT be able to capture without promotion to last rank"
    );
}

#[test]
fn test_white_pawn_must_promote_on_last_rank() {
    // Test that white pawns must promote when capturing to the last rank

    // White pawn capturing on rank 8 (must promote)
    let mut pos = Position::empty();
    let white_pawn_sq = Square::new(4, 7);
    let target_sq = Square::new(4, 8);

    pos.board.put_piece(white_pawn_sq, Piece::new(PieceType::Pawn, Color::White));
    pos.board.put_piece(target_sq, Piece::new(PieceType::Pawn, Color::Black));
    // Kings are required for move generation (place kings away from the action)
    pos.board
        .put_piece(Square::new(0, 8), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(Square::new(8, 0), Piece::new(PieceType::King, Color::White));
    pos.side_to_move = Color::White;

    let mut move_gen = MoveGen::new();
    let mut moves = MoveList::new();
    move_gen.generate_all(&pos, &mut moves);

    // Check that only promoting capture is generated (not non-promoting)
    let promote_capture = moves.as_slice().iter().any(|&mv| {
        !mv.is_drop() && mv.from() == Some(white_pawn_sq) && mv.to() == target_sq && mv.is_promote()
    });

    let normal_capture = moves.as_slice().iter().any(|&mv| {
        !mv.is_drop()
            && mv.from() == Some(white_pawn_sq)
            && mv.to() == target_sq
            && !mv.is_promote()
    });

    assert!(
        promote_capture,
        "White pawn must be able to capture with promotion to last rank"
    );
    assert!(
        !normal_capture,
        "White pawn should NOT be able to capture without promotion to last rank"
    );
}
