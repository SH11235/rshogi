use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use engine_core::{
    search::{
        tt::{BucketSize, TTStoreArgs, TranspositionTable},
        NodeType,
    },
    Color,
};
use std::hint::black_box;
use std::{env, time::Duration};

// Generate a set of hashes that all map to the same bucket by fixing the lower bits
fn clustered_hashes(n: usize, low_base: u64) -> Vec<u64> {
    // Keep lower 48 bits identical so that (hash & (num_buckets-1)) stays constant
    const LOW_MASK: u64 = (1u64 << 48) - 1;
    let low = low_base & LOW_MASK;
    (0..n).map(|i| low | ((i as u64) << 48)).collect()
}

fn bench_config() -> Criterion {
    let mut c = Criterion::default().configure_from_args();
    if let Ok(v) = env::var("BENCH_SAMPLE_SIZE") {
        if let Ok(n) = v.parse::<usize>() {
            c = c.sample_size(n);
        }
    }
    if let Ok(v) = env::var("BENCH_WARMUP_MS") {
        if let Ok(ms) = v.parse::<u64>() {
            c = c.warm_up_time(Duration::from_millis(ms));
        }
    }
    if let Ok(v) = env::var("BENCH_MEASUREMENT_MS") {
        if let Ok(ms) = v.parse::<u64>() {
            c = c.measurement_time(Duration::from_millis(ms));
        }
    }
    c
}

fn bench_fixed_bucket_collision(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_collision_fixed");

    // Configurable table size (MB) and key counts
    let table_mb = env::var("BENCH_TABLE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4);
    let keys_len = env::var("BENCH_COLLISION_KEYS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1 << 14);
    let prefill = env::var("BENCH_PREFILL")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(128);

    // Use small-ish table to make replacement frequent
    let tt = TranspositionTable::new(table_mb);

    // Prepare clustered keys
    let keys = clustered_hashes(keys_len, 0x1234_5678_9ABC);

    // Pre-fill a bit to warm up the bucket
    for &k in &keys[0..prefill.min(keys.len())] {
        tt.store(TTStoreArgs::new(k, None, 100i16, 0i16, 12u8, NodeType::Exact, Color::Black));
    }

    group.throughput(Throughput::Elements(1));
    group.bench_function("store_clustered_fixed", |b| {
        let mut idx = 0usize;
        b.iter(|| {
            let k = keys[idx & (keys.len() - 1)];
            tt.store(black_box(TTStoreArgs::new(
                black_box(k),
                None,
                black_box(77i16),
                black_box(0i16),
                black_box(10u8),
                NodeType::Exact,
                Color::Black,
            )));
            idx = idx.wrapping_add(1);
        });
    });

    group.finish();
}

fn bench_flexible_bucket_collision(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_collision_flexible");

    let variants = [
        ("flex_4", BucketSize::Small),
        ("flex_8", BucketSize::Medium),
        ("flex_16", BucketSize::Large),
    ];

    let table_mb = env::var("BENCH_TABLE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(4);
    let keys_per_entry = env::var("BENCH_KEYS_PER_ENTRY")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(1024);
    let prefill_mult = env::var("BENCH_PREFILL_MULT")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8);

    for (name, bsize) in variants {
        let tt = TranspositionTable::new_with_config(table_mb, Some(bsize));

        // keys sized to create several times the bucket capacity worth of unique entries
        let total_keys = bsize.entries() * keys_per_entry;
        let keys = clustered_hashes(total_keys, 0xDEAD_BEEF_CAFE);

        // Pre-fill one bucket well beyond its capacity to ensure worst-entry replacement path
        let prefill = (bsize.entries() * prefill_mult).min(keys.len());
        for &k in &keys[0..prefill] {
            tt.store(TTStoreArgs::new(k, None, 90i16, 0i16, 8u8, NodeType::Exact, Color::Black));
        }

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("store_clustered", name), &tt, |b, tt| {
            let mut idx = 0usize;
            b.iter(|| {
                let k = keys[idx & (keys.len() - 1)];
                tt.store(black_box(TTStoreArgs::new(
                    black_box(k),
                    None,
                    black_box(65i16),
                    black_box(0i16),
                    black_box(7u8),
                    NodeType::Exact,
                    Color::Black,
                )));
                idx = idx.wrapping_add(1);
            });
        });

        group.throughput(Throughput::Elements(1));
        // Also probe the same clustered keys (mix of hits/misses depending on replacement)
        group.bench_with_input(BenchmarkId::new("probe_clustered", name), &tt, |b, tt| {
            let mut idx = 0usize;
            b.iter(|| {
                let k = keys[idx & (keys.len() - 1)];
                black_box(tt.probe_entry(black_box(k), Color::Black));
                idx = idx.wrapping_add(1);
            });
        });
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = bench_config();
    targets = bench_fixed_bucket_collision, bench_flexible_bucket_collision
}
criterion_main!(benches);
