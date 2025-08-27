use criterion::{criterion_group, criterion_main, Criterion};
use engine_core::shogi::moves::{Move, MoveList};
use std::hint::black_box;

fn bench_movelist_creation(c: &mut Criterion) {
    c.bench_function("MoveList::new", |b| {
        b.iter(|| {
            let list = MoveList::new();
            black_box(list);
        })
    });

    c.bench_function("MoveList::with_capacity(128)", |b| {
        b.iter(|| {
            let list = MoveList::with_capacity(128);
            black_box(list);
        })
    });
}

fn bench_movelist_push(c: &mut Criterion) {
    c.bench_function("MoveList push 80 moves", |b| {
        b.iter(|| {
            let mut list = MoveList::new();
            for i in 0..80 {
                let mv = Move::normal(
                    engine_core::shogi::Square::new((i % 9) as u8, (i / 9) as u8),
                    engine_core::shogi::Square::new(((i + 1) % 9) as u8, ((i + 1) / 9) as u8),
                    false,
                );
                list.push(mv);
            }
            black_box(list);
        })
    });

    c.bench_function("MoveList push 150 moves (exceeds inline capacity)", |b| {
        b.iter(|| {
            let mut list = MoveList::new();
            for i in 0..150 {
                let mv = Move::normal(
                    engine_core::shogi::Square::new((i % 9) as u8, (i / 9 % 9) as u8),
                    engine_core::shogi::Square::new(((i + 1) % 9) as u8, ((i + 1) / 9 % 9) as u8),
                    false,
                );
                list.push(mv);
            }
            black_box(list);
        })
    });
}

fn bench_movelist_iteration(c: &mut Criterion) {
    let mut list = MoveList::new();
    for i in 0..80 {
        let mv = Move::normal(
            engine_core::shogi::Square::new((i % 9) as u8, (i / 9) as u8),
            engine_core::shogi::Square::new(((i + 1) % 9) as u8, ((i + 1) / 9) as u8),
            false,
        );
        list.push(mv);
    }

    c.bench_function("MoveList iterate 80 moves", |b| {
        b.iter(|| {
            let mut sum = 0u32;
            for &mv in list.iter() {
                sum += mv.to_u32();
            }
            black_box(sum);
        })
    });
}

criterion_group!(benches, bench_movelist_creation, bench_movelist_push, bench_movelist_iteration);
criterion_main!(benches);
