//! Benchmark for SIMD-optimized TT operations

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::search::tt_simd::{scalar, simd};
use std::hint::black_box;

fn bench_key_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_key_search");

    // Prepare test data
    let keys = [
        0x1234567890ABCDEF,
        0xFEDCBA0987654321,
        0x1111111111111111,
        0x2222222222222222,
    ];

    let targets = vec![
        (keys[0], "hit_pos0"),        // Hit at position 0
        (keys[2], "hit_pos2"),        // Hit at position 2
        (keys[3], "hit_pos3"),        // Hit at position 3
        (0x3333333333333333, "miss"), // Miss
    ];

    for (target, name) in &targets {
        group.bench_with_input(BenchmarkId::new("scalar", name), target, |b, &target| {
            b.iter(|| scalar::find_matching_key(black_box(&keys), black_box(target)));
        });

        group.bench_with_input(BenchmarkId::new("simd", name), target, |b, &target| {
            b.iter(|| simd::find_matching_key(black_box(&keys), black_box(target)));
        });
    }

    group.finish();
}

fn bench_priority_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_priority_scores");

    // Prepare test data
    let test_cases = vec![
        (
            "typical",
            [10u8, 15, 20, 12],
            [0u8, 1, 2, 3],
            [false, true, false, true],
            [true, false, true, false],
        ),
        (
            "deep",
            [50u8, 60, 70, 80],
            [0u8, 0, 0, 0],
            [true, true, true, true],
            [true, true, true, true],
        ),
        (
            "shallow",
            [1u8, 2, 3, 4],
            [7u8, 7, 7, 7],
            [false, false, false, false],
            [false, false, false, false],
        ),
    ];

    let current_age = 4u8;

    for (name, depths, ages, is_pv, is_exact) in &test_cases {
        group.bench_with_input(
            BenchmarkId::new("scalar", name),
            &(depths, ages, is_pv, is_exact),
            |b, &(depths, ages, is_pv, is_exact)| {
                b.iter(|| {
                    scalar::calculate_priority_scores(
                        black_box(depths),
                        black_box(ages),
                        black_box(is_pv),
                        black_box(is_exact),
                        black_box(current_age),
                    )
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("simd", name),
            &(depths, ages, is_pv, is_exact),
            |b, &(depths, ages, is_pv, is_exact)| {
                b.iter(|| {
                    simd::calculate_priority_scores(
                        black_box(depths),
                        black_box(ages),
                        black_box(is_pv),
                        black_box(is_exact),
                        black_box(current_age),
                    )
                });
            },
        );
    }

    group.finish();
}

fn bench_bulk_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_bulk_operations");

    // Simulate processing many buckets
    const NUM_BUCKETS: usize = 1000;

    // Generate random test data
    let mut all_keys = Vec::with_capacity(NUM_BUCKETS);
    let mut all_targets = Vec::with_capacity(NUM_BUCKETS);

    use rand::Rng;
    let mut rng = rand::thread_rng();

    for _ in 0..NUM_BUCKETS {
        let keys = [
            rng.gen::<u64>(),
            rng.gen::<u64>(),
            rng.gen::<u64>(),
            rng.gen::<u64>(),
        ];
        all_keys.push(keys);

        // 75% hit rate
        let target = if rng.gen_bool(0.75) {
            keys[rng.gen_range(0..4)]
        } else {
            rng.gen::<u64>()
        };
        all_targets.push(target);
    }

    group.bench_function("scalar_bulk_search", |b| {
        b.iter(|| {
            for i in 0..NUM_BUCKETS {
                scalar::find_matching_key(black_box(&all_keys[i]), black_box(all_targets[i]));
            }
        });
    });

    group.bench_function("simd_bulk_search", |b| {
        b.iter(|| {
            for i in 0..NUM_BUCKETS {
                simd::find_matching_key(black_box(&all_keys[i]), black_box(all_targets[i]));
            }
        });
    });

    group.finish();
}

criterion_group!(benches, bench_key_search, bench_priority_calculation, bench_bulk_operations);
criterion_main!(benches);
