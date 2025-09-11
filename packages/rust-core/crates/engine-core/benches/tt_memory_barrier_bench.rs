//! Benchmark for measuring memory barrier reduction optimization in TT
//!
//! This benchmark compares the performance of TT probe operations
//! before and after memory barrier optimization.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::search::tt::TranspositionTable;
use engine_core::search::NodeType;
use rand::{rng, Rng};
use std::hint::black_box;

/// Generate random hash values for testing
fn generate_random_hashes(count: usize) -> Vec<u64> {
    let mut rng = rng();
    (0..count).map(|_| rng.random()).collect()
}

/// Benchmark TT probe operations with various hit rates
fn bench_tt_probe(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_probe_memory_barrier");

    // Different table sizes to test
    let table_sizes = [1, 8, 32, 128];

    for size_mb in table_sizes {
        // Create TT with specified size
        let tt = TranspositionTable::new(size_mb);

        // Generate test data
        let test_count = 10000;
        let store_count = test_count / 2; // 50% fill rate
        let hashes = generate_random_hashes(test_count);

        // Store some entries
        for (i, &hash) in hashes.iter().enumerate().take(store_count) {
            tt.store(
                hash,
                None,
                (i % 1000) as i16,
                (i % 500) as i16,
                (i % 20) as u8,
                match i % 3 {
                    0 => NodeType::Exact,
                    1 => NodeType::LowerBound,
                    _ => NodeType::UpperBound,
                },
            );
        }

        // Benchmark probe operations - mix of hits and misses
        group.bench_with_input(
            BenchmarkId::new("mixed_access", format!("{size_mb}MB")),
            &hashes,
            |b, hashes| {
                let mut idx = 0;
                b.iter(|| {
                    let hash = hashes[idx % hashes.len()];
                    let result = tt.probe(hash);
                    black_box(result);
                    idx += 1;
                });
            },
        );

        // Benchmark probe operations - mostly hits
        let stored_hashes: Vec<u64> = hashes.iter().take(store_count).copied().collect();
        group.bench_with_input(
            BenchmarkId::new("mostly_hits", format!("{size_mb}MB")),
            &stored_hashes,
            |b, stored_hashes| {
                let mut idx = 0;
                b.iter(|| {
                    let hash = stored_hashes[idx % stored_hashes.len()];
                    let result = tt.probe(hash);
                    black_box(result);
                    idx += 1;
                });
            },
        );

        // Benchmark probe operations - all misses
        let miss_hashes: Vec<u64> = hashes.iter().skip(store_count).copied().collect();
        group.bench_with_input(
            BenchmarkId::new("all_misses", format!("{size_mb}MB")),
            &miss_hashes,
            |b, miss_hashes| {
                let mut idx = 0;
                b.iter(|| {
                    let hash = miss_hashes[idx % miss_hashes.len()];
                    let result = tt.probe(hash);
                    black_box(result);
                    idx += 1;
                });
            },
        );
    }

    group.finish();
}

/// Benchmark concurrent TT access to measure memory barrier impact
fn bench_tt_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_concurrent_memory_barrier");

    // Create a shared TT
    let tt = TranspositionTable::new(32); // 32MB table

    // Pre-populate with entries
    let hashes = generate_random_hashes(100000);
    for (i, &hash) in hashes.iter().enumerate().take(50000) {
        tt.store(hash, None, (i % 1000) as i16, (i % 500) as i16, (i % 20) as u8, NodeType::Exact);
    }

    // Benchmark single-threaded baseline
    group.bench_function("single_thread", |b| {
        let mut idx = 0;
        b.iter(|| {
            for _ in 0..100 {
                let hash = hashes[idx % hashes.len()];
                let result = tt.probe(hash);
                black_box(result);
                idx += 1;
            }
        });
    });

    // Note: Multi-threaded benchmarks would require Arc and thread spawning,
    // which is more complex with Criterion. For now, we focus on single-threaded
    // performance which still benefits from memory barrier reduction.

    group.finish();
}

/// Benchmark to specifically measure SIMD vs scalar probe performance
fn bench_simd_vs_scalar(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_simd_vs_scalar");

    // Test with different bucket sizes
    let tt_small = TranspositionTable::new(8); // Will use 4-entry buckets
    let tt_medium =
        TranspositionTable::new_with_config(16, Some(engine_core::search::tt::BucketSize::Medium));
    let tt_large =
        TranspositionTable::new_with_config(64, Some(engine_core::search::tt::BucketSize::Large));

    let test_hashes = generate_random_hashes(10000);

    // Populate tables
    for (i, &hash) in test_hashes.iter().enumerate().take(5000) {
        let score = (i % 1000) as i16;
        let eval = (i % 500) as i16;
        let depth = (i % 20) as u8;
        let node_type = NodeType::Exact;

        tt_small.store(hash, None, score, eval, depth, node_type);
        tt_medium.store(hash, None, score, eval, depth, node_type);
        tt_large.store(hash, None, score, eval, depth, node_type);
    }

    // Benchmark small buckets (4 entries)
    group.bench_function("bucket_4_entries", |b| {
        let mut idx = 0;
        b.iter(|| {
            let hash = test_hashes[idx % test_hashes.len()];
            let result = tt_small.probe(hash);
            black_box(result);
            idx += 1;
        });
    });

    // Benchmark medium buckets (8 entries)
    group.bench_function("bucket_8_entries", |b| {
        let mut idx = 0;
        b.iter(|| {
            let hash = test_hashes[idx % test_hashes.len()];
            let result = tt_medium.probe(hash);
            black_box(result);
            idx += 1;
        });
    });

    // Benchmark large buckets (16 entries)
    group.bench_function("bucket_16_entries", |b| {
        let mut idx = 0;
        b.iter(|| {
            let hash = test_hashes[idx % test_hashes.len()];
            let result = tt_large.probe(hash);
            black_box(result);
            idx += 1;
        });
    });

    group.finish();
}

criterion_group!(benches, bench_tt_probe, bench_tt_concurrent, bench_simd_vs_scalar);
criterion_main!(benches);
