#![cfg(feature = "nnue_single_diff")]

use engine_core::evaluation::nnue::{single::SingleChannelNet, single_state::SingleAcc};
use engine_core::movegen::MoveGenerator;
use engine_core::shogi::Move;
use engine_core::usi::parse_usi_square;
use engine_core::{Color, Piece, PieceType, Position};
use proptest::prelude::*;
use rand::RngCore;
use rand_xoshiro::rand_core::SeedableRng;

fn coeff_set() -> Vec<f32> {
    vec![-0.5, -0.25, 0.0, 0.25, 0.5]
}

fn arb_acc_dim() -> impl Strategy<Value = usize> {
    prop::sample::select(vec![4usize, 6, 8])
}

fn build_net(
    acc_dim: usize,
    uid: u64,
    w0_c: f32,
    b0_c: f32,
    w2_c: f32,
    b2_c: f32,
) -> SingleChannelNet {
    let n_feat =
        engine_core::shogi::SHOGI_BOARD_SIZE * engine_core::evaluation::nnue::features::FE_END;
    SingleChannelNet {
        n_feat,
        acc_dim,
        scale: 600.0,
        w0: vec![w0_c; n_feat * acc_dim],
        b0: Some(vec![b0_c; acc_dim]),
        w2: vec![w2_c; acc_dim],
        b2: b2_c,
        uid,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 12, .. ProptestConfig::default() })]

    #[test]
    fn prop_triple_equality_random_chain(
        acc_dim in arb_acc_dim(),
        seed in any::<u64>(),
        w0_c in prop::sample::select(coeff_set()),
        b0_c in prop::sample::select(coeff_set()),
        w2_c in prop::sample::select(coeff_set()),
        b2_c in prop::sample::select(vec![-0.5f32, 0.0, 0.5])
    ) {
        let net = build_net(acc_dim, 0xD1, w0_c, b0_c, w2_c, b2_c);
        let mut pos = Position::startpos();
        let mut acc = SingleAcc::refresh(&pos, &net);
        let gen = MoveGenerator::new();
        let mut rng = rand_xoshiro::Xoshiro128Plus::seed_from_u64(seed);

        for _ in 0..24 {
            let moves = gen.generate_all(&pos).unwrap_or_default();
            if moves.is_empty() { break; }
            let mv = moves[(rng.next_u32() as usize) % moves.len()];
            let next = SingleAcc::apply_update(&acc, &pos, mv, &net);
            let _u = pos.do_move(mv);

            // ReLU/acc_dim invariants
            // 公開APIから検査できる範囲に限定
            prop_assert_eq!(next.acc_for(Color::Black).len(), acc_dim);
            prop_assert_eq!(next.acc_for(Color::White).len(), acc_dim);
            prop_assert!(next.acc_for(Color::Black).iter().all(|&v| v >= 0.0));
            prop_assert!(next.acc_for(Color::White).iter().all(|&v| v >= 0.0));

            // Triple equality
            let s_acc = net.evaluate_from_accumulator(next.acc_for(pos.side_to_move));
            let full = SingleAcc::refresh(&pos, &net);
            let s_full = net.evaluate_from_accumulator(full.acc_for(pos.side_to_move));
            let s_dir = net.evaluate(&pos);
            prop_assert_eq!(s_acc, s_full);
            prop_assert_eq!(s_acc, s_dir);

            acc = next;
        }
    }

    #[test]
    fn prop_king_move_refresh_fallback_matches_refresh(
        acc_dim in arb_acc_dim(),
        w0_c in prop::sample::select(coeff_set()),
        b0_c in prop::sample::select(coeff_set()),
        w2_c in prop::sample::select(coeff_set()),
        b2_c in prop::sample::select(vec![-0.5f32, 0.0, 0.5])
    ) {
        let net = build_net(acc_dim, 0xD2, w0_c, b0_c, w2_c, b2_c);
        let mut pos = Position::empty();
        pos.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        pos.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        pos.side_to_move = Color::Black;

        let acc0 = SingleAcc::refresh(&pos, &net);
        let mv = Move::normal_with_piece(
            parse_usi_square("5i").unwrap(),
            parse_usi_square("5h").unwrap(),
            false,
            PieceType::King,
            None,
        );
        let acc1 = SingleAcc::apply_update(&acc0, &pos, mv, &net);

        let u = pos.do_move(mv);
        let s_acc = net.evaluate_from_accumulator(acc1.acc_for(pos.side_to_move));
        let full = SingleAcc::refresh(&pos, &net);
        let s_full = net.evaluate_from_accumulator(full.acc_for(pos.side_to_move));
        prop_assert_eq!(s_acc, s_full);
        pos.undo_move(mv, u);
    }
}
