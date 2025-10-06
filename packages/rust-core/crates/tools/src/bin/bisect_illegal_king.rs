use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use engine_core::evaluation::evaluate::MaterialEvaluator;
use engine_core::search::ab::{ClassicBackend, PruneToggles, SearchProfile};
use engine_core::search::api::SearcherBackend;
use engine_core::search::{SearchLimitsBuilder, TranspositionTable};
use engine_core::usi::{parse_usi_move, position_to_sfen};
use engine_core::Position;
use regex::Regex;
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Bisection helper to reproduce enemy-king capture crashes",
    disable_help_subcommand = true
)]
struct Args {
    /// Space separated move list (USI notation). Ignored if --moves-file is set.
    #[arg(long = "moves")]
    moves_inline: Option<String>,

    /// Path to file containing the move list (single line, space separated).
    #[arg(long = "moves-file")]
    moves_file: Option<PathBuf>,

    /// Starting SFEN (defaults to startpos).
    #[arg(long = "start-sfen")]
    start_sfen: Option<String>,

    /// Search depth.
    #[arg(long = "depth", default_value_t = 16)]
    depth: u8,

    /// Transposition-table size in MB (0 disables TT).
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

    /// Disable Static Beta pruning.
    #[arg(long = "disable-static-beta", default_value_t = false)]
    disable_static_beta: bool,

    /// Only test the first N moves (no bisection).
    #[arg(long = "prefix")]
    prefix_len: Option<usize>,

    /// Verbose logging.
    #[arg(short = 'v', long = "verbose", default_value_t = false)]
    verbose: bool,

    /// Regex of WARN tags that should be treated as failures.
    #[arg(
        long = "trigger-regex",
        default_value = "(?i)undo_(from|piece_type|king_.*)|king_capture_detected|nmp_roundtrip|iid_roundtrip|tt_(probe|store)_mismatch"
    )]
    trigger_regex: String,
}

fn main() -> Result<()> {
    let _ = env_logger::builder().is_test(false).try_init();

    let args = Args::parse();

    if env::var("SHOGI_PANIC_ON_KING_CAPTURE").is_err() {
        env::set_var("SHOGI_PANIC_ON_KING_CAPTURE", "0");
    }
    if env::var("DIAG_ABORT_ON_WARN").is_err() {
        env::set_var("DIAG_ABORT_ON_WARN", "1");
    }

    let trigger_regex = Regex::new(&args.trigger_regex)
        .with_context(|| format!("invalid trigger regex: {}", args.trigger_regex))?;

    let moves = load_moves(&args)?;
    if moves.is_empty() {
        return Err(anyhow!("move list is empty"));
    }

    let base_pos = if let Some(sfen) = &args.start_sfen {
        Position::from_sfen(sfen).map_err(|e| anyhow!("invalid start SFEN: {sfen}: {e}"))?
    } else {
        Position::startpos()
    };

    if let Some(prefix) = args.prefix_len {
        run_single(&args, &base_pos, &moves, prefix, &trigger_regex)?;
    } else {
        run_bisect(&args, &base_pos, &moves, &trigger_regex)?;
    }

    Ok(())
}

fn load_moves(args: &Args) -> Result<Vec<String>> {
    let raw = if let Some(path) = &args.moves_file {
        fs::read_to_string(path)
            .with_context(|| format!("failed to read moves file {}", path.display()))?
    } else if let Some(inline) = &args.moves_inline {
        inline.clone()
    } else {
        return Err(anyhow!("either --moves or --moves-file must be provided"));
    };

    Ok(raw
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect())
}

fn run_single(
    args: &Args,
    base: &Position,
    moves: &[String],
    prefix_len: usize,
    trigger_regex: &Regex,
) -> Result<()> {
    if prefix_len > moves.len() {
        return Err(anyhow!("prefix {} exceeds move list length {}", prefix_len, moves.len()));
    }
    let pos = apply_moves(base, &moves[..prefix_len])?;
    let outcome = probe_position(args, &pos)?;

    println!("prefix_len={prefix_len}");
    println!("sfen={}", position_to_sfen(&pos));
    println!("moves={}", moves[..prefix_len].join(" "));
    match outcome {
        ProbeOutcome::Ok => println!("result=ok"),
        ProbeOutcome::Warn(tag) => {
            let matched = trigger_regex.is_match(&tag);
            println!("result={}", if matched { "warn" } else { "warn-ignored" });
            println!("warn_tag={tag}");
            println!("warn_matched={matched}");
        }
        ProbeOutcome::Panic(msg) => {
            println!("result=panic");
            println!("panic_message={msg}");
        }
    }
    Ok(())
}

fn run_bisect(args: &Args, base: &Position, moves: &[String], trigger_regex: &Regex) -> Result<()> {
    let mut low = 0usize;
    let mut high = moves.len();
    let mut found: Option<usize> = None;

    while low <= high {
        let mid = (low + high) / 2;
        let pos = apply_moves(base, &moves[..mid])?;
        let outcome = probe_position(args, &pos)?;
        if args.verbose {
            eprintln!("checked prefix {mid}: {outcome:?}");
        }
        match outcome {
            ProbeOutcome::Warn(tag) => {
                if !trigger_regex.is_match(&tag) {
                    if mid == moves.len() {
                        break;
                    }
                    low = mid + 1;
                    continue;
                }
                found = Some(mid);
                if mid == 0 {
                    break;
                }
                high = mid.saturating_sub(1);
            }
            ProbeOutcome::Panic(_) => {
                found = Some(mid);
                if mid == 0 {
                    break;
                }
                high = mid.saturating_sub(1);
            }
            ProbeOutcome::Ok => {
                if mid == moves.len() {
                    break;
                }
                low = mid + 1;
            }
        }
    }

    if let Some(prefix) = found {
        let pos = apply_moves(base, &moves[..prefix])?;
        let outcome = probe_position(args, &pos)?;
        println!("panic_prefix_len={prefix}");
        println!("sfen={}", position_to_sfen(&pos));
        println!("moves={}", moves[..prefix].join(" "));
        match outcome {
            ProbeOutcome::Panic(msg) => {
                println!("panic_message={msg}");
            }
            ProbeOutcome::Warn(tag) => {
                let matched = trigger_regex.is_match(&tag);
                println!("warn_tag={tag}");
                println!("warn_matched={matched}");
            }
            ProbeOutcome::Ok => {}
        }
    } else {
        println!("panic_prefix_len=not-found");
    }
    Ok(())
}

#[derive(Debug)]
enum ProbeOutcome {
    Ok,
    Warn(String),
    Panic(String),
}

fn probe_position(args: &Args, pos: &Position) -> Result<ProbeOutcome> {
    let toggles = PruneToggles {
        enable_nmp: !args.disable_nmp,
        enable_iid: !args.disable_iid,
        enable_razor: !args.disable_razor,
        enable_probcut: !args.disable_probcut,
        enable_static_beta_pruning: !args.disable_static_beta,
    };

    let evaluator = Arc::new(MaterialEvaluator);
    let backend = if args.hash_mb == 0 {
        let mut profile = SearchProfile::enhanced_material();
        profile.prune = toggles;
        ClassicBackend::with_profile_apply_defaults(evaluator, profile)
    } else {
        let tt = Arc::new(TranspositionTable::new(args.hash_mb));
        ClassicBackend::with_tt_and_toggles_apply_defaults(evaluator, tt, toggles)
    };

    let limits = SearchLimitsBuilder::default().depth(args.depth).build();

    let result = catch_unwind(AssertUnwindSafe(|| backend.think_blocking(pos, &limits, None)));
    match result {
        Ok(_) => {
            #[cfg(any(debug_assertions, feature = "diagnostics"))]
            if let Some(tag) = engine_core::search::ab::diagnostics::last_fault_tag() {
                return Ok(ProbeOutcome::Warn(tag));
            }
            Ok(ProbeOutcome::Ok)
        }
        Err(panic) => Ok(ProbeOutcome::Panic(format_panic(panic))),
    }
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

fn format_panic(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}
