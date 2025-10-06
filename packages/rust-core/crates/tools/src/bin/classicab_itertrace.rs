use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use engine_core::evaluation::evaluate::MaterialEvaluator;
use engine_core::search::ab::{ClassicBackend, PruneToggles, SearchProfile};
use engine_core::search::api::SearcherBackend;
use engine_core::search::{SearchLimitsBuilder, TranspositionTable};
use engine_core::usi::{move_to_usi, parse_usi_move, position_to_sfen};
use engine_core::Position;

#[derive(Parser, Debug)]
#[command(
    name = "classicab_itertrace",
    about = "Run ClassicAB search directly against a single SFEN to reproduce illegal king capture",
    disable_help_subcommand = true
)]
struct Args {
    /// Space separated move list (USI). Applied after the starting SFEN.
    #[arg(long = "moves")]
    moves_inline: Option<String>,

    /// File containing a single line of space separated USI moves.
    #[arg(long = "moves-file")]
    moves_file: Option<PathBuf>,

    /// Starting SFEN. Defaults to startpos when omitted.
    #[arg(long = "start-sfen")]
    start_sfen: Option<String>,

    /// Maximum depth for the iterative deepening search.
    #[arg(long = "depth", default_value_t = 16)]
    depth: u8,

    /// Transposition-table size in MB (0 disables TT usage).
    #[arg(long = "hash-mb", default_value_t = 64)]
    hash_mb: usize,

    /// Disable Null-Move pruning.
    #[arg(long = "disable-nmp", default_value_t = false)]
    disable_nmp: bool,

    /// Disable IID.
    #[arg(long = "disable-iid", default_value_t = false)]
    disable_iid: bool,

    /// Disable ProbCut.
    #[arg(long = "disable-probcut", default_value_t = false)]
    disable_probcut: bool,

    /// Disable Razor pruning.
    #[arg(long = "disable-razor", default_value_t = false)]
    disable_razor: bool,

    /// Disable static beta pruning.
    #[arg(long = "disable-static-beta", default_value_t = false)]
    disable_static_beta: bool,

    /// Number of repeated runs (TT is cleared between runs unless --retain-tt is set).
    #[arg(long = "repeat", default_value_t = 1)]
    repeat: u32,

    /// Retain TT contents between repeated runs.
    #[arg(long = "retain-tt", default_value_t = false)]
    retain_tt: bool,

    /// Emit the final SFEN after moves are applied.
    #[arg(long = "print-final-sfen", default_value_t = false)]
    print_final_sfen: bool,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let moves = load_moves(&args)?;
    let base_pos = if let Some(sfen) = &args.start_sfen {
        Position::from_sfen(sfen).map_err(|e| anyhow!("invalid start SFEN: {sfen}: {e}"))?
    } else {
        Position::startpos()
    };
    let pos = apply_moves(&base_pos, &moves)?;

    if args.print_final_sfen {
        println!("final_sfen={}", position_to_sfen(&pos));
    }

    let toggles = PruneToggles {
        enable_nmp: !args.disable_nmp,
        enable_iid: !args.disable_iid,
        enable_razor: !args.disable_razor,
        enable_probcut: !args.disable_probcut,
        enable_static_beta_pruning: !args.disable_static_beta,
    };

    let evaluator = Arc::new(MaterialEvaluator);
    let shared_tt = if args.hash_mb == 0 || !args.retain_tt {
        None
    } else {
        Some(Arc::new(TranspositionTable::new(args.hash_mb)))
    };
    let limits = SearchLimitsBuilder::default().depth(args.depth).build();

    for run_idx in 0..args.repeat {
        if run_idx > 0 {
            println!("--- run {} ---", run_idx + 1);
        }
        let backend = if args.hash_mb == 0 {
            let mut profile = SearchProfile::enhanced_material();
            profile.prune = toggles;
            ClassicBackend::with_profile_apply_defaults(Arc::clone(&evaluator), profile)
        } else {
            let tt = if let Some(tt) = &shared_tt {
                Arc::clone(tt)
            } else {
                Arc::new(TranspositionTable::new(args.hash_mb))
            };
            ClassicBackend::with_tt_and_toggles_apply_defaults(Arc::clone(&evaluator), tt, toggles)
        };
        println!(
            "starting_search depth={} hash_mb={} nmp={} iid={} probcut={} razor={} sbp={}",
            args.depth,
            args.hash_mb,
            !args.disable_nmp,
            !args.disable_iid,
            !args.disable_probcut,
            !args.disable_razor,
            !args.disable_static_beta
        );
        let result = backend.think_blocking(&pos, &limits, None);
        let nodes = result.nodes;
        let seldepth = result.seldepth;
        let score = result.score;
        let reason = format!("{:?}", result.end_reason);
        let best = result.best_move.map(|mv| move_to_usi(&mv)).unwrap_or_else(|| "-".to_string());
        println!(
            "search_completed nodes={} seldepth={} score={} reason={} best_move={}",
            nodes, seldepth, score, reason, best
        );
    }

    Ok(())
}

fn load_moves(args: &Args) -> Result<Vec<String>> {
    if let Some(path) = &args.moves_file {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read moves file {}", path.display()))?;
        return Ok(parse_moves_string(&raw));
    }
    if let Some(inline) = &args.moves_inline {
        return Ok(parse_moves_string(inline));
    }
    Ok(Vec::new())
}

fn parse_moves_string(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn apply_moves(base: &Position, moves: &[String]) -> Result<Position> {
    let mut pos = base.clone();
    for (idx, mv_str) in moves.iter().enumerate() {
        let mv = parse_usi_move(mv_str)
            .with_context(|| format!("failed to parse move '{mv_str}' at ply {}", idx))?;
        if !pos.is_legal_move(mv) {
            return Err(anyhow!(
                "illegal move '{mv_str}' at ply {} (sfen={})",
                idx,
                position_to_sfen(&pos)
            ));
        }
        pos.do_move(mv);
    }
    Ok(pos)
}
