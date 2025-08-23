//! Comprehensive benchmark for Transposition Table with Hashfull Control and GC

use engine_core::search::{
    tt::{TTEntryParams, TranspositionTable},
    NodeType,
};
use engine_core::shogi::Position;
use rand::{RngCore, SeedableRng};
use std::time::Instant;

#[derive(Default)]
struct BenchmarkResults {
    stores_per_sec: f64,
    final_hashfull: u16,
    final_estimate: u16,
    filtered_count: u64,
    gc_triggered: u64,
    gc_entries_cleared: u64,
    gc_time_ms: u64,
    cas_attempts: u64,
    cas_successes: u64,
}

fn main() {
    println!("=== Transposition Table Hashfull Control & GC Benchmark ===\n");

    // Test configurations
    let configs = vec![
        ("Baseline (no features)", vec![]),
        ("Phase 1 (hashfull_filter)", vec!["hashfull_filter"]),
        ("Phase 1+2 (filter + GC)", vec!["hashfull_filter", "gc"]),
        ("All features + metrics", vec!["hashfull_filter", "gc", "tt_metrics"]),
    ];

    let table_sizes = vec![1, 8, 16]; // MB

    for (config_name, features) in configs {
        println!("\n## Configuration: {config_name}");
        println!("Features: {features:?}");
        println!("{}", "-".repeat(60));

        for size_mb in &table_sizes {
            println!("\n### Table size: {size_mb}MB");
            let results = run_benchmark(*size_mb, &features);
            print_results(&results);
        }
    }

    // Long-running stability test
    println!("\n## Long-running Stability Test (16MB, 1M operations)");
    println!("{}", "-".repeat(60));
    run_stability_test(16);
}

fn run_benchmark(size_mb: usize, _features: &[&str]) -> BenchmarkResults {
    let tt = TranspositionTable::new(size_mb);
    let mut rng = rand_xoshiro::Xoshiro256PlusPlus::seed_from_u64(12345);
    let mut results = BenchmarkResults::default();

    // Generate test positions
    let base_position = Position::startpos();
    let num_operations = 200_000;

    let start_time = Instant::now();
    let mut _last_gc_time = Instant::now();

    for i in 0..num_operations {
        // Generate entry with realistic distribution
        let hash = base_position.hash.wrapping_add(rng.next_u64());
        let depth = (rng.next_u32() % 20 + 1) as u8;
        let node_type = match rng.next_u32() % 100 {
            0..=10 => NodeType::Exact,       // 10%
            11..=55 => NodeType::LowerBound, // 45%
            _ => NodeType::UpperBound,       // 45%
        };
        let is_pv = rng.next_u32() % 100 < 5; // 5% PV nodes

        let params = TTEntryParams {
            key: hash,
            mv: None,
            score: (rng.next_u32() % 2000) as i16 - 1000,
            eval: (rng.next_u32() % 2000) as i16 - 1000,
            depth,
            node_type,
            age: 0,
            is_pv,
            ..Default::default()
        };

        tt.store_with_params(params);

        // Periodic GC check (every 1000 stores)
        if i % 1000 == 0 && tt.should_trigger_gc() {
            let gc_start = Instant::now();
            while !tt.perform_incremental_gc(256) {
                // Continue GC
            }
            let gc_duration = gc_start.elapsed();
            results.gc_time_ms += gc_duration.as_millis() as u64;
            _last_gc_time = Instant::now();
        }

        // Sample metrics periodically
        if i % 10000 == 0 && i > 0 {
            let hashfull = tt.hashfull();
            let estimate = tt.hashfull_estimate();
            if i % 50000 == 0 {
                println!("  Progress: {i} ops, hashfull={hashfull} (est={estimate})");
            }
        }
    }

    let elapsed = start_time.elapsed();
    results.stores_per_sec = num_operations as f64 / elapsed.as_secs_f64();
    results.final_hashfull = tt.hashfull();
    results.final_estimate = tt.hashfull_estimate();

    // Note: Metrics collection would be available with tt_metrics feature in engine-core

    results
}

fn run_stability_test(size_mb: usize) {
    let tt = TranspositionTable::new(size_mb);
    let mut rng = rand_xoshiro::Xoshiro256PlusPlus::seed_from_u64(42);

    let start_time = Instant::now();
    let mut checkpoints = vec![];

    for i in 0..1_000_000 {
        let hash = rng.next_u64();
        let depth = (rng.next_u32() % 30 + 1) as u8;

        tt.store(
            hash,
            None,
            (rng.next_u32() % 2000) as i16 - 1000,
            (rng.next_u32() % 2000) as i16 - 1000,
            depth,
            if rng.next_u32() % 2 == 0 {
                NodeType::LowerBound
            } else {
                NodeType::UpperBound
            },
        );

        // GC maintenance
        if i % 1000 == 0 && tt.should_trigger_gc() {
            while !tt.perform_incremental_gc(512) {}
        }

        // Checkpoint every 100k operations
        if i % 100_000 == 0 && i > 0 {
            let elapsed = start_time.elapsed();
            let hashfull = tt.hashfull();
            let estimate = tt.hashfull_estimate();
            let rate = i as f64 / elapsed.as_secs_f64();

            checkpoints.push((i, elapsed, hashfull, estimate, rate));
            println!(
                "  {:6}k ops: {:.2}s, hashfull={:3} (est={:3}), rate={:.0} ops/s",
                i / 1000,
                elapsed.as_secs_f64(),
                hashfull,
                estimate,
                rate
            );
        }
    }

    // Analyze stability
    let rates: Vec<f64> = checkpoints.iter().map(|(_, _, _, _, r)| *r).collect();
    let avg_rate = rates.iter().sum::<f64>() / rates.len() as f64;
    let variance = rates.iter().map(|r| (r - avg_rate).powi(2)).sum::<f64>() / rates.len() as f64;
    let std_dev = variance.sqrt();
    let cv = std_dev / avg_rate * 100.0;

    println!("\n  Stability Analysis:");
    println!("  Average rate: {avg_rate:.0} ops/s");
    println!("  Std deviation: {std_dev:.0} ops/s");
    println!("  Coefficient of variation: {cv:.1}%");
    println!(
        "  Stability: {}",
        if cv < 5.0 {
            "Excellent"
        } else if cv < 10.0 {
            "Good"
        } else {
            "Fair"
        }
    );
}

fn print_results(results: &BenchmarkResults) {
    println!("  Throughput: {:.2}M stores/sec", results.stores_per_sec / 1_000_000.0);
    println!(
        "  Final hashfull: {} (estimate: {})",
        results.final_hashfull, results.final_estimate
    );

    if results.filtered_count > 0 {
        println!("  Filtered stores: {}", results.filtered_count);
    }

    if results.gc_triggered > 0 {
        println!("  GC triggered: {} times", results.gc_triggered);
        println!("  GC cleared: {} entries", results.gc_entries_cleared);
        println!("  GC time: {}ms", results.gc_time_ms);
    }

    if results.cas_attempts > 0 {
        let cas_success_rate = results.cas_successes as f64 / results.cas_attempts as f64 * 100.0;
        println!("  CAS success rate: {cas_success_rate:.1}%");
    }
}
