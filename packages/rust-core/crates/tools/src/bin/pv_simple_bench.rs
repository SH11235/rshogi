//! PVテーブル効果の簡易測定

use std::sync::Arc;
use std::time::Instant;

use engine_core::evaluate::MaterialEvaluator;
use engine_core::search::search_enhanced::EnhancedSearcher;
use engine_core::shogi::Move;
use engine_core::Position;

/// Run search with panic recovery
fn run_search_safe(
    searcher: &mut EnhancedSearcher,
    pos: &mut Position,
    depth: u8,
) -> Result<(Option<Move>, i32, u64), String> {
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
    println!("PV Table Performance Test");
    println!("=========================\n");

    let evaluator = Arc::new(MaterialEvaluator);
    let pos = Position::startpos();

    // 深さごとに探索時間を測定
    println!("Iterative Deepening Performance:");
    println!("Depth | Time (ms) | Nodes     | Best Move | Score | PV Length");
    println!("------|-----------|-----------|-----------|-------|----------");

    let mut searcher = EnhancedSearcher::new(16, evaluator.clone());
    let mut total_time = 0u128;
    let mut total_nodes = 0u64;

    for depth in 1..=7 {
        let mut pos_copy = pos.clone();
        let start = Instant::now();

        match run_search_safe(&mut searcher, &mut pos_copy, depth) {
            Ok((best_move, score, nodes)) => {
                let elapsed = start.elapsed().as_millis();
                total_time += elapsed;
                total_nodes += nodes;

                let pv = searcher.principal_variation();
                let pv_len = pv.len();

                println!(
                    "{depth:5} | {elapsed:9} | {nodes:9} | {best_move:9?} | {score:5} | {pv_len:9}"
                );
            }
            Err(e) => {
                eprintln!("Search failed at depth {depth}: {e}");
                break;
            }
        }
    }

    println!("\nTotal time: {total_time} ms");
    println!("Total nodes: {total_nodes}");

    // PVの内容を表示
    println!("\nFinal Principal Variation:");
    let pv = searcher.principal_variation();
    for (i, mv) in pv.iter().enumerate().take(10) {
        println!("  {}: {}", i + 1, mv);
    }
}
