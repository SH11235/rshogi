//! 公開 PV 検証の変更前後を比較する手動ベンチ（通常の cargo test では実行しない）。
//!
//! ここで測れるのは Material Lv1・MultiPV 1/4・単一局面群の内部比較に限る。
//! 本番 NNUE・MultiPV 8・候補手が多い局面・固定時間 100/1000 ms での比較は、
//! 同一 profile の 2 つの USI エンジンを外部から abba 順に駆動し、
//! info 公開回数と固定時間あたりの処理ノード数を比べる必要がある。
use rshogi_core::eval::{MaterialLevel, set_material_level};
use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search, SearchInfo};
use rshogi_core::types::Move;
use std::time::Instant;

#[test]
#[ignore = "release ビルドで ABBA 順に手動実行する計測用ベンチ"]
fn public_pv_bench() {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            set_material_level(MaterialLevel::Lv1);
            let mode = std::env::var("PV_BENCH_MODE").unwrap_or_else(|_| "nodes".to_owned());
            let threads: usize = std::env::var("PV_BENCH_THREADS")
                .unwrap_or_else(|_| "1".to_owned())
                .parse()
                .unwrap();
            let openings: &[&[&str]] = &[
                &[],
                &["7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e"],
                &[
                    "7g7f", "8c8d", "2g2f", "3c3d", "2f2e", "8d8e", "2e2d", "2c2d", "2h2d", "8e8f",
                    "8g8f", "8b8f",
                ],
            ];
            for (position, opening) in openings.iter().enumerate() {
                for multi_pv in [1, 4] {
                    let mut root = Position::new();
                    root.set_hirate();
                    for usi in *opening {
                        let mv = root.to_move(Move::from_usi(usi).unwrap()).unwrap();
                        let check = root.gives_check(mv);
                        root.do_move(mv, check);
                    }
                    let mut search = Search::new(16);
                    search.set_num_threads(threads);
                    let mut limits = LimitsType::new();
                    limits.multi_pv = multi_pv;
                    match mode.as_str() {
                        "nodes" => limits.nodes = 100_000,
                        "depth" => limits.depth = 10,
                        "time" => limits.movetime = 200,
                        _ => panic!("PV_BENCH_MODE must be nodes/depth/time"),
                    }
                    let mut callbacks = 0;
                    let mut emitted_moves = 0;
                    let start = Instant::now();
                    let result = search.go(
                        &mut root,
                        limits,
                        Some(|info: &SearchInfo| {
                            callbacks += 1;
                            emitted_moves += info.pv.len();
                            std::hint::black_box(info);
                        }),
                    );
                    println!(
                        "PV_BENCH,{mode},{threads},{position},{multi_pv},{},{},{},{},{},{},{}",
                        start.elapsed().as_nanos(),
                        result.nodes,
                        result.depth,
                        result.score.raw(),
                        result.best_move.to_usi(),
                        callbacks,
                        emitted_moves
                    );
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
