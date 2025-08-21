//! Real search benchmarks for Transposition Table
//!
//! Measures TT performance in actual search scenarios:
//! - CAS contention patterns
//! - Store frequency
//! - Hit rates
//! - Multi-threaded performance

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use engine_core::evaluate::MaterialEvaluator;
use engine_core::search::unified::{TTOperations, UnifiedSearcher};
use engine_core::search::{SearchLimitsBuilder, SearchResult};
use engine_core::Position;
use std::hint::black_box;
use std::thread;
use std::time::Duration;

/// Test positions for different game phases
fn get_test_positions() -> Vec<(&'static str, &'static str)> {
    vec![
        ("startpos", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
        (
            "early_middlegame",
            "ln1gkg1nl/1r2s2b1/p1pppp1pp/1p4p2/9/2P4P1/PP1PPPP1P/1B5R1/LNSGKGSNL w - 5",
        ),
        (
            "middlegame",
            "l3k2nl/2r1gs3/p2pppp1p/1ppb3p1/3P5/2P1P1PP1/PP1S1P2P/1BG2S1R1/LN2KG1NL b P 25",
        ),
        (
            "complex_middlegame",
            "ln1g1g1nl/1ks2r3/1pppp1bpp/p5p2/9/P1P1P1P2/1P1PSP1PP/1BG2S1R1/LN2KG1NL w - 15",
        ),
        ("endgame", "8l/6k2/7p1/5Pp1p/1p5P1/2P3P1P/1P7/1K7/9 b GS2Nrb2gs2n3p 50"),
        (
            "tactical",
            "ln1gk2nl/1r4gs1/p1ppppb1p/1p5p1/9/2P6/PP1PPPPPP/1BG4R1/LNS1KGSNL b P 13",
        ),
    ]
}

/// Benchmark single-threaded search with TT
fn bench_search_single_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_real_search_single");
    group.sample_size(20); // Increased sample size
    group.measurement_time(Duration::from_secs(10)); // Increased measurement time

    let positions = get_test_positions();
    let evaluator = MaterialEvaluator;

    for (name, sfen) in positions {
        group.bench_with_input(BenchmarkId::from_parameter(name), &sfen, |b, sfen| {
            b.iter_batched(
                || {
                    // Setup: Create position and searcher with fresh TT
                    let pos = Position::from_sfen(sfen).expect("Valid SFEN");
                    let searcher =
                        UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(evaluator);
                    (searcher, pos)
                },
                |(mut searcher, mut pos)| {
                    // Search to depth 5 for benchmark
                    let limits = SearchLimitsBuilder::default()
                        .depth(5)
                        .nodes(10000) // Add node limit for safety
                        .build();

                    let result = searcher.search(&mut pos, limits);

                    // Get TT stats if available
                    let tt_stats = searcher.get_tt_stats();
                    black_box((result, tt_stats));
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

/// Benchmark multi-threaded search with independent TTs
/// Note: This simulates parallel search where each thread has its own TT
fn bench_search_multi_thread(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_real_search_multi");
    group.sample_size(5);
    group.measurement_time(Duration::from_secs(5));

    let thread_counts = vec![2, 4, 8];
    let evaluator = MaterialEvaluator;

    // Use complex middlegame position for testing
    let test_sfen = "l3k2nl/2r1gs3/p2pppp1p/1ppb3p1/3P5/2P1P1PP1/PP1S1P2P/1BG2S1R1/LN2KG1NL b P 25";

    for num_threads in thread_counts {
        group.bench_with_input(
            BenchmarkId::new("threads", num_threads),
            &num_threads,
            |b, &num_threads| {
                b.iter_batched(
                    || Position::from_sfen(test_sfen).expect("Valid SFEN"),
                    |pos| {
                        let mut handles = vec![];

                        // Spawn search threads
                        for i in 0..num_threads {
                            let mut pos_clone = pos.clone();

                            let handle = thread::spawn(move || {
                                // Create searcher with its own TT
                                let mut searcher =
                                    UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(
                                        evaluator,
                                    );

                                // Add some variation to avoid identical searches
                                let depth = 3 + (i % 2); // Reduced depth for benchmark
                                let limits = SearchLimitsBuilder::default()
                                    .depth(depth)
                                    .nodes(5000) // Add node limit
                                    .build();

                                let result = searcher.search(&mut pos_clone, limits);
                                let tt_stats = searcher.get_tt_stats();
                                (result, tt_stats)
                            });

                            handles.push(handle);
                        }

                        // Collect results
                        type ResultType = (SearchResult, Option<(f32, u64, u64)>);
                        let results: Vec<ResultType> = handles
                            .into_iter()
                            .map(|h| h.join().expect("Thread panicked"))
                            .collect();

                        black_box(results);
                    },
                    criterion::BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

/// Benchmark search with varying TT sizes
fn bench_tt_size_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_size_impact");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(3));

    let evaluator = MaterialEvaluator;

    // Use middlegame position
    let test_sfen = "l3k2nl/2r1gs3/p2pppp1p/1ppb3p1/3P5/2P1P1PP1/PP1S1P2P/1BG2S1R1/LN2KG1NL b P 25";

    // Benchmark each TT size separately due to const generic constraints

    // 1MB TT
    group.bench_with_input(BenchmarkId::new("size_mb", 1), &test_sfen, |b, sfen| {
        b.iter_batched(
            || {
                let pos = Position::from_sfen(sfen).expect("Valid SFEN");
                let searcher = UnifiedSearcher::<MaterialEvaluator, true, true, 1>::new(evaluator);
                (searcher, pos)
            },
            |(mut searcher, mut pos)| {
                let limits = SearchLimitsBuilder::default()
                    .depth(5) // Reduced depth for benchmark
                    .nodes(10000) // Add node limit
                    .build();

                let result = searcher.search(&mut pos, limits);
                let tt_stats = searcher.get_tt_stats();
                black_box((result, tt_stats));
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // 4MB TT
    group.bench_with_input(BenchmarkId::new("size_mb", 4), &test_sfen, |b, sfen| {
        b.iter_batched(
            || {
                let pos = Position::from_sfen(sfen).expect("Valid SFEN");
                let searcher = UnifiedSearcher::<MaterialEvaluator, true, true, 4>::new(evaluator);
                (searcher, pos)
            },
            |(mut searcher, mut pos)| {
                let limits = SearchLimitsBuilder::default()
                    .depth(5) // Reduced depth for benchmark
                    .nodes(10000) // Add node limit
                    .build();

                let result = searcher.search(&mut pos, limits);
                let tt_stats = searcher.get_tt_stats();
                black_box((result, tt_stats));
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // 16MB TT
    group.bench_with_input(BenchmarkId::new("size_mb", 16), &test_sfen, |b, sfen| {
        b.iter_batched(
            || {
                let pos = Position::from_sfen(sfen).expect("Valid SFEN");
                let searcher = UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(evaluator);
                (searcher, pos)
            },
            |(mut searcher, mut pos)| {
                let limits = SearchLimitsBuilder::default()
                    .depth(5) // Reduced depth for benchmark
                    .nodes(10000) // Add node limit
                    .build();

                let result = searcher.search(&mut pos, limits);
                let tt_stats = searcher.get_tt_stats();
                black_box((result, tt_stats));
            },
            criterion::BatchSize::SmallInput,
        );
    });

    // 64MB TT
    group.bench_with_input(BenchmarkId::new("size_mb", 64), &test_sfen, |b, sfen| {
        b.iter_batched(
            || {
                let pos = Position::from_sfen(sfen).expect("Valid SFEN");
                let searcher = UnifiedSearcher::<MaterialEvaluator, true, true, 64>::new(evaluator);
                (searcher, pos)
            },
            |(mut searcher, mut pos)| {
                let limits = SearchLimitsBuilder::default()
                    .depth(5) // Reduced depth for benchmark
                    .nodes(10000) // Add node limit
                    .build();

                let result = searcher.search(&mut pos, limits);
                let tt_stats = searcher.get_tt_stats();
                black_box((result, tt_stats));
            },
            criterion::BatchSize::SmallInput,
        );
    });

    group.finish();
}

/// Benchmark iterative deepening with TT
fn bench_iterative_deepening(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_iterative_deepening");
    group.sample_size(5);
    group.measurement_time(Duration::from_secs(5));

    let evaluator = MaterialEvaluator;
    let positions = vec![
        (
            "tactical",
            "ln1gk2nl/1r4gs1/p1ppppb1p/1p5p1/9/2P6/PP1PPPPPP/1BG4R1/LNS1KGSNL b P 13",
        ),
        ("quiet", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
    ];

    for (name, sfen) in positions {
        group.bench_with_input(BenchmarkId::from_parameter(name), &sfen, |b, sfen| {
            b.iter_batched(
                || {
                    let pos = Position::from_sfen(sfen).expect("Valid SFEN");
                    let searcher =
                        UnifiedSearcher::<MaterialEvaluator, true, true, 32>::new(evaluator);
                    (searcher, pos)
                },
                |(mut searcher, mut pos)| {
                    // Simulate iterative deepening
                    let mut last_result = None;

                    for depth in 1..=6 {
                        // Reduced max depth for benchmark
                        let limits = SearchLimitsBuilder::default()
                            .depth(depth)
                            .nodes(depth as u64 * 2000) // Progressive node limit
                            .build();

                        last_result = Some(searcher.search(&mut pos, limits));
                    }

                    black_box(last_result);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

/// Benchmark PV line search (high TT contention)
fn bench_pv_search_contention(c: &mut Criterion) {
    let mut group = c.benchmark_group("tt_pv_contention");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(3));

    let evaluator = MaterialEvaluator;

    // Positions where PV lines tend to overlap
    let positions = vec![
        (
            "opening_theory",
            "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        ),
        (
            "endgame_tablebase",
            "8l/6k2/7p1/5Pp1p/1p5P1/2P3P1P/1P7/1K7/9 b GS2Nrb2gs2n3p 50",
        ),
    ];

    for (name, sfen) in positions {
        group.bench_with_input(BenchmarkId::from_parameter(name), &sfen, |b, sfen| {
            b.iter_batched(
                || {
                    let pos = Position::from_sfen(sfen).expect("Valid SFEN");
                    let searcher =
                        UnifiedSearcher::<MaterialEvaluator, true, true, 16>::new(evaluator);
                    (searcher, pos)
                },
                |(mut searcher, mut pos)| {
                    // Moderate depth search to stress PV updates
                    let limits = SearchLimitsBuilder::default()
                        .depth(6) // Reduced depth for benchmark
                        .nodes(20000) // Add node limit
                        .build();

                    let result = searcher.search(&mut pos, limits);
                    black_box(result);
                },
                criterion::BatchSize::SmallInput,
            );
        });
    }

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default();
    targets = bench_search_single_thread,
              bench_search_multi_thread,
              bench_tt_size_impact,
              bench_iterative_deepening,
              bench_pv_search_contention
}

criterion_main!(benches);
