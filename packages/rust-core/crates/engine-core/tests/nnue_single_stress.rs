#![cfg(feature = "nnue_single_diff")]

use engine_core::evaluation::nnue::{single::SingleChannelNet, single_state::SingleAcc};
use engine_core::movegen::MoveGenerator;
use engine_core::Position;
use rand::RngCore;
use rand_xoshiro::rand_core::SeedableRng;

fn make_net(uid: u64) -> SingleChannelNet {
    let n_feat =
        engine_core::shogi::SHOGI_BOARD_SIZE * engine_core::evaluation::nnue::features::FE_END;
    let d = 8usize;
    SingleChannelNet {
        n_feat,
        acc_dim: d,
        scale: 600.0,
        w0: vec![0.125; n_feat * d],
        b0: Some(vec![0.0; d]),
        w2: vec![0.5; d],
        b2: 0.0,
        uid,
    }
}

#[test]
#[ignore]
fn stress_2000plies_triple_equality() {
    let net = make_net(0xEE);
    let mut pos = Position::startpos();
    let mut acc = SingleAcc::refresh(&pos, &net);
    let gen = MoveGenerator::new();
    let mut rng = rand_xoshiro::Xoshiro128Plus::seed_from_u64(0xDEADBEEF);

    for _ in 0..2000 {
        let moves = gen.generate_all(&pos).unwrap_or_default();
        if moves.is_empty() {
            break;
        }
        let mv = moves[(rng.next_u32() as usize) % moves.len()];
        let next = SingleAcc::apply_update(&acc, &pos, mv, &net);
        let _u = pos.do_move(mv);

        let s_acc = net.evaluate_from_accumulator_pre(next.acc_for(pos.side_to_move));
        let full = SingleAcc::refresh(&pos, &net);
        let s_full = net.evaluate_from_accumulator_pre(full.acc_for(pos.side_to_move));
        let s_dir = net.evaluate(&pos);
        assert_eq!(s_acc, s_full);
        assert_eq!(s_acc, s_dir);

        acc = next;
    }
}
