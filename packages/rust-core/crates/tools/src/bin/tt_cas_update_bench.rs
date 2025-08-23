//! Benchmark specifically targeting try_update_entry_generic CAS operations
//!
//! This benchmark creates scenarios where multiple threads update
//! the same TT entries with increasing depths, maximizing CAS operations
//! in the update path.

use engine_core::search::{tt::TranspositionTable, NodeType};
use rand::prelude::*;
use rand_xoshiro::Xoshiro256PlusPlus;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Configuration for the benchmark
struct BenchConfig {
    tt_size_mb: usize,
    num_threads: usize,
    duration_secs: u64,
    num_positions: usize,      // Number of unique positions
    depth_increment_rate: f32, // Probability of incrementing depth
}

/// Shared statistics
struct SharedStats {
    total_stores: AtomicU64,
    depth_updates: AtomicU64,
}

impl SharedStats {
    fn new() -> Self {
        Self {
            total_stores: AtomicU64::new(0),
            depth_updates: AtomicU64::new(0),
        }
    }
}

fn run_benchmark(config: &BenchConfig) -> (Duration, u64, Option<DetailedMetrics>) {
    // Create TT with metrics enabled
    let mut tt = TranspositionTable::new(config.tt_size_mb);
    tt.enable_metrics();
    let tt = Arc::new(tt);

    let stats = Arc::new(SharedStats::new());
    let stop_flag = Arc::new(AtomicBool::new(false));
    let start_time = Instant::now();

    // Pre-generate positions
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
    let positions: Vec<u64> = (0..config.num_positions).map(|_| rng.next_u64()).collect();

    // First, populate the TT with initial entries
    println!("Populating TT with initial entries...");
    for &hash in &positions {
        tt.store(hash, None, 0, 0, 1, NodeType::Exact);
    }

    // Launch worker threads
    let mut handles = vec![];
    for thread_id in 0..config.num_threads {
        let tt_clone = Arc::clone(&tt);
        let stats_clone = Arc::clone(&stats);
        let stop_flag_clone = Arc::clone(&stop_flag);
        let positions = positions.clone();
        let depth_inc_rate = config.depth_increment_rate;

        let handle = thread::spawn(move || {
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(thread_id as u64 * 1000);
            let mut stores = 0u64;
            let mut depth_updates = 0u64;
            let mut current_depths: Vec<u8> = vec![1; positions.len()];

            while !stop_flag_clone.load(Ordering::Relaxed) {
                // Pick a random position
                let pos_idx = rng.next_u64() as usize % positions.len();
                let hash = positions[pos_idx];

                // Decide whether to increment depth
                if rng.random::<f32>() < depth_inc_rate {
                    current_depths[pos_idx] = current_depths[pos_idx].saturating_add(1);
                    depth_updates += 1;
                }

                let depth = current_depths[pos_idx];
                let score = (rng.next_u32() % 200) as i16 - 100;
                let eval = (rng.next_u32() % 200) as i16 - 100;

                // Store with current depth
                tt_clone.store(hash, None, score, eval, depth, NodeType::Exact);
                stores += 1;

                // Sometimes probe to verify
                if stores % 100 == 0 {
                    let _ = tt_clone.probe(hash);
                }

                // Small delay to create more contention
                if stores % 10 == 0 {
                    std::hint::spin_loop();
                }
            }

            stats_clone.total_stores.fetch_add(stores, Ordering::Relaxed);
            stats_clone.depth_updates.fetch_add(depth_updates, Ordering::Relaxed);
        });

        handles.push(handle);
    }

    // Run for specified duration
    thread::sleep(Duration::from_secs(config.duration_secs));
    stop_flag.store(true, Ordering::Relaxed);

    // Wait for all threads
    for handle in handles {
        handle.join().unwrap();
    }

    let total_duration = start_time.elapsed();
    let total_stores = stats.total_stores.load(Ordering::Relaxed);

    // Get metrics
    let metrics = tt.metrics().as_ref().map(|m| DetailedMetrics {
        cas_attempts: m.cas_attempts.load(Ordering::Relaxed),
        cas_successes: m.cas_successes.load(Ordering::Relaxed),
        cas_failures: m.cas_failures.load(Ordering::Relaxed),
        cas_key_match: m.cas_key_match.load(Ordering::Relaxed),
        update_existing: m.update_existing.load(Ordering::Relaxed),
        depth_filtered: m.depth_filtered.load(Ordering::Relaxed),
        hashfull: tt.hashfull() as f32 / 10.0,
    });

    (total_duration, total_stores, metrics)
}

#[derive(Debug)]
struct DetailedMetrics {
    cas_attempts: u64,
    cas_successes: u64,
    cas_failures: u64,
    cas_key_match: u64,
    update_existing: u64,
    depth_filtered: u64,
    hashfull: f32,
}

fn main() {
    println!("=== TT CAS Update Path Benchmark ===\n");
    println!("This benchmark specifically targets try_update_entry_generic");
    println!("to observe CAS operations during depth-based updates.\n");

    // Test configurations
    let configs = [
        BenchConfig {
            tt_size_mb: 1,
            num_threads: 8,
            duration_secs: 5,
            num_positions: 1000,
            depth_increment_rate: 0.3,
        },
        BenchConfig {
            tt_size_mb: 1,
            num_threads: 8,
            duration_secs: 5,
            num_positions: 100,
            depth_increment_rate: 0.5,
        },
        BenchConfig {
            tt_size_mb: 1,
            num_threads: 8,
            duration_secs: 5,
            num_positions: 50,
            depth_increment_rate: 0.7,
        },
        BenchConfig {
            tt_size_mb: 1,
            num_threads: 8,
            duration_secs: 5,
            num_positions: 20,
            depth_increment_rate: 0.9,
        },
    ];

    for (i, config) in configs.iter().enumerate() {
        println!(
            "Test {}: {} threads, {} positions, {:.0}% depth increment rate",
            i + 1,
            config.num_threads,
            config.num_positions,
            config.depth_increment_rate * 100.0
        );

        let (duration, total_stores, metrics) = run_benchmark(config);

        let stores_per_sec = total_stores as f64 / duration.as_secs_f64();
        println!("  Duration: {:.2}s", duration.as_secs_f64());
        println!("  Total stores: {total_stores}");
        println!("  Stores/sec: {stores_per_sec:.0}");

        if let Some(m) = metrics {
            println!("\n  Table Status:");
            println!("    Hashfull: {:.1}%", m.hashfull);

            println!("\n  CAS Statistics:");
            println!("    Attempts: {}", m.cas_attempts);
            println!(
                "    Successes: {} ({:.1}%)",
                m.cas_successes,
                if m.cas_attempts > 0 {
                    m.cas_successes as f64 / m.cas_attempts as f64 * 100.0
                } else {
                    0.0
                }
            );
            println!(
                "    Failures: {} ({:.1}%)",
                m.cas_failures,
                if m.cas_attempts > 0 {
                    m.cas_failures as f64 / m.cas_attempts as f64 * 100.0
                } else {
                    0.0
                }
            );
            println!(
                "    Key matches: {} ({:.1}% of failures)",
                m.cas_key_match,
                if m.cas_failures > 0 {
                    m.cas_key_match as f64 / m.cas_failures as f64 * 100.0
                } else {
                    0.0
                }
            );

            println!("\n  Update Patterns:");
            println!("    Update existing: {}", m.update_existing);
            println!("    Depth filtered: {}", m.depth_filtered);

            // Calculate update efficiency
            let total_update_attempts = m.update_existing + m.depth_filtered;
            if total_update_attempts > 0 {
                println!(
                    "    Update success rate: {:.1}%",
                    m.update_existing as f64 / total_update_attempts as f64 * 100.0
                );
            }

            // Phase 5 optimization impact
            if m.cas_key_match > 0 {
                println!("\n  Phase 5 Optimization Impact:");
                println!("    CAS operations saved: {}", m.cas_key_match);
                let potential_retries = m.cas_failures + m.cas_key_match;
                println!(
                    "    Retry reduction: {:.1}%",
                    m.cas_key_match as f64 / potential_retries as f64 * 100.0
                );
            }
        }

        println!("\n{}", "-".repeat(60));
        println!();
    }

    println!("\nBenchmark complete!");
}
