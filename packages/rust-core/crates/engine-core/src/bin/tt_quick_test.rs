//! Quick test to compare TT v1 and v2 performance

use engine_core::search::{tt::TranspositionTable, tt_v2::TranspositionTableV2};
use std::time::Instant;

fn main() {
    println!("TT Performance Quick Comparison\n");

    const SIZE_MB: usize = 16;
    const TEST_POSITIONS: usize = 1_000_000;

    // Test v1
    println!("Testing TT v1...");
    let tt_v1 = TranspositionTable::new(SIZE_MB);
    let mut v1_store_time = 0u128;
    let mut v1_probe_time = 0u128;

    for i in 0..TEST_POSITIONS {
        let hash = (i as u64).wrapping_mul(0x123456789ABCDEF);

        let start = Instant::now();
        tt_v1.store(
            hash,
            None,
            (i % 1000) as i16,
            0,
            (i % 20) as u8,
            engine_core::search::tt::NodeType::Exact,
        );
        v1_store_time += start.elapsed().as_nanos();

        let start = Instant::now();
        tt_v1.probe(hash);
        v1_probe_time += start.elapsed().as_nanos();
    }

    println!("  Store: {:.2} ns/op", v1_store_time as f64 / TEST_POSITIONS as f64);
    println!("  Probe: {:.2} ns/op", v1_probe_time as f64 / TEST_POSITIONS as f64);
    println!("  Hash fill: {}‰\n", tt_v1.hashfull());

    // Test v2
    println!("Testing TT v2...");
    let tt_v2 = TranspositionTableV2::new(SIZE_MB);
    let mut v2_store_time = 0u128;
    let mut v2_probe_time = 0u128;

    for i in 0..TEST_POSITIONS {
        let hash = (i as u64).wrapping_mul(0x123456789ABCDEF);

        let start = Instant::now();
        tt_v2.store(
            hash,
            None,
            (i % 1000) as i16,
            0,
            (i % 20) as u8,
            engine_core::search::tt_v2::NodeType::Exact,
        );
        v2_store_time += start.elapsed().as_nanos();

        let start = Instant::now();
        tt_v2.probe(hash);
        v2_probe_time += start.elapsed().as_nanos();
    }

    println!("  Store: {:.2} ns/op", v2_store_time as f64 / TEST_POSITIONS as f64);
    println!("  Probe: {:.2} ns/op", v2_probe_time as f64 / TEST_POSITIONS as f64);
    println!("  Hash fill: {}‰\n", tt_v2.hashfull());

    // Results
    println!("Performance Comparison:");
    println!("  Store speedup: {:.1}x", v1_store_time as f64 / v2_store_time as f64);
    println!("  Probe speedup: {:.1}x", v1_probe_time as f64 / v2_probe_time as f64);

    // Cache efficiency test
    println!("\nCache Efficiency Test (clustered access):");

    let clusters = 100;
    let per_cluster = 10000;

    // Fill tables
    for c in 0..clusters {
        let base = (c as u64) * 0x100000000;
        for i in 0..per_cluster {
            let hash = base + i as u64;
            tt_v1.store(hash, None, 100, 0, 10, engine_core::search::tt::NodeType::Exact);
            tt_v2.store(hash, None, 100, 0, 10, engine_core::search::tt_v2::NodeType::Exact);
        }
    }

    // Test clustered access
    let mut v1_hits = 0;
    let mut v2_hits = 0;

    let start = Instant::now();
    for c in 0..clusters {
        let base = (c as u64) * 0x100000000;
        for i in 0..per_cluster {
            if tt_v1.probe(base + i as u64).is_some() {
                v1_hits += 1;
            }
        }
    }
    let v1_time = start.elapsed();

    let start = Instant::now();
    for c in 0..clusters {
        let base = (c as u64) * 0x100000000;
        for i in 0..per_cluster {
            if tt_v2.probe(base + i as u64).is_some() {
                v2_hits += 1;
            }
        }
    }
    let v2_time = start.elapsed();

    println!("  v1: {} hits in {:.2}ms", v1_hits, v1_time.as_secs_f64() * 1000.0);
    println!("  v2: {} hits in {:.2}ms", v2_hits, v2_time.as_secs_f64() * 1000.0);
    println!("  Speedup: {:.1}x", v1_time.as_secs_f64() / v2_time.as_secs_f64());
}
