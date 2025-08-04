use criterion::{criterion_group, criterion_main, Criterion};
use engine_core::search::tt_simd::{scalar, simd};
use std::hint::black_box;

fn bench_8_entry_search(c: &mut Criterion) {
    let mut group = c.benchmark_group("8_entry_search");

    // Test data: 8 unique keys
    let keys: [u64; 8] = [
        0x1111111111111111,
        0x2222222222222222,
        0x3333333333333333,
        0x4444444444444444,
        0x5555555555555555,
        0x6666666666666666,
        0x7777777777777777,
        0x8888888888888888,
    ];

    // Test hit at different positions
    let test_positions = [
        ("hit_first", keys[0]),
        ("hit_middle", keys[4]),
        ("hit_last", keys[7]),
        ("miss", 0x9999999999999999),
    ];

    for (name, target) in test_positions {
        // Benchmark scalar implementation
        group.bench_function(format!("scalar/{name}"), |b| {
            b.iter(|| black_box(scalar::find_matching_key_8(&keys, black_box(target))));
        });

        // Benchmark SIMD implementation
        group.bench_function(format!("simd/{name}"), |b| {
            b.iter(|| black_box(simd::find_matching_key_8(&keys, black_box(target))));
        });
    }

    group.finish();
}

fn bench_8_entry_priority(c: &mut Criterion) {
    let mut group = c.benchmark_group("8_entry_priority");

    let depths: [u8; 8] = [10, 20, 15, 5, 25, 30, 8, 12];
    let ages: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
    let is_pv: [bool; 8] = [true, false, false, true, false, true, false, true];
    let is_exact: [bool; 8] = [false, true, false, false, true, false, true, false];
    let current_age = 2;

    // Benchmark scalar priority calculation
    group.bench_function("scalar", |b| {
        b.iter(|| {
            black_box(scalar::calculate_priority_scores_8(
                &depths,
                &ages,
                &is_pv,
                &is_exact,
                black_box(current_age),
            ))
        });
    });

    // Benchmark SIMD priority calculation
    group.bench_function("simd", |b| {
        b.iter(|| {
            black_box(simd::calculate_priority_scores_8(
                &depths,
                &ages,
                &is_pv,
                &is_exact,
                black_box(current_age),
            ))
        });
    });

    group.finish();
}

fn bench_bulk_8_entry(c: &mut Criterion) {
    let mut group = c.benchmark_group("8_entry_bulk");

    // Create many test keys for bulk operations
    let test_keys: Vec<[u64; 8]> = (0..100)
        .map(|i| {
            [
                (i as u64) * 0x1111111111111111,
                (i as u64) * 0x2222222222222222,
                (i as u64) * 0x3333333333333333,
                (i as u64) * 0x4444444444444444,
                (i as u64) * 0x5555555555555555,
                (i as u64) * 0x6666666666666666,
                (i as u64) * 0x7777777777777777,
                (i as u64) * 0x8888888888888888,
            ]
        })
        .collect();

    // Benchmark bulk scalar searches
    group.bench_function("scalar_bulk", |b| {
        b.iter(|| {
            let mut count = 0;
            for keys in &test_keys {
                // Search for a value that's likely a miss
                if scalar::find_matching_key_8(keys, 0x9999999999999999).is_some() {
                    count += 1;
                }
            }
            black_box(count)
        });
    });

    // Benchmark bulk SIMD searches
    group.bench_function("simd_bulk", |b| {
        b.iter(|| {
            let mut count = 0;
            for keys in &test_keys {
                // Search for a value that's likely a miss
                if simd::find_matching_key_8(keys, 0x9999999999999999).is_some() {
                    count += 1;
                }
            }
            black_box(count)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_8_entry_search, bench_8_entry_priority, bench_bulk_8_entry);
criterion_main!(benches);
