//! Tests ensuring single-check detection (checkers) has correct color orientation
//! for Knight, Gold, and Silver, and that non-king capture moves are generated.

use crate::movegen::MoveGenerator;
use crate::shogi::{Color, Piece, PieceType, Position};
use crate::usi::{parse_sfen, parse_usi_move, parse_usi_square};

fn pos_with_black_king_at_5i() -> Position {
    let mut pos = Position::empty();
    // Black king at 5i
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board.rebuild_occupancy_bitboards();
    pos.side_to_move = Color::Black;
    pos
}

#[test]
fn smoke_sfen_has_2c2d_after_knight_drop() {
    // SFEN 出典: ログの position_error 行に記録された局面
    //   sfen: lnp+R3Gl/8k/4pgspp/p4ppN1/9/1P5P1/P1NG1PP1P/2K2S1R1/L2G3NL w 5P2b2s 38
    // 期待: 合法手に 2c2d（同歩。2dの桂に対する取り）が含まれる。
    let sfen = "lnp+R3Gl/8k/4pgspp/p4ppN1/9/1P5P1/P1NG1PP1P/2K2S1R1/L2G3NL w 5P2b2s 38";
    let pos = parse_sfen(sfen).expect("valid SFEN from log");
    assert_eq!(pos.side_to_move, Color::White, "side-to-move must be White");

    let mg = MoveGenerator::new();
    let moves = mg.generate_all(&pos).expect("generate_all");
    let want = parse_usi_move("2c2d").unwrap();
    let exists = moves
        .as_slice()
        .iter()
        .any(|&m| m.from() == want.from() && m.to() == want.to());
    assert!(exists, "legal moves must contain 2c2d (pawn capture of dropped knight)");
}

#[test]
fn smoke_no_mate_after_6e6b_in_logged_line() {
    // 再現: ログの position 行（… 7e6e 6f5g+ で終わる）を復元し、
    // そこから 6e6b を指した後に「相手がチェック中かつ合法手が存在する」ことを確認する。
    // これにより “score mate 3” 表示が実局面では詰みでなかった可能性をスモークで検知する。
    let moves_str = "3i4h 3c3d 5g5f 4a3b 2g2f 8c8d 2f2e 8d8e 5i5h 3a4b 2e2d 2c2d 2h2d 4b3c 2d2f P*2c 1i1h 7a7b 5h5i 6a5b 9g9f 7b8c 7i7h 5a4b 6g6f 4b3a 9f9e 7c7d P*2g 7d7e 2f2e 8c7d 8h7i 6c6d 6i5h 8e8f 8g8f 8b8f 9e9d 9c9d 1g1f 8f6f P*8b 8a7c 8b8a+ 9a9b 5f5e 4c4d 2e2f 7d6e 2f6f 6e6f R*7a R*4a 7a7c+ 4a8a 5e5d 5c5d 7c7b 8a5a 7b9b 5b4c 9b9d P*9f 9d6d P*6e 6d6e 6f5e 6e7e 9f9g+ 9i9g P*6f L*5g 6f6g+ 7h6g P*6e 6g7h 5e6f 7e6e 6f5g+";
    let mut moves: Vec<String> = moves_str.split_whitespace().map(|s| s.to_string()).collect();
    let mut pos = crate::usi::create_position(true, None, &moves).expect("rebuild pos");

    // 6e6b を適用
    let mv = parse_usi_move("6e6b").unwrap();
    let undo = pos.do_move(mv);

    // 相手番の局面で、少なくとも1手は合法手が存在する（=即詰みではない）ことを確認
    let mg = MoveGenerator::new();
    let legal = mg.generate_all(&pos).expect("generate_all");
    let has_legal = !legal.is_empty();

    // 後始末
    pos.undo_move(mv, undo);

    assert!(has_legal, "after 6e6b: opponent must have at least one legal move (not mate-in-1)");
}

#[test]
fn knight_check_single_capture_allowed() {
    // Setup: Black king 5i, White knight at 4g gives check to 5i.
    // Black gold at 5h can capture the checking knight (5h4g).
    let mut pos = pos_with_black_king_at_5i();
    pos.board.put_piece(
        parse_usi_square("4g").unwrap(),
        Piece::new(PieceType::Knight, Color::White),
    );
    pos.board.put_piece(
        parse_usi_square("5h").unwrap(),
        Piece::new(PieceType::Gold, Color::Black),
    );
    pos.board.rebuild_occupancy_bitboards();

    // Sanity: side to move is Black (in check)
    assert!(pos.is_in_check());

    let mg = MoveGenerator::new();
    let moves = mg.generate_all(&pos).expect("generate_all");
    let want = parse_usi_move("5h4g").unwrap();
    assert!(
        moves.as_slice().iter().any(|&m| m.from() == want.from() && m.to() == want.to()),
        "non-king capture of checking knight (5h4g) must be generated"
    );
}

#[test]
fn gold_check_single_capture_allowed() {
    // Setup: Black king 5i, White gold at 5h gives check to 5i.
    // Black gold at 5g can capture the checking gold (5g5h).
    let mut pos = pos_with_black_king_at_5i();
    pos.board.put_piece(
        parse_usi_square("5h").unwrap(),
        Piece::new(PieceType::Gold, Color::White),
    );
    pos.board.put_piece(
        parse_usi_square("5g").unwrap(),
        Piece::new(PieceType::Gold, Color::Black),
    );
    pos.board.rebuild_occupancy_bitboards();

    assert!(pos.is_in_check());
    let mg = MoveGenerator::new();
    let moves = mg.generate_all(&pos).expect("generate_all");
    let want = parse_usi_move("5g5h").unwrap();
    assert!(
        moves.as_slice().iter().any(|&m| m.from() == want.from() && m.to() == want.to()),
        "non-king capture of checking gold (5g5h) must be generated"
    );
}

#[test]
fn silver_check_single_capture_allowed() {
    // Setup: Black king 5i, White silver at 4h gives check to 5i (white forward-right).
    // Black gold at 5h can capture the checking silver (5h4h).
    let mut pos = pos_with_black_king_at_5i();
    pos.board.put_piece(
        parse_usi_square("4h").unwrap(),
        Piece::new(PieceType::Silver, Color::White),
    );
    pos.board.put_piece(
        parse_usi_square("5h").unwrap(),
        Piece::new(PieceType::Gold, Color::Black),
    );
    pos.board.rebuild_occupancy_bitboards();

    assert!(pos.is_in_check());
    let mg = MoveGenerator::new();
    let moves = mg.generate_all(&pos).expect("generate_all");
    let want = parse_usi_move("5h4h").unwrap();
    assert!(
        moves.as_slice().iter().any(|&m| m.from() == want.from() && m.to() == want.to()),
        "non-king capture of checking silver (5h4h) must be generated"
    );
}
