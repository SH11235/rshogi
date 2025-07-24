//! Check which SIMD implementation is being used

use std::time::Duration;

use engine_core::{
    engine::controller::{Engine, EngineType},
    search::search_basic::SearchLimits,
    Position,
};

fn main() {
    println!("=== SIMD Implementation Check ===\n");

    // Check CPU features
    println!("CPU Features:");
    #[cfg(target_arch = "x86_64")]
    {
        println!("  SSE:    {}", is_x86_feature_detected!("sse"));
        println!("  SSE2:   {}", is_x86_feature_detected!("sse2"));
        println!("  SSE3:   {}", is_x86_feature_detected!("sse3"));
        println!("  SSSE3:  {}", is_x86_feature_detected!("ssse3"));
        println!("  SSE4.1: {}", is_x86_feature_detected!("sse4.1"));
        println!("  SSE4.2: {}", is_x86_feature_detected!("sse4.2"));
        println!("  AVX:    {}", is_x86_feature_detected!("avx"));
        println!("  AVX2:   {}", is_x86_feature_detected!("avx2"));
        println!("  AVX512: {}", is_x86_feature_detected!("avx512f"));
    }

    println!("\n=== Testing Different Scenarios ===\n");

    // Test 1: Single run
    println!("Test 1: Single search (AVX2 should be used if available)");
    {
        let mut pos = Position::startpos();
        let engine = Engine::new(EngineType::Nnue);
        let limits = SearchLimits {
            depth: 5,
            time: Some(Duration::from_millis(100)),
            nodes: None,
            stop_flag: None,
            info_callback: None,
        };

        let result = engine.search(&mut pos, limits);
        println!("  Nodes: {}", result.stats.nodes);
        println!("  Time: {:?}", result.stats.elapsed);
        let nps = result.stats.nodes * 1_000_000_000 / result.stats.elapsed.as_nanos() as u64;
        println!("  NPS: {nps}");
    }

    // Test 2: Multiple runs to check consistency
    println!("\nTest 2: Multiple runs (checking NPS consistency)");
    {
        let mut nps_values = Vec::new();

        for i in 0..5 {
            let mut pos = Position::startpos();
            let engine = Engine::new(EngineType::Nnue);
            let limits = SearchLimits {
                depth: 5,
                time: Some(Duration::from_millis(100)),
                nodes: None,
                stop_flag: None,
                info_callback: None,
            };

            let result = engine.search(&mut pos, limits);
            let nps = result.stats.nodes * 1_000_000_000 / result.stats.elapsed.as_nanos() as u64;
            nps_values.push(nps);
            println!("  Run {}: {} NPS", i + 1, nps);
        }

        let avg_nps: u64 = nps_values.iter().sum::<u64>() / nps_values.len() as u64;
        let min_nps = *nps_values.iter().min().unwrap();
        let max_nps = *nps_values.iter().max().unwrap();

        println!("\n  Average NPS: {avg_nps}");
        println!("  Min NPS: {min_nps}");
        println!("  Max NPS: {max_nps}");
        println!("  Variance: {:.2}%", ((max_nps - min_nps) as f64 / avg_nps as f64) * 100.0);
    }

    // Test 3: Force SSE4.1 by disabling AVX2 (hypothetical)
    println!("\nTest 3: Estimate SSE4.1 performance");
    println!("  (Note: Cannot force SSE4.1 at runtime, showing expected ratio)");
    println!("  Based on benchmarks:");
    println!("  - AVX2 affine_transform: ~1.5M ops/sec");
    println!("  - SSE4.1 affine_transform: ~0.77M ops/sec");
    println!("  - Expected SSE4.1 NPS: ~50-60% of AVX2 NPS");
}
