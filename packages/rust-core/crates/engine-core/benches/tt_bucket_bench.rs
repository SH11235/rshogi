//! Benchmark for TTBucket SIMD optimizations

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::search::tt::NodeType;
use engine_core::search::TranspositionTable;
use rand::Rng;
use std::hint::black_box;

fn setup_filled_tt(size_mb: usize) -> TranspositionTable {
    let tt = TranspositionTable::new(size_mb);
    let mut rng = rand::rng();

    // Fill TT with random entries
    for _ in 0..10000 {
        let hash = rng.random::<u64>();
        let score = rng.random_range(-1000..1000);
        let eval = rng.random_range(-500..500);
        let depth = rng.random_range(1..20);

        tt.store(hash, None, score, eval, depth, NodeType::Exact);
    }

    tt
}

fn bench_tt_probe(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_probe");

    let tt = setup_filled_tt(16);
    let mut rng = rand::rng();

    // Prepare test hashes - mix of hits and misses
    let mut test_hashes = Vec::new();
    for _ in 0..1000 {
        test_hashes.push(rng.random::<u64>());
    }

    group.bench_function("mixed_access", |b| {
        let mut idx = 0;
        b.iter(|| {
            let hash = test_hashes[idx % test_hashes.len()];
            let result = tt.probe(black_box(hash));
            idx += 1;
            black_box(result)
        });
    });

    group.finish();
}

fn bench_tt_store(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_store");

    let tt = setup_filled_tt(16);
    let mut rng = rand::rng();

    // Prepare test data
    let mut test_data = Vec::new();
    for _ in 0..1000 {
        test_data.push((
            rng.random::<u64>(),
            rng.random_range(-1000..1000),
            rng.random_range(-500..500),
            rng.random_range(1..20),
        ));
    }

    group.bench_function("replacement", |b| {
        let mut idx = 0;
        b.iter(|| {
            let (hash, score, eval, depth) = test_data[idx % test_data.len()];
            tt.store(
                black_box(hash),
                None,
                black_box(score),
                black_box(eval),
                black_box(depth),
                NodeType::Exact,
            );
            idx += 1;
        });
    });

    group.finish();
}

fn bench_tt_parallel(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_parallel");

    // Test with different thread counts
    for num_threads in [1, 2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                let tt = std::sync::Arc::new(setup_filled_tt(16));

                b.iter(|| {
                    let mut handles = vec![];

                    for _ in 0..num_threads {
                        let tt_clone = tt.clone();
                        let handle = std::thread::spawn(move || {
                            let mut rng = rand::rng();
                            for _ in 0..100 {
                                let hash = rng.random::<u64>();

                                // Mix of probes and stores
                                if rng.random::<bool>() {
                                    let _ = tt_clone.probe(hash);
                                } else {
                                    tt_clone.store(
                                        hash,
                                        None,
                                        rng.random_range(-1000..1000),
                                        rng.random_range(-500..500),
                                        rng.random_range(1..20),
                                        NodeType::Exact,
                                    );
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

criterion_group!(benches, bench_tt_probe, bench_tt_store, bench_tt_parallel);
criterion_main!(benches);
