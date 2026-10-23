//! `go nodes` は helper を含む全スレッドの合計ノード数で打ち切る。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::position::Position;
use crate::search::{LimitsType, Search, SearchInfo, TimeManagement};
use crate::types::Move;

fn on_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

fn time_manager(limits: &LimitsType) -> TimeManagement {
    let mut tm =
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    tm.init(limits, crate::types::Color::Black, 1, 256);
    tm
}

#[test]
fn node_limit_uses_shared_total_when_present() {
    let own_only = LimitsType {
        nodes: 1_000,
        ..Default::default()
    };
    let tm = time_manager(&own_only);
    assert!(!tm.node_limit_reached(&own_only, 999));
    assert!(tm.node_limit_reached(&own_only, 1_000));

    let total = Arc::new(AtomicU64::new(0));
    let shared = LimitsType {
        nodes: 1_000,
        searched_nodes: Some(Arc::clone(&total)),
        ..Default::default()
    };
    let mut tm = time_manager(&shared);
    tm.publish_nodes(600);
    assert!(!tm.node_limit_reached(&shared, 600));
    // 他の worker が publish した分も合計に入る。
    total.fetch_add(400, Ordering::Relaxed);
    assert!(tm.node_limit_reached(&shared, 600));
    // publish は前回からの増分だけを足す。
    tm.publish_nodes(700);
    assert_eq!(total.load(Ordering::Relaxed), 1_100);

    let pondering = LimitsType {
        ponder: true,
        ..shared.clone()
    };
    assert!(!time_manager(&pondering).node_limit_reached(&pondering, 0));
}

#[test]
fn multi_thread_go_nodes_stops_on_total() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        const LIMIT: u64 = 200_000;
        for threads in [1, 2, 4] {
            let mut pos = Position::new();
            pos.set_hirate();
            let mut search = Search::new(16);
            search.set_num_threads(threads);
            let result = search.go(
                &mut pos,
                LimitsType {
                    nodes: LIMIT,
                    ..Default::default()
                },
                None::<fn(&SearchInfo)>,
            );
            assert_ne!(result.best_move, Move::NONE);
            // 超過は各 worker の未 publish 分 (check_abort の間引き間隔) だけ。
            // スレッドごとの判定なら threads 倍近くまで膨らむ。
            assert!(
                (LIMIT..=LIMIT + LIMIT / 10).contains(&result.nodes),
                "threads={threads} nodes={}",
                result.nodes
            );
        }
    });
}

#[test]
fn multi_thread_go_ponder_nodes_waits_for_ponderhit() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    let mut search = Search::new(16);
    search.set_num_threads(2);
    search.reset_flags();
    let stop = search.stop_flag();
    let ponderhit = search.ponderhit_handle();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let mut pos = Position::new();
            pos.set_hirate();
            let result = search.go(
                &mut pos,
                LimitsType {
                    ponder: true,
                    nodes: 2_000,
                    ..Default::default()
                },
                None::<fn(&SearchInfo)>,
            );
            tx.send(result.best_move).unwrap();
        })
        .unwrap();
    // 予算を使い切った後も ponder 中は bestmove を返さない。
    let early = rx.recv_timeout(Duration::from_millis(300));
    ponderhit.signal();
    let released = rx.recv_timeout(Duration::from_secs(5));
    if released.is_err() {
        stop.store(true, Ordering::SeqCst);
    }
    worker.join().unwrap();
    assert!(early.is_err(), "ponder must not stop on the node budget");
    assert_ne!(released.expect("ponderhit must stop on the exhausted budget"), Move::NONE);
}
