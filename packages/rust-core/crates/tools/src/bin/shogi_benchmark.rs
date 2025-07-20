//! Shogi AI Benchmark

use engine_core::benchmark::run_benchmark;

fn main() {
    let result = run_benchmark();

    println!("\n=== Benchmark Results ===");
    println!("Move Generation: {} moves/sec", result.movegen_speed);
    println!("Search NPS: {} nodes/sec", result.nps);
    println!("Total Nodes: {}", result.nodes);
    println!("Total Time: {:?}", result.elapsed);
}
