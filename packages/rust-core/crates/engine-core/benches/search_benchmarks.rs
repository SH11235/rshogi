//! Search engine benchmarks
//!
//! Measures performance of different search configurations

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::{
    evaluation::evaluate::MaterialEvaluator, search::unified::UnifiedSearcher,
    search::SearchLimitsBuilder, Position,
};
use std::hint::black_box;
use std::time::Duration;

/// Test positions for benchmarking
struct BenchPosition {
    name: &'static str,
    sfen: &'static str,
    expected_depth: u8,
}

const BENCH_POSITIONS: &[BenchPosition] = &[
    BenchPosition {
        name: "startpos",
        sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        expected_depth: 4, // Reduced from 8 for faster benchmarks
    },
    BenchPosition {
        name: "midgame",
        sfen: "3g1ks2/5g3/2n1pp1p1/p3P1p2/1pP5P/P8/2N2PP2/6K2/L4G1NL b RSBPrslp 45",
        expected_depth: 3, // Reduced from 6
    },
    BenchPosition {
        name: "endgame",
        sfen: "1n5n1/2s3k2/3p1p1p1/2p3p2/9/2P3P2/3P1P1P1/2K6/1N5N1 b RBGSLPrbgs2l13p 80",
        expected_depth: 3, // Reduced depth for reasonable benchmark time
    },
    BenchPosition {
        name: "tactical",
        sfen: "ln1g1g1nl/1ks4r1/1pppp1bpp/p3spp2/9/P1P1P4/1P1PSPPPP/1BK1GS1R1/LN3G1NL b - 17",
        expected_depth: 3, // Reduced from 7
    },
];

/// Benchmark basic searcher
fn bench_basic_searcher(c: &mut Criterion) {
    let mut group = c.benchmark_group("basic_searcher");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(20);

    let evaluator = MaterialEvaluator;

    for bench_pos in BENCH_POSITIONS {
        group.bench_with_input(
            BenchmarkId::new("depth_fixed", bench_pos.name),
            bench_pos,
            |b, pos_info| {
                b.iter(|| {
                    let mut pos = Position::from_sfen(pos_info.sfen).unwrap();
                    let limits =
                        SearchLimitsBuilder::default().depth(pos_info.expected_depth).build();
                    let mut searcher =
                        UnifiedSearcher::<MaterialEvaluator, true, false>::new(evaluator);
                    let result = searcher.search(&mut pos, limits);
                    black_box(result)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("time_fixed", bench_pos.name),
            bench_pos,
            |b, pos_info| {
                b.iter(|| {
                    let mut pos = Position::from_sfen(pos_info.sfen).unwrap();
                    let limits = SearchLimitsBuilder::default().fixed_time_ms(10).build();
                    let mut searcher =
                        UnifiedSearcher::<MaterialEvaluator, true, false>::new(evaluator);
                    let result = searcher.search(&mut pos, limits);
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark enhanced searcher
fn bench_enhanced_searcher(c: &mut Criterion) {
    let mut group = c.benchmark_group("enhanced_searcher");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(20);

    let evaluator = MaterialEvaluator;

    for bench_pos in BENCH_POSITIONS {
        group.bench_with_input(
            BenchmarkId::new("depth_fixed", bench_pos.name),
            bench_pos,
            |b, pos_info| {
                b.iter(|| {
                    let mut pos = Position::from_sfen(pos_info.sfen).unwrap();
                    let mut searcher =
                        UnifiedSearcher::<MaterialEvaluator, true, true>::new(evaluator);
                    let result = searcher.search(
                        &mut pos,
                        SearchLimitsBuilder::default().depth(pos_info.expected_depth).build(),
                    );
                    black_box(result)
                });
            },
        );

        group.bench_with_input(
            BenchmarkId::new("time_fixed", bench_pos.name),
            bench_pos,
            |b, pos_info| {
                b.iter(|| {
                    let mut pos = Position::from_sfen(pos_info.sfen).unwrap();
                    let mut searcher =
                        UnifiedSearcher::<MaterialEvaluator, true, true>::new(evaluator);
                    let result = searcher
                        .search(&mut pos, SearchLimitsBuilder::default().fixed_time_ms(10).build());
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

/// Benchmark node counting accuracy
fn bench_node_counting(c: &mut Criterion) {
    let mut group = c.benchmark_group("node_counting");
    group.measurement_time(Duration::from_secs(5));

    let evaluator = MaterialEvaluator;
    let pos = Position::from_sfen(BENCH_POSITIONS[0].sfen).unwrap();

    group.bench_function("basic_nodes_per_second", |b| {
        b.iter(|| {
            let limits = SearchLimitsBuilder::default().fixed_time_ms(50).build();
            let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, false>::new(evaluator);
            let result = searcher.search(&mut pos.clone(), limits);
            black_box(result.stats.nodes)
        });
    });

    group.bench_function("enhanced_nodes_per_second", |b| {
        b.iter(|| {
            let mut searcher = UnifiedSearcher::<MaterialEvaluator, true, true>::new(evaluator);
            let result = searcher
                .search(&mut pos.clone(), SearchLimitsBuilder::default().fixed_time_ms(50).build());
            black_box(result.stats.nodes)
        });
    });

    group.finish();
}

/// Benchmark transposition table performance
fn bench_tt_performance(c: &mut Criterion) {
    let mut group = c.benchmark_group("transposition_table");

    let sizes = vec![8, 16, 32, 64];

    for size in sizes {
        // Benchmark original TT
        group.bench_with_input(
            BenchmarkId::new("v1_probe_hit_rate", size),
            &size,
            |b, &size_mb| {
                let tt = engine_core::search::tt::TranspositionTable::new(size_mb);
                let pos = Position::startpos();
                let hash = pos.zobrist_hash();

                // Pre-fill TT
                for i in 0..1000 {
                    let test_hash = hash.wrapping_add(i);
                    tt.store(test_hash, None, 100, 0, 5, engine_core::search::NodeType::Exact);
                }

                b.iter(|| {
                    for i in 0..100 {
                        let test_hash = hash.wrapping_add(i * 10);
                        black_box(tt.probe_entry(test_hash));
                    }
                });
            },
        );

        // Benchmark new bucket-based TT
        group.bench_with_input(
            BenchmarkId::new("v2_probe_hit_rate", size),
            &size,
            |b, &size_mb| {
                let tt = engine_core::search::tt::TranspositionTable::new(size_mb);
                let pos = Position::startpos();
                let hash = pos.zobrist_hash();

                // Pre-fill TT
                for i in 0..1000 {
                    let test_hash = hash.wrapping_add(i);
                    tt.store(test_hash, None, 100, 0, 5, engine_core::search::NodeType::Exact);
                }

                b.iter(|| {
                    for i in 0..100 {
                        let test_hash = hash.wrapping_add(i * 10);
                        black_box(tt.probe_entry(test_hash));
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark unified searcher configurations
fn bench_unified_searcher(c: &mut Criterion) {
    let mut group = c.benchmark_group("unified_searcher");
    group.measurement_time(Duration::from_secs(5));
    group.sample_size(20);

    let evaluator = MaterialEvaluator;

    for bench_pos in BENCH_POSITIONS {
        // Test basic configuration (TT only, no pruning)
        group.bench_with_input(
            BenchmarkId::new("basic_config", bench_pos.name),
            bench_pos,
            |b, pos_info| {
                b.iter(|| {
                    let mut pos = Position::from_sfen(pos_info.sfen).unwrap();
                    let mut searcher = UnifiedSearcher::<_, true, false>::new(evaluator);
                    let result = searcher.search(
                        &mut pos,
                        SearchLimitsBuilder::default().depth(pos_info.expected_depth).build(),
                    );
                    black_box(result)
                });
            },
        );

        // Test enhanced configuration (TT + pruning)
        group.bench_with_input(
            BenchmarkId::new("enhanced_config", bench_pos.name),
            bench_pos,
            |b, pos_info| {
                b.iter(|| {
                    let mut pos = Position::from_sfen(pos_info.sfen).unwrap();
                    let mut searcher = UnifiedSearcher::<_, true, true>::new(evaluator);
                    let result = searcher.search(
                        &mut pos,
                        SearchLimitsBuilder::default().depth(pos_info.expected_depth).build(),
                    );
                    black_box(result)
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_basic_searcher,
    bench_enhanced_searcher,
    bench_unified_searcher,
    bench_node_counting,
    bench_tt_performance
);
criterion_main!(benches);
