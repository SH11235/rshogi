use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};

use engine_core::evaluation::evaluate::MaterialEvaluator;
use engine_core::search::{unified::UnifiedSearcher, SearchLimits};
use engine_core::usi::parse_sfen;
use engine_core::Position;

#[derive(Debug, Clone, Copy)]
struct TestPosition {
    name: &'static str,
    sfen: &'static str,
    depth: u8,
}

const POSITIONS: &[TestPosition] = &[
    TestPosition {
        name: "Initial position",
        sfen: "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        depth: 3,
    },
    TestPosition {
        name: "Mid-game position",
        sfen: "ln1g1g1nl/1r1s1k3/1pp1ppp1p/p2p3p1/9/P1P1P3P/1P1PSP1P1/1BK1G2R1/LN1G3NL b BSP 1",
        depth: 3,
    },
    TestPosition {
        name: "Endgame position",
        sfen: "8l/4g1k2/4ppn2/8p/9/8P/4PP3/4GK3/5G2L b RBSNrbsnl3p 1",
        depth: 3,
    },
    TestPosition {
        name: "King in check",
        sfen: "lnsgkg1nl/6r2/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL w - 1",
        depth: 3,
    },
    TestPosition {
        name: "Many captures available",
        sfen: "ln1gkg1nl/1r1s3b1/pppppp1pp/6p2/9/2P4P1/PP1PPPP1P/1BS5R/LN1GKG1NL b P 1",
        depth: 3,
    },
];

fn bench_regression_nodes(c: &mut Criterion) {
    let mut group = c.benchmark_group("regression_nodes_depth3");
    // Keep benches snappy in CI
    group.sample_size(10);

    for p in POSITIONS {
        let id = BenchmarkId::new(p.name, p.depth);
        group.bench_function(id, |b| {
            b.iter(|| {
                let mut pos: Position = parse_sfen(p.sfen).expect("valid sfen");
                let mut searcher =
                    UnifiedSearcher::<MaterialEvaluator, true, true>::new(MaterialEvaluator);
                let limits = SearchLimits::builder().depth(p.depth).build();
                let result = searcher.search(&mut pos, limits);
                // Return nodes so the optimizer can't eliminate work
                std::hint::black_box(result.stats.nodes)
            })
        });
    }

    group.finish();
}

criterion_group!(benches, bench_regression_nodes);
criterion_main!(benches);
