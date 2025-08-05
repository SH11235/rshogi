//! TT CAS Benchmark - Multi-threaded benchmark for CAS optimization
//!
//! Measures the impact of Write-Through strategy on TT performance
//! with various thread counts (1/8/16/32)

use clap::{Arg, Command};
use engine_core::{
    movegen::MoveGen,
    search::tt::{DetailedTTMetrics, NodeType, TranspositionTable},
    shogi::{board::Position, MoveList},
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Barrier,
};
use std::thread;
use std::time::{Duration, Instant};

/// Shared statistics for all threads
struct SharedStats {
    total_nodes: AtomicU64,
    total_positions: AtomicU64,
}

impl SharedStats {
    fn new() -> Self {
        SharedStats {
            total_nodes: AtomicU64::new(0),
            total_positions: AtomicU64::new(0),
        }
    }
}

/// Worker thread function
fn worker_thread(
    thread_id: usize,
    iterations: u32,
    depth: u8,
    tt: Arc<TranspositionTable>,
    barrier: Arc<Barrier>,
    stats: Arc<SharedStats>,
) -> Duration {
    let mut total_duration = Duration::ZERO;

    // Wait for all threads to be ready
    barrier.wait();

    for _ in 0..iterations {
        // Each thread works on slightly different positions to simulate real workload
        let mut pos = Position::startpos();

        // Make a few different initial moves per thread to create diversity
        let mut moves = MoveList::new();
        let mut mg = MoveGen::new();
        mg.generate_all(&pos, &mut moves);

        if moves.len() > thread_id {
            let mv = moves[thread_id % moves.len()];
            let undo_info = pos.do_move(mv);
            pos.undo_move(mv, undo_info);
        }

        let start = Instant::now();
        let nodes = perft_worker(&mut pos, depth, &tt, thread_id);
        let duration = start.elapsed();

        total_duration += duration;
        stats.total_nodes.fetch_add(nodes, Ordering::Relaxed);
        stats.total_positions.fetch_add(1, Ordering::Relaxed);
    }

    total_duration
}

/// Perft with thread-specific behavior
fn perft_worker(
    pos: &mut Position,
    depth: u8,
    tt: &Arc<TranspositionTable>,
    thread_id: usize,
) -> u64 {
    if depth == 0 {
        return 1;
    }

    let mut moves = MoveList::new();
    let mut mg = MoveGen::new();
    mg.generate_all(pos, &mut moves);

    let mut nodes = 0;
    let hash = pos.zobrist_hash();

    // Add thread ID to hash to create more contention patterns
    let modified_hash = hash ^ (thread_id as u64);

    // Try TT probe
    if let Some(_entry) = tt.probe(modified_hash) {
        // Simulate some work
    }

    for &mv in moves.iter() {
        let undo_info = pos.do_move(mv);
        nodes += perft_worker(pos, depth - 1, tt, thread_id);
        pos.undo_move(mv, undo_info);
    }

    // Store in TT with thread-specific pattern
    tt.store(modified_hash, None, (nodes % 32768) as i16, 0, depth, NodeType::Exact);

    nodes
}

/// Run benchmark with specified thread count
fn run_benchmark(
    thread_count: usize,
    depth: u8,
    iterations: u32,
    tt_size_mb: usize,
) -> (Duration, u64, Option<DetailedTTMetrics>) {
    let mut tt = TranspositionTable::new(tt_size_mb);
    tt.enable_metrics();
    let tt = Arc::new(tt);

    let barrier = Arc::new(Barrier::new(thread_count));
    let stats = Arc::new(SharedStats::new());
    let mut handles = vec![];

    println!("\nRunning with {thread_count} threads...");
    let start_time = Instant::now();

    for thread_id in 0..thread_count {
        let tt_clone = Arc::clone(&tt);
        let barrier_clone = Arc::clone(&barrier);
        let stats_clone = Arc::clone(&stats);

        let handle = thread::spawn(move || {
            worker_thread(thread_id, iterations, depth, tt_clone, barrier_clone, stats_clone)
        });

        handles.push(handle);
    }

    // Wait for all threads to complete
    for handle in handles {
        handle.join().unwrap();
    }

    let total_duration = start_time.elapsed();
    let total_nodes = stats.total_nodes.load(Ordering::Relaxed);

    // Get metrics
    let metrics = tt.metrics.as_ref().map(|m| DetailedTTMetrics {
        cas_attempts: AtomicU64::new(m.cas_attempts.load(Ordering::Relaxed)),
        cas_successes: AtomicU64::new(m.cas_successes.load(Ordering::Relaxed)),
        cas_failures: AtomicU64::new(m.cas_failures.load(Ordering::Relaxed)),
        update_existing: AtomicU64::new(m.update_existing.load(Ordering::Relaxed)),
        replace_empty: AtomicU64::new(m.replace_empty.load(Ordering::Relaxed)),
        replace_worst: AtomicU64::new(m.replace_worst.load(Ordering::Relaxed)),
        atomic_stores: AtomicU64::new(m.atomic_stores.load(Ordering::Relaxed)),
        atomic_loads: AtomicU64::new(m.atomic_loads.load(Ordering::Relaxed)),
        prefetch_count: AtomicU64::new(m.prefetch_count.load(Ordering::Relaxed)),
        prefetch_hits: AtomicU64::new(m.prefetch_hits.load(Ordering::Relaxed)),
        depth_filtered: AtomicU64::new(m.depth_filtered.load(Ordering::Relaxed)),
        hashfull_filtered: AtomicU64::new(m.hashfull_filtered.load(Ordering::Relaxed)),
        effective_updates: AtomicU64::new(m.effective_updates.load(Ordering::Relaxed)),
    });

    (total_duration, total_nodes, metrics)
}

fn main() {
    let matches = Command::new("TT CAS Benchmark")
        .about("Multi-threaded benchmark for TT CAS optimization")
        .arg(
            Arg::new("threads")
                .short('t')
                .long("threads")
                .value_name("THREADS")
                .help("Number of threads (comma-separated list)")
                .default_value("1,8,16,32"),
        )
        .arg(
            Arg::new("depth")
                .short('d')
                .long("depth")
                .value_name("DEPTH")
                .help("Search depth")
                .default_value("4"),
        )
        .arg(
            Arg::new("iterations")
                .short('i')
                .long("iterations")
                .value_name("ITERATIONS")
                .help("Iterations per thread")
                .default_value("3"),
        )
        .arg(
            Arg::new("tt-size")
                .long("tt-size")
                .value_name("MB")
                .help("Transposition table size in MB")
                .default_value("128"),
        )
        .get_matches();

    let threads_str = matches.get_one::<String>("threads").unwrap();
    let thread_counts: Vec<usize> =
        threads_str.split(',').filter_map(|s| s.trim().parse().ok()).collect();

    let depth: u8 = matches.get_one::<String>("depth").unwrap().parse().unwrap();
    let iterations: u32 = matches.get_one::<String>("iterations").unwrap().parse().unwrap();
    let tt_size_mb: usize = matches.get_one::<String>("tt-size").unwrap().parse().unwrap();

    println!("=== TT CAS Multi-threaded Benchmark ===");
    println!("Depth: {depth}");
    println!("Iterations per thread: {iterations}");
    println!("TT Size: {tt_size_mb} MB");
    println!("Thread counts: {thread_counts:?}");

    let mut results = vec![];

    for &thread_count in &thread_counts {
        let (duration, nodes, metrics) = run_benchmark(thread_count, depth, iterations, tt_size_mb);

        let nps = nodes as f64 / duration.as_secs_f64();

        println!("\n--- {thread_count} Thread(s) ---");
        println!("Total nodes: {nodes}");
        println!("Total time: {duration:?}");
        println!("NPS: {nps:.0}");

        if let Some(ref m) = metrics {
            let cas_attempts = m.cas_attempts.load(Ordering::Relaxed);
            let cas_failures = m.cas_failures.load(Ordering::Relaxed);
            let failure_rate = if cas_attempts > 0 {
                (cas_failures as f64 / cas_attempts as f64) * 100.0
            } else {
                0.0
            };

            println!("\nCAS Statistics:");
            println!("  Attempts: {cas_attempts}");
            println!("  Failures: {cas_failures}");
            println!("  Failure rate: {failure_rate:.2}%");

            let total_updates = m.update_existing.load(Ordering::Relaxed)
                + m.replace_empty.load(Ordering::Relaxed)
                + m.replace_worst.load(Ordering::Relaxed);

            if total_updates > 0 {
                println!("\nUpdate patterns:");
                println!(
                    "  Existing: {} ({:.1}%)",
                    m.update_existing.load(Ordering::Relaxed),
                    (m.update_existing.load(Ordering::Relaxed) as f64 / total_updates as f64)
                        * 100.0
                );
                println!(
                    "  Empty: {} ({:.1}%)",
                    m.replace_empty.load(Ordering::Relaxed),
                    (m.replace_empty.load(Ordering::Relaxed) as f64 / total_updates as f64) * 100.0
                );
                println!(
                    "  Worst: {} ({:.1}%)",
                    m.replace_worst.load(Ordering::Relaxed),
                    (m.replace_worst.load(Ordering::Relaxed) as f64 / total_updates as f64) * 100.0
                );
            }
        }

        results.push((thread_count, nps, metrics));
    }

    // Summary comparison
    println!("\n=== Performance Summary ===");
    println!("Threads | NPS          | CAS Failure Rate");
    println!("--------|--------------|------------------");

    for (threads, nps, metrics) in &results {
        let failure_rate = if let Some(ref m) = metrics {
            let attempts = m.cas_attempts.load(Ordering::Relaxed);
            let failures = m.cas_failures.load(Ordering::Relaxed);
            if attempts > 0 {
                (failures as f64 / attempts as f64) * 100.0
            } else {
                0.0
            }
        } else {
            0.0
        };

        println!("{threads:7} | {nps:12.0} | {failure_rate:>16.2}%");
    }

    // Calculate scalability
    if let Some((_, base_nps, _)) = results.first() {
        println!("\nScalability (relative to single thread):");
        for (threads, nps, _) in &results {
            let scalability = nps / base_nps;
            println!("  {threads} threads: {scalability:.2}x");
        }
    }
}
