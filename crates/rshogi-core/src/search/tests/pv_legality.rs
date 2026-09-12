//! 探索が報告する PV の合法性テスト

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::eval::EvalHash;
use crate::movegen::is_legal_with_pass;
use crate::position::Position;
use crate::search::alpha_beta::SearchWorker;
use crate::search::engine::{Search, SearchInfo};
use crate::search::{LimitsType, NodeType, SearchTuneParams, TimeManagement};
use crate::tt::TranspositionTable;
use crate::types::{Move, Value};

const ROOTS: [&str; 5] = [
    "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
    "lnsgkgsnl/1r7/p1ppp1bpp/1p3pp2/7P1/2P6/PP1PPPP1P/1B3S1R1/LNSGKG1NL b - 9",
    "l4S2l/4g1gs1/5p1p1/pr2N1pkp/4Gn3/PP3PPPP/2GPP4/1K7/L3r+s2L w BS2N5Pb 1",
    "6n1l/2+S1k4/2lp4p/1np1B2b1/3PP4/1N1S3rP/1P2+pPP+p1/1p1G5/3KG2r1 b GSN2L4Pgs2p 1",
    "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
];
const DEPTH: i32 = 10;

fn on_large_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

fn assert_legal_line(root_sfen: &str, pv: &[Move], context: &str) {
    let mut pos = Position::new();
    pos.set_sfen(root_sfen).unwrap();
    for &mv in pv {
        let legal = mv != Move::NONE
            && (mv.is_pass() || pos.pseudo_legal(mv))
            && is_legal_with_pass(&pos, mv);
        assert!(
            legal,
            "{context}: {} は不正な手。pv={:?}",
            mv.to_usi(),
            pv.iter().map(|m| m.to_usi()).collect::<Vec<_>>()
        );
        let check = pos.gives_check(mv);
        pos.do_move(mv, check);
    }
}

/// 兄弟ノードの読み筋が子の続きとして親へ渡ると、途中で不正になる PV が報告される。
#[test]
fn every_reported_pv_is_legal() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        for root_sfen in ROOTS {
            for multi_pv in [1, 4] {
                let mut search = Search::new(16);
                let mut pos = Position::new();
                pos.set_sfen(root_sfen).unwrap();
                let limits = LimitsType {
                    depth: DEPTH,
                    multi_pv,
                    ..Default::default()
                };
                let mut infos = Vec::new();
                let result =
                    search.go(&mut pos, limits, Some(|info: &SearchInfo| infos.push(info.clone())));
                let case = format!("sfen={root_sfen} multi_pv={multi_pv}");

                assert_eq!(result.depth, DEPTH, "{case}");
                assert!(!result.pv.is_empty(), "{case}");
                assert_eq!(result.pv[0], result.best_move, "{case}");
                assert_legal_line(root_sfen, &result.pv, &format!("{case} result"));

                let final_lines: BTreeSet<usize> = infos
                    .iter()
                    .filter(|info| info.depth == DEPTH)
                    .map(|info| info.multi_pv)
                    .collect();
                assert_eq!(final_lines, (1..=multi_pv).collect(), "{case}");

                for info in &infos {
                    let context = format!("{case} depth={} line={}", info.depth, info.multi_pv);
                    assert!(!info.pv.is_empty(), "{context}");
                    assert_legal_line(root_sfen, &info.pv, &context);
                }
            }
        }
    });
}

/// ply 1 の行に兄弟ノードの手を仕込んでから PV ノードとして探索し、返った後のその行を返す。
fn search_child_with_stale_row(sfen: &str, depth: i32, alpha: Value) -> Vec<Move> {
    let limits = LimitsType {
        depth: 1,
        ..Default::default()
    };
    let mut worker = SearchWorker::new(
        Arc::new(TranspositionTable::new(1)),
        Arc::new(EvalHash::new(1)),
        0,
        0,
        SearchTuneParams::default(),
    );
    worker.prepare_search(&limits);
    let mut pos = Position::new();
    pos.set_sfen(sfen).unwrap();
    let ply = 1;
    worker.state.pv_table.update(ply as usize, Move::from_usi("3a2b").unwrap());
    let mut tm =
        TimeManagement::new(Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    worker.search_node_wrapper::<{ NodeType::PV as u8 }>(
        &mut pos,
        depth,
        alpha,
        Value::INFINITE,
        ply,
        false,
        &limits,
        &mut tm,
    );
    worker.state.pv_table.line(ply as usize).to_vec()
}

#[test]
fn pv_node_early_return_drops_sibling_line() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        // ply 1 で alpha >= mate_in(2) なら Mate Distance Pruning で即 return する。
        let line = search_child_with_stale_row(ROOTS[0], 4, Value::mate_in(2));
        assert!(line.is_empty(), "{line:?}");
    });
}

#[test]
fn qsearch_pv_node_drops_sibling_line() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        // 平手は駒を取る手が無いので、qsearch は手を選ばずに返る。
        let line = search_child_with_stale_row(ROOTS[0], 0, -Value::INFINITE);
        assert!(line.is_empty(), "{line:?}");
    });
}

#[test]
fn qsearch_pv_node_records_best_capture() {
    let _guard = crate::eval::material::test_support::lock_material();
    crate::eval::set_material_level(crate::eval::MaterialLevel::Lv1);
    on_large_stack(|| {
        // 取り返されない歩をただで取れる局面。qsearch はその捕獲手を自分の行に書く。
        let line =
            search_child_with_stale_row("4k4/9/9/9/4p4/4R4/9/9/4K4 b - 1", 0, -Value::INFINITE);
        let line: Vec<_> = line.iter().map(|m| m.to_usi()).collect();
        assert_eq!(line.first().map(String::as_str), Some("5f5e"), "{line:?}");
    });
}
