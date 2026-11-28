//! `go_with_updates` が段の途中の経過を渡し、探索そのものは変えないこと。
use std::sync::{Arc, Mutex};

use crate::position::Position;
use crate::search::{
    AnalysisBudget, AnalysisSession, CurrMove, LimitsType, Search, SearchInfo, SearchStatus,
    SearchUpdate, SearchUpdateOptions,
};
use crate::types::{Bound, Move};

fn on_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

/// 届いた順の記録。`order` は Progress と PvLine の (時間, 局面数)。
#[derive(Default)]
struct Recorded {
    progress: Vec<SearchStatus>,
    currmoves: Vec<CurrMove>,
    pv_lines: Vec<SearchInfo>,
    order: Vec<(u64, u64)>,
}

type Shared = Arc<Mutex<Recorded>>;

fn recorder() -> (Shared, impl FnMut(&SearchUpdate<'_>) + Send + 'static) {
    let recorded = Shared::default();
    let sink = Arc::clone(&recorded);
    let on_update = move |update: &SearchUpdate<'_>| {
        let mut sink = sink.lock().unwrap();
        match update {
            SearchUpdate::Progress(p) => {
                sink.progress.push(*p);
                sink.order.push((p.time_ms, p.nodes));
            }
            SearchUpdate::CurrMove(c) => sink.currmoves.push(*c),
            SearchUpdate::PvLine(info) => {
                sink.order.push((info.time_ms, info.nodes));
                sink.pv_lines.push((*info).clone());
            }
        }
    };
    (recorded, on_update)
}

fn take(recorded: Shared) -> Recorded {
    Arc::try_unwrap(recorded).ok().unwrap().into_inner().unwrap()
}

const EVERYTHING: SearchUpdateOptions = SearchUpdateOptions {
    min_nodes: 0,
    progress_interval_ms: 5,
};

fn check_multi_pv_updates(rec: &Recorded, total_nodes: u64) {
    // 平手の合法手は 30 手。通し番号は確定済みの本の数を含む。
    assert!(rec.currmoves.iter().any(|c| c.multi_pv == 3));
    for c in &rec.currmoves {
        assert!((1..=3).contains(&c.multi_pv));
        assert!(c.number >= c.multi_pv && c.number <= 30, "{c:?}");
        assert!(c.depth >= 1);
    }

    // 最後の本は on_info で出るので、途中経過は 1・2 本目だけ。2 本目を読み終えると 1・2 本目が届く。
    assert!(rec.pv_lines.iter().any(|info| info.multi_pv == 2));
    for info in &rec.pv_lines {
        assert!((1..=2).contains(&info.multi_pv), "multipv {}", info.multi_pv);
        assert_eq!(info.bound, Bound::Exact);
        assert!(!info.pv.is_empty());
    }

    assert!(!rec.progress.is_empty());
    for pair in rec.progress.windows(2) {
        assert!(pair[1].time_ms >= pair[0].time_ms + EVERYTHING.progress_interval_ms);
    }
    // Progress と PvLine を届いた順に並べても、時間と局面数は戻らない。
    for pair in rec.order.windows(2) {
        assert!(pair[0].0 <= pair[1].0 && pair[0].1 <= pair[1].1, "{pair:?}");
    }
    assert!(rec.order.iter().all(|&(_, nodes)| nodes <= total_nodes));
}

#[test]
fn updates_report_currmove_pv_lines_and_progress() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        for threads in [1, 2] {
            let mut pos = Position::new();
            pos.set_hirate();
            let mut search = Search::new(16);
            search.set_num_threads(threads);
            let (recorded, on_update) = recorder();
            // go nodes を使わず、共有カウンタが途中経過のためだけに有効になる条件で確かめる。
            let limits = LimitsType {
                depth: 9,
                multi_pv: 3,
                ..Default::default()
            };
            let result = search.go_with_updates(
                &mut pos,
                limits,
                None::<fn(&SearchInfo)>,
                EVERYTHING,
                on_update,
            );
            drop(search);
            check_multi_pv_updates(&take(recorded), result.nodes);
        }
    });
}

#[test]
fn updates_wait_for_min_nodes_and_keep_search_unchanged() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        let limits = LimitsType {
            nodes: 300_000,
            multi_pv: 2,
            ..Default::default()
        };
        let mut pos = Position::new();
        pos.set_hirate();
        let (recorded, on_update) = recorder();
        let options = SearchUpdateOptions {
            min_nodes: u64::MAX,
            ..EVERYTHING
        };
        let with_updates = Search::new(16).go_with_updates(
            &mut pos,
            limits.clone(),
            None::<fn(&SearchInfo)>,
            options,
            on_update,
        );
        let rec = take(recorded);
        assert!(rec.progress.is_empty() && rec.currmoves.is_empty() && rec.pv_lines.is_empty());

        let plain = Search::new(16).go(&mut pos, limits, None::<fn(&SearchInfo)>);
        assert_ne!(plain.best_move, Move::NONE);
        assert_eq!(with_updates.best_move, plain.best_move);
        assert_eq!(with_updates.nodes, plain.nodes);
        assert_eq!(with_updates.depth, plain.depth);
    });
}

#[test]
fn analysis_run_with_updates_reports_progress() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        for threads in [1, 2] {
            let mut search = Search::new(16);
            search.set_num_threads(threads);
            let mut pos = Position::new();
            pos.set_hirate();
            let mut session = AnalysisSession::new(search, pos, 3).unwrap();
            let run = session.prepare(AnalysisBudget::Depth(9)).unwrap();
            let (recorded, on_update) = recorder();
            let finished = session
                .run_with_updates(&run, None::<fn(&SearchInfo)>, EVERYTHING, on_update)
                .unwrap();
            check_multi_pv_updates(&take(recorded), finished.result.nodes);
        }
    });
}
