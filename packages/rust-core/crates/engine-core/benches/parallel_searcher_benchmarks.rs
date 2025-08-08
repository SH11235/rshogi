//! ParallelSearcher benchmarks using Criterion
//!
//! Clean benchmarks for the new simplified parallel search implementation

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{parallel::ParallelSearcher, SearchLimitsBuilder, ShardedTranspositionTable},
    shogi::Position,
    time_management::TimeControl,
};
use std::sync::Arc;
use std::time::Duration;

/// Configuration for benchmarks
struct BenchConfig {
    threads: Vec<usize>,
    depth: u8,
    tt_size_mb: usize,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            threads: vec![1, 2, 4, 8],
            depth: 6,
            tt_size_mb: 128,
        }
    }
}

/// Get standard test positions
fn get_test_positions() -> Vec<(&'static str, Position)> {
    vec![
        ("startpos", Position::startpos()),
        (
            "midgame",
            Position::from_sfen(
                "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17",
            )
            .unwrap(),
        ),
        (
            "endgame",
            Position::from_sfen(
                "1n5n1/2s3k2/3p1p1p1/2p3p2/9/2P3P2/3P1P1P1/2K6/1N5N1 b RBGSLPrbgs2l13p 80",
            )
            .unwrap(),
        ),
    ]
}

/// Benchmark: Pure search performance at fixed depth
fn bench_depth_search(c: &mut Criterion) {
    let config = BenchConfig::default();
    let mut group = c.benchmark_group("simple_parallel/depth_search");

    // Configure measurement
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(20);

    let evaluator = Arc::new(MaterialEvaluator);
    let positions = get_test_positions();

    for (pos_name, position) in &positions {
        for &thread_count in &config.threads {
            group.bench_with_input(
                BenchmarkId::new(*pos_name, thread_count),
                &thread_count,
                |b, &threads| {
                    b.iter(|| {
                        // Fresh TT for each iteration to ensure consistency
                        let tt = Arc::new(ShardedTranspositionTable::new(config.tt_size_mb));
                        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                        let mut pos_clone = position.clone();

                        let limits = SearchLimitsBuilder::default().depth(config.depth).build();

                        let result = searcher.search(&mut pos_clone, limits);
                        result.stats.nodes // Return nodes for throughput measurement
                    });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark: Nodes per second with fixed time
fn bench_nps_throughput(c: &mut Criterion) {
    let config = BenchConfig::default();
    let mut group = c.benchmark_group("simple_parallel/nps_throughput");

    group.measurement_time(Duration::from_secs(15));
    group.sample_size(10);

    let evaluator = Arc::new(MaterialEvaluator);
    let position = Position::startpos();
    let fixed_ms = 100; // 100ms searches

    for &thread_count in &config.threads {
        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(thread_count),
            &thread_count,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let mut total_time = Duration::ZERO;
                    let mut _total_nodes = 0u64;

                    for _ in 0..iters {
                        let tt = Arc::new(ShardedTranspositionTable::new(config.tt_size_mb));
                        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                        let mut pos_clone = position.clone();

                        let limits = SearchLimitsBuilder::default()
                            .time_control(TimeControl::FixedTime {
                                ms_per_move: fixed_ms,
                            })
                            .depth(20) // High depth as safety limit
                            .build();

                        let start = std::time::Instant::now();
                        let _result = searcher.search(&mut pos_clone, limits);
                        let elapsed = start.elapsed();

                        total_time += elapsed;
                        _total_nodes += _result.stats.nodes;
                    }

                    total_time
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Stop latency measurement
fn bench_stop_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("simple_parallel/stop_latency");

    group.measurement_time(Duration::from_secs(5));
    group.sample_size(30);

    let evaluator = Arc::new(MaterialEvaluator);
    let position = Position::startpos();
    let thread_counts = vec![1, 2, 4, 8];
    let target_ms = 50; // 50ms target

    for &thread_count in &thread_counts {
        group.bench_with_input(
            BenchmarkId::from_parameter(thread_count),
            &thread_count,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let mut total_overshoot = Duration::ZERO;

                    for _ in 0..iters {
                        let tt = Arc::new(ShardedTranspositionTable::new(64)); // Smaller TT for latency test
                        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                        let mut pos_clone = position.clone();

                        let limits = SearchLimitsBuilder::default()
                            .time_control(TimeControl::FixedTime {
                                ms_per_move: target_ms,
                            })
                            .build();

                        let start = std::time::Instant::now();
                        searcher.search(&mut pos_clone, limits);
                        let elapsed = start.elapsed();

                        // Measure overshoot
                        if elapsed > Duration::from_millis(target_ms) {
                            total_overshoot += elapsed - Duration::from_millis(target_ms);
                        }
                    }

                    total_overshoot
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Speedup efficiency
fn bench_speedup_efficiency(c: &mut Criterion) {
    let config = BenchConfig::default();
    let mut group = c.benchmark_group("simple_parallel/speedup");

    group.measurement_time(Duration::from_secs(20));
    group.sample_size(5);

    let evaluator = Arc::new(MaterialEvaluator);
    let position = Position::startpos();

    // First, get baseline with 1 thread
    let tt = Arc::new(ShardedTranspositionTable::new(config.tt_size_mb));
    let mut baseline_searcher = ParallelSearcher::new(evaluator.clone(), tt, 1);
    let mut pos_clone = position.clone();

    let limits = SearchLimitsBuilder::default().depth(config.depth).build();

    let baseline_start = std::time::Instant::now();
    let baseline_result = baseline_searcher.search(&mut pos_clone, limits.clone());
    let baseline_time = baseline_start.elapsed();
    let baseline_nps = (baseline_result.stats.nodes as f64) / baseline_time.as_secs_f64();

    println!("Baseline (1 thread): {} NPS", baseline_nps as u64);

    // Test scaling with multiple threads
    for &thread_count in &config.threads {
        if thread_count == 1 {
            continue; // Skip baseline
        }

        group.bench_with_input(
            BenchmarkId::from_parameter(thread_count),
            &thread_count,
            |b, &threads| {
                b.iter(|| {
                    let tt = Arc::new(ShardedTranspositionTable::new(config.tt_size_mb));
                    let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                    let mut pos_clone = position.clone();

                    let start = std::time::Instant::now();
                    let result = searcher.search(&mut pos_clone, limits.clone());
                    let elapsed = start.elapsed();

                    let nps = (result.stats.nodes as f64) / elapsed.as_secs_f64();
                    let speedup = nps / baseline_nps;

                    (result.stats.nodes, speedup)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark: Consistency check (PV stability)
fn bench_pv_consistency(c: &mut Criterion) {
    let mut group = c.benchmark_group("simple_parallel/pv_consistency");

    group.measurement_time(Duration::from_secs(5));
    group.sample_size(20);

    let evaluator = Arc::new(MaterialEvaluator);
    let positions = get_test_positions();
    let thread_counts = vec![1, 2, 4];

    for (pos_name, position) in &positions {
        for &thread_count in &thread_counts {
            group.bench_with_input(
                BenchmarkId::new(*pos_name, thread_count),
                &thread_count,
                |b, &threads| {
                    // Get reference PV with single thread
                    let tt = Arc::new(ShardedTranspositionTable::new(128));
                    let mut ref_searcher = ParallelSearcher::new(evaluator.clone(), tt, 1);
                    let mut ref_pos = position.clone();

                    let limits = SearchLimitsBuilder::default().depth(6).build();
                    let ref_result = ref_searcher.search(&mut ref_pos, limits.clone());
                    let ref_move = ref_result.best_move;

                    b.iter(|| {
                        let tt = Arc::new(ShardedTranspositionTable::new(128));
                        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                        let mut pos_clone = position.clone();

                        let result = searcher.search(&mut pos_clone, limits.clone());

                        // Return whether PV matches
                        result.best_move == ref_move
                    });
                },
            );
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_depth_search,
    bench_nps_throughput,
    bench_stop_latency,
    bench_speedup_efficiency,
    bench_pv_consistency
);

criterion_main!(benches);
