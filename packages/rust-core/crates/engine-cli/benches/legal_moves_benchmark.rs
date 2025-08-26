use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_core::{movegen::MoveGen, shogi::MoveList, Position};

fn bench_direct_movegen(c: &mut Criterion) {
    // Initialize tables once before benchmarking
    engine_core::init_engine_tables();

    c.bench_function("movegen_startpos", |b| {
        b.iter(|| {
            let pos = Position::startpos();
            let mut movegen = MoveGen::new();
            let mut moves = MoveList::new();
            movegen.generate_all(black_box(&pos), &mut moves);
            moves.len()
        });
    });
}

fn bench_movegen_various_positions(c: &mut Criterion) {
    // Initialize tables once before benchmarking
    engine_core::init_engine_tables();

    // Test various positions
    let positions = vec![
        ("startpos", "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1"),
        (
            "midgame",
            "ln1g1g1nl/1ks2r3/1pppp1bpp/p3spp2/9/P1P1PPP1P/1PSPS2P1/1BK1GR3/LN3G1NL b Pp 1",
        ),
        ("endgame", "9/4k4/9/9/9/9/9/4K4/9 b 2r2b4g4s4n4l18p 1"),
    ];

    for (name, sfen) in positions {
        let pos = Position::from_sfen(sfen).expect("Valid SFEN");

        c.bench_function(&format!("movegen_{}", name), |b| {
            b.iter(|| {
                let mut movegen = MoveGen::new();
                let mut moves = MoveList::new();
                movegen.generate_all(black_box(&pos), &mut moves);
                moves.len()
            });
        });
    }
}

fn bench_has_any_legal_move(c: &mut Criterion) {
    // Initialize tables once before benchmarking
    engine_core::init_engine_tables();

    c.bench_function("has_any_legal_move_startpos", |b| {
        b.iter(|| {
            let pos = Position::startpos();
            let mut movegen = MoveGen::new();
            movegen.has_any_legal_move(black_box(&pos))
        });
    });
}

criterion_group!(
    benches,
    bench_direct_movegen,
    bench_movegen_various_positions,
    bench_has_any_legal_move
);
criterion_main!(benches);
