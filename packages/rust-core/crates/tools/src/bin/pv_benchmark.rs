//! PVテーブル効果測定用ベンチマーク

use engine_core::ai::{
    board::Position, evaluate::MaterialEvaluator, search_enhanced::EnhancedSearcher,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Run search with panic recovery
fn run_search_safe(
    searcher: &mut EnhancedSearcher,
    pos: &mut Position,
    depth: u8,
) -> Result<(Option<engine_core::ai::moves::Move>, i32, u64), String> {
    use std::panic;

    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let (best_move, score) = searcher.search(pos, depth.into(), None, None);
        let nodes = searcher.nodes();
        (best_move, score, nodes)
    }));

    match result {
        Ok((best_move, score, nodes)) => Ok((best_move, score, nodes)),
        Err(_) => Err("Search panicked".to_string()),
    }
}

fn main() {
    println!("PV Table Performance Benchmark");
    println!("==============================\n");

    let evaluator = Arc::new(MaterialEvaluator);

    // テスト局面（初期局面と中盤局面）
    let test_positions = vec![
        ("Initial position", Position::startpos()),
        // 他のテスト局面も追加可能
    ];

    for (name, pos) in test_positions {
        println!("Testing: {name}");

        // PVテーブルありの探索
        let mut searcher_with_pv = EnhancedSearcher::new(16, evaluator.clone());

        // 深さ6まで探索を5回実行して平均を取る
        let mut total_nodes = 0u64;
        let mut total_time = Duration::ZERO;
        let depth = 6;
        let iterations = 5;

        for i in 0..iterations {
            let mut pos_copy = pos.clone();
            let start = Instant::now();

            match run_search_safe(&mut searcher_with_pv, &mut pos_copy, depth) {
                Ok((best_move, score, nodes)) => {
                    let elapsed = start.elapsed();
                    total_nodes += nodes;
                    total_time += elapsed;

                    if i == 0 {
                        println!("  Best move: {best_move:?}, Score: {score}");
                        println!("  Nodes searched: {nodes}");
                        let pv = searcher_with_pv.principal_variation();
                        println!(
                            "  PV (length {}): {:?}",
                            pv.len(),
                            pv.iter().take(5).collect::<Vec<_>>()
                        );
                    }
                }
                Err(e) => {
                    eprintln!("  Search failed at iteration {}: {}", i + 1, e);
                    continue;
                }
            }
        }

        let avg_nodes = total_nodes / iterations as u64;
        let avg_time = total_time / iterations as u32;
        let nps = if avg_time.as_secs_f64() > 0.0 {
            avg_nodes as f64 / avg_time.as_secs_f64()
        } else {
            0.0
        };

        println!("  Average over {iterations} iterations:");
        println!("    Nodes: {avg_nodes}");
        println!("    Time: {avg_time:?}");
        println!("    NPS: {nps:.0}");
        println!();
    }

    // 反復深化での効果測定
    println!("\nIterative Deepening with PV Table:");
    println!("==================================");

    let pos = Position::startpos();
    let mut searcher = EnhancedSearcher::new(16, evaluator.clone());

    for depth in 1..=8 {
        let mut pos_copy = pos.clone();
        let start = Instant::now();

        match run_search_safe(&mut searcher, &mut pos_copy, depth) {
            Ok((best_move, score, nodes)) => {
                let elapsed = start.elapsed();
                let nps = if elapsed.as_secs_f64() > 0.0 {
                    nodes as f64 / elapsed.as_secs_f64()
                } else {
                    0.0
                };

                println!(
                    "  Depth {depth}: {nodes} nodes in {elapsed:?} ({nps:.0} nps), score: {score}, move: {best_move:?}"
                );
            }
            Err(e) => {
                eprintln!("  Search failed at depth {depth}: {e}");
                break;
            }
        }
    }
}
