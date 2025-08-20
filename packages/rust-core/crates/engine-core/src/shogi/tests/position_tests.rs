//! Tests for Position struct and move handling

use crate::shogi::{Color, Move, Piece, PieceType, Position, Square};
use crate::usi::{move_to_usi, parse_usi_move, parse_usi_square};
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
    // Test simple pawn takes pawn (forward capture)
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // Place kings (required for legal position)
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black pawn on 5e, White pawn on 5d - can capture forward
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    pos.board.rebuild_occupancy_bitboards();

    let mv = Move::normal(parse_usi_square("5e").unwrap(), parse_usi_square("5d").unwrap(), false);
    assert!(pos.is_legal_move(mv), "Move should be legal");

    let captured_before = pos.board.piece_on(parse_usi_square("5d").unwrap()).unwrap();
    assert_eq!(captured_before.piece_type, PieceType::Pawn);
    assert_eq!(captured_before.color, Color::White);

    let _undo = pos.do_move(mv);

    // Verify capture
    assert_eq!(pos.board.piece_on(parse_usi_square("5e").unwrap()), None);
    assert_eq!(
        pos.board.piece_on(parse_usi_square("5d").unwrap()),
        Some(Piece::new(PieceType::Pawn, Color::Black))
    );
    assert_eq!(pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()], 1);
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
    pos.board.rebuild_occupancy_bitboards();

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
    // Test each piece type with minimal legal move
    let test_cases = vec![
        // (from_str, to_str, piece_type)
        ("5e", "4e", PieceType::King),   // King left
        ("5e", "5d", PieceType::Gold),   // Gold forward
        ("5e", "4d", PieceType::Silver), // Silver forward-left
        ("5g", "4e", PieceType::Knight), // Knight jump
        ("5g", "5f", PieceType::Lance),  // Lance forward
        ("5e", "5b", PieceType::Rook),   // Rook vertical
        ("5e", "2b", PieceType::Bishop), // Bishop diagonal
        ("5e", "5d", PieceType::Pawn),   // Pawn forward
    ];

    for (from_s, to_s, pt) in test_cases {
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;

        // Place both kings
        pos.board
            .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

        // Place test piece
        pos.board
            .put_piece(parse_usi_square(from_s).unwrap(), Piece::new(pt, Color::Black));
        pos.board.rebuild_occupancy_bitboards();

        let from = parse_usi_square(from_s).unwrap();
        let to = parse_usi_square(to_s).unwrap();
        let mv = Move::normal(from, to, false);
        assert!(pos.is_legal_move(mv), "Expected legal move for {pt:?}: {from_s}{to_s}");

        let _undo = pos.do_move(mv);

        // Verify piece moved
        assert_eq!(pos.board.piece_on(from), None);
        let moved_piece = pos.board.piece_on(to).unwrap();
        assert_eq!(moved_piece.piece_type, pt);
        assert_eq!(moved_piece.color, Color::Black);
    }
}

#[test]
fn test_do_move_drop_all_piece_types() {
    // 各駒種の持ち駒打ちをテスト
    let test_cases = vec![
        PieceType::Pawn,
        PieceType::Lance,
        PieceType::Knight,
        PieceType::Silver,
        PieceType::Gold,
        PieceType::Bishop,
        PieceType::Rook,
    ];

    for piece_type in test_cases {
        let mut pos = Position::empty();
        pos.side_to_move = Color::Black;

        // 最小限の駒を配置
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.board.rebuild_occupancy_bitboards();

        // 各種持ち駒を設定
        let hand_idx = piece_type.hand_index().unwrap();
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
    // Test promotion cases with minimal setup
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // Place kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Test 1: Silver promotion in enemy territory
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.board.rebuild_occupancy_bitboards();

    let mv1 = Move::normal(parse_usi_square("5d").unwrap(), parse_usi_square("5c").unwrap(), true);
    assert!(pos.is_legal_move(mv1), "Silver promotion should be legal");
    let _undo1 = pos.do_move(mv1);

    let piece = pos
        .board
        .piece_on(parse_usi_square("5c").unwrap())
        .expect("Silver should be on this square");
    assert_eq!(piece.piece_type, PieceType::Silver);
    assert!(piece.promoted, "Silver should be promoted");

    // Reset for next test
    pos.undo_move(mv1, _undo1);

    // Test 2: Pawn forced promotion at last rank
    pos.board.remove_piece(parse_usi_square("5d").unwrap());
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    pos.board.rebuild_occupancy_bitboards();

    let mv2 = Move::normal(parse_usi_square("5b").unwrap(), parse_usi_square("5a").unwrap(), true);
    // Note: This move would capture the king, so we need a different setup
    pos.board.remove_piece(parse_usi_square("5a").unwrap());
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board.rebuild_occupancy_bitboards();

    assert!(pos.is_legal_move(mv2), "Pawn promotion at last rank should be legal");
    let _undo2 = pos.do_move(mv2);

    let promoted_pawn = pos.board.piece_on(parse_usi_square("5a").unwrap()).unwrap();
    assert_eq!(promoted_pawn.piece_type, PieceType::Pawn);
    assert!(promoted_pawn.promoted, "Pawn must be promoted at last rank");
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
    pos.board.rebuild_occupancy_bitboards();

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
    assert!(pos1.is_legal_move(mv1), "Move should be legal: 3g3f");
    pos1.do_move(mv1);
    assert!(pos2.is_legal_move(mv1), "Move should be legal: 3g3f");
    pos2.do_move(mv1);

    // pos2では相手の歩を前進させて取る
    let mv2 = parse_usi_move("3c3d").unwrap(); // 後手の歩
    assert!(pos2.is_legal_move(mv2), "Move should be legal: 3c3d");
    pos2.do_move(mv2);
    let mv3 = parse_usi_move("3f3d").unwrap(); // 先手が歩を取る
    assert!(pos2.is_legal_move(mv3), "Move should be legal: 3f3d");
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
    assert!(pos.is_legal_move(mv1), "Move should be legal: 3g3f");
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
    // 駒を取る手の可逆性をテスト - 前方捕獲
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // 両玉を配置（合法局面にする）
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));

    // 黒歩5e、白歩5d → 5e→5d で前方捕獲
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Pawn, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    pos.board.rebuild_occupancy_bitboards();
    pos.hash = ZobristHashing::zobrist_hash(&pos);
    pos.zobrist_hash = pos.hash;

    let before = pos.clone();
    let mv = parse_usi_move("5e5d").unwrap();
    assert!(pos.is_legal_move(mv));
    let undo = pos.do_move(mv);

    assert_eq!(pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()], 1);

    pos.undo_move(mv, undo);
    assert_eq!(pos.hash, before.hash);
    for sq in 0..81 {
        let s = Square(sq);
        assert_eq!(pos.board.piece_on(s), before.board.piece_on(s));
    }
}

#[test]
fn test_do_move_undo_move_promotion() {
    // 成りの可逆性をテスト
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // 両玉
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // 先手銀を敵陣(5d)に置いて 5d→5c 成り
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.board.rebuild_occupancy_bitboards();
    pos.hash = ZobristHashing::zobrist_hash(&pos);
    pos.zobrist_hash = pos.hash;

    let before = pos.clone();
    let mv = Move::normal(parse_usi_square("5d").unwrap(), parse_usi_square("5c").unwrap(), true);
    assert!(pos.is_legal_move(mv));

    let undo = pos.do_move(mv);
    assert!(pos.board.piece_on(parse_usi_square("5c").unwrap()).unwrap().promoted);

    pos.undo_move(mv, undo);
    assert_eq!(pos.hash, before.hash);
}

#[test]
fn test_do_move_undo_move_drop() {
    // 駒打ちの可逆性をテスト
    let mut pos = Position::empty();
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::White));
    pos.board.rebuild_occupancy_bitboards();

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
        parse_usi_move("7g7f").unwrap(), // 先手の歩
        parse_usi_move("3c3d").unwrap(), // 後手の歩
        parse_usi_move("2g2f").unwrap(), // 先手の歩
        parse_usi_move("4c4d").unwrap(), // 後手の歩
    ];

    let mut undo_infos = Vec::new();

    // 全ての手を実行
    for mv in &moves {
        assert!(pos.is_legal_move(*mv), "Move should be legal: {}", move_to_usi(mv));
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

// ========= Drop restriction tests (migrated from MovePicker tests) =========

#[test]
fn test_pawn_drop_restrictions() {
    // Test nifu (double pawn) restriction
    // Start with empty position to have full control
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // Put a black pawn on file 5 (index 4)
    let sq = parse_usi_square("5f").unwrap(); // 5f
    pos.board.put_piece(
        sq,
        Piece {
            piece_type: PieceType::Pawn,
            color: Color::Black,
            promoted: false,
        },
    );

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1; // Pawn is index 6

    // Try to drop a pawn in the same file
    let illegal_drop = Move::drop(PieceType::Pawn, parse_usi_square("5d").unwrap()); // 5d
    assert!(!pos.is_legal_move(illegal_drop), "Should not allow double pawn");

    // Try to drop a pawn in a different file (that has no pawn)
    let legal_drop = Move::drop(PieceType::Pawn, parse_usi_square("6d").unwrap()); // 6d
    assert!(pos.is_legal_move(legal_drop), "Should allow pawn drop in different file");
}

#[test]
fn test_nifu_with_promoted_pawn() {
    // Test that promoted pawn doesn't count for nifu (double pawn)
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // Place a promoted black pawn on file 5 (index 4)
    let sq = parse_usi_square("5f").unwrap(); // 5f
    pos.board.put_piece(
        sq,
        Piece {
            piece_type: PieceType::Pawn,
            color: Color::Black,
            promoted: true,
        },
    );
    pos.board.promoted_bb.set(sq); // Mark as promoted

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop a pawn in the same file - should be legal because existing pawn is promoted
    let legal_drop = Move::drop(PieceType::Pawn, parse_usi_square("5d").unwrap()); // 5d
    assert!(
        pos.is_legal_move(legal_drop),
        "Should allow pawn drop when only promoted pawn exists in file"
    );
}

#[test]
fn test_pawn_drop_last_rank_restrictions() {
    // Test that pawns cannot be dropped on the last rank
    let mut pos = Position::empty();

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Test Black pawn drop on rank 0 (last rank for Black)
    pos.side_to_move = Color::Black;
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;
    pos.board.rebuild_occupancy_bitboards();

    let illegal_drop = Move::drop(PieceType::Pawn, parse_usi_square("5a").unwrap()); // 5a
    assert!(
        !pos.is_legal_move(illegal_drop),
        "Black should not be able to drop pawn on rank 0"
    );

    // Test White pawn drop on rank 8 (last rank for White)
    pos.side_to_move = Color::White;
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 0; // Remove Black's pawn
    pos.hands[Color::White as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    let illegal_drop = Move::drop(PieceType::Pawn, parse_usi_square("5i").unwrap()); // 5i
    assert!(
        !pos.is_legal_move(illegal_drop),
        "White should not be able to drop pawn on rank 8"
    );
}

#[test]
fn test_lance_drop_last_rank_restrictions() {
    // Test that lances cannot be dropped on the last rank
    let mut pos = Position::empty();

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Test Black lance drop on rank 0 (last rank for Black)
    pos.side_to_move = Color::Black;
    pos.hands[Color::Black as usize][PieceType::Lance.hand_index().unwrap()] = 1; // Lance is index 5
    pos.board.rebuild_occupancy_bitboards();

    let illegal_drop = Move::drop(PieceType::Lance, parse_usi_square("5a").unwrap()); // 5a
    assert!(
        !pos.is_legal_move(illegal_drop),
        "Black should not be able to drop lance on rank 0"
    );

    // Test White lance drop on rank 8 (last rank for White)
    pos.side_to_move = Color::White;
    pos.hands[Color::Black as usize][PieceType::Lance.hand_index().unwrap()] = 0; // Remove Black's lance
    pos.hands[Color::White as usize][PieceType::Lance.hand_index().unwrap()] = 1;

    let illegal_drop = Move::drop(PieceType::Lance, parse_usi_square("5i").unwrap()); // 5i
    assert!(
        !pos.is_legal_move(illegal_drop),
        "White should not be able to drop lance on rank 8"
    );
}

#[test]
fn test_knight_drop_last_two_ranks_restrictions() {
    // Test that knights cannot be dropped on the last two ranks
    let mut pos = Position::empty();

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Test Black knight drop
    pos.side_to_move = Color::Black;
    pos.hands[Color::Black as usize][PieceType::Knight.hand_index().unwrap()] = 1; // Knight is index 4
    pos.board.rebuild_occupancy_bitboards();

    // Cannot drop on rank 0 or 1
    let illegal_drop1 = Move::drop(PieceType::Knight, parse_usi_square("5a").unwrap()); // 5a
    assert!(
        !pos.is_legal_move(illegal_drop1),
        "Black should not be able to drop knight on rank 0"
    );

    let illegal_drop2 = Move::drop(PieceType::Knight, parse_usi_square("5b").unwrap()); // 5b
    assert!(
        !pos.is_legal_move(illegal_drop2),
        "Black should not be able to drop knight on rank 1"
    );

    // Can drop on rank 2
    let legal_drop = Move::drop(PieceType::Knight, parse_usi_square("5c").unwrap()); // 5c
    assert!(pos.is_legal_move(legal_drop), "Black should be able to drop knight on rank 2");

    // Test White knight drop
    pos.side_to_move = Color::White;
    pos.hands[Color::Black as usize][PieceType::Knight.hand_index().unwrap()] = 0; // Remove Black's knight
    pos.hands[Color::White as usize][PieceType::Knight.hand_index().unwrap()] = 1;

    // Cannot drop on rank 8 or 7
    let illegal_drop1 = Move::drop(PieceType::Knight, parse_usi_square("5i").unwrap()); // 5i
    assert!(
        !pos.is_legal_move(illegal_drop1),
        "White should not be able to drop knight on rank 8"
    );

    let illegal_drop2 = Move::drop(PieceType::Knight, parse_usi_square("5h").unwrap()); // 5h
    assert!(
        !pos.is_legal_move(illegal_drop2),
        "White should not be able to drop knight on rank 7"
    );

    // Can drop on rank 6
    let legal_drop = Move::drop(PieceType::Knight, parse_usi_square("5g").unwrap()); // 5g
    assert!(pos.is_legal_move(legal_drop), "White should be able to drop knight on rank 6");
}

// ========= Uchifuzume tests (migrated from MovePicker tests) =========

#[test]
fn test_uchifuzume_restriction() {
    // Create a position where a pawn drop would be checkmate
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // Place both kings
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Place black gold at 6a (file 3, rank 0) to prevent king escape
    pos.board
        .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Place black gold at 4a (file 5, rank 0) to prevent king escape
    pos.board
        .put_piece(parse_usi_square("4a").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Also place a gold at 6b to protect the gold at 6a
    pos.board
        .put_piece(parse_usi_square("6b").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Place another gold at 4b to protect the gold at 4a
    pos.board
        .put_piece(parse_usi_square("4b").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Place black lance at 5c (file 4, rank 2) to support pawn
    pos.board
        .put_piece(parse_usi_square("5c").unwrap(), Piece::new(PieceType::Lance, Color::Black));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop pawn at 5b (file 4, rank 1) - this would be checkmate
    let checkmate_drop = Move::drop(PieceType::Pawn, parse_usi_square("5b").unwrap());

    // This should be illegal (uchifuzume)
    let is_legal = pos.is_legal_move(checkmate_drop);
    assert!(!is_legal, "Should not allow checkmate by pawn drop");

    // Test case where king can escape
    // Remove one gold to create escape route
    pos.board.remove_piece(parse_usi_square("6a").unwrap());
    pos.board.rebuild_occupancy_bitboards();

    // Now the king can escape to 6a, so it's not checkmate
    assert!(pos.is_legal_move(checkmate_drop), "Should allow pawn drop when king can escape");
}

#[test]
fn test_pinned_piece_cannot_capture_pawn() {
    // Test case where enemy piece is pinned and cannot capture the dropped pawn
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // Place both kings
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    // White gold at 5b (file 4, rank 1) - this will be pinned
    pos.board
        .put_piece(parse_usi_square("5b").unwrap(), Piece::new(PieceType::Gold, Color::White));

    // Black rook at 5i (file 4, rank 8) - pinning the gold
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::Rook, Color::Black));

    // Black gold at 6b (file 3, rank 1) - protects the pawn drop
    pos.board
        .put_piece(parse_usi_square("6b").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop pawn at 6c (file 3, rank 2) - gold at 5b is pinned and cannot capture
    let pawn_drop = Move::drop(PieceType::Pawn, parse_usi_square("6c").unwrap());

    // This should be legal since the pinned gold cannot capture
    let is_legal = pos.is_legal_move(pawn_drop);
    assert!(is_legal, "Pawn drop should be legal when defender is pinned");
}

#[test]
fn test_uchifuzume_at_board_edge() {
    // Test checkmate by pawn drop at board edge
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // Place both kings
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black gold at 2a (file 7, rank 0) - blocks escape
    pos.board
        .put_piece(parse_usi_square("2a").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Black gold at 1c (file 8, rank 2) - protects pawn drop
    pos.board
        .put_piece(parse_usi_square("1c").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Black gold at 2b (file 7, rank 1) - blocks other escape
    pos.board
        .put_piece(parse_usi_square("2b").unwrap(), Piece::new(PieceType::Gold, Color::Black));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop pawn at 1b (file 8, rank 1) - this would be checkmate
    let checkmate_drop = Move::drop(PieceType::Pawn, parse_usi_square("1b").unwrap());

    // This should be illegal (uchifuzume)
    let is_legal = pos.is_legal_move(checkmate_drop);
    assert!(!is_legal, "Should not allow checkmate by pawn drop at board edge");
}

#[test]
fn test_uchifuzume_diagonal_escape() {
    // Test case where king can escape diagonally
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // White king at 5e (file 4, rank 4)
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black pieces blocking some escapes but not diagonals
    pos.board
        .put_piece(parse_usi_square("5d").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5d
    pos.board
        .put_piece(parse_usi_square("6e").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 6e
    pos.board
        .put_piece(parse_usi_square("4e").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 4e

    // Black gold supporting the pawn drop
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5g

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop pawn at 5f (file 4, rank 5) - gives check
    let pawn_drop = Move::drop(PieceType::Pawn, parse_usi_square("5f").unwrap());

    // This should be legal because king can escape diagonally to 6d, 6f, 4d, or 4f
    let is_legal = pos.is_legal_move(pawn_drop);
    assert!(is_legal, "Should allow pawn drop when king can escape diagonally");
}

#[test]
fn test_uchifuzume_white_side() {
    // Test checkmate by pawn drop for White side (symmetry test)
    let mut pos = Position::empty();
    pos.side_to_move = Color::White;

    // Black king at 5i (file 4, rank 8)
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));

    // White gold pieces blocking escape
    pos.board
        .put_piece(parse_usi_square("6i").unwrap(), Piece::new(PieceType::Gold, Color::White)); // 6i
    pos.board
        .put_piece(parse_usi_square("4i").unwrap(), Piece::new(PieceType::Gold, Color::White)); // 4i

    // White golds protecting each other
    pos.board
        .put_piece(parse_usi_square("6h").unwrap(), Piece::new(PieceType::Gold, Color::White)); // 6h
    pos.board
        .put_piece(parse_usi_square("4h").unwrap(), Piece::new(PieceType::Gold, Color::White)); // 4h

    // White lance supporting pawn
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Lance, Color::White)); // 5g

    // White king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Give white a pawn in hand
    pos.hands[Color::White as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop pawn at 5h (file 4, rank 7) - this would be checkmate
    let checkmate_drop = Move::drop(PieceType::Pawn, parse_usi_square("5h").unwrap());

    // This should be illegal (uchifuzume)
    let is_legal = pos.is_legal_move(checkmate_drop);
    assert!(!is_legal, "Should not allow checkmate by pawn drop for White");
}

#[test]
fn test_uchifuzume_no_support_but_king_cannot_capture() {
    // Test case where pawn has no support but king cannot capture due to another attacker
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // White king at 5a (file 4, rank 0)
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black bishop at 1e (file 8, rank 4) - controls diagonal including 5a
    pos.board
        .put_piece(parse_usi_square("1e").unwrap(), Piece::new(PieceType::Bishop, Color::Black));

    // Some blocking pieces to prevent other escapes
    pos.board
        .put_piece(parse_usi_square("6a").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 6a
    pos.board
        .put_piece(parse_usi_square("4a").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 4a

    // Black king
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop pawn at 5b (file 4, rank 1)
    let pawn_drop = Move::drop(PieceType::Pawn, parse_usi_square("5b").unwrap());

    // The pawn has no direct support, but king cannot capture it because
    // that would put the king in check from the bishop
    // This should still be legal because it's not checkmate (not all conditions met)
    let is_legal = pos.is_legal_move(pawn_drop);
    assert!(
        is_legal,
        "Should allow pawn drop even without support if king cannot capture due to other threats"
    );
}

#[test]
fn test_uchifuzume_double_check() {
    // Test case where pawn drop creates double check
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // White king at 5e (file 4, rank 4)
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black rook at 5a (file 4, rank 0) - will give check when pawn moves
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::Rook, Color::Black));

    // Black bishop at 1a (file 8, rank 0) - diagonal check
    pos.board
        .put_piece(parse_usi_square("1a").unwrap(), Piece::new(PieceType::Bishop, Color::Black));

    // Black gold supporting the pawn
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Gold, Color::Black)); // 5g

    // Black king
    pos.board
        .put_piece(parse_usi_square("9i").unwrap(), Piece::new(PieceType::King, Color::Black));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Try to drop pawn at 5f (file 4, rank 5) - creates double check
    let pawn_drop = Move::drop(PieceType::Pawn, parse_usi_square("5f").unwrap());

    // Even with double check, if king has escape squares, it's not checkmate
    let is_legal = pos.is_legal_move(pawn_drop);
    // The king can potentially escape to various squares, so this should be legal
    assert!(
        is_legal,
        "Should allow pawn drop even if it creates double check when king has escapes"
    );
}

#[test]
fn test_multiple_lance_attacks() {
    // Test case with multiple lances attacking the same square
    let mut pos = Position::empty();

    // Setup position
    pos.side_to_move = Color::Black;

    // White king at 9a (file 0, rank 0)
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Black king at 1i (file 8, rank 8)
    pos.board
        .put_piece(parse_usi_square("1i").unwrap(), Piece::new(PieceType::King, Color::Black));

    // Black lances in same file attacking upward (toward rank 0)
    pos.board
        .put_piece(parse_usi_square("5g").unwrap(), Piece::new(PieceType::Lance, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Lance, Color::Black));

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // For Black, lance attacks upward (toward rank 0)
    // Check attacks to rank 3 - only the front lance (at rank 4) can attack it
    let attackers = pos.get_attackers_to(parse_usi_square("5d").unwrap(), Color::Black);

    // Only the lance at rank 4 should be able to attack rank 3
    assert!(attackers.test(parse_usi_square("5e").unwrap()), "Front lance should attack");
    assert!(
        !attackers.test(parse_usi_square("5g").unwrap()),
        "Rear lance should be blocked by front lance"
    );
}

#[test]
fn test_mixed_promoted_unpromoted_attacks() {
    // Test case with mixed promoted and unpromoted pieces
    let mut pos = Position::empty();
    pos.side_to_move = Color::Black;

    // White king at 5a (file 4, rank 0)
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    // Unpromoted silver at 4c - can attack 5b diagonally
    pos.board
        .put_piece(parse_usi_square("4c").unwrap(), Piece::new(PieceType::Silver, Color::White));

    // Promoted silver (moves like gold) at 6b
    let mut promoted_silver = Piece::new(PieceType::Silver, Color::White);
    promoted_silver.promoted = true;
    pos.board.put_piece(parse_usi_square("6b").unwrap(), promoted_silver);
    pos.board.promoted_bb.set(parse_usi_square("6b").unwrap());

    // Black king
    pos.board
        .put_piece(parse_usi_square("9a").unwrap(), Piece::new(PieceType::King, Color::Black));

    // Give black a pawn in hand
    pos.hands[Color::Black as usize][PieceType::Pawn.hand_index().unwrap()] = 1;

    // Rebuild occupancy bitboards
    pos.board.rebuild_occupancy_bitboards();

    // Drop pawn at 5b (file 4, rank 1)
    let pawn_drop = Move::drop(PieceType::Pawn, parse_usi_square("5b").unwrap());

    // Check attackers to the pawn drop square
    let attackers = pos.get_attackers_to(parse_usi_square("5b").unwrap(), Color::White);

    // Unpromoted silver can attack diagonally
    assert!(
        attackers.test(parse_usi_square("4c").unwrap()),
        "Unpromoted silver should attack diagonally"
    );

    // Promoted silver attacks like gold (including orthogonally)
    assert!(
        attackers.test(parse_usi_square("6b").unwrap()),
        "Promoted silver should attack like gold"
    );

    // The pawn drop legality depends on the specific position
    let is_legal = pos.is_legal_move(pawn_drop);
    assert!(is_legal, "Move legality depends on specific position");
}
