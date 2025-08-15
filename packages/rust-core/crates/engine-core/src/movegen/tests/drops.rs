//! Drop move tests

use crate::{
    movegen::generator::MoveGenImpl, usi::parse_usi_square, Color, Piece, PieceType, Position,
};

#[test]
fn test_movegen_drop_pawn_mate() {
    let mut pos = Position::empty();
    // White king with no escape squares - White is at top (rank 0)
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(parse_usi_square("4a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(parse_usi_square("6b").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(parse_usi_square("4b").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Black has a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // Pawn

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    // Pawn drop at 5b would be checkmate - should not be allowed
    let sq_5b = parse_usi_square("5b").unwrap(); // 5b = file 5 (index 4), rank b (index 1)
    let illegal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5b);
    assert!(illegal_drop.is_none(), "Drop pawn mate should not be allowed");
}

#[test]
fn test_drop_pawn_mate_with_escape() {
    let mut pos = Position::empty();
    // White king with escape square - White is at top
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    // No piece at (5, 0) - king can escape there

    // Black has a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // Pawn

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    let sq_5b = parse_usi_square("5b").unwrap();
    // Pawn drop at 5b gives check but king can escape - should be allowed
    let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5b);
    assert!(legal_drop.is_some(), "Non-mate pawn drop should be allowed");
}

#[test]
fn test_drop_pawn_mate_with_capture() {
    let mut pos = Position::empty();
    // White king trapped - White is at top
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board
        .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    pos.board
        .put_piece(parse_usi_square("4a").unwrap(), Piece::new(PieceType::Gold, Color::Black));
    // White gold that can capture the pawn
    pos.board
        .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Gold, Color::White));

    // Black has a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // Pawn

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    let sq_5b = parse_usi_square("5b").unwrap();
    // Pawn drop at 5b can be captured - should be allowed
    let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5b);
    assert!(legal_drop.is_some(), "Capturable pawn drop should be allowed");
}

#[test]
fn test_drop_pawn_mate_without_support() {
    let mut pos = Position::empty();
    // Black king far away - pawn has no support
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black has a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // Pawn

    let mut gen = MoveGenImpl::new(&pos);
    let moves = gen.generate_all();

    let sq_4g = parse_usi_square("5h").unwrap();
    // Pawn drop at 4g has no support - king can capture it - should be allowed
    let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_4g);
    assert!(legal_drop.is_some(), "Unsupported pawn drop should be allowed");
}

#[test]
fn test_drop_pawn_mate_pinned_defender() {
    let mut pos = Position::empty();

    // Black to move - testing Black's pawn drop
    pos.side_to_move = Color::Black;

    // Create a scenario where:
    // - White king is trapped with no escape squares
    // - A pawn drop would give check
    // - The only defender (silver) is pinned

    // White pieces (now at top of board)
    pos.board
        .put_piece(parse_usi_square("1b").unwrap(), Piece::new(PieceType::King, Color::White)); // 1b
    pos.board
        .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Silver, Color::White)); // 2b - will be pinned

    // Black pieces
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Rook, Color::Black)); // 5b - pins silver
    pos.board
        .put_piece(parse_usi_square("1d").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 1d - supports pawn
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9i - far away

    // Block escape squares for White king
    pos.board
        .put_piece(parse_usi_square("2a").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 2a
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 1a
    pos.board
        .put_piece(parse_usi_square("2c").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 2c - block escape

    // Black has a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // Pawn

    // Rebuild occupancy bitboards after manual piece placement
    pos.board.rebuild_occupancy_bitboards();

    let mut gen = MoveGenImpl::new(&pos);

    // Try to drop pawn at 1c (would give check to king at 1b)
    let sq_1c = parse_usi_square("1c").unwrap(); // 1c = file 1 (index 8), rank c (index 2)

    // Verify that drop pawn mate is detected
    assert!(gen.is_drop_pawn_mate(sq_1c, Color::White), "Drop pawn mate should be detected");

    let moves = gen.generate_all();

    // Pawn drop at 1c - silver is pinned and cannot capture - should not be allowed
    let illegal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_1c);
    assert!(
        illegal_drop.is_none(),
        "Drop pawn mate with pinned defender should not be allowed"
    );
}

#[test]
fn test_drop_pawn_not_mate_with_escape() {
    let mut pos = Position::empty();

    // 玉に逃げ道があるケース（打ち歩詰めではない）
    pos.side_to_move = Color::Black;

    // 後手の配置
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::King, Color::White)); // 5h
    pos.board
        .put_piece(parse_usi_square("6h").unwrap(), Piece::new(PieceType::Silver, Color::White)); // 6h

    // 先手の配置
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5f - 歩を支える
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9a
                                                                                                // 逃げ場の一部をブロック（でも完全ではない）
    pos.board
        .put_piece(parse_usi_square("6i").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 6i

    // 先手が歩を持っている
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    let mut gen = MoveGenImpl::new(&pos);

    // 5gに歩を打つ
    let sq_5g = parse_usi_square("5g").unwrap();

    // 打ち歩詰めではないことを確認（5iに逃げられる）
    assert!(
        !gen.is_drop_pawn_mate(sq_5g, Color::White),
        "Should not be drop pawn mate when king has escape squares"
    );

    let moves = gen.generate_all();
    let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5g);
    assert!(legal_drop.is_some(), "Pawn drop should be allowed when king can escape");
}

#[test]
fn test_drop_pawn_not_mate_can_capture_with_promoted() {
    let mut pos = Position::empty();

    // 成り駒が歩を取れるケース
    pos.side_to_move = Color::Black;

    // 後手の配置
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::King, Color::White)); // 5h
    pos.board.put_piece(
        parse_usi_square("6g").unwrap(),
        Piece::promoted(PieceType::Silver, Color::White),
    ); // 6g - 成銀

    // 先手の配置
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5f - 歩を支える
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9a

    // 玉の逃げ場をブロック
    pos.board
        .put_piece(parse_usi_square("6h").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 6h
    pos.board
        .put_piece(parse_usi_square("6i").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 6i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5i
    pos.board
        .put_piece(parse_usi_square("4i").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 4i
    pos.board
        .put_piece(parse_usi_square("4h").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 4h

    // 先手が歩を持っている
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    let mut gen = MoveGenImpl::new(&pos);

    // 5gに歩を打つ
    let sq_5g = parse_usi_square("5g").unwrap();

    // 打ち歩詰めではないことを確認（成銀が取れる）
    assert!(
        !gen.is_drop_pawn_mate(sq_5g, Color::White),
        "Should not be drop pawn mate when promoted piece can capture"
    );

    let moves = gen.generate_all();
    let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5g);
    assert!(
        legal_drop.is_some(),
        "Pawn drop should be allowed when promoted piece can capture"
    );
}

#[test]
fn test_drop_pawn_not_mate_long_range_defender() {
    let mut pos = Position::empty();

    // 遠距離からの守りのケース（飛車）
    pos.side_to_move = Color::Black;

    // 後手の配置
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::King, Color::White)); // 5h
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Rook, Color::White)); // 5d - 遠くから守る

    // 先手の配置
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5f - 歩を支える
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9a

    // 先手が歩を持っている
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    let mut gen = MoveGenImpl::new(&pos);

    // 5gに歩を打つ
    let sq_5g = parse_usi_square("5g").unwrap();

    // 打ち歩詰めではないことを確認（飛車が取れる）
    assert!(
        !gen.is_drop_pawn_mate(sq_5g, Color::White),
        "Should not be drop pawn mate when rook can capture from distance"
    );

    let moves = gen.generate_all();
    let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5g);
    assert!(legal_drop.is_some(), "Pawn drop should be allowed when rook can capture");
}

#[test]
fn test_drop_pawn_mate_at_edge() {
    let mut pos = Position::empty();

    // エッジケース：盤端（1筋）での打ち歩詰め
    pos.side_to_move = Color::Black;

    // 後手の配置（1筋の端、rank 0）
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White)); // 1a
    pos.board
        .put_piece(parse_usi_square("2a").unwrap(), Piece::new(PieceType::Silver, Color::White)); // 2a - ピンされる

    // 先手の配置
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::Black)); // 5a - 銀をピン
    pos.board
        .put_piece(parse_usi_square("1c").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 1c - 歩を支える
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black)); // 9i

    // 玉の逃げ場をブロック（盤端なので元々限定的）
    pos.board
        .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 2b

    // 先手が歩を持っている
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards after manual piece placement
    pos.board.rebuild_occupancy_bitboards();

    let mut gen = MoveGenImpl::new(&pos);

    // 1bに歩を打つ
    let sq_1b = parse_usi_square("1b").unwrap();

    // 打ち歩詰めが検出されることを確認
    assert!(
        gen.is_drop_pawn_mate(sq_1b, Color::White),
        "Drop pawn mate at board edge should be detected"
    );

    let moves = gen.generate_all();
    let illegal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_1b);
    assert!(illegal_drop.is_none(), "Drop pawn mate at board edge should not be allowed");
}

#[test]
fn test_drop_pawn_false_positive_cases() {
    let mut pos = Position::empty();

    // 偽陽性防止：歩を支える駒がピンされている
    pos.side_to_move = Color::Black;

    // 後手の配置
    pos.board
        .put_piece(parse_usi_square("5h").unwrap(), Piece::new(PieceType::King, Color::White)); // 5h
    pos.board
        .put_piece(parse_usi_square("8h").unwrap(), Piece::new(PieceType::Rook, Color::White)); // 8h

    // 先手の配置
    pos.board
        .put_piece(parse_usi_square("5f").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5f - 歩を支えるが...
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black)); // 5a - 金がピンされている！

    // 先手が歩を持っている
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    let mut gen = MoveGenImpl::new(&pos);

    // 5gに歩を打つ
    let sq_5g = parse_usi_square("5g").unwrap();

    // 打ち歩詰めではないことを確認（歩に紐がついていない - 金がピンされているため）
    assert!(
        !gen.is_drop_pawn_mate(sq_5g, Color::White),
        "Should not be drop pawn mate when supporting piece is pinned"
    );

    let moves = gen.generate_all();
    let legal_drop = moves.as_slice().iter().find(|m| m.is_drop() && m.to() == sq_5g);
    assert!(
        legal_drop.is_some(),
        "Pawn drop should be allowed when support is invalid due to pin"
    );
}
