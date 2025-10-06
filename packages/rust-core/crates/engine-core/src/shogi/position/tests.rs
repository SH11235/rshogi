//! Tests for Position functionality

use crate::shogi::board::{Color, PieceType};
use crate::shogi::moves::Move;
use crate::usi::parse_usi_square;

use super::Position;

#[test]
fn test_startpos() {
    let pos = Position::startpos();

    // Check king positions
    assert_eq!(pos.board.king_square(Color::Black), Some(parse_usi_square("5i").unwrap()));
    assert_eq!(pos.board.king_square(Color::White), Some(parse_usi_square("5a").unwrap()));

    // Check pawn count
    assert_eq!(
        pos.board.piece_bb[Color::Black as usize][PieceType::Pawn as usize].count_ones(),
        9
    );
    assert_eq!(
        pos.board.piece_bb[Color::White as usize][PieceType::Pawn as usize].count_ones(),
        9
    );

    // No pieces in hand at start
    for color in 0..2 {
        for piece_type in 0..7 {
            assert_eq!(pos.hands[color][piece_type], 0);
        }
    }
}

#[test]
fn king_bitboard_stays_in_sync_for_prefix_sequence() {
    let mut pos = Position::startpos();

    let moves: [&str; 61] = [
        "7i6h", "3c3d", "5g5f", "4a3b", "3i4h", "3a4b", "2h1h", "4b3c", "7g7f", "8c8d", "7f7e",
        "8d8e", "6h7g", "7a7b", "5i6h", "5a4b", "6h5i", "4b3a", "4g4f", "7b8c", "6i6h", "6a5b",
        "6g6f", "7c7d", "7e7d", "8c7d", "P*7i", "7d7e", "1h2h", "4c4d", "6h6g", "5b4c", "7g6h",
        "P*7f", "5i5h", "8e8f", "6f6e", "8f8g+", "8h5e", "8b9b", "5h5g", "7f7g+", "5e7c+", "8a7c",
        "8i7g", "8g7g", "4f4e", "7g6g", "6h6g", "7c6e", "5g4g", "4d4e", "P*8d", "4e4f", "4g3h",
        "7e8d", "6g6f", "B*8g", "6f7e", "8d7e", "3h3i",
    ];

    let mut undo_stack = Vec::with_capacity(moves.len());

    for (ply, usi) in moves.iter().enumerate() {
        let mv = Move::from_usi(usi).unwrap_or_else(|e| panic!("failed to parse move {usi}: {e}"));
        let undo = pos.do_move(mv);
        undo_stack.push((mv, undo));

        let black_sq = pos
            .board
            .king_square(Color::Black)
            .unwrap_or_else(|| panic!("black king missing at ply {}", ply + 1));
        let white_sq = pos
            .board
            .king_square(Color::White)
            .unwrap_or_else(|| panic!("white king missing at ply {}", ply + 1));

        let black_piece = pos
            .board
            .piece_on(black_sq)
            .unwrap_or_else(|| panic!("no piece on reported black king square at ply {}", ply + 1));
        let white_piece = pos
            .board
            .piece_on(white_sq)
            .unwrap_or_else(|| panic!("no piece on reported white king square at ply {}", ply + 1));

        assert_eq!(
            black_piece.piece_type,
            PieceType::King,
            "non-king piece detected on black king square after move {} (ply {})",
            usi,
            ply + 1
        );
        assert_eq!(
            white_piece.piece_type,
            PieceType::King,
            "non-king piece detected on white king square after move {} (ply {})",
            usi,
            ply + 1
        );
    }

    for (ply, (mv, undo)) in undo_stack.into_iter().enumerate().rev() {
        pos.undo_move(mv, undo);

        let black_sq = pos
            .board
            .king_square(Color::Black)
            .unwrap_or_else(|| panic!("black king missing after undo at ply {}", ply));
        let white_sq = pos
            .board
            .king_square(Color::White)
            .unwrap_or_else(|| panic!("white king missing after undo at ply {}", ply));

        let black_piece = pos.board.piece_on(black_sq).unwrap_or_else(|| {
            panic!("no piece on reported black king square after undo at ply {}", ply)
        });
        let white_piece = pos.board.piece_on(white_sq).unwrap_or_else(|| {
            panic!("no piece on reported white king square after undo at ply {}", ply)
        });

        assert_eq!(
            black_piece.piece_type,
            PieceType::King,
            "non-king piece detected on black king square after undo of move {} (ply {})",
            crate::usi::move_to_usi(&mv),
            ply
        );
        assert_eq!(
            white_piece.piece_type,
            PieceType::King,
            "non-king piece detected on white king square after undo of move {} (ply {})",
            crate::usi::move_to_usi(&mv),
            ply
        );
    }
}

#[test]
fn test_count_piece_on_board() {
    // Test with starting position
    let pos = Position::startpos();

    // Check piece counts
    assert_eq!(pos.count_piece_on_board(PieceType::King), 2);
    assert_eq!(pos.count_piece_on_board(PieceType::Rook), 2);
    assert_eq!(pos.count_piece_on_board(PieceType::Bishop), 2);
    assert_eq!(pos.count_piece_on_board(PieceType::Gold), 4);
    assert_eq!(pos.count_piece_on_board(PieceType::Silver), 4);
    assert_eq!(pos.count_piece_on_board(PieceType::Knight), 4);
    assert_eq!(pos.count_piece_on_board(PieceType::Lance), 4);
    assert_eq!(pos.count_piece_on_board(PieceType::Pawn), 18);

    // Test with empty position
    let empty_pos = Position::empty();
    assert_eq!(empty_pos.count_piece_on_board(PieceType::Rook), 0);
    assert_eq!(empty_pos.count_piece_on_board(PieceType::Pawn), 0);
}

#[test]
fn test_count_piece_in_hand() {
    let mut pos = Position::empty();

    // Add some pieces to hands
    pos.hands[Color::Black as usize][PieceType::Rook.hand_index().unwrap()] = 1; // Rook
    pos.hands[Color::Black as usize][PieceType::Bishop.hand_index().unwrap()] = 2; // Bishop
    pos.hands[Color::White as usize][PieceType::Pawn.hand_index().unwrap()] = 5; // Pawn

    // Test counts
    assert_eq!(pos.count_piece_in_hand(Color::Black, PieceType::Rook), 1);
    assert_eq!(pos.count_piece_in_hand(Color::Black, PieceType::Bishop), 2);
    assert_eq!(pos.count_piece_in_hand(Color::Black, PieceType::Pawn), 0);
    assert_eq!(pos.count_piece_in_hand(Color::White, PieceType::Pawn), 5);

    // King should always return 0
    assert_eq!(pos.count_piece_in_hand(Color::Black, PieceType::King), 0);
    assert_eq!(pos.count_piece_in_hand(Color::White, PieceType::King), 0);
}
