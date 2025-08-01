use clap::Parser;
use engine_core::ai::{search_options::TimeControl, shared::create_fixed_searcher};
use engine_core::engine::create_usi_engine;
use engine_core::position::Position;
use engine_core::search::SearchOptions;
use engine_core::types::{EngineType, Move};
use engine_core::usi::{move_to_usi, parse_sfen};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "debug_position")]
#[command(about = "Debug tool for analyzing specific shogi positions")]
struct Args {
    /// SFEN string of the position to analyze
    #[arg(short, long)]
    sfen: Option<String>,

    /// Maximum search depth (default: 5)
    #[arg(short, long, default_value = "5")]
    depth: u8,

    /// Time limit per search in milliseconds (default: 1000)
    #[arg(short, long, default_value = "1000")]
    time: u64,

    /// Engine type to use (material, nnue, enhanced, enhanced_nnue)
    #[arg(short, long, default_value = "material")]
    engine: String,

    /// Show detailed move ordering information
    #[arg(short = 'o', long)]
    show_ordering: bool,

    /// Show transposition table statistics
    #[arg(short = 't', long)]
    show_tt_stats: bool,

    /// Run perft analysis instead of search
    #[arg(short, long)]
    perft: Option<u8>,

    /// Show all legal moves for the position
    #[arg(short, long)]
    moves: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Default to initial position if no SFEN provided
    let sfen = args
        .sfen
        .as_deref()
        .unwrap_or("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1");

    println!("Analyzing position: {}", sfen);

    let position = parse_sfen(sfen)?;

    // Show legal moves if requested
    if args.moves {
        println!("\nLegal moves:");
        let moves = position.generate_legal_moves();
        for (i, mv) in moves.iter().enumerate() {
            println!("  {}: {}", i + 1, move_to_usi(mv));
        }
        println!("Total: {} moves", moves.len());
        if !args.perft.is_some() {
            return Ok(());
        }
    }

    // Run perft if requested
    if let Some(depth) = args.perft {
        println!("\nRunning perft to depth {}...", depth);
        let start = Instant::now();
        let nodes = perft(&position, depth);
        let elapsed = start.elapsed();
        println!("Perft({}) = {} nodes", depth, nodes);
        println!("Time: {:.3}s", elapsed.as_secs_f64());
        println!("NPS: {:.0}", nodes as f64 / elapsed.as_secs_f64());
        return Ok(());
    }

    // Parse engine type
    let engine_type = match args.engine.to_lowercase().as_str() {
        "material" => EngineType::Material,
        "nnue" => EngineType::Nnue,
        "enhanced" => EngineType::Enhanced,
        "enhanced_nnue" => EngineType::EnhancedNnue,
        _ => {
            eprintln!("Invalid engine type: {}. Using Material.", args.engine);
            EngineType::Material
        }
    };

    println!("Using engine type: {:?}", engine_type);
    println!("Max depth: {}", args.depth);
    println!("Time limit: {}ms", args.time);

    // Create search options
    let mut options = SearchOptions::default();
    options.time_control = TimeControl::TimeLimit(Duration::from_millis(args.time));
    options.max_depth = Some(args.depth);

    // Create engine
    let mut engine = create_usi_engine(engine_type);
    engine.set_position(position.clone());

    println!("\nStarting search...");
    let start = Instant::now();

    // Perform search
    let result = engine.search(options);

    let elapsed = start.elapsed();

    println!("\n=== Search Results ===");
    println!("Best move: {}", move_to_usi(&result.best_move));
    println!("Score: {}", result.score);
    println!("Depth reached: {}", result.depth);
    println!("Nodes searched: {}", result.nodes);
    println!("Time: {:.3}s", elapsed.as_secs_f64());
    println!("NPS: {:.0}", result.nodes as f64 / elapsed.as_secs_f64());

    if let Some(pv) = result.pv {
        println!("PV: {}", pv.iter().map(move_to_usi).collect::<Vec<_>>().join(" "));
    }

    // Show TT stats if requested
    if args.show_tt_stats {
        println!("\n=== Transposition Table Stats ===");
        // This would require exposing TT stats from the engine
        println!("(TT statistics not yet implemented)");
    }

    // Show move ordering info if requested
    if args.show_ordering {
        println!("\n=== Move Ordering Analysis ===");
        // Generate and order moves to show ordering
        let moves = position.generate_legal_moves();
        println!("Total moves: {}", moves.len());
        // This would require exposing move ordering logic
        println!("(Detailed move ordering not yet implemented)");
    }

    Ok(())
}

fn perft(position: &Position, depth: u8) -> u64 {
    if depth == 0 {
        return 1;
    }

    let moves = position.generate_legal_moves();
    let mut nodes = 0;

    for mv in moves {
        let mut new_position = position.clone();
        if new_position.make_move(&mv).is_ok() {
            nodes += perft(&new_position, depth - 1);
        }
    }

    nodes
}
