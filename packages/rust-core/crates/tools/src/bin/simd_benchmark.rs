//! SIMD implementation benchmark

use engine_core::ai::nnue::simd;
use std::time::Instant;

fn benchmark_affine_transform() {
    println!("\n=== Affine Transform Benchmark ===");

    let input_dim = 512;
    let output_dim = 32;
    let iterations = 100_000;

    let input = vec![10i8; input_dim];
    let weights = vec![1i8; input_dim * output_dim];
    let biases = vec![100i32; output_dim];

    // Scalar benchmark
    {
        let mut output = vec![0i32; output_dim];
        let start = Instant::now();

        for _ in 0..iterations {
            simd::scalar::affine_transform_scalar(
                &input,
                &weights,
                &biases,
                &mut output,
                input_dim,
                output_dim,
            );
        }

        let elapsed = start.elapsed();
        println!(
            "Scalar: {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }

    // SSE4.1 benchmark
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("sse4.1") {
        let mut output = vec![0i32; output_dim];
        let start = Instant::now();

        for _ in 0..iterations {
            unsafe {
                simd::x86_64::affine_transform_sse41(
                    &input,
                    &weights,
                    &biases,
                    &mut output,
                    input_dim,
                    output_dim,
                );
            }
        }

        let elapsed = start.elapsed();
        println!(
            "SSE4.1: {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }

    // AVX2 benchmark
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") {
        let mut output = vec![0i32; output_dim];
        let start = Instant::now();

        for _ in 0..iterations {
            unsafe {
                simd::x86_64::affine_transform_avx2(
                    &input,
                    &weights,
                    &biases,
                    &mut output,
                    input_dim,
                    output_dim,
                );
            }
        }

        let elapsed = start.elapsed();
        println!(
            "AVX2:   {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }
}

fn benchmark_clipped_relu() {
    println!("\n=== ClippedReLU Benchmark ===");

    let size = 256;
    let iterations = 1_000_000;

    let input = vec![50i32; size];

    // Scalar benchmark
    {
        let mut output = vec![0i8; size];
        let start = Instant::now();

        for _ in 0..iterations {
            simd::scalar::clipped_relu_scalar(&input, &mut output, size);
        }

        let elapsed = start.elapsed();
        println!(
            "Scalar: {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }

    // SSE4.1 benchmark
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("sse4.1") {
        let mut output = vec![0i8; size];
        let start = Instant::now();

        for _ in 0..iterations {
            unsafe {
                simd::x86_64::clipped_relu_sse41(&input, &mut output, size);
            }
        }

        let elapsed = start.elapsed();
        println!(
            "SSE4.1: {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }

    // AVX2 benchmark
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") {
        let mut output = vec![0i8; size];
        let start = Instant::now();

        for _ in 0..iterations {
            unsafe {
                simd::x86_64::clipped_relu_avx2(&input, &mut output, size);
            }
        }

        let elapsed = start.elapsed();
        println!(
            "AVX2:   {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }
}

fn benchmark_transform_features() {
    println!("\n=== Transform Features Benchmark ===");

    let size = 256;
    let iterations = 1_000_000;

    let us = vec![1000i16; size];
    let them = vec![2000i16; size];

    // Scalar benchmark
    {
        let mut output = vec![0i8; size * 2];
        let start = Instant::now();

        for _ in 0..iterations {
            simd::scalar::transform_features_scalar(&us, &them, &mut output, size);
        }

        let elapsed = start.elapsed();
        println!(
            "Scalar: {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }

    // SSE4.1 benchmark
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("sse4.1") {
        let mut output = vec![0i8; size * 2];
        let start = Instant::now();

        for _ in 0..iterations {
            unsafe {
                simd::x86_64::transform_features_sse41(&us, &them, &mut output, size);
            }
        }

        let elapsed = start.elapsed();
        println!(
            "SSE4.1: {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }

    // AVX2 benchmark
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") {
        let mut output = vec![0i8; size * 2];
        let start = Instant::now();

        for _ in 0..iterations {
            unsafe {
                simd::x86_64::transform_features_avx2(&us, &them, &mut output, size);
            }
        }

        let elapsed = start.elapsed();
        println!(
            "AVX2:   {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }
}

fn benchmark_update_accumulator() {
    println!("\n=== Update Accumulator Benchmark ===");

    let num_features = 128;
    let num_indices = 4;
    let iterations = 100_000;

    let weights = vec![10i16; 256 * num_features];
    let indices: Vec<usize> = (0..num_indices).collect();

    // Scalar benchmark
    {
        let mut accumulator = vec![0i16; 256];
        let start = Instant::now();

        for _ in 0..iterations {
            simd::scalar::update_accumulator_scalar(&mut accumulator, &weights, &indices, true);
        }

        let elapsed = start.elapsed();
        println!(
            "Scalar: {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }

    // SSE4.1 benchmark
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("sse4.1") {
        let mut accumulator = vec![0i16; 256];
        let start = Instant::now();

        for _ in 0..iterations {
            unsafe {
                simd::x86_64::update_accumulator_sse41(&mut accumulator, &weights, &indices, true);
            }
        }

        let elapsed = start.elapsed();
        println!(
            "SSE4.1: {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }

    // AVX2 benchmark
    #[cfg(target_arch = "x86_64")]
    if is_x86_feature_detected!("avx2") {
        let mut accumulator = vec![0i16; 256];
        let start = Instant::now();

        for _ in 0..iterations {
            unsafe {
                simd::x86_64::update_accumulator_avx2(&mut accumulator, &weights, &indices, true);
            }
        }

        let elapsed = start.elapsed();
        println!(
            "AVX2:   {:.3} ms ({:.0} ops/sec)",
            elapsed.as_millis(),
            iterations as f64 / elapsed.as_secs_f64()
        );
    }
}

fn main() {
    println!("=== SIMD Implementation Benchmark ===");

    // Print CPU features
    println!("\nCPU Features:");
    #[cfg(target_arch = "x86_64")]
    {
        println!("  SSE4.1: {}", is_x86_feature_detected!("sse4.1"));
        println!("  AVX2:   {}", is_x86_feature_detected!("avx2"));
    }

    benchmark_affine_transform();
    benchmark_clipped_relu();
    benchmark_transform_features();
    benchmark_update_accumulator();

    println!("\nBenchmark complete!");
}
