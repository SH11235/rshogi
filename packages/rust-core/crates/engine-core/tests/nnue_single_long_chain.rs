use engine_core::evaluation::nnue::{single::SingleChannelNet, single_state::SingleAcc};
use engine_core::movegen::MoveGenerator;
use engine_core::{Color, Piece, PieceType, Position};
use rand::RngCore;
use rand_xoshiro::rand_core::SeedableRng;

fn make_small_net(acc_dim: usize, uid: u64) -> SingleChannelNet {
    // 二進小数の係数で丸め差を抑制
    let n_feat =
        engine_core::shogi::SHOGI_BOARD_SIZE * engine_core::evaluation::nnue::features::FE_END;
    let w0_val = 0.125_f32; // 1/8
    let b0_val = -0.25_f32; // -1/4
    let w2_val = 0.5_f32; // 1/2
    SingleChannelNet {
        n_feat,
        acc_dim,
        scale: 600.0,
        w0: vec![w0_val; n_feat * acc_dim],
        b0: Some(vec![b0_val; acc_dim]),
        w2: vec![w2_val; acc_dim],
        b2: 0.0,
        uid,
    }
}

fn verify_shape(acc: &SingleAcc, d: usize) {
    // 公開APIから検査できる範囲に限定（ReLU は評価時に適用されるため非負は保証しない）
    assert_eq!(acc.acc_for(Color::Black).len(), d);
    assert_eq!(acc.acc_for(Color::White).len(), d);
}

#[test]
fn long_chain_150plies_triple_equality() {
    let d = 8usize;
    let net = make_small_net(d, 0xA1);
    let mut pos = Position::startpos();
    let mut acc = SingleAcc::refresh(&pos, &net);
    let gen = MoveGenerator::new();
    let mut rng = rand_xoshiro::Xoshiro128Plus::seed_from_u64(0xC0FFEE);

    for _ply in 0..150 {
        let moves = gen.generate_all(&pos).unwrap_or_default();
        if moves.is_empty() {
            break;
        }
        let idx = (rng.next_u32() as usize) % moves.len();
        let mv = moves[idx];

        let next = SingleAcc::apply_update(&acc, &pos, mv, &net);
        let u = pos.do_move(mv);

        // 形状不変式
        verify_shape(&next, d);

        // 三者一致
        let eval_acc = net.evaluate_from_accumulator_pre(next.acc_for(pos.side_to_move));
        let full = SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(full.acc_for(pos.side_to_move));
        let eval_dir = net.evaluate(&pos);
        if !(eval_acc == eval_full && eval_acc == eval_dir) {
            eprintln!(
                "ply={} mv={:?} acc={} full={} dir={}",
                _ply, mv, eval_acc, eval_full, eval_dir
            );
        }
        assert_eq!(eval_acc, eval_full);
        assert_eq!(eval_acc, eval_dir);

        // 進行
        acc = next;
        // 念のため、玉だけの簡単局面でも一度通す
        if _ply == 0 {
            let mut simple = Position::empty();
            use engine_core::usi::parse_usi_square;
            simple.board.put_piece(
                parse_usi_square("5i").unwrap(),
                Piece::new(PieceType::King, Color::Black),
            );
            simple.board.put_piece(
                parse_usi_square("5a").unwrap(),
                Piece::new(PieceType::King, Color::White),
            );
            let acc_s = SingleAcc::refresh(&simple, &net);
            let s_acc = net.evaluate_from_accumulator_pre(acc_s.acc_for(simple.side_to_move));
            let s_full = net.evaluate(&simple);
            assert_eq!(s_acc, s_full);
        }

        pos.undo_move(mv, u);
        // 進め直し（posは戻したので、もう一回同じ手で進める）
        let _ = pos.do_move(mv);
    }
}

#[test]
fn long_chain_200plies_triple_equality() {
    let d = 8usize;
    let net = make_small_net(d, 0xA2);
    let mut pos = Position::startpos();
    let mut acc = SingleAcc::refresh(&pos, &net);
    let gen = MoveGenerator::new();
    let mut rng = rand_xoshiro::Xoshiro128Plus::seed_from_u64(0xBADC0DE);

    for _ply in 0..200 {
        let moves = gen.generate_all(&pos).unwrap_or_default();
        if moves.is_empty() {
            break;
        }
        let idx = (rng.next_u32() as usize) % moves.len();
        let mv = moves[idx];

        let next = SingleAcc::apply_update(&acc, &pos, mv, &net);
        let _u = pos.do_move(mv);

        verify_shape(&next, d);
        let eval_acc = net.evaluate_from_accumulator_pre(next.acc_for(pos.side_to_move));
        let full = SingleAcc::refresh(&pos, &net);
        let eval_full = net.evaluate_from_accumulator_pre(full.acc_for(pos.side_to_move));
        let eval_dir = net.evaluate(&pos);
        assert_eq!(eval_acc, eval_full);
        assert_eq!(eval_acc, eval_dir);

        acc = next;
    }
}
