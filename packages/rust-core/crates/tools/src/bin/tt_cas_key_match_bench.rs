//! Benchmark to measure Phase 5 CAS key match optimization
//!
//! This benchmark specifically tests the scenario where multiple threads
//! try to write the same position, which is common in parallel search.

use engine_core::search::tt::{NodeType, TranspositionTable};
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
    // Percentage of operations that target the same position (0-100)
    same_position_percentage: u32,
    // Number of unique positions to cycle through
    num_unique_positions: usize,
}

/// Shared statistics
struct SharedStats {
    total_operations: AtomicU64,
    same_position_writes: AtomicU64,
}

impl SharedStats {
    fn new() -> Self {
        Self {
            total_operations: AtomicU64::new(0),
            same_position_writes: AtomicU64::new(0),
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

    // Pre-generate some common positions that threads will compete for
    let mut rng = Xoshiro256PlusPlus::seed_from_u64(42);
    let common_positions: Vec<u64> =
        (0..config.num_unique_positions).map(|_| rng.next_u64()).collect();

    // Launch worker threads
    let mut handles = vec![];
    for thread_id in 0..config.num_threads {
        let tt_clone = Arc::clone(&tt);
        let stats_clone = Arc::clone(&stats);
        let stop_flag_clone = Arc::clone(&stop_flag);
        let common_positions = common_positions.clone();
        let same_pos_pct = config.same_position_percentage;

        let handle = thread::spawn(move || {
            let mut rng = Xoshiro256PlusPlus::seed_from_u64(thread_id as u64 * 1000);
            let mut operations = 0u64;
            let mut same_pos_writes = 0u64;

            while !stop_flag_clone.load(Ordering::Relaxed) {
                // Decide whether to target a common position or a random one
                let use_common = rng.next_u32() % 100 < same_pos_pct;

                let hash = if use_common {
                    // Pick a common position
                    let idx = (rng.next_u64() as usize) % common_positions.len();
                    same_pos_writes += 1;
                    common_positions[idx]
                } else {
                    // Random position
                    rng.next_u64()
                };

                // Vary the depth to create different priorities
                let depth = (rng.next_u32() % 20 + 1) as u8;
                let score = (rng.next_u32() % 2000) as i16 - 1000;
                let eval = (rng.next_u32() % 2000) as i16 - 1000;

                // Store entry multiple times with slight variations to create CAS conflicts
                for _ in 0..3 {
                    // Vary the depth slightly to create different priorities
                    let varied_depth = depth + (thread_id as u8 % 3);
                    let varied_score = score + (thread_id as i16 * 10);
                    tt_clone.store(hash, None, varied_score, eval, varied_depth, NodeType::Exact);

                    // Small delay to increase chance of concurrent access
                    std::hint::spin_loop();
                }

                operations += 3;

                // Occasionally probe to create read contention
                if operations % 10 == 0 {
                    let probe_hash = if rng.next_u32() % 100 < same_pos_pct {
                        let idx = (rng.next_u64() as usize) % common_positions.len();
                        common_positions[idx]
                    } else {
                        rng.next_u64()
                    };
                    let _ = tt_clone.probe(probe_hash);
                }

                // Fill the table quickly in the beginning to force CAS operations
                if operations < 10000 {
                    // Store many entries quickly to fill the table
                    for _ in 0..100 {
                        let fill_hash = rng.next_u64();
                        tt_clone.store(fill_hash, None, score, eval, depth, NodeType::Exact);
                    }
                }
            }

            stats_clone.total_operations.fetch_add(operations, Ordering::Relaxed);
            stats_clone.same_position_writes.fetch_add(same_pos_writes, Ordering::Relaxed);
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
    let total_operations = stats.total_operations.load(Ordering::Relaxed);

    // Get hashfull percentage
    let hashfull = tt.hashfull() as f32 / 10.0; // Convert to percentage

    // Get metrics
    let metrics = tt.metrics.as_ref().map(|m| DetailedMetrics {
        cas_attempts: m.cas_attempts.load(Ordering::Relaxed),
        cas_successes: m.cas_successes.load(Ordering::Relaxed),
        cas_failures: m.cas_failures.load(Ordering::Relaxed),
        cas_key_match: m.cas_key_match.load(Ordering::Relaxed),
        update_existing: m.update_existing.load(Ordering::Relaxed),
        replace_empty: m.replace_empty.load(Ordering::Relaxed),
        replace_worst: m.replace_worst.load(Ordering::Relaxed),
        hashfull,
    });

    (total_duration, total_operations, metrics)
}

#[derive(Debug)]
struct DetailedMetrics {
    cas_attempts: u64,
    cas_successes: u64,
    cas_failures: u64,
    cas_key_match: u64,
    update_existing: u64,
    replace_empty: u64,
    replace_worst: u64,
    hashfull: f32,
}

fn main() {
    println!("=== TT CAS Key Match Optimization Benchmark ===\n");

    // Test configurations - use very small TT to force replacements
    let configs = [
        // Low contention
        BenchConfig {
            tt_size_mb: 1, // 128KB would be better but minimum is 1MB
            num_threads: 8,
            duration_secs: 5,
            same_position_percentage: 50,
            num_unique_positions: 100,
        },
        // Medium contention
        BenchConfig {
            tt_size_mb: 1,
            num_threads: 8,
            duration_secs: 5,
            same_position_percentage: 70,
            num_unique_positions: 20,
        },
        // High contention (typical for PV nodes)
        BenchConfig {
            tt_size_mb: 1,
            num_threads: 8,
            duration_secs: 5,
            same_position_percentage: 80,
            num_unique_positions: 10,
        },
        // Very high contention
        BenchConfig {
            tt_size_mb: 1,
            num_threads: 8,
            duration_secs: 5,
            same_position_percentage: 90,
            num_unique_positions: 5,
        },
    ];

    for (i, config) in configs.iter().enumerate() {
        println!(
            "Test {}: {} threads, {}% same position rate, {} unique positions",
            i + 1,
            config.num_threads,
            config.same_position_percentage,
            config.num_unique_positions
        );

        let (duration, total_ops, metrics) = run_benchmark(config);

        let ops_per_sec = total_ops as f64 / duration.as_secs_f64();
        println!("  Duration: {:.2}s", duration.as_secs_f64());
        println!("  Total operations: {total_ops}");
        println!("  Operations/sec: {ops_per_sec:.0}");

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

            // Verify CAS metrics consistency
            if m.cas_attempts > 0 {
                let cas_sum = m.cas_successes + m.cas_failures;
                if cas_sum != m.cas_attempts {
                    println!("    WARNING: CAS metrics inconsistency! attempts={}, successes+failures={}",
                            m.cas_attempts, cas_sum);
                }
            }

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
            let total_updates = m.update_existing + m.replace_empty + m.replace_worst;
            println!(
                "    Update existing: {} ({:.1}%)",
                m.update_existing,
                if total_updates > 0 {
                    m.update_existing as f64 / total_updates as f64 * 100.0
                } else {
                    0.0
                }
            );
            println!(
                "    Replace empty: {} ({:.1}%)",
                m.replace_empty,
                if total_updates > 0 {
                    m.replace_empty as f64 / total_updates as f64 * 100.0
                } else {
                    0.0
                }
            );
            println!(
                "    Replace worst: {} ({:.1}%)",
                m.replace_worst,
                if total_updates > 0 {
                    m.replace_worst as f64 / total_updates as f64 * 100.0
                } else {
                    0.0
                }
            );

            // Note: hashfull not available from outside benchmark function

            // Calculate efficiency improvement from Phase 5
            if m.cas_key_match > 0 {
                println!("\n  Phase 5 Optimization Impact:");
                println!("    Avoided CAS retries: {}", m.cas_key_match);
                println!(
                    "    Efficiency gain: {:.1}% reduction in wasted CAS attempts",
                    m.cas_key_match as f64 / (m.cas_failures + m.cas_key_match) as f64 * 100.0
                );
            }
        }

        println!("\n{}", "-".repeat(60));
        println!();
    }

    println!("\nBenchmark complete!");
}
