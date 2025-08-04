use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::search::tt::{BucketSize, NodeType, TranspositionTable};
use std::hint::black_box;

fn bench_bucket_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("bucket_sizes");

    // Test different bucket sizes
    for size in [BucketSize::Small, BucketSize::Medium, BucketSize::Large] {
        let size_str = format!("{size:?}");

        // Create TT with specific bucket size
        let tt = TranspositionTable::new_with_config(16, Some(size));

        // Benchmark probe operation
        group.bench_with_input(BenchmarkId::new("probe", &size_str), &tt, |b, tt| {
            let hash = 0x1234567890ABCDEF;
            b.iter(|| {
                black_box(tt.probe(black_box(hash)));
            });
        });

        // Benchmark store operation
        group.bench_with_input(BenchmarkId::new("store", &size_str), &tt, |b, tt| {
            let mut hash = 0x1234567890ABCDEF_u64;
            b.iter(|| {
                hash = hash.wrapping_add(1); // Different hash each iteration
                tt.store(
                    black_box(hash),
                    None,
                    black_box(100),
                    black_box(50),
                    black_box(10),
                    NodeType::Exact,
                );
            });
        });
    }

    group.finish();
}

fn bench_8_entry_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("8_entry_simd");

    // Create TT with 8-entry buckets
    let tt = TranspositionTable::new_with_config(16, Some(BucketSize::Medium));

    // Pre-fill with some entries
    for i in 0..1000 {
        let hash = ((i as u64) << 32) | (i as u64);
        tt.store(hash, None, (i % 200) as i16, 0, (i % 20) as u8, NodeType::Exact);
    }

    // Benchmark hit rate
    group.bench_function("probe_hit", |b| {
        let hash = (500_u64 << 32) | 500;
        b.iter(|| {
            black_box(tt.probe(black_box(hash)));
        });
    });

    // Benchmark miss rate
    group.bench_function("probe_miss", |b| {
        let hash = (9999_u64 << 32) | 9999;
        b.iter(|| {
            black_box(tt.probe(black_box(hash)));
        });
    });

    // Benchmark mixed operations
    group.bench_function("mixed_ops", |b| {
        let mut counter = 0u64;
        b.iter(|| {
            counter += 1;
            let hash = (counter << 32) | (counter & 0xFFFF);

            // 70% probes, 30% stores
            if counter % 10 < 7 {
                black_box(tt.probe(black_box(hash)));
            } else {
                tt.store(
                    black_box(hash),
                    None,
                    black_box((counter % 200) as i16),
                    black_box(0),
                    black_box((counter % 20) as u8),
                    NodeType::Exact,
                );
            }
        });
    });

    group.finish();
}

fn bench_memory_patterns(c: &mut Criterion) {
    let mut group = c.benchmark_group("memory_patterns");

    // Compare memory access patterns for different bucket sizes
    let configs = [
        ("4_entries", BucketSize::Small),
        ("8_entries", BucketSize::Medium),
        ("16_entries", BucketSize::Large),
    ];

    for (name, bucket_size) in configs {
        let tt = TranspositionTable::new_with_config(32, Some(bucket_size));

        // Sequential access pattern
        group.bench_function(format!("{name}_sequential"), |b| {
            let mut hash = 0u64;
            b.iter(|| {
                hash += 1;
                black_box(tt.probe(black_box(hash)));
            });
        });

        // Random access pattern
        group.bench_function(format!("{name}_random"), |b| {
            let mut hash = 0x1234567890ABCDEF_u64;
            b.iter(|| {
                // Simple PRNG for consistent random pattern
                hash = hash.wrapping_mul(6364136223846793005).wrapping_add(1);
                black_box(tt.probe(black_box(hash)));
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_bucket_sizes, bench_8_entry_operations, bench_memory_patterns);
criterion_main!(benches);
