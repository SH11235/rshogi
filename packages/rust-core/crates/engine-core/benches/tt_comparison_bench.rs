//! Comprehensive comparison benchmark between TT v1 and v2
//!
//! Measures cache efficiency, replacement strategy effectiveness, and overall performance

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use engine_core::search::{tt::TranspositionTable, tt_v2::TranspositionTableV2};
use rand::Rng;
use std::hint::black_box;
use std::time::Duration;

/// Generate test hashes with different access patterns
fn generate_test_hashes(pattern: &str, count: usize) -> Vec<u64> {
    let mut rng = rand::rng();

    match pattern {
        "random" => (0..count).map(|_| rng.random()).collect(),
        "sequential" => (0..count as u64).map(|i| i * 0x1000).collect(),
        "clustered" => {
            // Simulate positions that are close in the search tree
            let clusters = 10;
            let per_cluster = count / clusters;
            let mut hashes = Vec::with_capacity(count);

            for _c in 0..clusters {
                let base = rng.random::<u64>();
                for i in 0..per_cluster {
                    hashes.push(base.wrapping_add(i as u64));
                }
            }
            hashes
        }
        "realistic" => {
            // Simulate iterative deepening pattern
            let mut hashes = Vec::with_capacity(count);
            let base_positions = 100;

            for depth in 1..=10 {
                for pos in 0..base_positions {
                    let base = (pos as u64) * 0x123456789ABCDEF;
                    for variation in 0..depth {
                        hashes.push(base.wrapping_add(variation as u64));
                        if hashes.len() >= count {
                            return hashes;
                        }
                    }
                }
            }
            hashes
        }
        _ => panic!("Unknown pattern: {pattern}"),
    }
}

/// Benchmark cache efficiency with different access patterns
fn bench_cache_efficiency(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_cache_efficiency");
    group.measurement_time(Duration::from_secs(10));

    let patterns = vec!["sequential", "random", "clustered", "realistic"];
    let operations = 100_000;

    for pattern in patterns {
        let hashes = generate_test_hashes(pattern, operations);

        // Benchmark v1
        group.bench_with_input(BenchmarkId::new("v1", pattern), &hashes, |b, hashes| {
            let tt = TranspositionTable::new(16);

            // Pre-fill with some entries
            for (i, &hash) in hashes.iter().take(10_000).enumerate() {
                tt.store(hash, None, i as i16, 0, 10, engine_core::search::tt::NodeType::Exact);
            }

            b.iter(|| {
                let mut hits = 0;
                for &hash in hashes.iter() {
                    if tt.probe(hash).is_some() {
                        hits += 1;
                    }
                }
                black_box(hits)
            });
        });

        // Benchmark v2
        group.bench_with_input(BenchmarkId::new("v2", pattern), &hashes, |b, hashes| {
            let tt = TranspositionTableV2::new(16);

            // Pre-fill with some entries
            for (i, &hash) in hashes.iter().take(10_000).enumerate() {
                tt.store(hash, None, i as i16, 0, 10, engine_core::search::tt_v2::NodeType::Exact);
            }

            b.iter(|| {
                let mut hits = 0;
                for &hash in hashes.iter() {
                    if tt.probe(hash).is_some() {
                        hits += 1;
                    }
                }
                black_box(hits)
            });
        });
    }

    group.finish();
}

/// Benchmark replacement strategy effectiveness
fn bench_replacement_strategy(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_replacement");
    group.measurement_time(Duration::from_secs(5));

    let table_size_mb = 1; // Small table to force replacements
    let unique_positions = 50_000;

    group.bench_function("v1_replacement", |b| {
        b.iter(|| {
            let mut tt = TranspositionTable::new(table_size_mb);
            let mut rng = rand::rng();

            // Simulate search with different depths
            for _iteration in 0..5 {
                tt.new_search();

                for _ in 0..unique_positions {
                    let hash = rng.random();
                    let depth = rng.random_range(1..20);
                    let score = rng.random_range(-1000..1000);
                    let node_type = match rng.random_range(0..3) {
                        0 => engine_core::search::tt::NodeType::Exact,
                        1 => engine_core::search::tt::NodeType::LowerBound,
                        _ => engine_core::search::tt::NodeType::UpperBound,
                    };

                    tt.store(hash, None, score, 0, depth, node_type);
                }
            }

            black_box(tt.hashfull())
        });
    });

    group.bench_function("v2_replacement", |b| {
        b.iter(|| {
            let mut tt = TranspositionTableV2::new(table_size_mb);
            let mut rng = rand::rng();

            // Simulate search with different depths
            for _iteration in 0..5 {
                tt.new_search();

                for _ in 0..unique_positions {
                    let hash = rng.random();
                    let depth = rng.random_range(1..20);
                    let score = rng.random_range(-1000..1000);
                    let node_type = match rng.random_range(0..3) {
                        0 => engine_core::search::tt_v2::NodeType::Exact,
                        1 => engine_core::search::tt_v2::NodeType::LowerBound,
                        _ => engine_core::search::tt_v2::NodeType::UpperBound,
                    };

                    tt.store(hash, None, score, 0, depth, node_type);
                }
            }

            black_box(tt.hashfull())
        });
    });

    group.finish();
}

/// Benchmark mixed read/write workload
fn bench_mixed_workload(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_mixed_workload");
    group.throughput(Throughput::Elements(10_000));

    group.bench_function("v1_mixed_70_30", |b| {
        let tt = TranspositionTable::new(16);
        let mut rng = rand::rng();

        b.iter(|| {
            for _ in 0..10_000 {
                let hash = rng.random();

                if rng.random_range(0..100) < 70 {
                    // 70% reads
                    black_box(tt.probe(hash));
                } else {
                    // 30% writes
                    tt.store(hash, None, 100, 0, 10, engine_core::search::tt::NodeType::Exact);
                }
            }
        });
    });

    group.bench_function("v2_mixed_70_30", |b| {
        let tt = TranspositionTableV2::new(16);
        let mut rng = rand::rng();

        b.iter(|| {
            for _ in 0..10_000 {
                let hash = rng.random();

                if rng.random_range(0..100) < 70 {
                    // 70% reads
                    black_box(tt.probe(hash));
                } else {
                    // 30% writes
                    tt.store(hash, None, 100, 0, 10, engine_core::search::tt_v2::NodeType::Exact);
                }
            }
        });
    });

    group.finish();
}

/// Benchmark prefetch effectiveness
fn bench_prefetch_effectiveness(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_prefetch");

    let tt_v1 = TranspositionTable::new(16);
    let tt_v2 = TranspositionTableV2::new(16);
    let hashes = generate_test_hashes("realistic", 10_000);

    // Fill tables
    for (i, &hash) in hashes.iter().take(5_000).enumerate() {
        tt_v1.store(hash, None, i as i16, 0, 10, engine_core::search::tt::NodeType::Exact);
        tt_v2.store(hash, None, i as i16, 0, 10, engine_core::search::tt_v2::NodeType::Exact);
    }

    group.bench_function("v1_with_prefetch", |b| {
        b.iter(|| {
            for i in 0..hashes.len() - 1 {
                // Prefetch next entry
                tt_v1.prefetch(hashes[i + 1]);
                // Access current entry
                black_box(tt_v1.probe(hashes[i]));
            }
        });
    });

    group.bench_function("v2_with_prefetch", |b| {
        b.iter(|| {
            for i in 0..hashes.len() - 1 {
                // Prefetch next entry
                tt_v2.prefetch(hashes[i + 1]);
                // Access current entry
                black_box(tt_v2.probe(hashes[i]));
            }
        });
    });

    group.bench_function("v1_without_prefetch", |b| {
        b.iter(|| {
            for &hash in &hashes {
                black_box(tt_v1.probe(hash));
            }
        });
    });

    group.bench_function("v2_without_prefetch", |b| {
        b.iter(|| {
            for &hash in &hashes {
                black_box(tt_v2.probe(hash));
            }
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_cache_efficiency,
    bench_replacement_strategy,
    bench_mixed_workload,
    bench_prefetch_effectiveness
);
criterion_main!(benches);
