//! Benchmark to measure the impact of TT prefetching
//!
//! This benchmark compares search performance with and without TT prefetching

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::{
    evaluation::evaluate::MaterialEvaluator,
    search::{unified::UnifiedSearcher, SearchLimitsBuilder},
    Position,
};
use std::hint::black_box;
use std::time::Duration;

/// Test positions for benchmarking TT access patterns
const BENCH_POSITIONS: &[(&str, &str)] = &[
    ("startpos", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
    ("midgame", "3g1ks2/5g3/2n1pp1p1/p3P1p2/1pP5P/P8/2N2PP2/6K2/L4G1NL b RSBPrslp 45"),
    (
        "tactical",
        "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17",
    ),
];

fn bench_tt_access_pattern(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_access_pattern");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(20);

    for (name, sfen) in BENCH_POSITIONS {
        group.bench_with_input(BenchmarkId::new("search_depth_5", name), sfen, |b, &sfen| {
            b.iter(|| {
                let mut pos = Position::from_sfen(sfen).unwrap();
                let mut searcher =
                    UnifiedSearcher::<MaterialEvaluator, true, true, 32>::new(MaterialEvaluator);

                // Search to depth 5 which should generate significant TT traffic
                let limits = SearchLimitsBuilder::default().depth(5).build();
                let result = searcher.search(&mut pos, limits);

                black_box(result);
            });
        });

        group.bench_with_input(BenchmarkId::new("search_fixed_nodes", name), sfen, |b, &sfen| {
            b.iter(|| {
                let mut pos = Position::from_sfen(sfen).unwrap();
                let mut searcher =
                    UnifiedSearcher::<MaterialEvaluator, true, true, 32>::new(MaterialEvaluator);

                // Fixed nodes to get consistent measurements
                let limits = SearchLimitsBuilder::default().fixed_nodes(50000).build();
                let result = searcher.search(&mut pos, limits);

                black_box(result);
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_tt_access_pattern);
criterion_main!(benches);
