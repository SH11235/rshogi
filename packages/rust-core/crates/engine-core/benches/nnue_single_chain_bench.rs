use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::hint::black_box;
use std::time::Duration;

// ベンチの目的:
// - SINGLE 差分経路の評価スループットを測定する（常時有効化）
// - 黒手（3g→3f）を n 回: 適用→評価→Undo を繰り返す

fn make_test_single_net() -> engine_core::evaluation::nnue::single::SingleChannelNet {
    use engine_core::evaluation::nnue::{features, single::SingleChannelNet};
    let n_feat = engine_core::shogi::SHOGI_BOARD_SIZE * features::FE_END;
    let d = 8usize; // 小さめの次元でテスト（メモリと計測時間を抑制）
    SingleChannelNet {
        n_feat,
        acc_dim: d,
        scale: 600.0,
        w0: vec![0.2; n_feat * d],
        b0: Some(vec![0.05; d]),
        w2: vec![1.0; d],
        b2: 0.1,
        uid: 42,
    }
}

fn make_move_black() -> engine_core::shogi::Move {
    use engine_core::shogi::Move;
    use engine_core::usi::parse_usi_square;
    // テストで利用している安全な黒手（合法）
    Move::make_normal(parse_usi_square("3g").unwrap(), parse_usi_square("3f").unwrap())
}

/// 差分チェーンベンチ（常時有効）
/// - N 回、親局面から「黒手→評価→Undo」を繰り返す
fn bench_single_chain(c: &mut Criterion) {
    use engine_core::evaluation::nnue::single::SingleChannelNet;
    use engine_core::evaluation::nnue::single_state::SingleAcc;
    use engine_core::{Color, Position};

    let mut g = c.benchmark_group("nnue_single_chain");
    g.sample_size(50);
    g.warm_up_time(Duration::from_secs(2));
    g.measurement_time(Duration::from_secs(5));
    // 反復回数（長すぎるとCIが重くなるため控えめに）
    let iters = [2000usize, 8000usize];

    let net: SingleChannelNet = make_test_single_net();
    let m_black = make_move_black();

    for &n in &iters {
        g.bench_with_input(
            BenchmarkId::new("chain_eval", format!("iters={n}")),
            &n,
            |b, &n| {
                b.iter_batched(
                    // 初期状態を生成
                    || {
                        let pos = Position::startpos();
                        let acc = SingleAcc::refresh(&pos, &net);
                        (pos, acc)
                    },
                    // 黒→評価→Undo を n 回繰り返す
                    |(mut pos, mut acc)| {
                        for _ in 0..n {
                            // 黒手（子局面は白番）。戻して原状復帰。
                            let acc1 = engine_core::evaluation::nnue::single_state::SingleAcc::apply_update(
                                &acc,
                                &pos,
                                m_black,
                                &net,
                            );
                            let acc1 = black_box(acc1);
                            let undo_b = pos.do_move(m_black);
                            // 子局面(黒手後)は白番のため、White 視点の pre を評価
                            let eval = engine_core::evaluation::nnue::single::SingleChannelNet::evaluate_from_accumulator_pre(
                                &net,
                                acc1.acc_for(Color::White),
                            );
                            // 最適化除け
                            black_box(eval);
                            pos.undo_move(m_black, undo_b);
                            acc = acc; // 原状復帰（acc1 は破棄）
                        }
                        black_box(pos);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    g.finish();
}

criterion_group!(benches, bench_single_chain);
criterion_main!(benches);
