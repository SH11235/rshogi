//! TT Prefetch Benchmark v5 - With detailed metrics and prefetch control

use clap::{Arg, Command};
use engine_core::{
    movegen::MoveGen,
    search::tt::{NodeType, TranspositionTable},
    shogi::{board::Position, MoveList},
};
use std::time::{Duration, Instant};

/// Run perft benchmark for a position
fn benchmark_position(
    sfen: &str,
    depth: u8,
    iterations: u32,
    tt_size_mb: usize,
    disable_prefetch: bool,
) -> (Duration, u64) {
    let mut total_nodes = 0;
    let mut total_duration = Duration::ZERO;

    // Create TT with metrics
    let mut tt = TranspositionTable::new(tt_size_mb);
    tt.enable_metrics();

    for i in 0..iterations {
        // Clear TT before each iteration
        tt.clear();

        // Reset metrics
        if let Some(metrics) = tt.metrics() {
            metrics.reset();
        }

        // Parse position
        let mut pos = Position::startpos();
        if sfen != "startpos" {
            eprintln!("Only startpos supported for now");
        }

        let start = Instant::now();
        let nodes = perft(&mut pos, depth, &tt, disable_prefetch);
        let duration = start.elapsed();

        total_nodes += nodes;
        total_duration += duration;

        // Print metrics for each iteration
        println!("\nIteration {}", i + 1);
        println!("Nodes: {nodes}");
        println!("Time: {duration:?}");
        println!("NPS: {:.0}", nodes as f64 / duration.as_secs_f64());

        // Print TT metrics
        if let Some(metrics) = tt.metrics() {
            metrics.print_summary();
        }
    }

    (total_duration, total_nodes)
}

/// Perft implementation
fn perft(pos: &mut Position, depth: u8, tt: &TranspositionTable, disable_prefetch: bool) -> u64 {
    if depth == 0 {
        return 1;
    }

    let mut moves = MoveList::new();
    let mut mg = MoveGen::new();
    mg.generate_all(pos, &mut moves);

    let mut nodes = 0;
    let hash = pos.zobrist_hash();

    // Prefetch TT if enabled
    if !disable_prefetch {
        tt.prefetch_l1(hash);
    }

    // Try TT probe
    if let Some(_entry) = tt.probe(hash) {
        // TT hit - we could use stored perft value if we stored it
    }

    for &mv in moves.iter() {
        // Legal move check is done in generate_all
        // if !mg.is_legal(pos, mv) {
        //     continue;
        // }

        let undo_info = pos.do_move(mv);
        nodes += perft(pos, depth - 1, tt, disable_prefetch);
        pos.undo_move(mv, undo_info);
    }

    // Store in TT (simplified - just marking position as visited)
    tt.store(hash, None, 0, 0, depth, NodeType::Exact);

    nodes
}

fn main() {
    let matches = Command::new("TT Prefetch Benchmark v5")
        .about("Benchmark TT prefetch with detailed metrics")
        .arg(
            Arg::new("sfen")
                .short('s')
                .long("sfen")
                .value_name("SFEN")
                .help("Position in SFEN format")
                .default_value("startpos"),
        )
        .arg(
            Arg::new("depth")
                .short('d')
                .long("depth")
                .value_name("DEPTH")
                .help("Search depth")
                .default_value("5"),
        )
        .arg(
            Arg::new("iterations")
                .short('i')
                .long("iterations")
                .value_name("ITERATIONS")
                .help("Number of iterations")
                .default_value("3"),
        )
        .arg(
            Arg::new("tt-size")
                .short('t')
                .long("tt-size")
                .value_name("MB")
                .help("Transposition table size in MB")
                .default_value("16"),
        )
        .arg(
            Arg::new("disable-prefetch")
                .long("disable-prefetch")
                .help("Disable TT prefetching")
                .action(clap::ArgAction::SetTrue),
        )
        .get_matches();

    let sfen = matches.get_one::<String>("sfen").unwrap();
    let depth: u8 = matches.get_one::<String>("depth").unwrap().parse().unwrap();
    let iterations: u32 = matches.get_one::<String>("iterations").unwrap().parse().unwrap();
    let tt_size_mb: usize = matches.get_one::<String>("tt-size").unwrap().parse().unwrap();
    let disable_prefetch = matches.get_flag("disable-prefetch");

    println!("=== TT Prefetch Benchmark v5 ===");
    println!("SFEN: {sfen}");
    println!("Depth: {depth}");
    println!("Iterations: {iterations}");
    println!("TT Size: {tt_size_mb} MB");
    println!(
        "Prefetch: {}",
        if disable_prefetch {
            "DISABLED"
        } else {
            "ENABLED"
        }
    );
    println!();

    let (total_duration, total_nodes) =
        benchmark_position(sfen, depth, iterations, tt_size_mb, disable_prefetch);

    println!("\n=== Summary ===");
    println!("Total nodes: {total_nodes}");
    println!("Total time: {total_duration:?}");
    println!("Average NPS: {:.0}", total_nodes as f64 / total_duration.as_secs_f64());
}
