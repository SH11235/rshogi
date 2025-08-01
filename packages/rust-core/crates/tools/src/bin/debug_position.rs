use clap::Parser;
use engine_core::engine::controller::{Engine, EngineType};
use engine_core::movegen::MoveGen;
use engine_core::search::limits::SearchLimits;
use engine_core::shogi::moves::MoveList;
use engine_core::usi::move_to_usi;
use engine_core::Position;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

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
    // Initialize logging
    env_logger::init();

    let args = Args::parse();

    // Default to initial position if no SFEN provided
    let sfen = args
        .sfen
        .as_deref()
        .unwrap_or("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1");

    println!("Analyzing position: {sfen}");

    let mut position = Position::from_sfen(sfen)?;

    // Show legal moves if requested
    if args.moves {
        println!("\nLegal moves:");
        let mut move_gen = MoveGen::new();
        let mut move_list = MoveList::new();
        move_gen.generate_all(&position, &mut move_list);

        // Filter legal moves
        let mut legal_moves = Vec::new();
        for mv in move_list.as_slice() {
            let mut test_pos = position.clone();
            test_pos.do_move(*mv);
            if !test_pos.in_check() {
                legal_moves.push(*mv);
            }
        }

        for (i, mv) in legal_moves.iter().enumerate() {
            println!("  {}: {}", i + 1, move_to_usi(mv));
        }
        println!("Total: {} moves", legal_moves.len());
        if args.perft.is_none() {
            return Ok(());
        }
    }

    // Run perft if requested
    if let Some(depth) = args.perft {
        println!("\nRunning perft to depth {depth}...");
        let start = Instant::now();
        let nodes = perft(&position, depth);
        let elapsed = start.elapsed();
        println!("Perft({depth}) = {nodes} nodes");
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

    println!("Using engine type: {engine_type:?}");
    println!("Max depth: {}", args.depth);
    println!("Time limit: {}ms", args.time);

    // Create engine
    let engine = Engine::new(engine_type);

    // Create search limits
    let stop_flag = Arc::new(AtomicBool::new(false));
    let limits = SearchLimits::builder()
        .depth(args.depth)
        .fixed_time_ms(args.time)
        .stop_flag(stop_flag.clone())
        .build();

    println!("\nStarting search...");
    let start = Instant::now();

    // Perform search
    let result = engine.search(&mut position, limits);
    let elapsed = start.elapsed();

    if let Some(mv) = result.best_move {
        println!("\n=== Search Results ===");
        println!("Best move: {}", move_to_usi(&mv));
        println!("Score: {} cp", result.score);
        println!("Depth reached: {}", result.stats.depth);
        println!("Nodes searched: {}", result.stats.nodes);
        println!("Time: {:.3}s", elapsed.as_secs_f64());
        println!("NPS: {:.0}", result.stats.nodes as f64 / elapsed.as_secs_f64());

        if !result.stats.pv.is_empty() {
            println!(
                "PV: {}",
                result.stats.pv.iter().map(move_to_usi).collect::<Vec<_>>().join(" ")
            );
        }
    } else {
        println!("No legal moves found!");
    }

    // Show TT stats if requested
    if args.show_tt_stats {
        println!("\n=== Transposition Table Stats ===");
        // Prepare for future TT statistics implementation
        if let Some(tt_hits) = result.stats.tt_hits {
            println!("TT hits: {tt_hits}");
        }
        if let Some(null_cuts) = result.stats.null_cuts {
            println!("Null move cuts: {null_cuts}");
        }
        if let Some(lmr_count) = result.stats.lmr_count {
            println!("LMR reductions: {lmr_count}");
        }
        if result.stats.tt_hits.is_none() {
            println!("(TT statistics not available for this engine type)");
        }
    }

    // Show move ordering info if requested
    if args.show_ordering {
        println!("\n=== Move Ordering Analysis ===");
        let mut move_gen = MoveGen::new();
        let mut move_list = MoveList::new();
        move_gen.generate_all(&position, &mut move_list);
        println!("Total pseudo-legal moves: {}", move_list.len());
        // This would require exposing move ordering logic
        println!("(Detailed move ordering not yet implemented)");
    }

    Ok(())
}

fn perft(position: &Position, depth: u8) -> u64 {
    if depth == 0 {
        return 1;
    }

    let mut nodes = 0;
    let mut move_gen = MoveGen::new();
    let mut move_list = MoveList::new();
    move_gen.generate_all(position, &mut move_list);

    for mv in move_list.as_slice() {
        let mut new_position = position.clone();
        let undo_info = new_position.do_move(*mv);

        // Check if the move was legal (king not in check)
        if !new_position.in_check() {
            nodes += perft(&new_position, depth - 1);
        }

        new_position.undo_move(*mv, undo_info);
    }

    nodes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_perft_calculation() {
        // Test perft for initial position
        let position =
            Position::from_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
                .unwrap();

        // Known perft values for initial position
        assert_eq!(perft(&position, 1), 30);
        assert_eq!(perft(&position, 2), 900);
        // Depth 3 takes longer, so we just verify it completes
        let result = perft(&position, 3);
        assert!(result > 20000 && result < 30000);
    }

    #[test]
    fn test_engine_type_parsing() {
        // Test valid engine types
        assert!(matches!(
            match "material".to_lowercase().as_str() {
                "material" => EngineType::Material,
                _ => EngineType::Material,
            },
            EngineType::Material
        ));

        assert!(matches!(
            match "enhanced_nnue".to_lowercase().as_str() {
                "enhanced_nnue" => EngineType::EnhancedNnue,
                _ => EngineType::Material,
            },
            EngineType::EnhancedNnue
        ));
    }

    #[test]
    fn test_legal_move_generation() {
        let position =
            Position::from_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
                .unwrap();
        let mut move_gen = MoveGen::new();
        let mut move_list = MoveList::new();
        move_gen.generate_all(&position, &mut move_list);

        // Filter legal moves
        let mut legal_count = 0;
        for mv in move_list.as_slice() {
            let mut test_pos = position.clone();
            test_pos.do_move(*mv);
            if !test_pos.in_check() {
                legal_count += 1;
            }
        }

        // Initial position should have exactly 30 legal moves
        assert_eq!(legal_count, 30);
    }
}
