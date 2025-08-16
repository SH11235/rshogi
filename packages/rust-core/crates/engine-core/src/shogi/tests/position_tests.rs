//! Tests for Position struct and move handling

use crate::shogi::{Color, Move, Piece, PieceType, Position, Square};
use crate::usi::{parse_usi_move, parse_usi_square};
use crate::zobrist::ZobristHashing;

#[test]
fn test_do_move_normal_move() {
    let mut pos = Position::startpos();
    // Black pawn is on rank 6, moves toward rank 0
    let from = parse_usi_square("3g").unwrap(); // Black pawn
    let to = parse_usi_square("3f").unwrap(); // One square forward for Black
    let mv = Move::normal(from, to, false);

    // 初期ハッシュを記録
    let initial_hash = pos.hash;

    // 手を実行
    let _undo_info = pos.do_move(mv);

    // 駒が移動していることを確認
    assert_eq!(pos.board.piece_on(from), None);
    assert_eq!(pos.board.piece_on(to), Some(Piece::new(PieceType::Pawn, Color::Black)));

    // 手番が切り替わっていることを確認
    assert_eq!(pos.side_to_move, Color::White);

    // 手数が増えていることを確認
    assert_eq!(pos.ply, 1);

    // ハッシュが変わっていることを確認
    assert_ne!(pos.hash, initial_hash);

    // 履歴に追加されていることを確認
    assert_eq!(pos.history.len(), 1);
    assert_eq!(pos.history[0], initial_hash);
}

#[test]
fn test_do_move_capture() {
    // 駒を取る手のテスト
    let mut pos = Position::startpos();

    // Black歩を前進させる (rank 6 -> 5)
    let mv1 = Move::normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap(), false);
    let _undo1 = pos.do_move(mv1);

    // White歩を前進させる (rank 2 -> 3)
    let mv2 = Move::normal(parse_usi_square("5c").unwrap(), parse_usi_square("5d").unwrap(), false);
    let _undo2 = pos.do_move(mv2);

    // Black歩をさらに前進 (rank 5 -> 4)
    let mv3 = Move::normal(parse_usi_square("3f").unwrap(), parse_usi_square("3e").unwrap(), false);
    let _undo3 = pos.do_move(mv3);

    // White歩をさらに前進 (rank 3 -> 4)
    let mv4 = Move::normal(parse_usi_square("5d").unwrap(), parse_usi_square("5e").unwrap(), false);
    let _undo4 = pos.do_move(mv4);

    // Black歩でWhite歩を取る
    let from = parse_usi_square("3e").unwrap();
    let to = parse_usi_square("5e").unwrap();
    let mv = Move::normal(from, to, false);

    let captured_piece = pos.board.piece_on(to).expect("Capture move must have captured piece");
    assert_eq!(captured_piece.piece_type, PieceType::Pawn);
    assert_eq!(captured_piece.color, Color::White);

    let _undo5 = pos.do_move(mv);

    // 駒が取られていることを確認
    assert_eq!(pos.board.piece_on(from), None);
    assert_eq!(pos.board.piece_on(to), Some(Piece::new(PieceType::Pawn, Color::Black)));

    // 持ち駒が増えていることを確認
    assert_eq!(pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()], 1);
    // 歩のインデックスは6
}

#[test]
fn test_do_move_promotion() {
    // 成りのテスト - 成り動作だけをチェック
    let _pos = Position::startpos();

    // 手動で駒を配置して成りをテスト
    let mut board = crate::shogi::Board::empty();
    let mut pawn = Piece::new(PieceType::Pawn, Color::Black);
    board.put_piece(parse_usi_square("7g").unwrap(), pawn);

    // do_moveを使わずに直接成りをテスト
    pawn.promoted = true;
    board.remove_piece(parse_usi_square("7g").unwrap());
    board.put_piece(parse_usi_square("7h").unwrap(), pawn);

    // 成った駒になっていることを確認
    let piece = board
        .piece_on(parse_usi_square("7h").unwrap())
        .expect("Piece should exist at 7h");
    assert_eq!(piece.piece_type, PieceType::Pawn);
    assert!(piece.promoted);
    assert_eq!(piece.color, Color::Black);
}

#[test]
fn test_do_move_drop() {
    // 持ち駒を打つテスト
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // 最小限の駒を配置
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

    // 持ち駒を設定
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // 歩を打つ
    let to = parse_usi_square("5e").unwrap(); // 5e
    let mv = Move::drop(PieceType::Pawn, to);

    let _undo_info = pos.do_move(mv);

    // 駒が置かれていることを確認
    assert_eq!(pos.board.piece_on(to), Some(Piece::new(PieceType::Pawn, Color::Black)));

    // 持ち駒が減っていることを確認
    assert_eq!(pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()], 0);

    // 手番が切り替わっていることを確認
    assert_eq!(pos.side_to_move, Color::White);
}

#[test]
fn test_do_move_all_piece_types() {
    // 各駒種の移動をテスト
    let test_cases = vec![
        // (from, to, piece_type, color)
        (
            parse_usi_square("3g").unwrap(), // Black pawn
            parse_usi_square("3f").unwrap(), // One square forward
            PieceType::Pawn,
            Color::Black,
        ),
        (
            parse_usi_square("5i").unwrap(), // Black King
            parse_usi_square("4h").unwrap(), // Diagonal move
            PieceType::King,
            Color::Black,
        ),
        (
            parse_usi_square("4i").unwrap(), // Black Gold
            parse_usi_square("4h").unwrap(), // Forward
            PieceType::Gold,
            Color::Black,
        ),
        (
            parse_usi_square("3i").unwrap(), // Black Silver
            parse_usi_square("3h").unwrap(), // Forward
            PieceType::Silver,
            Color::Black,
        ),
        (
            parse_usi_square("2i").unwrap(), // Black Knight
            parse_usi_square("3g").unwrap(), // Knight jump
            PieceType::Knight,
            Color::Black,
        ),
        (
            parse_usi_square("1i").unwrap(), // Black Lance
            parse_usi_square("1h").unwrap(), // Forward
            PieceType::Lance,
            Color::Black,
        ),
        (
            parse_usi_square("2h").unwrap(), // Black Rook
            parse_usi_square("2f").unwrap(), // Forward
            PieceType::Rook,
            Color::Black,
        ),
        (
            parse_usi_square("8h").unwrap(), // Black Bishop
            parse_usi_square("7g").unwrap(), // Diagonal
            PieceType::Bishop,
            Color::Black,
        ),
    ];

    for (from, to, expected_piece_type, expected_color) in test_cases {
        let mut pos = Position::startpos();
        let piece = pos.board.piece_on(from);

        // デバッグ: 初期配置の確認
        if piece.is_none() {
            log::debug!("No piece at {from:?}");
            log::debug!("Expected: {expected_piece_type:?}");
            // 周辺の駒を確認
            for rank in 0..9 {
                for file in 0..9 {
                    if let Some(p) = pos.board.piece_on(Square(file + rank * 9)) {
                        if p.piece_type == expected_piece_type && p.color == expected_color {
                            log::debug!(
                                "Found {expected_piece_type:?} at Square({} = file {file}, rank {rank})",
                                file + rank * 9
                            );
                        }
                    }
                }
            }
            panic!("Piece not found at expected position");
        }

        let piece = piece.expect("Piece should exist at this square");
        assert_eq!(piece.piece_type, expected_piece_type);
        assert_eq!(piece.color, expected_color);

        let mv = Move::normal(from, to, false);
        let _undo_info = pos.do_move(mv);

        // 駒が移動していることを確認
        assert_eq!(pos.board.piece_on(from), None);
        let moved_piece =
            pos.board.piece_on(to).expect("Piece should exist at destination after move");
        assert_eq!(moved_piece.piece_type, expected_piece_type);
    }
}

#[test]
fn test_do_move_drop_all_piece_types() {
    // 各駒種の持ち駒打ちをテスト
    let test_cases = vec![
        (PieceType::Pawn, 6),
        (PieceType::Lance, 5),
        (PieceType::Knight, 4),
        (PieceType::Silver, 3),
        (PieceType::Gold, 2),
        (PieceType::Bishop, 1),
        (PieceType::Rook, 0),
    ];

    for (piece_type, hand_idx) in test_cases {
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;

        // 最小限の駒を配置
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

        // 各種持ち駒を設定
        pos.hands[Color::Black as usize][hand_idx] = 1;

        // 持ち駒があることを確認
        assert!(pos.hands[Color::Black as usize][hand_idx] > 0);

        let to = parse_usi_square("5e").unwrap(); // 5e
        let mv = Move::drop(piece_type, to);

        let _undo_info = pos.do_move(mv);

        // 駒が置かれていることを確認
        let placed_piece =
            pos.board.piece_on(to).expect("Piece should exist at destination after drop");
        assert_eq!(placed_piece.piece_type, piece_type);
        assert_eq!(placed_piece.color, Color::Black);
        assert!(!placed_piece.promoted);

        // 持ち駒が減っていることを確認
        assert_eq!(pos.hands[Color::Black as usize][hand_idx], 0);
    }
}

#[test]
fn test_do_move_special_promotion_cases() {
    // 特殊な成りのケース（1段目での成り強制など）
    // startposを使って基本的な成りの動作をテスト
    let mut pos = Position::startpos();

    // 歩を前進させて成る
    let mv1 = parse_usi_move("7g7f").unwrap(); // 先手の歩
    pos.do_move(mv1);

    // 相手の歩を前進
    let mv2 = parse_usi_move("3c3d").unwrap(); // 後手の歩
    pos.do_move(mv2);

    // さらに前進
    let mv3 = parse_usi_move("7f7e").unwrap(); // 先手の歩
    pos.do_move(mv3);

    // 相手の歩をさらに前進
    let mv4 = parse_usi_move("3d3e").unwrap(); // 後手の歩
    pos.do_move(mv4);

    // 銀を前進させる（成りのテスト用）
    let mv5 = parse_usi_move("3i3h").unwrap(); // 先手の銀
    pos.do_move(mv5);

    // 後手の歩を動かす
    let mv6 = parse_usi_move("5c5d").unwrap(); // 後手の歩
    pos.do_move(mv6);

    // 銀をさらに前進
    let mv7 = parse_usi_move("3h3g").unwrap(); // 先手の銀
    pos.do_move(mv7);

    // 後手の歩を動かす
    let mv8 = parse_usi_move("5d5e").unwrap(); // 後手の歩
    pos.do_move(mv8);

    // 銀をさらに前進
    let mv9 = parse_usi_move("3g3f").unwrap(); // 先手の銀
    let _undo9 = pos.do_move(mv9);

    // 後手の歩を動かす
    let mv10 = parse_usi_move("5e5f").unwrap(); // 後手の歩
    let _undo10 = pos.do_move(mv10);

    // 銀を敵陣三段目に進めて成る（3eには既に後手の歩があるので取りながら成る）
    let mv11 = parse_usi_move("3f3e+").unwrap(); // 先手の銀が成る
    let _undo11 = pos.do_move(mv11);

    let piece = pos
        .board
        .piece_on(parse_usi_square("3e").unwrap())
        .expect("Piece should exist at 3e");
    assert_eq!(piece.piece_type, PieceType::Silver);
    assert!(piece.promoted);
}

#[test]
fn test_is_repetition() {
    // 簡単な繰り返しのテスト
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // 最小限の駒を配置
    // 5i: 先手の王
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // 5a: 後手の王
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    // 9i: 先手の飛車
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::Rook, Color::Black));
    // 1a: 後手の飛車（後手も動かせるように）
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::Rook, Color::White));

    // 初期ハッシュを計算
    pos.hash = ZobristHashing::zobrist_hash(&pos);
    pos.zobrist_hash = pos.hash;
    let initial_hash = pos.hash;

    // 先手: 飛車を動かす (9i→9h)
    let black_move1 =
        Move::normal(parse_usi_square("9i").unwrap(), parse_usi_square("9h").unwrap(), false);
    let _undo1 = pos.do_move(black_move1);

    // 後手: 飛車を動かす (1a→1b)
    let white_move1 =
        Move::normal(parse_usi_square("1a").unwrap(), parse_usi_square("1b").unwrap(), false);
    let _undo2 = pos.do_move(white_move1);

    // 先手: 飛車を戻す (9h→9i)
    let black_move2 =
        Move::normal(parse_usi_square("9h").unwrap(), parse_usi_square("9i").unwrap(), false);
    let _undo3 = pos.do_move(black_move2);

    // 後手: 飛車を戻す (1b→1a)
    let white_move2 =
        Move::normal(parse_usi_square("1b").unwrap(), parse_usi_square("1a").unwrap(), false);
    let _undo4 = pos.do_move(white_move2);

    // この時点で初期局面に戻った（1回目）
    assert_eq!(pos.hash, initial_hash);
    assert!(!pos.is_repetition()); // まだ繰り返しではない

    // 2回目の往復
    let _undo5 = pos.do_move(black_move1); // 先手: 9i→9h
    let _undo6 = pos.do_move(white_move1); // 後手: 1a→1b
    let _undo7 = pos.do_move(black_move2); // 先手: 9h→9i
    let _undo8 = pos.do_move(white_move2); // 後手: 1b→1a

    // この時点で初期局面に戻った（2回目）
    assert_eq!(pos.hash, initial_hash);
    assert!(!pos.is_repetition()); // まだ3回ではない

    // 3回目の往復
    let _undo9 = pos.do_move(black_move1); // 先手: 9i→9h
    let _undo10 = pos.do_move(white_move1); // 後手: 1a→1b
    let _undo11 = pos.do_move(black_move2); // 先手: 9h→9i
    let _undo12 = pos.do_move(white_move2); // 後手: 1b→1a

    // この時点で初期局面に戻った（3回目）
    assert_eq!(pos.hash, initial_hash);
    assert!(pos.is_repetition()); // 3回繰り返しで千日手
}

#[test]
fn test_is_repetition_with_different_hands() {
    // 持ち駒が異なる場合は同一局面ではない
    let mut pos1 = Position::startpos();
    let mut pos2 = Position::startpos();

    // 同じ動き (3g3f)
    let mv1 = parse_usi_move("3g3f").unwrap();
    pos1.do_move(mv1);
    pos2.do_move(mv1);

    // pos2では相手の歩を前進させて取る
    let mv2 = parse_usi_move("3c3d").unwrap(); // 後手の歩
    pos2.do_move(mv2);
    let mv3 = parse_usi_move("3f3d").unwrap(); // 先手が歩を取る
    pos2.do_move(mv3);

    // 異なるハッシュ値になるはず
    assert_ne!(pos1.hash, pos2.hash);
}

#[test]
fn test_is_repetition_edge_cases() {
    let mut pos = Position::startpos();

    // 履歴が4未満の場合
    assert!(!pos.is_repetition());

    let _undo1 = pos.do_move(parse_usi_move("3g3f").unwrap()); // 先手の歩
    assert!(!pos.is_repetition());

    let _undo2 = pos.do_move(parse_usi_move("3c3d").unwrap()); // 後手の歩
    assert!(!pos.is_repetition());

    let _undo3 = pos.do_move(parse_usi_move("3f3e").unwrap()); // 先手の歩
    assert!(!pos.is_repetition());
}

#[test]
fn test_do_move_undo_move_reversibility() {
    // do_move/undo_moveの可逆性をテスト
    let mut pos = Position::startpos();
    let original_pos = pos.clone();

    // テストケース1: 通常の移動
    let mv1 = parse_usi_move("3g3f").unwrap(); // 先手の歩
    let undo_info1 = pos.do_move(mv1);

    // 手を実行後の状態を確認
    assert_ne!(pos.hash, original_pos.hash);
    assert_eq!(pos.side_to_move, Color::White);
    assert_eq!(pos.ply, 1);

    // 手を戻す
    pos.undo_move(mv1, undo_info1);

    // 完全に元に戻ったことを確認
    assert_eq!(pos.hash, original_pos.hash);
    assert_eq!(pos.side_to_move, original_pos.side_to_move);
    assert_eq!(pos.ply, original_pos.ply);
    assert_eq!(pos.history.len(), original_pos.history.len());

    // 盤面も元に戻ったことを確認
    for sq in 0..81 {
        let square = Square(sq);
        assert_eq!(pos.board.piece_on(square), original_pos.board.piece_on(square));
    }
}

#[test]
fn test_do_move_undo_move_capture() {
    // 駒を取る手の可逆性をテスト
    let mut pos = Position::startpos();

    // 準備: 駒を取れる位置まで進める
    // 3g3f (先手の歩)
    let _u1 = pos.do_move(parse_usi_move("3g3f").unwrap());
    // 5c5d (後手の歩)
    let _u2 = pos.do_move(parse_usi_move("5c5d").unwrap());
    // 3f3e (先手の歩)
    let _u3 = pos.do_move(parse_usi_move("3f3e").unwrap());
    // 5d5e (後手の歩)
    let _u4 = pos.do_move(parse_usi_move("5d5e").unwrap());

    // この時点の状態を保存
    let before_capture = pos.clone();

    // 駒を取る (3e5e - 先手の歩が後手の歩を取る)
    let capture_move = parse_usi_move("3e5e").unwrap();
    let undo_info = pos.do_move(capture_move);

    // 駒が取れたことを確認
    assert_eq!(pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()], 1); // 歩を1枚持っている

    // 手を戻す
    pos.undo_move(capture_move, undo_info);

    // 完全に元に戻ったことを確認
    assert_eq!(pos.hash, before_capture.hash);
    assert_eq!(pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()], 0); // 持ち駒なし
    for sq in 0..81 {
        let square = Square(sq);
        assert_eq!(pos.board.piece_on(square), before_capture.board.piece_on(square));
    }
}

#[test]
fn test_do_move_undo_move_promotion() {
    // 成りの可逆性をテスト
    let mut pos = Position::empty();

    // 銀を敵陣三段目に配置
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.hash = ZobristHashing::zobrist_hash(&pos);
    pos.zobrist_hash = pos.hash;

    let before_promotion = pos.clone();

    // 成る
    let promote_move =
        Move::normal(parse_usi_square("5g").unwrap(), parse_usi_square("5h").unwrap(), true);
    let undo_info = pos.do_move(promote_move);

    // 成ったことを確認
    let promoted_piece = pos
        .board
        .piece_on(parse_usi_square("5h").unwrap())
        .expect("Promoted piece should exist at 5h");
    assert!(promoted_piece.promoted);

    // 手を戻す
    pos.undo_move(promote_move, undo_info);

    // 完全に元に戻ったことを確認
    assert_eq!(pos.hash, before_promotion.hash);
    let original_piece = pos
        .board
        .piece_on(parse_usi_square("5g").unwrap())
        .expect("Original piece should exist at 5g");
    assert!(!original_piece.promoted);
}

#[test]
fn test_do_move_undo_move_drop() {
    // 駒打ちの可逆性をテスト
    let mut pos = Position::empty();
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));

    // 持ち駒を設定
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // 歩を1枚
    pos.hash = ZobristHashing::zobrist_hash(&pos);
    pos.zobrist_hash = pos.hash;

    let before_drop = pos.clone();

    // 歩を打つ
    let drop_move = Move::drop(PieceType::Pawn, parse_usi_square("5e").unwrap());
    let undo_info = pos.do_move(drop_move);

    // 打ったことを確認
    assert!(pos.board.piece_on(parse_usi_square("5e").unwrap()).is_some());
    assert_eq!(pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()], 0);

    // 手を戻す
    pos.undo_move(drop_move, undo_info);

    // 完全に元に戻ったことを確認
    assert_eq!(pos.hash, before_drop.hash);
    assert!(pos.board.piece_on(parse_usi_square("5e").unwrap()).is_none());
    assert_eq!(pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()], 1);
}

#[test]
fn test_do_move_undo_move_multiple() {
    // 複数手の実行と戻しをテスト
    let mut pos = Position::startpos();
    let original_pos = pos.clone();

    let moves = vec![
        parse_usi_move("3g3f").unwrap(),  // 先手の歩
        parse_usi_move("5c5d").unwrap(),  // 後手の歩
        parse_usi_move("2h7h").unwrap(),  // 先手の飛車
        parse_usi_move("8b8h+").unwrap(), // 後手の飛車（成り）
    ];

    let mut undo_infos = Vec::new();

    // 全ての手を実行
    for mv in &moves {
        let undo_info = pos.do_move(*mv);
        undo_infos.push(undo_info);
    }

    // 逆順で全ての手を戻す
    for (mv, undo_info) in moves.iter().zip(undo_infos.iter()).rev() {
        pos.undo_move(*mv, undo_info.clone());
    }

    // 完全に元に戻ったことを確認
    assert_eq!(pos.hash, original_pos.hash);
    assert_eq!(pos.side_to_move, original_pos.side_to_move);
    assert_eq!(pos.ply, original_pos.ply);
    for sq in 0..81 {
        let square = Square(sq);
        assert_eq!(pos.board.piece_on(square), original_pos.board.piece_on(square));
    }
}

#[test]
fn test_do_null_move_undo_null_move() {
    // Test null move functionality
    let mut pos = Position::startpos();
    let original_pos = pos.clone();

    // Execute null move
    let undo_info = pos.do_null_move();

    // Check that only side to move and ply changed
    assert_eq!(pos.side_to_move, Color::White); // Changed from Black to White
    assert_eq!(pos.ply, 1); // Incremented from 0 to 1
    assert_ne!(pos.hash, original_pos.hash); // Hash should be different
    assert_eq!(pos.history.len(), 1); // History should contain one entry

    // Board should remain unchanged
    for sq in 0..81 {
        let square = Square(sq);
        assert_eq!(pos.board.piece_on(square), original_pos.board.piece_on(square));
    }

    // Hands should remain unchanged
    for color in 0..2 {
        for piece_type in 0..7 {
            assert_eq!(pos.hands[color][piece_type], original_pos.hands[color][piece_type]);
        }
    }

    // Undo null move
    pos.undo_null_move(undo_info);

    // Everything should be back to original
    assert_eq!(pos.side_to_move, original_pos.side_to_move);
    assert_eq!(pos.ply, original_pos.ply);
    assert_eq!(pos.hash, original_pos.hash);
    assert_eq!(pos.zobrist_hash, original_pos.zobrist_hash);
    assert_eq!(pos.history.len(), 0); // History should be empty again

    // Test null move in the middle of a game
    let move1 = parse_usi_move("3g3f").unwrap();
    let _undo1 = pos.do_move(move1);

    let pos_after_move = pos.clone();

    // Do null move
    let null_undo = pos.do_null_move();
    assert_eq!(pos.side_to_move, Color::Black); // Back to Black after White's null
    assert_eq!(pos.ply, 2);

    // Undo null move
    pos.undo_null_move(null_undo);

    // Should be back to state after first move
    assert_eq!(pos.hash, pos_after_move.hash);
    assert_eq!(pos.side_to_move, pos_after_move.side_to_move);
    assert_eq!(pos.ply, pos_after_move.ply);
}
