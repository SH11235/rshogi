use anyhow::{anyhow, Result};
use clap::Parser;
use engine_core::evaluation::nnue::single_state::SingleAcc;
use engine_core::evaluation::nnue::weights::load_single_weights;
use engine_core::movegen::MoveGenerator;
use engine_core::shogi::{Move, Position};
use std::time::{Duration, Instant};

#[derive(Parser, Debug)]
#[command(about = "NNUE Single (diff vs refresh) micro-benchmark", version)]
struct Args {
    /// Path to SINGLE_CHANNEL weights (trainer export with END_HEADER)
    #[arg(long, value_name = "FILE")]
    single_weights: String,

    /// Seconds to run for each benchmark section
    #[arg(long, default_value_t = 3)]
    seconds: u64,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let net = load_single_weights(&args.single_weights)
        .map_err(|e| anyhow!("failed to load SINGLE weights: {e}"))?;

    // Build start position and a small suite
    let mut positions = Vec::new();
    positions.push(Position::startpos());
    // A light endgame position: just kings to keep movegen cheap
    {
        let mut p = Position::empty();
        use engine_core::usi::parse_usi_square;
        use engine_core::{Color, Piece, PieceType};
        p.board
            .put_piece(parse_usi_square("5i").unwrap(), Piece::new(PieceType::King, Color::Black));
        p.board
            .put_piece(parse_usi_square("5a").unwrap(), Piece::new(PieceType::King, Color::White));
        positions.push(p);
    }

    // Measure refresh throughput
    let refresh_target = Duration::from_secs(args.seconds);
    let mut refresh_iters: u64 = 0;
    let start = Instant::now();
    while start.elapsed() < refresh_target {
        for pos in &positions {
            let acc = SingleAcc::refresh(pos, &net);
            let _s = net.evaluate_from_accumulator(acc.acc_for(pos.side_to_move));
            refresh_iters += 1;
            if start.elapsed() >= refresh_target {
                break;
            }
        }
    }
    let refresh_eps = (refresh_iters as f64 / start.elapsed().as_secs_f64()) as u64;

    // Prepare a few legal moves per position
    let mut suites: Vec<(Position, Vec<Move>)> = Vec::new();
    for p in positions.clone() {
        let gen = MoveGenerator::new();
        let moves = gen
            .generate_all(&p)
            .unwrap_or_default()
            .into_iter()
            .take(32)
            .collect::<Vec<_>>();
        suites.push((p.clone(), moves));
    }

    // Measure incremental throughput (apply_update only; keep pos/acc in sync by using a fixed base)
    let inc_target = Duration::from_secs(args.seconds);
    let start2 = Instant::now();
    let mut inc_iters: u64 = 0;
    'outer: loop {
        for (p, moves) in suites.iter_mut() {
            if moves.is_empty() {
                continue;
            }
            let acc0 = SingleAcc::refresh(p, &net);
            for &mv in moves.iter() {
                let next = SingleAcc::apply_update(&acc0, p, mv, &net);
                let _s = net.evaluate_from_accumulator(next.acc_for(p.side_to_move));
                inc_iters += 1;
                if start2.elapsed() >= inc_target {
                    break 'outer;
                }
            }
        }
        if start2.elapsed() >= inc_target {
            break;
        }
    }
    let inc_eps = (inc_iters as f64 / start2.elapsed().as_secs_f64()) as u64;

    // Measure chained incremental throughput (apply_update + advance position)
    let chain_target = Duration::from_secs(args.seconds);
    let start3 = Instant::now();
    let mut chain_iters: u64 = 0;
    'outer_chain: loop {
        for (p, _moves) in suites.iter() {
            let mut p_chain = p.clone();
            let mut acc = SingleAcc::refresh(&p_chain, &net);
            let gen = MoveGenerator::new();
            loop {
                let moves = gen.generate_all(&p_chain).unwrap_or_default();
                if moves.is_empty() {
                    break;
                }
                // Take a few steps along the current frontier
                for mv in moves.into_iter().take(8) {
                    let next = SingleAcc::apply_update(&acc, &p_chain, mv, &net);
                    let _u = p_chain.do_move(mv);
                    let _s = net.evaluate_from_accumulator(next.acc_for(p_chain.side_to_move));
                    acc = next;
                    chain_iters += 1;
                    if start3.elapsed() >= chain_target {
                        break 'outer_chain;
                    }
                }
            }
            if start3.elapsed() >= chain_target {
                break 'outer_chain;
            }
        }
        if start3.elapsed() >= chain_target {
            break;
        }
    }
    let chain_eps = (chain_iters as f64 / start3.elapsed().as_secs_f64()) as u64;

    println!("=== NNUE Single Benchmark ===");
    println!("Weights: {}", args.single_weights);
    println!("Refresh-only: {} evals/sec", refresh_eps);
    println!("Incremental: {} evals/sec", inc_eps);
    println!("Incremental-Chain: {} evals/sec", chain_eps);
    if refresh_eps > 0 {
        println!("Speedup (ApplyOnce): {:.2}x", inc_eps as f64 / refresh_eps as f64);
        println!("Speedup (Chain): {:.2}x", chain_eps as f64 / refresh_eps as f64);
    }

    Ok(())
}
