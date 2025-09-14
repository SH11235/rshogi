use criterion::{criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use std::hint::black_box;

fn init_data(len: usize) -> (Vec<f32>, Vec<f32>) {
    let mut dst = vec![0.0f32; len];
    let mut row = vec![0.0f32; len];
    for i in 0..len {
        dst[i] = (i as f32 * 0.001).sin();
        row[i] = ((i as f32 + 3.0) * 0.002).cos();
    }
    (dst, row)
}

#[inline]
fn scalar_add(dst: &mut [f32], row: &[f32], k: f32) {
    for (d, r) in dst.iter_mut().zip(row.iter()) {
        *d += k * *r;
    }
}

pub fn bench_add_row_scaled(c: &mut Criterion) {
    let mut g = c.benchmark_group("nnue_add_row_f32");
    let sizes = [255usize, 256, 257, 2048];
    let ks = [1.0f32, -1.0, 0.75];

    for &len in &sizes {
        for &k in &ks {
            // dispatcher
            g.bench_with_input(
                BenchmarkId::new("dispatcher", format!("len={len},k={k}")),
                &len,
                |b, &len| {
                    let (dst0, row) = init_data(len);
                    b.iter_batched(
                        || (dst0.clone(), row.clone()),
                        |(mut dst, row)| {
                            engine_core::simd::add_row_scaled_f32(&mut dst, &row, k);
                            black_box(dst)
                        },
                        BatchSize::SmallInput,
                    );
                },
            );

            // scalar baseline
            g.bench_with_input(
                BenchmarkId::new("scalar", format!("len={len},k={k}")),
                &len,
                |b, &len| {
                    let (dst0, row) = init_data(len);
                    b.iter_batched(
                        || (dst0.clone(), row.clone()),
                        |(mut dst, row)| {
                            scalar_add(&mut dst, &row, k);
                            black_box(dst)
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }

    // hot-loop 版（コピーを避け、加算→減算で原状復帰）
    for &len in &sizes {
        let (mut dst_dispatch, row) = init_data(len);
        let (mut dst_scalar, _row2) = init_data(len);

        g.bench_with_input(
            BenchmarkId::new("dispatcher_hotloop", format!("len={len},k=0.75")),
            &len,
            |b, &_len| {
                b.iter(|| {
                    engine_core::simd::add_row_scaled_f32(
                        black_box(&mut dst_dispatch),
                        black_box(&row),
                        black_box(0.75),
                    );
                    engine_core::simd::add_row_scaled_f32(
                        black_box(&mut dst_dispatch),
                        black_box(&row),
                        black_box(-0.75),
                    );
                });
            },
        );

        g.bench_with_input(
            BenchmarkId::new("scalar_hotloop", format!("len={len},k=0.75")),
            &len,
            |b, &_len| {
                b.iter(|| {
                    scalar_add(black_box(&mut dst_scalar), black_box(&row), black_box(0.75));
                    scalar_add(black_box(&mut dst_scalar), black_box(&row), black_box(-0.75));
                });
            },
        );
    }

    g.finish();
}

criterion_group!(benches, bench_add_row_scaled);
criterion_main!(benches);
