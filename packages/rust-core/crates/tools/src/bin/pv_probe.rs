use anyhow::{anyhow, Result};
use clap::{ArgGroup, Parser};
use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimitsBuilder;
use engine_core::shogi::Position;
use engine_core::time_management::TimeControl;
use rand::seq::SliceRandom;
use rand::{rngs::StdRng, SeedableRng};
use serde::Serialize;
use std::fs;

const MATE_CP_ABS_THRESHOLD: i32 = 30_000;

#[derive(Parser, Debug)]
#[command(
    name = "pv-probe",
    about = "Probe PV spread (MultiPV=3) at fixed time per root position",
    version
)]
#[command(group(
    ArgGroup::new("out")
        .args(["json", "report"]) // optional
        .required(false),
))]
struct Cli {
    /// Candidate NNUE weights (Single v2 or Classic v1)
    #[arg(long)]
    cand: String,
    /// Book file with SFEN lines (either raw SFEN or lines prefixed with 'sfen ')
    #[arg(long)]
    book: String,
    /// Fixed time per PV sample (ms). Default 1500. Ignored if --depth is set.
    #[arg(long, default_value_t = 1500)]
    ms: u64,
    /// Optional fixed depth per PV sample (takes precedence over --ms)
    #[arg(long)]
    depth: Option<u8>,
    /// Threads per engine (Spec recommends 1)
    #[arg(long, default_value_t = 1)]
    threads: usize,
    /// Hash size in MB
    #[arg(long = "hash-mb", default_value_t = 256)]
    hash_mb: usize,
    /// Number of samples to collect (max)
    #[arg(long, default_value_t = 100)]
    samples: usize,
    /// Optional shuffle seed
    #[arg(long)]
    seed: Option<u64>,
    /// JSON output path (optional; '-' for STDOUT)
    #[arg(long)]
    json: Option<String>,
    /// Markdown report path (optional; '-' for STDOUT)
    #[arg(long)]
    report: Option<String>,
}

#[derive(Serialize, Clone)]
struct Summary {
    samples: usize,
    p50_cp: f64,
    p90_cp: f64,
    max_cp: f64,
}

#[derive(Serialize)]
struct OutJson {
    cand: String,
    book: String,
    ms: u64,
    threads: usize,
    hash_mb: usize,
    stats: Summary,
}

fn load_book(path: &str) -> Result<Vec<String>> {
    let text = fs::read_to_string(path).map_err(|e| anyhow!("read book {path}: {e}"))?;
    let mut v = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix("sfen ") {
            v.push(rest.trim().to_string());
        } else {
            // assume raw SFEN
            v.push(t.to_string());
        }
    }
    if v.is_empty() {
        return Err(anyhow!("book has no SFEN lines"));
    }
    Ok(v)
}

struct Runner {
    eng: Engine,
}
impl Runner {
    fn new(weights: &str, threads: usize, hash_mb: usize) -> Result<Self> {
        engine_core::init::init_all_tables_once();
        let mut eng = Engine::new(EngineType::EnhancedNnue);
        eng.set_threads(threads);
        eng.set_hash_size(hash_mb);
        eng.set_multipv_persistent(1);
        eng.load_nnue_weights(weights).map_err(|e| anyhow!("load NNUE: {}", e))?;
        Ok(Self { eng })
    }
    fn probe_pv3_cp_time(&mut self, sfen: &str, ms: u64) -> Option<[i32; 3]> {
        self.eng.set_multipv_persistent(3);
        let limits = SearchLimitsBuilder::default()
            .time_control(TimeControl::FixedTime { ms_per_move: ms })
            .multipv(3)
            .build();
        let mut pos = Position::from_sfen(sfen).ok()?;
        let res = self.eng.search(&mut pos, limits);
        self.eng.set_multipv_persistent(1);
        if let Some(lines) = res.lines {
            if lines.len() < 3 {
                return None;
            }
            let a = lines[0].score_cp;
            let b = lines[1].score_cp;
            let c = lines[2].score_cp;
            Some([a, b, c])
        } else {
            None
        }
    }
    fn probe_pv3_cp_depth(&mut self, sfen: &str, depth: u8) -> Option<[i32; 3]> {
        self.eng.set_multipv_persistent(3);
        let limits = SearchLimitsBuilder::default().depth(depth).multipv(3).build();
        let mut pos = Position::from_sfen(sfen).ok()?;
        let res = self.eng.search(&mut pos, limits);
        self.eng.set_multipv_persistent(1);
        if let Some(lines) = res.lines {
            if lines.len() < 3 {
                return None;
            }
            let a = lines[0].score_cp;
            let b = lines[1].score_cp;
            let c = lines[2].score_cp;
            Some([a, b, c])
        } else {
            None
        }
    }
}

fn percentile(mut xs: Vec<f64>, q: f64) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let idx = ((xs.len() as f64 - 1.0) * q).round() as usize;
    xs[idx]
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut book = load_book(&cli.book)?;
    // shuffle and take N samples
    if let Some(seed) = cli.seed {
        book.shuffle(&mut StdRng::seed_from_u64(seed));
    }
    if book.len() > cli.samples {
        book.truncate(cli.samples);
    }

    let mut r = Runner::new(&cli.cand, cli.threads, cli.hash_mb)?;
    let mut spreads: Vec<f64> = Vec::new();
    for s in &book {
        let triple = if let Some(d) = cli.depth {
            r.probe_pv3_cp_depth(s, d)
        } else {
            r.probe_pv3_cp_time(s, cli.ms)
        };
        if let Some([a, b, c]) = triple {
            // skip mates for stability
            if [a, b, c].iter().any(|&cp| cp.abs() >= MATE_CP_ABS_THRESHOLD) {
                continue;
            }
            let min = a.min(b).min(c);
            let max = a.max(b).max(c);
            spreads.push((max - min) as f64);
        }
    }
    let stats = Summary {
        samples: spreads.len(),
        p50_cp: percentile(spreads.clone(), 0.50),
        p90_cp: percentile(spreads.clone(), 0.90),
        max_cp: spreads.iter().cloned().fold(0.0, f64::max),
    };

    // JSON out
    if let Some(j) = &cli.json {
        let out = OutJson {
            cand: cli.cand.clone(),
            book: cli.book.clone(),
            ms: cli.ms,
            threads: cli.threads,
            hash_mb: cli.hash_mb,
            stats: stats.clone(),
        };
        let s = serde_json::to_string_pretty(&out)?;
        if j == "-" {
            println!("{}", s);
        } else {
            fs::write(j, s)?;
        }
    }
    // Markdown out
    if let Some(m) = &cli.report {
        let md = format!(
            "# PV Probe\n\n- cand: {}\n- book: {}\n- ms: {}\n- threads: {}\n- hash_mb: {}\n\n## Summary\n- samples: {}\n- p50 spread: {:.0} cp\n- p90 spread: {:.0} cp\n- max spread: {:.0} cp\n",
            cli.cand, cli.book, cli.ms, cli.threads, cli.hash_mb,
            stats.samples, stats.p50_cp, stats.p90_cp, stats.max_cp
        );
        if m == "-" {
            println!("{}", md);
        } else {
            fs::write(m, md)?;
        }
    }
    // If no outputs requested, print short line
    if cli.json.is_none() && cli.report.is_none() {
        eprintln!("pv_probe: samples={} p90={:.0}cp", stats.samples, stats.p90_cp);
    }
    Ok(())
}
