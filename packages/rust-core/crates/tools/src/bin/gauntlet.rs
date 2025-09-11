use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use clap::{ArgAction, Args, Parser, Subcommand};
use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimitsBuilder;
use engine_core::time_management::TimeControl;
use engine_core::{search::types::RootLine, shogi::Position};
use once_cell::sync::Lazy;
use rand::{rngs::StdRng, Rng, SeedableRng};
use serde::Serialize;
use std::cmp::Ordering;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------- CLI ----------------

#[derive(Parser, Debug)]
#[command(
    name = "gauntlet",
    about = "One-command gauntlet runner",
    version,
    infer_subcommands = true,
    subcommand_negates_reqs = true,
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    // Allow subcommand-less usage by flattening run args
    #[command(flatten)]
    run_args: RunArgs,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run gauntlet matches and emit JSON/Markdown reports
    Run(RunArgs),
}

#[derive(Args, Debug, Clone)]
struct RunArgs {
    /// Baseline NNUE weights (file path)
    #[arg(long, value_name = "FILE")]
    base: String,
    /// Candidate NNUE weights (file path)
    #[arg(long, value_name = "FILE")]
    cand: String,
    /// Time control string: e.g., "0/1+0.1" (main+increment in seconds)
    #[arg(long, value_name = "SPEC", default_value = "0/1+0.1")]
    time: String,
    /// Number of games (total)
    #[arg(long, value_name = "N", default_value_t = 200)]
    games: usize,
    /// Threads per engine
    #[arg(long, value_name = "N", default_value_t = 1)]
    threads: usize,
    /// Hash size in MB
    #[arg(long = "hash-mb", value_name = "MB", default_value_t = 256)]
    hash_mb: usize,
    /// Opening book (EPD/SFEN lines)
    #[arg(long, value_name = "FILE")]
    book: String,
    /// MultiPV lines during games (should be 1). PV spread is measured separately with MultiPV=3
    #[arg(long, value_name = "K", default_value_t = 1)]
    multipv: u8,
    /// JSON output path
    #[arg(long, value_name = "FILE")]
    json: String,
    /// Markdown report path
    #[arg(long, value_name = "FILE")]
    report: String,
    /// Hidden: use deterministic stub engine for tests
    #[arg(long, hide = true, action = ArgAction::SetTrue)]
    stub: bool,
}

fn validate_args(a: &RunArgs) -> Result<()> {
    if a.multipv != 1 {
        return Err(anyhow!(
            "--multipv must be 1 for games (PV spread is measured with MultiPV=3 internally)"
        ));
    }
    if a.games % 2 != 0 {
        return Err(anyhow!("--games must be even for fair pairing"));
    }
    if a.threads != 1 {
        eprintln!("WARN: Spec 013 requires --threads=1 (got {}).", a.threads);
    }
    if a.json == "-" && a.report == "-" {
        return Err(anyhow!(
            "Use at most one of '--json -' or '--report -' (both write to STDOUT)"
        ));
    }
    Ok(())
}

// ---------------- Types ----------------

#[derive(Debug, Clone, Copy)]
struct TimeSpec {
    _main_ms: u64,
    inc_ms: u64,
}

#[derive(Debug, Serialize, Clone)]
struct EnvInfo {
    cpu: String,
    rustc: String,
    commit: String,
    toolchain: String,
}

#[derive(Debug, Serialize, Clone)]
struct ParamsOut {
    time: String,
    games: usize,
    threads: usize,
    hash_mb: usize,
    book: String,
    multipv: u8,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "snake_case")]
enum GateDecision {
    Pass,
    Reject,
    Provisional,
}

impl std::fmt::Display for GateDecision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            GateDecision::Pass => "pass",
            GateDecision::Reject => "reject",
            GateDecision::Provisional => "provisional",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Serialize, Clone)]
struct SummaryOut {
    winrate: f64,
    draw: f64,
    nps_delta_pct: f64,
    pv_spread_p90_cp: f64,
    gate: GateDecision,
    #[serde(skip_serializing_if = "Option::is_none")]
    reject_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    wins: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    losses: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    draws: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    games: Option<usize>,
}

#[derive(Debug, Serialize, Clone)]
struct SeriesItem {
    game_index: usize,
    opening_index: usize,
    sfen: String,
    color_cand: String,
    result: String,
    plies: u32,
    nodes_base: u64,
    nodes_cand: u64,
    nps_base: u64,
    nps_cand: u64,
}

#[derive(Debug, Serialize, Clone)]
struct GauntletOut {
    env: EnvInfo,
    params: ParamsOut,
    summary: SummaryOut,
    #[serde(skip_serializing_if = "Option::is_none")]
    training_config: Option<serde_json::Value>,
    series: Vec<SeriesItem>,
}

// ---------------- Impl ----------------

static Z_95: Lazy<f64> = Lazy::new(|| 1.959963984540054); // 95% Wilson

fn parse_time_spec(s: &str) -> Result<TimeSpec> {
    // Accept forms like "0/1+0.1" or "1+0.1" (seconds)
    // We parse the last "X+Y" as main+inc (in seconds)
    let core = if let Some(pos) = s.rfind('/') {
        &s[pos + 1..]
    } else {
        s
    };
    let mut parts = core.split('+');
    let main = parts
        .next()
        .ok_or_else(|| anyhow!("invalid time spec"))?
        .trim()
        .parse::<f64>()
        .with_context(|| format!("invalid main time '{}': expected seconds", core))?;
    let inc = parts
        .next()
        .unwrap_or("0")
        .trim()
        .parse::<f64>()
        .with_context(|| format!("invalid increment in '{}': expected seconds", core))?;
    Ok(TimeSpec {
        _main_ms: (main * 1000.0) as u64,
        inc_ms: (inc * 1000.0) as u64,
    })
}

fn load_book_positions(book_path: &Path) -> Result<Vec<String>> {
    let text = fs::read_to_string(book_path)
        .with_context(|| format!("failed to read book: {}", book_path.display()))?;
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(rest) = t.strip_prefix("sfen ") {
            // Take until end of line; engine_core::usi::parse_sfen can parse standard sfen
            out.push(rest.trim().to_string());
        }
    }
    if out.is_empty() {
        return Err(anyhow!("no SFEN lines found in book"));
    }
    Ok(out)
}

fn gather_env_info() -> EnvInfo {
    let rustc = Command::new("rustc")
        .arg("-V")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "rustc unknown".to_string());
    let toolchain = rustc.clone();
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|c| {
            c.lines()
                .find(|l| {
                    l.to_lowercase().starts_with("model name")
                        || l.to_lowercase().starts_with("hardware")
                })
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
        })
        .unwrap_or_else(|| "cpu unknown".to_string());
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    EnvInfo {
        cpu,
        rustc,
        commit,
        toolchain,
    }
}

fn build_markdown(out: &GauntletOut) -> String {
    let mut md = String::new();
    md.push_str("# Gauntlet Report\n\n");
    md.push_str(&format!(
        "- Score rate: {:.1}% (W:{} L:{} D:{})\n",
        out.summary.winrate * 100.0,
        out.summary.wins.unwrap_or(0),
        out.summary.losses.unwrap_or(0),
        out.summary.draws.unwrap_or(0)
    ));
    md.push_str(&format!("- Draw rate: {:.1}%\n", out.summary.draw * 100.0));
    md.push_str(&format!("- NPS delta: {:+.2}%\n", out.summary.nps_delta_pct));
    md.push_str(&format!("- PV spread p90: {:.0} cp\n", out.summary.pv_spread_p90_cp));
    md.push_str(&format!("- Gate: {}\n", out.summary.gate));
    if let Some(reason) = &out.summary.reject_reason {
        md.push_str(&format!("- Reason: {}\n", reason));
    }
    md.push_str("\n## Params\n");
    md.push_str(&format!(
        "- time={} games={} threads={} hash_mb={} multipv={} book='{}'\n",
        out.params.time,
        out.params.games,
        out.params.threads,
        out.params.hash_mb,
        out.params.multipv,
        out.params.book
    ));
    md
}

fn write_markdown(report_path: &Path, out: &GauntletOut) -> Result<()> {
    let md = build_markdown(out);
    fs::create_dir_all(report_path.parent().unwrap_or_else(|| Path::new(".")))?;
    fs::write(report_path, md)?;
    Ok(())
}

fn percentile_nearest_rank(mut v: Vec<f64>, p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
    let n = v.len();
    let rank = (p * n as f64).ceil().max(1.0) as usize - 1;
    v[rank.min(n - 1)]
}

fn wilson_lower_bound(wins: usize, losses: usize) -> f64 {
    // exclude draws; if no decisive games, return 0.5 lower bound
    let n = wins + losses;
    if n == 0 {
        return 0.5;
    }
    let z = *Z_95;
    let phat = wins as f64 / n as f64;
    let denom = 1.0 + z * z / n as f64;
    let center = phat + z * z / (2.0 * n as f64);
    let adj = z * ((phat * (1.0 - phat) + z * z / (4.0 * n as f64)) / n as f64).sqrt();
    (center - adj) / denom
}

// ---------------- Engine runner (real) ----------------

struct PlayerEngine {
    eng: Engine,
}

impl PlayerEngine {
    fn new(
        weights: &str,
        threads: usize,
        hash_mb: usize,
        engine_type: EngineType,
        multipv: u8,
    ) -> Result<Self> {
        engine_core::init::init_all_tables_once();
        let mut eng = Engine::new(engine_type);
        eng.set_threads(threads);
        eng.set_hash_size(hash_mb);
        eng.set_multipv_persistent(multipv);
        if matches!(engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
            if let Err(e) = eng.load_nnue_weights(weights) {
                return Err(anyhow!("failed to load NNUE weights '{}': {e}", weights));
            }
        }
        Ok(Self { eng })
    }

    fn search_best(
        &mut self,
        pos: &mut Position,
        tc: TimeControl,
    ) -> (Option<engine_core::shogi::Move>, u64, u64) {
        // Build limits
        let limits = SearchLimitsBuilder::default().time_control(tc).build();
        let res = self.eng.search(pos, limits);
        let best = res.best_move;
        let nodes = res.stats.nodes;
        let elapsed_ms = res.stats.elapsed.as_millis() as u64;
        (best, nodes, elapsed_ms.max(1))
    }

    fn eval_multipv3_root_cp(&mut self, pos: &mut Position, per_ms: u64) -> Option<[i32; 3]> {
        self.eng.set_multipv_persistent(3);
        let limits = SearchLimitsBuilder::default()
            .time_control(TimeControl::FixedTime {
                ms_per_move: per_ms,
            })
            .build();
        let res = self.eng.search(pos, limits);
        // restore MultiPV=1 for games
        self.eng.set_multipv_persistent(1);
        if let Some(lines) = res.lines {
            let mut cps: Vec<i32> = lines.iter().take(3).map(|l: &RootLine| l.score_cp).collect();
            if cps.len() < 3 {
                return None;
            }
            cps.truncate(3);
            Some([cps[0], cps[1], cps[2]])
        } else {
            None
        }
    }

    fn clear_tt(&mut self) {
        // Clear TT/heuristics to avoid cross-sample warming effects for fair NPS measurement
        self.eng.clear_hash();
    }
}

// ---------------- Stub runner (deterministic) ----------------

struct StubRunner {
    rng: StdRng,
}

impl StubRunner {
    fn new() -> Self {
        Self {
            rng: StdRng::seed_from_u64(42),
        }
    }
    fn play_game(&mut self, game_idx: usize) -> (String, u32, u64, u64, u64, u64) {
        // Produce deterministic result: 60% win, 10% draw
        let r = self.rng.random::<f64>();
        let result = if r < 0.6 {
            "win"
        } else if r < 0.7 {
            "draw"
        } else {
            "loss"
        };
        let plies = 60 + (game_idx as u32 % 20);
        // Candidate is ~+2% NPS vs base
        let nps_base = 1_000_000 + (game_idx as u64 % 1000);
        let nps_cand = (nps_base as f64 * 1.02) as u64;
        let nodes_base = nps_base; // 1 second equivalent
        let nodes_cand = nps_cand;
        (result.to_string(), plies, nodes_base, nodes_cand, nps_base, nps_cand)
    }
    fn pv_spread_p90_cp(&mut self) -> f64 {
        25.0
    }
}

// ---------------- Core run ----------------

fn run_real(args: &RunArgs) -> Result<GauntletOut> {
    validate_args(args)?;
    let time = parse_time_spec(&args.time)?;
    let book = load_book_positions(Path::new(&args.book))?;
    let mut base = PlayerEngine::new(
        &args.base,
        args.threads,
        args.hash_mb,
        EngineType::EnhancedNnue,
        args.multipv,
    )?;
    let mut cand = PlayerEngine::new(
        &args.cand,
        args.threads,
        args.hash_mb,
        EngineType::EnhancedNnue,
        args.multipv,
    )?;

    // NPS measurement (fixed positions, fixed time per position)
    let nps_sample_ms = time.inc_ms.max(100);
    let mut nps_base_sum = 0.0f64;
    let mut nps_cand_sum = 0.0f64;
    let n_samples = 100.min(book.len());
    for (i, s) in book.iter().take(n_samples).enumerate() {
        // Use independent positions and alternate measurement order
        let mut pos_b = Position::from_sfen(s).map_err(|e| anyhow!(e))?;
        let mut pos_c = Position::from_sfen(s).map_err(|e| anyhow!(e))?;
        // Clear TT before each measurement to avoid TT warming bias
        base.clear_tt();
        cand.clear_tt();
        let (nodes_b, el_b, nodes_c, el_c) = if i % 2 == 0 {
            let (_b, nodes_b, el_b) = base.search_best(
                &mut pos_b,
                TimeControl::FixedTime {
                    ms_per_move: nps_sample_ms,
                },
            );
            let (_c, nodes_c, el_c) = cand.search_best(
                &mut pos_c,
                TimeControl::FixedTime {
                    ms_per_move: nps_sample_ms,
                },
            );
            (nodes_b, el_b, nodes_c, el_c)
        } else {
            let (_c, nodes_c, el_c) = cand.search_best(
                &mut pos_c,
                TimeControl::FixedTime {
                    ms_per_move: nps_sample_ms,
                },
            );
            let (_b, nodes_b, el_b) = base.search_best(
                &mut pos_b,
                TimeControl::FixedTime {
                    ms_per_move: nps_sample_ms,
                },
            );
            (nodes_b, el_b, nodes_c, el_c)
        };
        let nps_b = (nodes_b as f64) * 1000.0 / (el_b as f64).max(1.0);
        let nps_c = (nodes_c as f64) * 1000.0 / (el_c as f64).max(1.0);
        nps_base_sum += nps_b;
        nps_cand_sum += nps_c;
    }
    let nps_b_avg: f64 = if n_samples > 0 {
        nps_base_sum / (n_samples as f64)
    } else {
        0.0
    };
    let nps_c_avg: f64 = if n_samples > 0 {
        nps_cand_sum / (n_samples as f64)
    } else {
        0.0
    };
    let nps_delta_pct: f64 = if nps_b_avg > 0.0 {
        (nps_c_avg - nps_b_avg) / nps_b_avg * 100.0
    } else {
        0.0
    };

    // PV spread p90 (candidate; MultiPV=3)
    let mut spreads: Vec<f64> = Vec::new();
    fn is_mate_cp(cp: i32) -> bool {
        cp.abs() >= 30_000
    }
    for s in book.iter().take(100.min(book.len())) {
        cand.clear_tt();
        let mut pos = Position::from_sfen(s).map_err(|e| anyhow!(e))?;
        if let Some(cps) = cand.eval_multipv3_root_cp(&mut pos, nps_sample_ms) {
            if cps.iter().any(|&cp| is_mate_cp(cp)) {
                continue; // Skip mate-valued samples for stable spread
            }
            let min = cps.iter().min().copied().unwrap_or(0);
            let max = cps.iter().max().copied().unwrap_or(0);
            spreads.push((max - min) as f64);
        }
    }
    let pv_spread_p90 = percentile_nearest_rank(spreads, 0.90);

    // Games
    let schedule = schedule_pairs(book.len(), args.games);
    let mut series: Vec<SeriesItem> = Vec::with_capacity(args.games);
    let mut wins = 0usize;
    let mut losses = 0usize;
    let mut draws = 0usize;
    for (g, (open_idx, cand_black)) in schedule.into_iter().enumerate() {
        let sfen = &book[open_idx];
        let mut pos = Position::from_sfen(sfen).map_err(|e| anyhow!(e))?;
        // Use fixed-time per move equal to increment (~0.1s) as minimal policy
        let movetime = time.inc_ms.max(100);
        let max_plies = 256u32;
        let mut plies = 0u32;
        let mut nodes_b_total = 0u64;
        let mut nodes_c_total = 0u64;
        let mut nps_b_last = 0u64;
        let mut nps_c_last = 0u64;

        loop {
            if plies >= max_plies || pos.is_repetition() {
                draws += 1;
                series.push(SeriesItem {
                    game_index: g,
                    opening_index: open_idx,
                    sfen: sfen.clone(),
                    color_cand: if cand_black { "Black" } else { "White" }.to_string(),
                    result: "draw".into(),
                    plies,
                    nodes_base: nodes_b_total,
                    nodes_cand: nodes_c_total,
                    nps_base: nps_b_last,
                    nps_cand: nps_c_last,
                });
                break;
            }
            // Choose which engine moves now
            let stm_black = pos.side_to_move == engine_core::shogi::Color::Black;
            let cand_to_move = (cand_black && stm_black) || (!cand_black && !stm_black);
            if cand_to_move {
                let (best, nodes, el) = cand.search_best(
                    &mut pos,
                    TimeControl::FixedTime {
                        ms_per_move: movetime,
                    },
                );
                if let Some(mv) = best {
                    let _ = pos.do_move(mv);
                } else {
                    // resign => base wins
                    losses += 1; // candidate resigned
                    series.push(SeriesItem {
                        game_index: g,
                        opening_index: open_idx,
                        sfen: sfen.clone(),
                        color_cand: if cand_black { "Black" } else { "White" }.to_string(),
                        result: "loss".into(),
                        plies,
                        nodes_base: nodes_b_total,
                        nodes_cand: nodes_c_total,
                        nps_base: nps_b_last,
                        nps_cand: nps_c_last,
                    });
                    break;
                }
                nodes_c_total = nodes_c_total.saturating_add(nodes);
                nps_c_last = ((nodes as u128) * 1000 / (el as u128).max(1)) as u64;
            } else {
                let (best, nodes, el) = base.search_best(
                    &mut pos,
                    TimeControl::FixedTime {
                        ms_per_move: movetime,
                    },
                );
                if let Some(mv) = best {
                    let _ = pos.do_move(mv);
                } else {
                    // resign => cand wins
                    wins += 1;
                    series.push(SeriesItem {
                        game_index: g,
                        opening_index: open_idx,
                        sfen: sfen.clone(),
                        color_cand: if cand_black { "Black" } else { "White" }.to_string(),
                        result: "win".into(),
                        plies,
                        nodes_base: nodes_b_total,
                        nodes_cand: nodes_c_total,
                        nps_base: nps_b_last,
                        nps_cand: nps_c_last,
                    });
                    break;
                }
                nodes_b_total = nodes_b_total.saturating_add(nodes);
                nps_b_last = ((nodes as u128) * 1000 / (el as u128).max(1)) as u64;
            }

            // Check if side to move is out of legal moves after move (mate detection)
            // We'll rely on search to find mate/resign; otherwise limit by plies/repetition
            plies += 1;
        }
    }

    let score_rate = if args.games > 0 {
        (wins as f64 + 0.5 * draws as f64) / (args.games as f64)
    } else {
        0.5
    };
    let winrate = score_rate; // Interpret as score rate (w=1,d=0.5,l=0)
    let draw = if args.games > 0 {
        draws as f64 / args.games as f64
    } else {
        0.0
    };
    let wl_lower = wilson_lower_bound(wins, losses);
    // Gate: 最終合格: 勝率+5%pt かつ NPS±3%
    let mut gate = GateDecision::Reject;
    let mut reason: Option<String> = None;
    if wl_lower > 0.5 {
        gate = GateDecision::Provisional;
    }
    if winrate >= 0.55 && nps_delta_pct.abs() <= 3.0 {
        gate = GateDecision::Pass;
    } else if !matches!(gate, GateDecision::Provisional) {
        // Reject with reason
        let mut rs = Vec::new();
        if winrate < 0.55 {
            rs.push(format!("score rate {:.1}% < 55%", winrate * 100.0));
        }
        if nps_delta_pct.abs() > 3.0 {
            rs.push(format!("|nps_delta| {:.2}% > 3%", nps_delta_pct.abs()));
        }
        reason = Some(rs.join(", "));
    }

    let out = GauntletOut {
        env: gather_env_info(),
        params: ParamsOut {
            time: args.time.clone(),
            games: args.games,
            threads: args.threads,
            hash_mb: args.hash_mb,
            book: args.book.clone(),
            multipv: args.multipv,
        },
        summary: SummaryOut {
            winrate,
            draw,
            nps_delta_pct,
            pv_spread_p90_cp: pv_spread_p90,
            gate,
            reject_reason: reason,
            wins: Some(wins),
            losses: Some(losses),
            draws: Some(draws),
            games: Some(args.games),
        },
        training_config: None,
        series,
    };

    Ok(out)
}

fn run_stub(args: &RunArgs) -> Result<GauntletOut> {
    validate_args(args)?;
    let _time = parse_time_spec(&args.time)?;
    let book = load_book_positions(Path::new(&args.book))?;
    let mut stub = StubRunner::new();

    // Simulate NPS
    let nps_b_avg: f64 = 1_000_000.0;
    let nps_c_avg: f64 = 1_020_000.0; // +2%
    let nps_delta_pct: f64 = (nps_c_avg - nps_b_avg) / nps_b_avg * 100.0;
    let pv_spread_p90 = stub.pv_spread_p90_cp();

    let mut series = Vec::new();
    let mut wins = 0usize;
    let mut losses = 0usize;
    let mut draws = 0usize;
    let schedule = schedule_pairs(book.len(), args.games);
    for (g, (open_idx, cand_black)) in schedule.into_iter().enumerate() {
        let sfen = &book[open_idx];
        let (res, plies, nb, nc, nps_b, nps_c) = stub.play_game(g);
        match res.as_str() {
            "win" => wins += 1,
            "loss" => losses += 1,
            _ => draws += 1,
        }
        series.push(SeriesItem {
            game_index: g,
            opening_index: open_idx,
            sfen: sfen.clone(),
            color_cand: if cand_black { "Black" } else { "White" }.to_string(),
            result: res,
            plies,
            nodes_base: nb,
            nodes_cand: nc,
            nps_base: nps_b,
            nps_cand: nps_c,
        });
    }
    let score_rate = if args.games > 0 {
        (wins as f64 + 0.5 * draws as f64) / (args.games as f64)
    } else {
        0.5
    };
    let winrate = score_rate;
    let draw = if args.games > 0 {
        draws as f64 / args.games as f64
    } else {
        0.0
    };
    let wl_lower = wilson_lower_bound(wins, losses);
    let mut gate = GateDecision::Reject;
    let mut reason: Option<String> = None;
    if wl_lower > 0.5 {
        gate = GateDecision::Provisional;
    }
    if winrate >= 0.55 && nps_delta_pct.abs() <= 3.0 {
        gate = GateDecision::Pass;
    } else if !matches!(gate, GateDecision::Provisional) {
        let mut rs = Vec::new();
        if winrate < 0.55 {
            rs.push(format!("score rate {:.1}% < 55%", winrate * 100.0));
        }
        if nps_delta_pct.abs() > 3.0 {
            rs.push(format!("|nps_delta| {:.2}% > 3%", nps_delta_pct.abs()));
        }
        reason = Some(rs.join(", "));
    }
    Ok(GauntletOut {
        env: gather_env_info(),
        params: ParamsOut {
            time: args.time.clone(),
            games: args.games,
            threads: args.threads,
            hash_mb: args.hash_mb,
            book: args.book.clone(),
            multipv: args.multipv,
        },
        summary: SummaryOut {
            winrate,
            draw,
            nps_delta_pct,
            pv_spread_p90_cp: pv_spread_p90,
            gate,
            reject_reason: reason,
            wins: Some(wins),
            losses: Some(losses),
            draws: Some(draws),
            games: Some(args.games),
        },
        training_config: None,
        series,
    })
}

fn emit_structured_jsonl_to<W: Write>(mut w: W, games: usize, summary: &SummaryOut) {
    // structured_v1 minimal line, extended with optional gauntlet metrics
    #[derive(Serialize)]
    struct Line<'a> {
        ts: &'a str,
        phase: &'a str,
        global_step: usize,
        epoch: u32,
        wall_time: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        gate: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        score_rate: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        draw: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        nps_delta_pct: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pv_spread_p90_cp: Option<f64>,
    }
    let ts_str = Utc::now().to_rfc3339();
    let line = Line {
        ts: &ts_str,
        phase: "gauntlet",
        global_step: games,
        epoch: 0,
        wall_time: 0.0,
        gate: Some(match summary.gate {
            GateDecision::Pass => "pass",
            GateDecision::Reject => "reject",
            GateDecision::Provisional => "provisional",
        }),
        score_rate: Some(summary.winrate),
        draw: Some(summary.draw),
        nps_delta_pct: Some(summary.nps_delta_pct),
        pv_spread_p90_cp: Some(summary.pv_spread_p90_cp),
    };
    if let Ok(s) = serde_json::to_string(&line) {
        let _ = writeln!(w, "{}", s);
    }
}

fn schedule_pairs(book_len: usize, games: usize) -> Vec<(usize, bool)> {
    (0..(games / 2))
        .flat_map(|i| {
            let open_idx = i % book_len;
            [(open_idx, true), (open_idx, false)]
        })
        .collect()
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let args = match cli.command {
        Some(Commands::Run(a)) => a,
        None => cli.run_args,
    };

    let out = if args.stub {
        run_stub(&args)?
    } else {
        run_real(&args)?
    };

    let json_to_stdout = args.json == "-";
    let report_to_stdout = args.report == "-";

    // JSON output
    if json_to_stdout {
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        let json_path = PathBuf::from(&args.json);
        if let Some(parent) = json_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&json_path, serde_json::to_string_pretty(&out)?)?;
    }

    // Markdown output
    if report_to_stdout {
        print!("{}", build_markdown(&out));
    } else {
        write_markdown(Path::new(&args.report), &out)?;
    }

    // structured_v1 line: default stdout, but if either main outputs are stdout, use stderr
    if json_to_stdout || report_to_stdout {
        emit_structured_jsonl_to(std::io::stderr(), out.params.games, &out.summary);
    } else {
        emit_structured_jsonl_to(std::io::stdout(), out.params.games, &out.summary);
    }
    Ok(())
}
