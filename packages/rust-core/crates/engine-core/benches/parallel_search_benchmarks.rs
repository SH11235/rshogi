//! Parallel search benchmarks using Criterion
//!
//! Measures performance of parallel search with different thread configurations

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{parallel::ParallelSearcher, SearchLimitsBuilder, TranspositionTable},
    Position,
};
use std::sync::Arc;
use std::time::Duration;

/// Benchmark parallel search with different thread counts
fn bench_parallel_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_search");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);

    let evaluator = Arc::new(MaterialEvaluator);
    let thread_counts = vec![1, 2, 4, 8];

    // Test positions
    let positions = vec![
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
    ];

    for (pos_name, position) in positions {
        for &thread_count in &thread_counts {
            group.bench_with_input(
                BenchmarkId::new(format!("depth_8/{pos_name}"), thread_count),
                &thread_count,
                |b, &threads| {
                    b.iter(|| {
                        let tt = Arc::new(TranspositionTable::new(64)); // 64MB TT
                        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                        let mut pos_clone = position.clone();
                        let limits = SearchLimitsBuilder::default().depth(8).build();
                        searcher.search(&mut pos_clone, limits)
                    });
                },
            );
        }
    }

    group.finish();
}

/// Benchmark nodes per second scaling
fn bench_nps_scaling(c: &mut Criterion) {
    let mut group = c.benchmark_group("nps_scaling");
    group.measurement_time(Duration::from_secs(5));

    let evaluator = Arc::new(MaterialEvaluator);
    let position = Position::startpos();
    let thread_counts = vec![1, 2, 4, 8];

    for &thread_count in &thread_counts {
        group.bench_with_input(
            BenchmarkId::new("fixed_time", thread_count),
            &thread_count,
            |b, &threads| {
                b.iter(|| {
                    let tt = Arc::new(TranspositionTable::new(64));
                    let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                    let mut pos_clone = position.clone();
                    let limits = SearchLimitsBuilder::default().fixed_time_ms(100).build();
                    let result = searcher.search(&mut pos_clone, limits);
                    result.stats.nodes // Return nodes searched
                });
            },
        );
    }

    group.finish();
}

/// Benchmark stop latency
fn bench_stop_latency(c: &mut Criterion) {
    let mut group = c.benchmark_group("stop_latency");
    group.measurement_time(Duration::from_secs(3));
    group.sample_size(20);

    let evaluator = Arc::new(MaterialEvaluator);
    let position = Position::startpos();
    let thread_counts = vec![1, 2, 4, 8];

    for &thread_count in &thread_counts {
        group.bench_with_input(
            BenchmarkId::new("100ms_limit", thread_count),
            &thread_count,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let mut total_overshoot = Duration::ZERO;

                    for _ in 0..iters {
                        let tt = Arc::new(TranspositionTable::new(32));
                        let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                        let mut pos_clone = position.clone();
                        let limits = SearchLimitsBuilder::default().fixed_time_ms(100).build();

                        let start = std::time::Instant::now();
                        searcher.search(&mut pos_clone, limits);
                        let elapsed = start.elapsed();

                        // Calculate overshoot
                        if elapsed > Duration::from_millis(100) {
                            total_overshoot += elapsed - Duration::from_millis(100);
                        }
                    }

                    total_overshoot
                });
            },
        );
    }

    group.finish();
}

/// Benchmark duplication rate
fn bench_duplication_rate(c: &mut Criterion) {
    let mut group = c.benchmark_group("duplication_rate");
    group.measurement_time(Duration::from_secs(5));

    let evaluator = Arc::new(MaterialEvaluator);
    let position = Position::startpos();
    let thread_counts = vec![2, 4, 8]; // Skip 1 thread (no duplication)

    for &thread_count in &thread_counts {
        group.bench_with_input(
            BenchmarkId::new("depth_10", thread_count),
            &thread_count,
            |b, &threads| {
                b.iter(|| {
                    let tt = Arc::new(TranspositionTable::new(128));
                    let mut searcher = ParallelSearcher::new(evaluator.clone(), tt, threads);
                    let mut pos_clone = position.clone();
                    let limits = SearchLimitsBuilder::default().depth(10).build();
                    searcher.search(&mut pos_clone, limits);
                    searcher.get_duplication_percentage()
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_parallel_search,
    bench_nps_scaling,
    bench_stop_latency,
    bench_duplication_rate
);
criterion_main!(benches);
