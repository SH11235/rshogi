//! Check and evasion tests

use crate::{movegen::MoveGenerator, usi::parse_usi_square, Color, Piece, PieceType, Position};

#[test]
fn test_movegen_in_check() {
    let mut pos = Position::empty();
    // Black king in check from white rook
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("6b").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // In check, only king moves and blocking moves are legal
    assert!(pos.is_in_check()); // Verify we detect check

    // King should be able to move to escape
    let king_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| m.from() == Some(parse_usi_square("5a").unwrap()))
        .collect();
    assert!(!king_moves.is_empty());

    // Gold can block the check
    let gold_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| m.from() == Some(parse_usi_square("6b").unwrap()))
        .collect();
    let block_move = gold_moves.iter().find(|m| {
        m.to() == parse_usi_square("5b").unwrap() || m.to() == parse_usi_square("5c").unwrap()
    });
    assert!(block_move.is_some());
}

#[test]
fn test_check_evasion_king_moves() {
    // 王手回避：玉の移動で逃げる
    let mut pos = Position::empty();

    // 先手玉が5五、後手飛車が5八で王手
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::White));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // 王手されているので、玉が移動するか、飛車の利きを遮るしかない
    // 玉は飛車の利きから逃げる必要がある
    let king_moves: Vec<_> = moves
        .as_slice()
        .iter()
        .filter(|m| !m.is_drop() && m.from() == Some(parse_usi_square("5e").unwrap()))
        .collect();

    // 玉の逃げ場所を確認（5筋以外）
    for m in &king_moves {
        assert_ne!(m.to().file(), 4, "King should not move on the same file as the rook");
    }
}

#[test]
fn test_check_evasion_block() {
    // 王手回避：合駒で防ぐ
    let mut pos = Position::empty();

    // 先手玉が5一、後手飛車が5八で王手、先手は金を持っている
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.hands[Color::Black as usize][PieceType::Gold.hand_index().unwrap()] = 1; // 金を持っている

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // 5筋に金を打って飛車の利きを遮る手があるはず
    let block_drops: Vec<_> =
        moves.as_slice().iter().filter(|m| m.is_drop() && m.to().file() == 4).collect();

    assert!(!block_drops.is_empty(), "Should be able to block with a drop");
}

#[test]
fn test_check_evasion_capture() {
    // 王手回避：王手している駒を取る
    let mut pos = Position::empty();

    // 先手玉が5五、後手金が4四で王手、先手銀が3三
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("4f").unwrap(), Piece::new(PieceType::Gold, Color::White));
    pos.board
        .put_piece(parse_usi_square("3g").unwrap(), Piece::new(PieceType::Silver, Color::Black));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // 銀で金を取る手があるはず
    let capture_move = moves.as_slice().iter().find(|m| {
        !m.is_drop()
            && m.from() == Some(parse_usi_square("3g").unwrap())
            && m.to() == parse_usi_square("4f").unwrap()
    });

    assert!(capture_move.is_some(), "Should be able to capture the checking piece");
}

#[test]
fn test_double_check_only_king_moves() {
    // 両王手の場合は玉が逃げるしかない
    let mut pos = Position::empty();

    // 先手玉が5五、後手飛車が5八と角が1九で両王手
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::White));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::Bishop, Color::White));

    let gen = MoveGenerator::new();
    let moves = gen.generate_all(&pos).expect("Failed to generate moves");

    // 全ての手が玉の移動であることを確認
    for m in moves.as_slice() {
        if !m.is_drop() {
            assert_eq!(
                m.from(),
                Some(parse_usi_square("5e").unwrap()),
                "Only king moves allowed in double check"
            );
        } else {
            panic!("No drops allowed in double check");
        }
    }
}
