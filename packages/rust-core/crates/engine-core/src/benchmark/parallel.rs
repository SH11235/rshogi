//! Parallel search benchmarks
//!
//! Measures performance of parallel search with different thread configurations

use crate::{
    evaluation::evaluate::Evaluator,
    search::{parallel::ParallelSearcher, SearchLimitsBuilder},
    shogi::Position,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Parallel benchmark results for a specific thread configuration
#[derive(Debug, Clone)]
pub struct ParallelBenchmarkResult {
    /// Number of threads used
    pub thread_count: usize,
    /// Nodes per second
    pub nps: u64,
    /// Speedup factor compared to single thread
    pub speedup: f64,
    /// Efficiency (speedup / thread_count)
    pub efficiency: f64,
    /// Node duplication rate (percentage)
    pub duplication_rate: f64,
    /// Stop latency in milliseconds
    pub stop_latency_ms: f64,
    /// Principal variation match rate (percentage)
    pub pv_match_rate: f64,
    /// Total nodes searched
    pub nodes: u64,
    /// Total time elapsed
    pub elapsed: Duration,
}

/// Configuration for parallel benchmark
pub struct ParallelBenchmarkConfig {
    /// Thread configurations to test
    pub thread_counts: Vec<usize>,
    /// Depth to search to
    pub search_depth: u8,
    /// Time limit per position (ms)
    pub time_limit_ms: Option<u64>,
    /// Positions to test
    pub positions: Vec<Position>,
    /// Whether to measure stop latency
    pub measure_stop_latency: bool,
}

impl Default for ParallelBenchmarkConfig {
    fn default() -> Self {
        Self {
            thread_counts: vec![1, 2, 4, 8],
            search_depth: 10,
            time_limit_ms: None,
            positions: vec![Position::startpos()],
            measure_stop_latency: true,
        }
    }
}

/// Run parallel search benchmark
pub fn run_parallel_benchmark<E: Evaluator + Send + Sync + 'static>(
    evaluator: Arc<E>,
    config: ParallelBenchmarkConfig,
) -> Vec<ParallelBenchmarkResult> {
    let mut results = Vec::new();

    // Get baseline (1 thread) performance first
    let baseline_result = benchmark_thread_config(
        evaluator.clone(),
        1,
        &config.positions,
        config.search_depth,
        config.time_limit_ms,
    );

    let baseline_nps = baseline_result.nps;
    results.push(baseline_result);

    // Test other thread configurations
    for &thread_count in &config.thread_counts {
        if thread_count == 1 {
            continue; // Already tested
        }

        println!("\nBenchmarking with {thread_count} threads...");

        let mut result = benchmark_thread_config(
            evaluator.clone(),
            thread_count,
            &config.positions,
            config.search_depth,
            config.time_limit_ms,
        );

        // Calculate speedup and efficiency
        result.speedup = result.nps as f64 / baseline_nps as f64;
        result.efficiency = result.speedup / thread_count as f64;

        // Measure stop latency if requested
        if config.measure_stop_latency {
            result.stop_latency_ms =
                measure_stop_latency(evaluator.clone(), thread_count, &config.positions[0]);
        }

        results.push(result);
    }

    results
}

/// Benchmark a specific thread configuration
fn benchmark_thread_config<E: Evaluator + Send + Sync + 'static>(
    evaluator: Arc<E>,
    thread_count: usize,
    positions: &[Position],
    search_depth: u8,
    time_limit_ms: Option<u64>,
) -> ParallelBenchmarkResult {
    let tt = Arc::new(crate::search::TranspositionTable::new(128)); // 128MB TT
    let mut total_nodes = 0u64;
    let mut total_time = Duration::ZERO;
    let mut pv_matches = 0;
    let total_positions = positions.len();

    // Store single-thread PVs for comparison
    let mut single_thread_pvs = Vec::new();
    if thread_count > 1 {
        // Get single thread PVs first
        let mut single_searcher = ParallelSearcher::new(evaluator.clone(), tt.clone(), 1);
        for pos in positions {
            let mut pos_clone = pos.clone();
            let limits = SearchLimitsBuilder::default().depth(search_depth).build();
            let result = single_searcher.search(&mut pos_clone, limits);
            single_thread_pvs.push(result.best_move);
        }
    }

    // Create searcher with specified thread count
    let mut searcher = ParallelSearcher::new(evaluator.clone(), tt.clone(), thread_count);

    // Run benchmark on all positions
    for (idx, pos) in positions.iter().enumerate() {
        let mut pos_clone = pos.clone();

        let limits = if let Some(time_ms) = time_limit_ms {
            SearchLimitsBuilder::default().fixed_time_ms(time_ms).build()
        } else {
            SearchLimitsBuilder::default().depth(search_depth).build()
        };

        let result = searcher.search(&mut pos_clone, limits);

        total_nodes += result.stats.nodes;
        total_time += result.stats.elapsed;

        // Check PV match for multi-threaded runs
        if thread_count > 1
            && idx < single_thread_pvs.len()
            && result.best_move == single_thread_pvs[idx]
        {
            pv_matches += 1;
        }
    }

    let nps = if total_time.as_secs_f64() > 0.0 {
        (total_nodes as f64 / total_time.as_secs_f64()) as u64
    } else {
        0
    };

    let duplication_rate = searcher.get_duplication_percentage();

    let pv_match_rate = if thread_count > 1 && total_positions > 0 {
        (pv_matches as f64 / total_positions as f64) * 100.0
    } else {
        100.0 // Single thread always matches itself
    };

    ParallelBenchmarkResult {
        thread_count,
        nps,
        speedup: 1.0,    // Will be calculated later
        efficiency: 1.0, // Will be calculated later
        duplication_rate,
        stop_latency_ms: 0.0, // Will be measured separately if needed
        pv_match_rate,
        nodes: total_nodes,
        elapsed: total_time,
    }
}

/// Measure stop latency for a given thread configuration
fn measure_stop_latency<E: Evaluator + Send + Sync + 'static>(
    evaluator: Arc<E>,
    thread_count: usize,
    position: &Position,
) -> f64 {
    let tt = Arc::new(crate::search::TranspositionTable::new(32)); // Smaller TT for latency test
    let mut total_latency = 0.0;
    let iterations = 10;

    for _ in 0..iterations {
        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt.clone(), thread_count);
        let mut pos_clone = position.clone();

        // Search with 100ms time limit
        let limits = SearchLimitsBuilder::default().fixed_time_ms(100).build();

        let start = Instant::now();
        let _result = searcher.search(&mut pos_clone, limits);
        let actual_time = start.elapsed();

        // Calculate overshoot in milliseconds
        let expected_ms = 100.0;
        let actual_ms = actual_time.as_secs_f64() * 1000.0;
        let latency = (actual_ms - expected_ms).max(0.0);

        total_latency += latency;
    }

    total_latency / iterations as f64
}

/// Print benchmark results in a formatted table
pub fn print_benchmark_results(results: &[ParallelBenchmarkResult]) {
    println!("\n=== Parallel Search Benchmark Results ===");
    println!();
    println!("Threads |      NPS | Speedup | Efficiency | Dup% | Stop Latency | PV Match%");
    println!("--------|----------|---------|------------|------|--------------|----------");

    for result in results {
        println!(
            "{:7} | {:8} | {:7.2}x | {:9.1}% | {:4.1} | {:10.1}ms | {:8.1}%",
            result.thread_count,
            result.nps,
            result.speedup,
            result.efficiency * 100.0,
            result.duplication_rate,
            result.stop_latency_ms,
            result.pv_match_rate,
        );
    }

    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluation::evaluate::MaterialEvaluator;

    #[test]
    fn test_parallel_benchmark_basic() {
        let evaluator = Arc::new(MaterialEvaluator);
        let config = ParallelBenchmarkConfig {
            thread_counts: vec![1, 2],
            search_depth: 2, // Very shallow for test
            time_limit_ms: None,
            positions: vec![Position::startpos()],
            measure_stop_latency: false,
        };

        let results = run_parallel_benchmark(evaluator, config);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].thread_count, 1);
        assert_eq!(results[1].thread_count, 2);

        // Basic sanity checks
        assert!(results[0].nps > 0);
        assert!(results[1].nps > 0);
        assert!(results[1].speedup > 1.0); // 2 threads should be faster
        assert!(results[1].efficiency > 0.5); // At least 50% efficiency
    }
}
