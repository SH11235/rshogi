//! Transposition table prefetch benchmark
//!
//! Measures the performance impact of prefetching in the transposition table

use engine_core::movegen::MoveGen;
use engine_core::search::adaptive_prefetcher::AdaptivePrefetcher;
use engine_core::search::tt::{NodeType, TranspositionTable};
use engine_core::shogi::board::Position;
use engine_core::shogi::MoveList;
use std::time::Instant;

fn perft(pos: &mut Position, depth: u32, tt: &TranspositionTable, use_prefetch: bool) -> u64 {
    if depth == 0 {
        return 1;
    }

    let hash = pos.zobrist_hash();

    // Prefetch if enabled
    if use_prefetch && depth > 1 {
        // Prefetch for the next level
        let mut moves = MoveList::new();
        let mut mg = MoveGen::new();
        mg.generate_all(pos, &mut moves);

        // Prefetch first few moves
        for (i, &mv) in moves.iter().take(4).enumerate() {
            let undo_info = pos.do_move(mv);
            let next_hash = pos.zobrist_hash();

            // Use different cache levels based on index
            if i == 0 {
                tt.prefetch_l1(next_hash);
            } else {
                tt.prefetch_l2(next_hash);
            }

            pos.undo_move(mv, undo_info);
        }
    }

    // Check TT
    if let Some(entry) = tt.probe(hash) {
        if entry.depth() >= depth as u8 {
            // Use TT value (simplified - in real engine would check bounds)
            return 1;
        }
    }

    let mut count = 0;
    let mut moves = MoveList::new();
    let mut mg = MoveGen::new();
    mg.generate_all(pos, &mut moves);

    for &mv in moves.iter() {
        let undo_info = pos.do_move(mv);
        count += perft(pos, depth - 1, tt, use_prefetch);
        pos.undo_move(mv, undo_info);
    }

    // Store in TT
    tt.store(hash, None, count as i16, 0, depth as u8, NodeType::Exact);

    count
}

fn benchmark_perft(depth: u32, use_prefetch: bool) -> (u64, f64) {
    let tt = TranspositionTable::new(128); // 128MB TT
    let mut pos = Position::startpos();

    let start = Instant::now();
    let nodes = perft(&mut pos, depth, &tt, use_prefetch);
    let elapsed = start.elapsed();

    let nps = nodes as f64 / elapsed.as_secs_f64();

    (nodes, nps)
}

fn main() {
    println!("=== Transposition Table Prefetch Benchmark ===\n");

    // Warm up
    println!("Warming up...");
    benchmark_perft(3, false);
    benchmark_perft(3, true);

    // Run benchmarks
    let depths = [4, 5, 6];

    for &depth in &depths {
        println!("\n--- Depth {depth} ---");

        // Without prefetch
        println!("Without prefetch:");
        let (nodes_without, nps_without) = benchmark_perft(depth, false);
        println!("  Nodes: {nodes_without}");
        println!("  NPS: {nps_without:.0}");

        // With prefetch
        println!("With prefetch:");
        let (nodes_with, nps_with) = benchmark_perft(depth, true);
        println!("  Nodes: {nodes_with}");
        println!("  NPS: {nps_with:.0}");

        // Calculate improvement
        let improvement = ((nps_with - nps_without) / nps_without) * 100.0;
        println!("  Improvement: {improvement:.2}%");
    }

    // Test adaptive prefetcher
    println!("\n--- Adaptive Prefetcher Test ---");
    let prefetcher = AdaptivePrefetcher::new();
    let tt = TranspositionTable::new(128);
    let pos = Position::startpos();

    // Simulate usage and collect stats
    for i in 0..1000 {
        let hash = pos.zobrist_hash() ^ (i as u64);
        tt.prefetch_l1(hash);

        // Randomly record hits/misses
        if i % 3 == 0 {
            prefetcher.record_hit();
        } else {
            prefetcher.record_miss();
        }
    }

    let stats = prefetcher.stats();
    println!("Prefetch Statistics:");
    println!("  Hits: {}", stats.hits);
    println!("  Misses: {}", stats.misses);
    println!("  Hit Rate: {:.2}%", stats.hit_rate * 100.0);
    println!("  Current Distance: {}", stats.distance);
}
