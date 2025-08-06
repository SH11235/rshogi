//! Benchmark to measure false-sharing optimization effects
//!
//! This benchmark tests the performance improvement from cache-padding
//! in SharedHistory and DuplicationStats

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::{
    search::parallel::{DuplicationStats, SharedHistory},
    shogi::{Color, PieceType, Square},
};
use std::hint::black_box;
use std::sync::{atomic::Ordering, Arc};
use std::thread;
use std::time::Duration;

/// Benchmark concurrent SharedHistory updates
fn bench_shared_history_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("shared_history_concurrent");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(20);

    for num_threads in [2, 4, 8].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_threads),
            num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let history = Arc::new(SharedHistory::new());
                    let mut handles = vec![];

                    for thread_id in 0..num_threads {
                        let history = history.clone();
                        let handle = thread::spawn(move || {
                            // Each thread updates different squares to measure false-sharing
                            for i in 0..1000 {
                                // Spread updates across different cache lines
                                let square_idx = (thread_id * 9 + i % 9) % 81;
                                let file = (square_idx % 9) as u8;
                                let rank = (square_idx / 9) as u8;
                                let square = Square::new(file + 1, rank + 1);

                                history.update(
                                    Color::Black,
                                    PieceType::Pawn,
                                    square,
                                    black_box(10),
                                );

                                // Also read to stress cache coherency
                                let _val = history.get(Color::Black, PieceType::Pawn, square);
                            }
                        });
                        handles.push(handle);
                    }

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark concurrent DuplicationStats updates
fn bench_duplication_stats_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("duplication_stats_concurrent");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(20);

    for num_threads in [2, 4, 8].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_threads),
            num_threads,
            |b, &num_threads| {
                b.iter(|| {
                    let stats = Arc::new(DuplicationStats::default());
                    let mut handles = vec![];

                    for thread_id in 0..num_threads {
                        let stats = stats.clone();
                        let handle = thread::spawn(move || {
                            for _ in 0..10000 {
                                // Threads alternately update unique_nodes and total_nodes
                                if thread_id % 2 == 0 {
                                    stats.unique_nodes.fetch_add(1, Ordering::Relaxed);
                                } else {
                                    stats.total_nodes.fetch_add(1, Ordering::Relaxed);
                                }

                                // Occasionally read both values to stress cache
                                if thread_id % 100 == 0 {
                                    let _dup = stats.get_duplication_percentage();
                                }
                            }
                        });
                        handles.push(handle);
                    }

                    for handle in handles {
                        handle.join().unwrap();
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark history aging operation
fn bench_history_aging(c: &mut Criterion) {
    let mut group = c.benchmark_group("history_aging");

    group.bench_function("age_history", |b| {
        let history = SharedHistory::new();

        // Pre-populate with some values
        for i in 0..81 {
            let file = (i % 9) as u8;
            let rank = (i / 9) as u8;
            let square = Square::new(file + 1, rank + 1);
            history.update(Color::Black, PieceType::Pawn, square, 1000);
            history.update(Color::White, PieceType::Rook, square, 2000);
        }

        b.iter(|| {
            history.age();
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_shared_history_concurrent,
    bench_duplication_stats_concurrent,
    bench_history_aging
);
criterion_main!(benches);
