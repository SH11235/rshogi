use anyhow::Result;
use clap::Parser;
use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimitsBuilder;
use engine_core::search::SearchLimits;
use engine_core::shogi::Position;
use engine_core::usi::{create_position, move_to_usi};
use std::io::{self, BufRead, Write};

/// Minimal USI-style harness for reproducing a single position and bestmove.
///
/// - Fixed single-threaded search
/// - Deterministic (no randomness)
/// - Focused on the 6f6d problem position from PLANS.md
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Minimal USI harness for single-position repro"
)]
struct Cli {
    /// If set, run the built-in 6f6d repro position and exit.
    #[arg(long)]
    repro_6f6d: bool,

    /// Optional fixed depth (default: 11)
    #[arg(long, default_value_t = 11)]
    depth: u8,
}

fn build_limits(depth: u8) -> SearchLimits {
    SearchLimitsBuilder::default().depth(depth).build()
}

fn run_single_position(pos: &Position, limits: SearchLimits) -> Result<String> {
    let mut pos_clone = pos.clone();
    let mut eng = Engine::new(EngineType::Enhanced);
    let res = eng.search(&mut pos_clone, limits);
    let final_best = eng.choose_final_bestmove(pos, None);
    let best = final_best
        .best_move
        .or(res.best_move)
        .ok_or_else(|| anyhow::anyhow!("no bestmove found (no legal moves or search failure)"))?;

    Ok(move_to_usi(&best))
}

fn run_repro_6f6d(depth: u8) -> Result<()> {
    // PLANS.md の問題局面（6f6d直前）手順
    const MOVES_BEFORE_6F6D: &[&str] = &[
        "3g3f", "3c3d", "2h1h", "4a3b", "1h3h", "8c8d", "3f3e", "3d3e", "3h3e", "8d8e", "6i7h",
        "7a7b", "3e3f", "5a4a", "3f5f", "3a4b", "4i4h", "4a3a", "5f3f", "4b3c", "3i2h", "4c4d",
        "5i6h", "7b8c", "3f6f", "6a5b", "6f3f", "7c7d", "6h5i", "7d7e", "7g7f", "7e7f", "8h5e",
        "8b9b", "3f7f", "P*7d", "8i7g", "5b4c", "7f6f", "8c7b", "7g8e", "7d7e",
    ];

    let mut pos = Position::startpos();
    for usi in MOVES_BEFORE_6F6D {
        let mv = engine_core::usi::parse_usi_move(usi)?;
        if !pos.is_legal_move(mv) {
            anyhow::bail!("illegal move in sequence: {}", usi);
        }
        pos.do_move(mv);
    }

    let limits = build_limits(depth);
    let best = run_single_position(&pos, limits)?;
    println!("repro_6f6d bestmove {}", best);
    Ok(())
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    if cli.repro_6f6d {
        return run_repro_6f6d(cli.depth);
    }

    // Fallback: very small USI loop (subset) for manual experiments.
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    let mut pos = Position::startpos();

    for line in stdin.lock().lines() {
        let line = line?;
        let cmd = line.trim();
        if cmd.is_empty() {
            continue;
        }
        if cmd == "quit" {
            break;
        }
        if cmd == "usi" {
            writeln!(stdout, "id name RustCore USI Harness")?;
            writeln!(stdout, "id author tools")?;
            writeln!(stdout, "usiok")?;
            stdout.flush()?;
            continue;
        }
        if cmd == "isready" {
            writeln!(stdout, "readyok")?;
            stdout.flush()?;
            continue;
        }
        if cmd.starts_with("position ") {
            // Use engine-core helper to build the position.
            let mut from_startpos = false;
            let mut sfen: Option<String> = None;
            let mut moves: Vec<String> = Vec::new();

            let mut it = cmd.split_whitespace().skip(1).peekable();
            while let Some(tok) = it.peek().cloned() {
                match tok {
                    "startpos" => {
                        let _ = it.next();
                        from_startpos = true;
                        sfen = None;
                    }
                    "sfen" => {
                        let _ = it.next();
                        let mut parts = Vec::new();
                        while let Some(t) = it.peek() {
                            if *t == "moves" {
                                break;
                            }
                            parts.push(it.next().unwrap().to_string());
                        }
                        sfen = Some(parts.join(" "));
                    }
                    "moves" => {
                        let _ = it.next();
                        for m in it.by_ref() {
                            moves.push(m.to_string());
                        }
                    }
                    _ => {
                        let _ = it.next();
                    }
                }
            }

            pos = create_position(from_startpos, sfen.as_deref(), &moves)?;
            continue;
        }
        if cmd.starts_with("go") {
            let limits = build_limits(cli.depth);
            let best = run_single_position(&pos, limits)?;
            writeln!(stdout, "bestmove {}", best)?;
            stdout.flush()?;
            continue;
        }
    }

    Ok(())
}
