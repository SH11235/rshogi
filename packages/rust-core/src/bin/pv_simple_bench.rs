//! PVテーブル効果の簡易測定

use shogi_core::ai::{
    board::Position, evaluate::MaterialEvaluator, search_enhanced::EnhancedSearcher,
};
use std::sync::Arc;
use std::time::Instant;

fn main() {
    println!("PV Table Performance Test");
    println!("=========================\n");

    let evaluator = Arc::new(MaterialEvaluator);
    let pos = Position::startpos();

    // 深さごとに探索時間を測定
    println!("Iterative Deepening Performance:");
    println!("Depth | Time (ms) | Best Move | Score | PV Length");
    println!("------|-----------|-----------|-------|----------");

    let mut searcher = EnhancedSearcher::new(16, evaluator.clone());
    let mut total_time = 0u128;

    for depth in 1..=7 {
        let start = Instant::now();
        let (best_move, score) = searcher.search(&mut pos.clone(), depth, None, None);
        let elapsed = start.elapsed().as_millis();
        total_time += elapsed;

        let pv = searcher.principal_variation();
        let pv_len = pv.len();

        println!("{depth:5} | {elapsed:9} | {best_move:9?} | {score:5} | {pv_len:9}");
    }

    println!("\nTotal time: {total_time} ms");

    // PVの内容を表示
    println!("\nFinal Principal Variation:");
    let pv = searcher.principal_variation();
    for (i, mv) in pv.iter().enumerate().take(10) {
        println!("  {}: {}", i + 1, mv);
    }
}
