//! Select near-even representative opening positions and write an EPD (SFEN) book.
//!
//! Strategy:
//! - Read candidate SFEN lines from one or more input files (lines starting with `#` are ignored).
//! - Normalize and de-duplicate by first 4 SFEN tokens.
//! - Shuffle deterministically with optional seed.
//! - Evaluate each position with EnhancedNnue for fixed time per move (default 100ms).
//!   - Use single-PV root evaluation (cp) for near-evenness judgment.
//!   - Skip samples containing mate scores.
//!   - Accept positions where |root cp| <= threshold (default 75cp).
//! - Stop when `count` positions are collected and write to output as `sfen ...` lines.
//!
//! Example:
//!   cargo run -p tools --bin select_representative_100 -- \
//!     -i start_sfens_ply24.txt -i start_sfens_ply32.txt \
//!     -o docs/reports/fixtures/opening/representative_100.epd \
//!     --weights runs/nnue_local/nn_best.fp32.bin --count 100 --time-ms 100 --cp-threshold 75 --seed 42

use std::{
    collections::HashSet,
    fs,
    io::{BufRead, BufReader},
    path::PathBuf,
};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use engine_core::{
    engine::controller::{Engine, EngineType},
    search::limits::SearchLimitsBuilder,
    shogi::Position,
    time_management::TimeControl,
};
use rand::{rngs::StdRng, seq::SliceRandom, SeedableRng};
use std::panic::{catch_unwind, AssertUnwindSafe};

#[derive(Parser, Debug)]
#[command(
    name = "select_representative_100",
    about = "Select near-even 100 openings into an EPD (SFEN) book"
)]
struct Cli {
    /// Input files containing SFEN lines (supports lines starting with `sfen ` or raw SFEN)
    #[arg(short = 'i', long = "input", value_name = "FILE", required = true, num_args = 1..)]
    inputs: Vec<PathBuf>,

    /// Output EPD path (will overwrite)
    #[arg(short = 'o', long = "output", value_name = "FILE")]
    output: PathBuf,

    /// NNUE weights path (baseline to judge evenness)
    #[arg(long = "weights", value_name = "FILE")]
    weights: String,

    /// Target number of positions (default 100)
    #[arg(long = "count", default_value_t = 100)]
    count: usize,

    /// Time per sample (ms)
    #[arg(long = "time-ms", default_value_t = 100)]
    time_ms: u64,

    /// Absolute cp threshold to accept (|cp| <= threshold)
    #[arg(long = "cp-threshold", default_value_t = 75)]
    cp_threshold: i32,

    /// Optional RNG seed for deterministic shuffling
    #[arg(long = "seed")]
    seed: Option<u64>,
}

const MATE_CP_ABS_THRESHOLD: i32 = 30_000;

fn is_mate_cp(cp: i32) -> bool {
    cp.abs() >= MATE_CP_ABS_THRESHOLD
}

fn read_candidates(paths: &[PathBuf]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for p in paths {
        let f = fs::File::open(p).with_context(|| format!("failed to open {}", p.display()))?;
        let rd = BufReader::new(f);
        for line in rd.lines() {
            let t = line?;
            let s = t.trim();
            if s.is_empty() || s.starts_with('#') {
                continue;
            }
            if let Some(rest) = s.strip_prefix("sfen ") {
                out.push(rest.trim().to_string());
            } else {
                // Accept raw SFEN too
                out.push(s.to_string());
            }
        }
    }
    if out.is_empty() {
        return Err(anyhow!("no SFEN candidates from inputs"));
    }
    Ok(out)
}

fn unique_normalized(sfens: Vec<String>) -> Vec<String> {
    use tools::common::sfen::normalize_4t;
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for s in sfens {
        if let Some(k) = normalize_4t(&s) {
            if seen.insert(k) {
                out.push(s);
            }
        }
    }
    out
}

fn eval_root_cp(eng: &mut Engine, pos: &mut Position, per_ms: u64) -> i32 {
    eng.set_multipv_persistent(1);
    let limits = SearchLimitsBuilder::default()
        .time_control(TimeControl::FixedTime {
            ms_per_move: per_ms,
        })
        .build();
    let res = eng.search(pos, limits);
    res.score
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Load & prepare candidates
    let mut cand = read_candidates(&cli.inputs)?;
    cand = unique_normalized(cand);
    if let Some(seed) = cli.seed {
        let mut rng = StdRng::seed_from_u64(seed);
        cand.as_mut_slice().shuffle(&mut rng);
    }

    // Init engine
    engine_core::init::init_all_tables_once();
    let mut eng = Engine::new(EngineType::EnhancedNnue);
    eng.set_threads(1);
    eng.set_hash_size(256);
    eng.set_multipv_persistent(1);
    eng.load_nnue_weights(&cli.weights)
        .map_err(|e| anyhow!("failed to load NNUE weights: {e}"))?;

    // Select near-even positions
    let mut selected: Vec<String> = Vec::with_capacity(cli.count);
    for s in cand.iter() {
        if selected.len() >= cli.count {
            break;
        }
        let mut pos = Position::from_sfen(s).map_err(|e| anyhow!(e))?;
        let root_cp = match catch_unwind(AssertUnwindSafe(|| {
            eval_root_cp(&mut eng, &mut pos, cli.time_ms)
        })) {
            Ok(cp) => cp,
            Err(_) => continue,
        };
        if is_mate_cp(root_cp) {
            continue;
        }
        if root_cp.abs() <= cli.cp_threshold {
            selected.push(s.clone());
        }
    }

    // Write output
    if let Some(dir) = cli.output.parent() {
        fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    }
    let mut out = String::new();
    out.push_str("# Representative opening book (auto-selected near-even positions)\n");
    out.push_str(&format!(
        "# count={} time_ms={} cp_threshold={} seed={:?}\n",
        selected.len(),
        cli.time_ms,
        cli.cp_threshold,
        cli.seed
    ));
    for s in &selected {
        out.push_str("sfen ");
        out.push_str(s);
        out.push('\n');
    }
    fs::write(&cli.output, out)
        .with_context(|| format!("failed to write {}", cli.output.display()))?;

    if selected.len() < cli.count {
        eprintln!(
            "WARN: only {} positions selected (requested {}) — consider relaxing threshold or adding inputs",
            selected.len(), cli.count
        );
    }

    println!("Wrote {} positions to {}", selected.len(), cli.output.display());
    Ok(())
}
