//! Benchmark for COW Position optimization
//!
//! Measures the performance improvement from Copy-on-Write optimization

use anyhow::Result;
use engine_core::shogi::{CowPosition, Position};
use std::hint::black_box;
use std::time::Instant;

const ITERATIONS: usize = 100_000;

fn benchmark_regular_clone(pos: &Position) -> (u128, usize) {
    let start = Instant::now();
    let mut total_size = 0;

    for _ in 0..ITERATIONS {
        let cloned = black_box(pos.clone());
        // Prevent optimization
        total_size += cloned.ply as usize;
    }

    let elapsed = start.elapsed().as_micros();
    (elapsed, total_size)
}

fn benchmark_cow_clone(cow_pos: &CowPosition) -> (u128, usize) {
    let start = Instant::now();
    let mut total_size = 0;

    for _ in 0..ITERATIONS {
        let cloned = black_box(cow_pos.clone());
        // Prevent optimization
        total_size += cloned.ply as usize;
    }

    let elapsed = start.elapsed().as_micros();
    (elapsed, total_size)
}

fn benchmark_cow_clone_with_modification(cow_pos: &CowPosition) -> (u128, usize) {
    let start = Instant::now();
    let mut total_size = 0;

    for _ in 0..ITERATIONS {
        let mut cloned = black_box(cow_pos.clone());
        // Trigger COW by modifying history
        cloned.push_history(12345);
        total_size += cloned.ply as usize;
    }

    let elapsed = start.elapsed().as_micros();
    (elapsed, total_size)
}

fn main() -> Result<()> {
    println!("COW Position Benchmark");
    println!("======================");
    println!("Iterations: {ITERATIONS}");
    println!();

    // Create a sample position
    let sfen = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let pos =
        Position::from_sfen(sfen).map_err(|e| anyhow::anyhow!("Failed to parse SFEN: {}", e))?;
    let cow_pos = CowPosition::from(&pos);

    // Run benchmarks
    println!("Running benchmarks...");

    // Regular Position clone
    let (regular_time, _) = benchmark_regular_clone(&pos);
    let regular_per_clone = regular_time as f64 / ITERATIONS as f64;
    println!("Regular Position clone: {regular_per_clone:.3} µs/clone");

    // COW Position clone (no modification)
    let (cow_time, _) = benchmark_cow_clone(&cow_pos);
    let cow_per_clone = cow_time as f64 / ITERATIONS as f64;
    println!("COW Position clone (no mod): {cow_per_clone:.3} µs/clone");

    // COW Position clone (with modification)
    let (cow_mod_time, _) = benchmark_cow_clone_with_modification(&cow_pos);
    let cow_mod_per_clone = cow_mod_time as f64 / ITERATIONS as f64;
    println!("COW Position clone (with mod): {cow_mod_per_clone:.3} µs/clone");

    println!();
    println!("Performance Summary:");
    println!("====================");

    let speedup_no_mod = regular_per_clone / cow_per_clone;
    let speedup_with_mod = regular_per_clone / cow_mod_per_clone;

    println!("COW speedup (no modification): {speedup_no_mod:.2}x");
    println!("COW speedup (with modification): {speedup_with_mod:.2}x");

    // Memory sharing analysis
    println!();
    println!("Memory Sharing Analysis:");
    println!("========================");

    let cow1 = cow_pos.clone();
    let cow2 = cow_pos.clone();
    let cow3 = cow_pos.clone();

    println!("After 3 clones:");
    println!("  Board ref count: {}", cow1.board_ref_count());
    println!("  Hands ref count: {}", cow1.hands_ref_count());
    println!("  History ref count: {}", cow1.history_ref_count());

    // Test modification impact
    let mut cow_modified = cow1.clone();
    cow_modified.push_history(99999);

    println!("After modifying one clone's history:");
    println!("  Original history ref count: {}", cow2.history_ref_count());
    println!("  Modified history ref count: {}", cow_modified.history_ref_count());

    Ok(())
}
