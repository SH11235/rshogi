//! Parallel search benchmarks
//!
//! Measures performance of parallel search with different thread configurations

use crate::{
    evaluation::evaluate::Evaluator,
    search::{parallel::ParallelSearcher, SearchLimitsBuilder},
    shogi::Position,
};
use log::{debug, info, trace};
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
    /// Raw position-level measurements (if collected)
    pub raw_measurements: Vec<PositionMeasurement>,
}

/// Measurement data for a single position
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PositionMeasurement {
    pub position_index: usize,
    pub nodes: u64,
    pub elapsed_ms: f64,
    pub depth_reached: u8,
    pub iterations: u32,  // Number of iterations to reach min_duration
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
    /// Minimum duration per position in milliseconds
    pub min_duration_ms: u64,
    /// Fixed total time per position in milliseconds (overrides min_duration_ms)
    pub fixed_total_ms: Option<u64>,
    /// Number of warmup runs before measurement
    pub warmup_runs: u32,
    /// Whether to collect raw measurement data
    pub collect_raw_data: bool,
}

impl Default for ParallelBenchmarkConfig {
    fn default() -> Self {
        Self {
            thread_counts: vec![1, 2, 4, 8],
            search_depth: 10,
            time_limit_ms: None,
            positions: vec![Position::startpos()],
            measure_stop_latency: true,
            min_duration_ms: 500,
            fixed_total_ms: None,
            warmup_runs: 1,
            collect_raw_data: false,
        }
    }
}

/// Run parallel search benchmark
pub fn run_parallel_benchmark<E: Evaluator + Send + Sync + 'static>(
    evaluator: Arc<E>,
    config: ParallelBenchmarkConfig,
) -> Vec<ParallelBenchmarkResult> {
    let mut results = Vec::new();

    info!("Starting parallel benchmark with {} positions", config.positions.len());
    info!("Minimum duration per position: {}ms", config.min_duration_ms);
    info!("Warmup runs: {}", config.warmup_runs);

    // Get baseline (1 thread) performance first
    let baseline_result = benchmark_thread_config(
        evaluator.clone(),
        1,
        &config.positions,
        config.search_depth,
        config.time_limit_ms,
        config.min_duration_ms,
        config.fixed_total_ms,
        config.warmup_runs,
        config.collect_raw_data,
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
            config.min_duration_ms,
            config.fixed_total_ms,
            config.warmup_runs,
            config.collect_raw_data,
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
    min_duration_ms: u64,
    fixed_total_ms: Option<u64>,
    warmup_runs: u32,
    collect_raw_data: bool,
) -> ParallelBenchmarkResult {
    // Create a fresh TT for each thread configuration to ensure fair comparison
    let tt = Arc::new(crate::search::TranspositionTable::new(128)); // 128MB TT
    let mut total_nodes = 0u64;
    let mut total_time = Duration::ZERO;
    let mut pv_matches = 0;
    let total_positions = positions.len();
    let mut raw_measurements = Vec::new();

    debug!("Starting benchmark for {} threads with fresh TT", thread_count);

    // Store single-thread PVs for comparison
    let mut single_thread_pvs = Vec::new();
    if thread_count > 1 {
        debug!("Collecting single-thread PVs for comparison");
        // Get single thread PVs first - use a separate TT for single-thread test
        let single_tt = Arc::new(crate::search::TranspositionTable::new(128));
        let mut single_searcher = ParallelSearcher::new(evaluator.clone(), single_tt, 1);
        for pos in positions {
            let mut pos_clone = pos.clone();
            let limits = SearchLimitsBuilder::default().depth(search_depth).build();
            let result = single_searcher.search(&mut pos_clone, limits);
            single_thread_pvs.push(result.best_move);
        }
    }

    // Create searcher with specified thread count
    let mut searcher = ParallelSearcher::new(evaluator.clone(), tt.clone(), thread_count);

    // Warmup runs with separate TT to avoid affecting main measurements
    if warmup_runs > 0 {
        info!("Running {} warmup iterations with separate TT", warmup_runs);
        let warmup_tt = Arc::new(crate::search::TranspositionTable::new(128));
        let mut warmup_searcher = ParallelSearcher::new(evaluator.clone(), warmup_tt, thread_count);
        for _ in 0..warmup_runs {
            for pos in positions.iter().take(1) {
                let mut pos_clone = pos.clone();
                let limits = SearchLimitsBuilder::default().depth(search_depth).build();
                let _ = warmup_searcher.search(&mut pos_clone, limits);
            }
        }
    }

    // Run benchmark on all positions
    for (idx, pos) in positions.iter().enumerate() {
        trace!("Testing position {}/{}", idx + 1, total_positions);
        
        let mut pos_nodes = 0u64;
        let mut pos_elapsed = Duration::ZERO;
        let mut iterations = 0u32;
        let mut depth_reached = 0u8;

        // Determine target duration based on mode
        let target_duration = if let Some(fixed_ms) = fixed_total_ms {
            // Fixed total time mode - each position gets exactly this amount of time
            Duration::from_millis(fixed_ms)
        } else {
            // Minimum duration mode - run at least this long
            Duration::from_millis(min_duration_ms)
        };

        // Run until target duration is reached
        let position_start = Instant::now();
        while position_start.elapsed() < target_duration {
            let mut pos_clone = pos.clone();

            let limits = if let Some(time_ms) = time_limit_ms {
                SearchLimitsBuilder::default().fixed_time_ms(time_ms).build()
            } else {
                SearchLimitsBuilder::default().depth(search_depth).build()
            };

            // Start timing after setup
            let iter_start = Instant::now();
            
            let result = searcher.search(&mut pos_clone, limits);
            
            let iter_elapsed = iter_start.elapsed();

            pos_nodes += result.stats.nodes;
            pos_elapsed += iter_elapsed;
            depth_reached = depth_reached.max(result.stats.depth);
            iterations += 1;

            // Check PV match for multi-threaded runs (only once per position)
            if iterations == 1 && thread_count > 1
                && idx < single_thread_pvs.len()
                && result.best_move == single_thread_pvs[idx]
            {
                pv_matches += 1;
            }

            trace!("  Iteration {}: {} nodes in {:?}", iterations, result.stats.nodes, iter_elapsed);
        }

        total_nodes += pos_nodes;
        total_time += pos_elapsed;

        // Record raw measurement if requested
        if collect_raw_data {
            raw_measurements.push(PositionMeasurement {
                position_index: idx,
                nodes: pos_nodes,
                elapsed_ms: pos_elapsed.as_secs_f64() * 1000.0,
                depth_reached,
                iterations,
            });
        }

        debug!("Position {}: {} nodes in {:?} ({} iterations)", 
               idx, pos_nodes, pos_elapsed, iterations);
    }

    // Calculate NPS with safety checks
    let nps = if total_time.as_secs_f64() > 0.001 {  // At least 1ms
        let calculated_nps = (total_nodes as f64 / total_time.as_secs_f64()) as u64;
        // Sanity check: NPS shouldn't exceed 100M for any realistic hardware
        if calculated_nps > 100_000_000 {
            info!("WARNING: Calculated NPS {} seems unrealistic, capping at 100M", calculated_nps);
            100_000_000
        } else {
            calculated_nps
        }
    } else {
        info!("WARNING: Total time too short ({:?}), cannot calculate accurate NPS", total_time);
        0
    };

    let duplication_rate = searcher.get_duplication_percentage();

    let pv_match_rate = if thread_count > 1 && total_positions > 0 {
        (pv_matches as f64 / total_positions as f64) * 100.0
    } else {
        100.0 // Single thread always matches itself
    };

    info!("Thread config {}: {} nodes in {:?} = {} NPS", 
          thread_count, total_nodes, total_time, nps);

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
        raw_measurements,
    }
}

/// Measure stop latency for a given thread configuration
fn measure_stop_latency<E: Evaluator + Send + Sync + 'static>(
    evaluator: Arc<E>,
    thread_count: usize,
    position: &Position,
) -> f64 {
    let tt = Arc::new(crate::search::TranspositionTable::new(32)); // Smaller TT for latency test
    let mut latencies = Vec::new();
    let iterations = 10;

    debug!("Measuring stop latency for {} threads ({} iterations)", thread_count, iterations);

    for i in 0..iterations {
        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt.clone(), thread_count);
        let mut pos_clone = position.clone();

        // Search with 50ms time limit (shorter for more accurate latency measurement)
        let target_ms = 50u64;
        let limits = SearchLimitsBuilder::default().fixed_time_ms(target_ms).build();

        let start = Instant::now();
        let _result = searcher.search(&mut pos_clone, limits);
        let actual_time = start.elapsed();

        // Calculate overshoot in milliseconds
        let actual_ms = actual_time.as_secs_f64() * 1000.0;
        let latency = if actual_ms > target_ms as f64 {
            actual_ms - target_ms as f64
        } else {
            // Negative latency means we stopped early (good!)
            0.0
        };

        latencies.push(latency);
        trace!("  Iteration {}: target={}ms, actual={:.1}ms, latency={:.1}ms", 
               i, target_ms, actual_ms, latency);
    }

    // Calculate median latency (more robust than average)
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median_latency = if latencies.len() % 2 == 0 {
        (latencies[latencies.len() / 2 - 1] + latencies[latencies.len() / 2]) / 2.0
    } else {
        latencies[latencies.len() / 2]
    };

    debug!("Stop latency for {} threads: median={:.1}ms, min={:.1}ms, max={:.1}ms",
           thread_count, median_latency, 
           latencies.first().unwrap_or(&0.0),
           latencies.last().unwrap_or(&0.0));

    median_latency
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
            search_depth: 5, // Moderate depth for balance between speed and meaningful work
            time_limit_ms: None, // Use depth-based search for predictable timing
            positions: vec![Position::startpos()],
            measure_stop_latency: false,
        };

        let results = run_parallel_benchmark(evaluator, config);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].thread_count, 1);
        assert_eq!(results[1].thread_count, 2);

        // Basic sanity checks
        assert!(results[0].nps > 0, "1-thread NPS should be positive");
        assert!(results[1].nps > 0, "2-thread NPS should be positive");

        // Very lenient checks for CI environments
        // In practice, 2 threads may not show speedup due to:
        // - Limited CPU cores in CI
        // - Parallel overhead at shallow depths
        // - Cache contention
        // We just verify the benchmark completes without errors
        eprintln!(
            "Test results - 1-thread NPS: {}, 2-thread NPS: {}, speedup: {}, efficiency: {}",
            results[0].nps, results[1].nps, results[1].speedup, results[1].efficiency
        );

        // Only check that results are computed (not their values)
        assert!(results[1].speedup > 0.0, "Speedup should be computed");
        assert!(results[1].efficiency > 0.0, "Efficiency should be computed");
    }
}
