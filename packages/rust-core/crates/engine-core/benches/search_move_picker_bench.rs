use criterion::{criterion_group, criterion_main, Criterion};
use engine_core::search::ab::{Heuristics, MovePicker};
use engine_core::usi::parse_sfen;
use engine_core::Position;
use std::time::Duration;

fn sample_positions() -> Vec<Position> {
    const MIDGAME_SFENS: &[&str] = &[
        "+Bn1g2s1l/2skg2r1/ppppp1n1p/5bpp1/5p1P1/2P6/PP1PP1P1P/1SK2S1R1/LN1G1G1NL w Lp 24",
        "1n1gk2nl/1r3sg2/2pppp1p1/sp4p1p/9/2P3P1P/1PSPPPSP1/7R1/1N1GKG1NL w BLPblp 24",
        "ln2k2nl/1r3sg2/1pppppppp/p1s6/6P2/2P6/PP1PPPPPP/1R5S1/LNB1K2NL b GPg 48",
    ];

    let mut positions = Vec::with_capacity(1 + MIDGAME_SFENS.len());
    positions.push(Position::startpos());
    for sfen in MIDGAME_SFENS {
        let pos = parse_sfen(sfen).expect("valid SFEN");
        positions.push(pos);
    }
    positions
}

fn bench_move_picker(c: &mut Criterion) {
    let positions = sample_positions();
    let heur = Heuristics::default();

    let mut group = c.benchmark_group("move_picker_iteration");
    group.sample_size(60);
    group.measurement_time(Duration::from_secs(2));

    group.bench_function("normal", |b| {
        b.iter(|| {
            let mut total = 0;
            for pos in &positions {
                let mut picker = MovePicker::new_normal(pos, None, None, [None; 2], None, None);
                while let Some(mv) = picker.next(&heur) {
                    std::hint::black_box(mv);
                    total += 1;
                }
            }
            std::hint::black_box(total);
        });
    });

    group.bench_function("qsearch", |b| {
        b.iter(|| {
            let mut total = 0;
            for pos in &positions {
                let mut picker = MovePicker::new_qsearch(pos, None, None, None, 12);
                while let Some(mv) = picker.next(&heur) {
                    std::hint::black_box(mv);
                    total += 1;
                }
            }
            std::hint::black_box(total);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_move_picker);
criterion_main!(benches);
