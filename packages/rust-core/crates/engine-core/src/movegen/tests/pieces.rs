//! Piece-specific move generation tests

use crate::{movegen::MoveGenerator, usi::parse_usi_square, Color, Piece, PieceType, Position};

#[test]
fn test_movegen_pawn_moves() {
    let mut pos = Position::empty();
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    // Black pawn on rank 5 (not in promotion zone)
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // Should include only one pawn move (not in promotion zone)
    let pawn_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| m.from() == Some(parse_usi_square("5f").unwrap()))
        .collect();
    assert_eq!(pawn_moves.len(), 1);
    assert!(!pawn_moves[0].is_promote());
}

#[test]
fn test_movegen_pawn_promotion() {
    let mut pos = Position::empty();
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    // Black pawn in promotion zone (rank 2, can move to rank 1)
    pos.board
        .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Pawn, Color::Black));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // Should include pawn moves (both promoted and unpromoted since it's in promotion zone)
    let pawn_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| m.from() == Some(parse_usi_square("5c").unwrap()))
        .collect();
    assert_eq!(pawn_moves.len(), 2); // One promoted, one unpromoted

    // Check that we have both promoted and unpromoted moves
    let promoted_count = pawn_moves.iter().filter(|m| m.is_promote()).count();
    let unpromoted_count = pawn_moves.iter().filter(|m| !m.is_promote()).count();
    assert_eq!(promoted_count, 1);
    assert_eq!(unpromoted_count, 1);
}

#[test]
fn test_forced_promotion_pawn() {
    // 歩の1段目成り強制
    let mut pos = Position::empty();

    // 先手歩を2段目に配置 (Black pawn on rank 1, moving to rank 0)
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // 歩が1段目に進む手は必ず成り (Black pawn moving to rank 0 must promote)
    let pawn_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| {
            !m.is_drop()
                && m.from() == Some(parse_usi_square("5b").unwrap())
                && m.to() == parse_usi_square("5a").unwrap()
        })
        .collect();

    assert_eq!(pawn_moves.len(), 1);
    assert!(pawn_moves[0].is_promote(), "Black pawn must promote on rank 0");
}

#[test]
fn test_forced_promotion_lance() {
    // 香車の1段目成り強制
    let mut pos = Position::empty();

    // 先手香を2段目に配置 (Black lance on rank 1, moving to rank 0)
    pos.board
        .put_piece(parse_usi_square("9b").unwrap(), Piece::new(PieceType::Lance, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // 香が1段目に進む手は必ず成り (Black lance moving to rank 0 must promote)
    // Find all lance moves and check they properly handle forced promotion
    let lance_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("9b").unwrap()))
        .collect();

    // At least one move should exist
    assert!(!lance_moves.is_empty(), "Lance should have at least one move");

    // Any move to rank 0 must be promoted
    for mv in &lance_moves {
        if mv.to() == parse_usi_square("9a").unwrap() {
            assert!(mv.is_promote(), "Black lance must promote when moving to rank 0");
        }
    }
}

#[test]
fn test_forced_promotion_knight() {
    // 桂馬の2段目成り強制
    let mut pos = Position::empty();

    // 先手桂を3段目に配置 (Black knight on rank 2)
    pos.board
        .put_piece(parse_usi_square("8c").unwrap(), Piece::new(PieceType::Knight, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // 桂が1段目に進む手は必ず成り (Black knight moving to rank 0)
    let knight_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("8c").unwrap()))
        .collect();

    // Black knight jumps 2 ranks forward (toward rank 0)
    for m in &knight_moves {
        if m.to().rank() == 0 {
            assert!(m.is_promote(), "Black knight must promote on rank 0");
        }
    }
}

#[test]
fn test_movegen_pinned_piece() {
    let mut pos = Position::empty();
    // Black gold is pinned by white rook
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Rook, Color::White));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // Pinned gold can only move along the pin ray (file 4)
    let gold_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| m.from() == Some(parse_usi_square("5c").unwrap()))
        .collect();

    // All gold moves should be on file 4
    for m in &gold_moves {
        assert_eq!(m.to().file(), 4);
    }
}

#[test]
fn test_pin_restriction() {
    // ピンされた駒の移動制限
    let mut pos = Position::empty();

    // 先手玉5一、先手金5五、後手飛車5九でピン
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::White));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // 金の動きは5筋のみ（ピンの方向）
    let gold_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5e").unwrap()))
        .collect();

    for m in &gold_moves {
        assert_eq!(m.to().file(), 4, "Pinned piece can only move along pin ray");
    }
}
