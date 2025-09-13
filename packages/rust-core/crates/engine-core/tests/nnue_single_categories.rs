#![cfg(feature = "nnue_single_diff")]

use engine_core::evaluation::nnue::{single::SingleChannelNet, single_state::SingleAcc};
use engine_core::shogi::Move;
use engine_core::usi::parse_usi_square;
use engine_core::{Color, Piece, PieceType, Position};

fn make_small_net(uid: u64) -> SingleChannelNet {
    let n_feat =
        engine_core::shogi::SHOGI_BOARD_SIZE * engine_core::evaluation::nnue::features::FE_END;
    let d = 8usize;
    SingleChannelNet {
        n_feat,
        acc_dim: d,
        scale: 600.0,
        w0: vec![0.25; n_feat * d], // 1/4
        b0: Some(vec![-0.25; d]),   // -1/4
        w2: vec![0.5; d],           // 1/2
        b2: 0.0,
        uid,
    }
}

fn triple_eq(net: &SingleChannelNet, pos: &Position, acc: &SingleAcc) {
    let s_acc = net.evaluate_from_accumulator(acc.acc_for(pos.side_to_move));
    let full = SingleAcc::refresh(pos, net);
    let s_full = net.evaluate_from_accumulator(full.acc_for(pos.side_to_move));
    let s_dir = net.evaluate(pos);
    assert_eq!(s_acc, s_full, "acc vs refresh mismatch: {} vs {}", s_acc, s_full);
    assert_eq!(s_acc, s_dir, "acc vs direct mismatch: {} vs {}", s_acc, s_dir);
}

#[test]
fn category_promotion_and_capture_to_hand_base() {
    // 成り → 捕獲 → 手駒基底種（近似：成駒捕獲による基底種化）
    let net = make_small_net(0xB1);
    let mut pos = Position::empty();
    // Kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    // Black silver ready to promote: 3c -> 3b+
    pos.board
        .put_piece(parse_usi_square("3c").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    // White pawn at 3b (to be captured by promoted silver)
    pos.board
        .put_piece(parse_usi_square("3b").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    pos.side_to_move = Color::Black;

    // acc0
    let mut acc = SingleAcc::refresh(&pos, &net);
    triple_eq(&net, &pos, &acc);

    // 1) Promote move 3c->3b+
    let mv1 = Move::normal_with_piece(
        parse_usi_square("3c").unwrap(),
        parse_usi_square("3b").unwrap(),
        true,
        PieceType::Silver,
        Some(PieceType::Pawn),
    );
    acc = SingleAcc::apply_update(&acc, &pos, mv1, &net);
    let u1 = pos.do_move(mv1);
    triple_eq(&net, &pos, &acc);

    // 2) White captures promoted silver with king (3a->3b)
    let mv2 = Move::normal_with_piece(
        parse_usi_square("3a").unwrap(),
        parse_usi_square("3b").unwrap(),
        false,
        PieceType::King,
        Some(PieceType::Silver),
    );
    acc = SingleAcc::apply_update(&acc, &pos, mv2, &net);
    let u2 = pos.do_move(mv2);
    triple_eq(&net, &pos, &acc);

    // Undo to keep Position consistent for any future checks
    pos.undo_move(mv2, u2);
    pos.undo_move(mv1, u1);
}

#[test]
fn category_consecutive_non_promotion() {
    // 連続不成：昇級域で不成を選択し続ける
    let net = make_small_net(0xB2);
    let mut pos = Position::empty();
    // Kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    // Place a white pawn far to avoid interactions
    pos.board
        .put_piece(parse_usi_square("7c").unwrap(), Piece::new(PieceType::Pawn, Color::White));
    // Black silver in promotion zone path: 4c -> 4b (no promote) -> 4a (no promote)
    pos.board
        .put_piece(parse_usi_square("4c").unwrap(), Piece::new(PieceType::Silver, Color::Black));
    pos.side_to_move = Color::Black;

    let mut acc = SingleAcc::refresh(&pos, &net);
    triple_eq(&net, &pos, &acc);

    let m1 = Move::normal_with_piece(
        parse_usi_square("4c").unwrap(),
        parse_usi_square("4b").unwrap(),
        false,
        PieceType::Silver,
        None,
    );
    acc = SingleAcc::apply_update(&acc, &pos, m1, &net);
    let _u1 = pos.do_move(m1);
    triple_eq(&net, &pos, &acc);

    // White king quiet move to pass a turn (2a->2b)
    let w1 = Move::normal_with_piece(
        parse_usi_square("2a").unwrap(),
        parse_usi_square("2b").unwrap(),
        false,
        PieceType::King,
        None,
    );
    acc = SingleAcc::apply_update(&acc, &pos, w1, &net);
    let _uw1 = pos.do_move(w1);
    triple_eq(&net, &pos, &acc);

    let m2 = Move::normal_with_piece(
        parse_usi_square("4b").unwrap(),
        parse_usi_square("4a").unwrap(),
        false,
        PieceType::Silver,
        None,
    );
    acc = SingleAcc::apply_update(&acc, &pos, m2, &net);
    let _u2 = pos.do_move(m2);
    triple_eq(&net, &pos, &acc);
}

#[test]
fn category_drop_and_capture_cycle_min() {
    // 打ち→捕獲の最小往復（手駒特徴の増減と盤上特徴の相殺）
    let net = make_small_net(0xB3);
    let mut pos = Position::empty();
    // Kings
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
    // White gold placed to capture the drop
    pos.board
        .put_piece(parse_usi_square("5e").unwrap(), Piece::new(PieceType::Gold, Color::White));
    // Black has two pawns in hand, to allow repeated drops later if needed
    let hand_idx = engine_core::shogi::piece_type_to_hand_index(PieceType::Pawn).unwrap();
    pos.hands[Color::Black as usize][hand_idx] = 1;
    pos.side_to_move = Color::Black;

    let mut acc = SingleAcc::refresh(&pos, &net);
    triple_eq(&net, &pos, &acc);

    // Black: drop pawn at 5f
    let d1 = Move::drop(PieceType::Pawn, parse_usi_square("5f").unwrap());
    acc = SingleAcc::apply_update(&acc, &pos, d1, &net);
    let _ud1 = pos.do_move(d1);
    triple_eq(&net, &pos, &acc);

    // White: capture 5e->5f
    let c1 = Move::normal_with_piece(
        parse_usi_square("5e").unwrap(),
        parse_usi_square("5f").unwrap(),
        false,
        PieceType::Gold,
        Some(PieceType::Pawn),
    );
    acc = SingleAcc::apply_update(&acc, &pos, c1, &net);
    let _uc1 = pos.do_move(c1);
    triple_eq(&net, &pos, &acc);
}

#[test]
fn category_null_move_triple_equality_single_acc() {
    // Null move（手番反転）でも acc は有効（両視点 post を保持）
    let net = make_small_net(0xC1);
    let mut pos = Position::empty();
    pos.board
        .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
    pos.board
        .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));

    let acc0 = SingleAcc::refresh(&pos, &net);
    let s0 = net.evaluate_from_accumulator(acc0.acc_for(pos.side_to_move));
    let s0f = net.evaluate(&pos);
    assert_eq!(s0, s0f);

    let undo = pos.do_null_move();
    let s1 = net.evaluate_from_accumulator(acc0.acc_for(pos.side_to_move));
    let s1f = net.evaluate(&pos);
    assert_eq!(s1, s1f);
    pos.undo_null_move(undo);
}
