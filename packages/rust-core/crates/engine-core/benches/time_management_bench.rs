use criterion::{black_box, criterion_group, criterion_main, Criterion};
use engine_core::time_management::{GamePhase, TimeControl, TimeLimits, TimeManager};
use engine_core::Color;
use std::sync::Arc;
use std::thread;

fn bench_elapsed_ms(c: &mut Criterion) {
    let limits = TimeLimits {
        time_control: TimeControl::Fischer {
            white_ms: 60000,
            black_ms: 60000,
            increment_ms: 1000,
        },
        ..Default::default()
    };

    let tm = Arc::new(TimeManager::new(&limits, Color::White, 0, GamePhase::Opening));

    c.bench_function("elapsed_ms_single_thread", |b| {
        b.iter(|| {
            black_box(tm.elapsed_ms());
        })
    });

    c.bench_function("elapsed_ms_multi_thread", |b| {
        b.iter(|| {
            let mut handles = vec![];

            for _ in 0..4 {
                let tm_clone = Arc::clone(&tm);
                let handle = thread::spawn(move || {
                    for _ in 0..1000 {
                        black_box(tm_clone.elapsed_ms());
                    }
                });
                handles.push(handle);
            }

            for handle in handles {
                handle.join().unwrap();
            }
        })
    });
}

criterion_group!(benches, bench_elapsed_ms);
criterion_main!(benches);
