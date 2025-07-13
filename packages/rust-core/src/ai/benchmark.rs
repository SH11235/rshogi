//! Benchmark for AI performance testing

use super::board::Position;
use super::movegen::MoveGen;
use super::search::{SearchLimits, Searcher};
use std::time::{Duration, Instant};

/// Performance test results
#[derive(Debug)]
pub struct BenchmarkResult {
    /// Nodes per second
    pub nps: u64,
    /// Total nodes searched
    pub nodes: u64,
    /// Time elapsed
    pub elapsed: Duration,
    /// Move generation speed (moves/sec)
    pub movegen_speed: u64,
}

/// Run performance benchmark
pub fn run_benchmark() -> BenchmarkResult {
    println!("Running Shogi AI Benchmark...");

    // Test move generation speed
    let movegen_result = benchmark_movegen();
    println!("Move generation: {movegen_result} moves/sec");

    // Test search performance
    let search_result = benchmark_search();

    BenchmarkResult {
        nps: search_result.0,
        nodes: search_result.1,
        elapsed: search_result.2,
        movegen_speed: movegen_result,
    }
}

/// Benchmark move generation
fn benchmark_movegen() -> u64 {
    let pos = Position::startpos();
    let iterations = 100_000;

    let start = Instant::now();

    for _ in 0..iterations {
        let mut gen = MoveGen::new(&pos);
        let moves = gen.generate_all();
        // Force evaluation to prevent optimization
        std::hint::black_box(moves.len());
    }

    let elapsed = start.elapsed();
    (iterations as f64 / elapsed.as_secs_f64()) as u64
}

/// Benchmark search performance
fn benchmark_search() -> (u64, u64, Duration) {
    let test_positions = vec![
        // Starting position
        Position::startpos(),
        // TODO: Add more test positions
    ];

    let mut total_nodes = 0u64;
    let mut total_time = Duration::ZERO;

    for (i, pos) in test_positions.iter().enumerate() {
        println!("Testing position {}...", i + 1);

        let limits = SearchLimits {
            depth: 8,
            time: Some(Duration::from_secs(5)),
            nodes: None,
        };

        let mut searcher = Searcher::new(limits);
        let mut pos_clone = pos.clone();
        let result = searcher.search(&mut pos_clone);

        total_nodes += result.stats.nodes;
        total_time += result.stats.elapsed;

        println!(
            "  Best move: {:?}, Score: {}, Nodes: {}, Time: {:?}",
            result.best_move, result.score, result.stats.nodes, result.stats.elapsed
        );
    }

    let nps = (total_nodes as f64 / total_time.as_secs_f64()) as u64;

    (nps, total_nodes, total_time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn test_benchmark() {
        let result = run_benchmark();

        // Should achieve reasonable performance
        // Note: Debug builds are much slower than release builds
        assert!(result.movegen_speed > 100_000); // At least 100k moves/sec in debug
        assert!(result.nps > 10_000); // At least 10k NPS in debug
    }
}

/// Main function for standalone benchmark
#[cfg(not(test))]
pub fn main() {
    let result = run_benchmark();

    println!("\n=== Benchmark Results ===");
    println!("Move Generation: {} moves/sec", result.movegen_speed);
    println!("Search NPS: {} nodes/sec", result.nps);
    println!("Total Nodes: {}", result.nodes);
    println!("Total Time: {:?}", result.elapsed);
}
