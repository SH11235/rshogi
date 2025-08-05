//! Benchmark tool for Transposition Table hashfull control implementation

use engine_core::search::tt::{NodeType, TTEntryParams, TranspositionTable};
use engine_core::shogi::Position;
use rand::{RngCore, SeedableRng};
use std::time::Instant;

fn main() {
    println!("=== Transposition Table Hashfull Control Benchmark ===\n");

    // Test different table sizes
    let table_sizes = vec![1, 8, 16, 32]; // MB

    for size_mb in table_sizes {
        println!("Testing with {size_mb}MB table:");
        run_benchmark(size_mb);
        println!();
    }
}

fn run_benchmark(size_mb: usize) {
    let tt = TranspositionTable::new(size_mb);
    let mut rng = rand_xoshiro::Xoshiro256PlusPlus::seed_from_u64(12345);

    // Generate test positions
    let mut positions = Vec::new();
    let base_position = Position::startpos();
    for i in 0..100000 {
        positions.push(base_position.hash + i as u64);
    }

    // Measure time for different fill levels
    let mut total_stores = 0;
    let _filtered_stores = 0;
    let start_time = Instant::now();

    for (i, &hash) in positions.iter().enumerate() {
        // Random depth and node type
        let depth: u8 = (rng.next_u32() % 19 + 1) as u8;
        let node_type = match rng.next_u32() % 3 {
            0 => NodeType::Exact,
            1 => NodeType::LowerBound,
            _ => NodeType::UpperBound,
        };

        // Store entry
        let params = TTEntryParams {
            key: hash,
            mv: None,
            score: (rng.next_u32() % 2000) as i16 - 1000,
            eval: (rng.next_u32() % 2000) as i16 - 1000,
            depth,
            node_type,
            age: 0,
            is_pv: rng.next_u32() % 10 == 0,
            ..Default::default()
        };

        tt.store_with_params(params);
        total_stores += 1;

        // Check hashfull periodically
        if i % 10000 == 0 && i > 0 {
            let hashfull = tt.hashfull();
            let hashfull_est = tt.hashfull_estimate();
            println!("  After {i} stores: hashfull={hashfull} (estimate={hashfull_est})");
        }
    }

    let elapsed = start_time.elapsed();
    let stores_per_sec = total_stores as f64 / elapsed.as_secs_f64();

    // Final metrics
    let final_hashfull = tt.hashfull();
    let final_estimate = tt.hashfull_estimate();

    println!("  Total stores: {total_stores}");
    println!("  Time elapsed: {:.2}s", elapsed.as_secs_f64());
    println!("  Stores/sec: {stores_per_sec:.0}");
    println!("  Final hashfull: {final_hashfull} (estimate: {final_estimate})");

    // Note: tt_metrics feature is only available in engine-core crate
    // To see filtered metrics, run with: --features "hashfull_filter tt_metrics"
}
