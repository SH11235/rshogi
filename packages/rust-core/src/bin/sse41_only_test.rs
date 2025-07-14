//! Test SSE4.1 implementation by manually calling it

use shogi_core::ai::nnue::simd;
use std::time::Instant;

fn main() {
    println!("=== SSE4.1 Only Performance Test ===\n");

    // Check CPU features
    println!("CPU Features:");
    #[cfg(target_arch = "x86_64")]
    {
        println!("  SSE4.1: {}", is_x86_feature_detected!("sse4.1"));
        println!("  AVX2:   {}", is_x86_feature_detected!("avx2"));
    }

    if !is_x86_feature_detected!("sse4.1") {
        println!("\nSSE4.1 not available on this CPU!");
        return;
    }

    // Test affine_transform performance
    println!("\n=== Affine Transform Performance ===");

    let input_dim = 512;
    let output_dim = 32;
    let iterations = 1_000_000;

    let input = vec![10i8; input_dim];
    let weights = vec![1i8; input_dim * output_dim];
    let biases = vec![100i32; output_dim];

    // Warm up
    {
        let mut output = vec![0i32; output_dim];
        for _ in 0..1000 {
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
    }

    // SSE4.1 benchmark
    {
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
        let ops_per_sec = iterations as f64 / elapsed.as_secs_f64();

        println!("SSE4.1 affine_transform:");
        println!("  Time: {:.3} ms", elapsed.as_millis());
        println!("  Operations/sec: {ops_per_sec:.0}");
        println!("  Nanoseconds/op: {:.1}", elapsed.as_nanos() as f64 / iterations as f64);
    }

    // Compare with dispatcher (should use AVX2)
    {
        let mut output = vec![0i32; output_dim];
        let start = Instant::now();

        for _ in 0..iterations {
            simd::SimdDispatcher::affine_transform(
                &input,
                &weights,
                &biases,
                &mut output,
                input_dim,
                output_dim,
            );
        }

        let elapsed = start.elapsed();
        let ops_per_sec = iterations as f64 / elapsed.as_secs_f64();

        println!("\nDispatcher (AVX2 if available):");
        println!("  Time: {:.3} ms", elapsed.as_millis());
        println!("  Operations/sec: {ops_per_sec:.0}");
        println!("  Nanoseconds/op: {:.1}", elapsed.as_nanos() as f64 / iterations as f64);
    }

    // Calculate expected NPS based on affine_transform performance
    println!("\n=== Estimated NPS Impact ===");
    println!("Based on affine_transform being ~40% of NNUE computation:");
    println!("  If AVX2 gives 918K NPS");
    println!("  And SSE4.1 affine_transform is ~51% of AVX2 speed");
    println!("  Expected SSE4.1 NPS: ~550-650K");
}
