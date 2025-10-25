use anyhow::{anyhow, Context, Result};
use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use engine_core::engine::controller::{Engine, EngineType};
use engine_core::search::limits::SearchLimitsBuilder;
use engine_core::shogi::Position;
use engine_core::time_management::TimeControl;
use rand_chacha::rand_core::{RngCore, SeedableRng as ChachaSeedableRng};
use rand_chacha::ChaCha12Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
use std::time::Instant;

const MATE_CP_ABS_THRESHOLD: i32 = 30_000;

#[derive(Parser, Debug)]
#[command(
    name = "pv-probe",
    about = "Probe PV spread and merge results",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    #[command(flatten)]
    run_args: RunArgs,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run PV probe and emit JSON/Markdown
    Run(RunArgs),
    /// Merge multiple v2 JSONs (strict by default)
    Merge(MergeArgs),
    /// Validate compatibility of multiple v2 JSONs (no merge)
    Validate(ValidateArgs),
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
enum ShardMode {
    Block,
    Stride,
}

#[derive(Copy, Clone, Debug, ValueEnum, PartialEq, Eq)]
enum TtMode {
    Off,
    Isolate,
    Carried,
}

#[derive(Args, Debug, Clone)]
#[command(group(
    ArgGroup::new("out")
        .args(["json", "report"]) // optional
        .required(false),
))]
struct RunArgs {
    /// Candidate NNUE weights (Single v2 or Classic v1)
    #[arg(long)]
    cand: String,
    /// Book file with SFEN lines (either raw SFEN or lines prefixed with 'sfen ')
    #[arg(long)]
    book: String,
    /// Fixed time per PV sample (ms)
    #[arg(long, default_value_t = 1500)]
    ms: u64,
    /// Optional fixed depth per PV sample (takes precedence over --ms)
    #[arg(long)]
    depth: Option<u8>,
    /// Threads per engine (Spec recommends 1)
    #[arg(long, default_value_t = 1)]
    threads: usize,
    /// Workers per process (each uses threads=1 engine)
    #[arg(long, default_value_t = 1)]
    workers: usize,
    /// Hash size per worker in MB
    #[arg(long = "hash-mb-per-worker", default_value_t = 256)]
    hash_mb_per_worker: usize,
    /// Target number of accepted samples
    #[arg(long = "target-samples", default_value_t = 100)]
    target_samples: usize,
    /// Maximum attempts to reach target samples
    #[arg(long = "max-attempts")]
    max_attempts: Option<usize>,
    /// Optional shuffle seed
    #[arg(long)]
    seed: Option<u64>,
    /// Total shards
    #[arg(long = "shards", default_value_t = 1)]
    shards: usize,
    /// Shard index (0-based)
    #[arg(long = "shard-index", default_value_t = 0)]
    shard_index: usize,
    /// Shard mode
    #[arg(long = "shard-mode", value_enum, default_value_t = ShardMode::Block)]
    shard_mode: ShardMode,
    /// Shuffle before sharding
    #[arg(long = "shuffle-before-shard", default_value_t = true)]
    shuffle_before_shard: bool,
    /// Transposition table mode
    #[arg(long = "tt", value_enum, default_value_t = TtMode::Off)]
    tt_mode: TtMode,
    /// Mate CP absolute threshold for skipping
    #[arg(long = "mate-cp-abs-threshold", default_value_t = MATE_CP_ABS_THRESHOLD)]
    mate_cp_abs_threshold: i32,
    /// JSON output path (optional; '-' for STDOUT)
    #[arg(long)]
    json: Option<String>,
    /// Markdown report path (optional; '-' for STDOUT)
    #[arg(long)]
    report: Option<String>,
    /// Quiet mode (suppress progress)
    #[arg(long)]
    quiet: bool,
}

#[derive(Args, Debug, Clone)]
struct MergeArgs {
    /// Input v2 JSON files (two or more)
    #[arg(required = true)]
    inputs: Vec<String>,
    /// Output JSON (merged)
    #[arg(long, default_value = "merged.pv_probe.json")]
    out: String,
    /// Relax validation to warnings
    #[arg(long = "warn")]
    warn: bool,
}

#[derive(Args, Debug, Clone)]
struct ValidateArgs {
    /// Input v2 JSON files (two or more)
    #[arg(required = true)]
    inputs: Vec<String>,
    /// Relax validation to warnings
    #[arg(long = "warn")]
    warn: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Summary {
    samples: usize,
    p50_cp: u32,
    p90_cp: u32,
    max_cp: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct MetaV2 {
    ts: String,
    cpu: String,
    toolchain: String,
    engine_commit: String,
    engine_build_profile: String,
    book_sha256: String,
    book_num_lines: usize,
    cand_sha256: String,
    rng_algo: String,
    rand_crate: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct ParamsV2 {
    cand: String,
    book: String,
    mode: String, // "fixed_time" | "fixed_depth"
    ms: Option<u64>,
    depth: Option<u8>,
    threads: usize,
    workers: usize,
    hash_mb_per_worker: usize,
    mate_cp_abs_threshold: i32,
    seed: Option<u64>,
    shards: usize,
    shard_index: usize,
    shard_mode: String, // "block"|"stride"
    shuffle_before_shard: bool,
    tt_mode: String,    // "off"|"isolate"|"carried"
    tt_backend: String, // "per_engine"|"global"
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct HistV2 {
    bucket_size_cp: u32, // fixed 1
    max_cp: u32,
    // counts をJSON相互運用のために10進文字列で保存（53bit丸め回避）
    #[serde(skip_serializing_if = "Option::is_none")]
    counts: Option<Vec<u64>>, // 互換用（読み取りのみ推奨）
    #[serde(skip_serializing_if = "Option::is_none")]
    counts_dec: Option<Vec<String>>, // 推奨: u64 を10進文字列として保存
    #[serde(default, skip_serializing_if = "String::is_empty")]
    counts_encoding: String, // "u64-decimal" など
    overflow: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct SkippedV2 {
    mates: u64,
    invalid_sfen: u64,
    short_pv: u64,
    bound_non_exact: u64,
    search_error: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct AttemptsV2 {
    total: u64,
    accepted: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct OutJsonV2 {
    format_version: u8,
    meta: MetaV2,
    params: ParamsV2,
    stats: Summary,
    hist: HistV2,
    skipped: SkippedV2,
    attempts: AttemptsV2,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn sha256_file(path: &str) -> Result<String> {
    let data = fs::read(path).with_context(|| format!("read file for sha256: {path}"))?;
    Ok(sha256_hex(&data))
}

fn load_book(path: &str) -> Result<(Vec<String>, String)> {
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
    let sha = sha256_hex(text.as_bytes());
    Ok((v, sha))
}

struct Runner {
    eng: Engine,
}
impl Runner {
    fn new(weights: &str, threads: usize, hash_mb: usize, tt_mode: TtMode) -> Result<Self> {
        engine_core::init::init_all_tables_once();
        let mut eng = Engine::new(EngineType::EnhancedNnue);
        eng.set_threads(threads);
        match tt_mode {
            TtMode::Off => eng.set_hash_size(0),
            _ => eng.set_hash_size(hash_mb),
        }
        eng.set_multipv_persistent(1);
        eng.load_nnue_weights(weights).map_err(|e| anyhow!("load NNUE: {}", e))?;
        Ok(Self { eng })
    }
    fn probe_pv3_cp_time(&mut self, sfen: &str, ms: u64, tt_mode: TtMode) -> Option<[i32; 3]> {
        if matches!(tt_mode, TtMode::Isolate) {
            self.eng.clear_hash();
        }
        self.eng.set_multipv_persistent(3u8);
        let limits = SearchLimitsBuilder::default()
            .time_control(TimeControl::FixedTime { ms_per_move: ms })
            .multipv(3)
            .build();
        let mut out: Option<[i32; 3]> = None;
        if let Ok(mut pos) = Position::from_sfen(sfen) {
            let res = self.eng.search(&mut pos, limits);
            if let Some(lines) = res.lines {
                if lines.len() >= 3 {
                    let a = lines[0].score_cp;
                    let b = lines[1].score_cp;
                    let c = lines[2].score_cp;
                    out = Some([a, b, c]);
                }
            }
        }
        self.eng.set_multipv_persistent(1u8);
        out
    }
    fn probe_pv3_cp_depth(&mut self, sfen: &str, depth: u8, tt_mode: TtMode) -> Option<[i32; 3]> {
        if matches!(tt_mode, TtMode::Isolate) {
            self.eng.clear_hash();
        }
        self.eng.set_multipv_persistent(3u8);
        let limits = SearchLimitsBuilder::default().depth(depth).multipv(3).build();
        let mut out: Option<[i32; 3]> = None;
        if let Ok(mut pos) = Position::from_sfen(sfen) {
            let res = self.eng.search(&mut pos, limits);
            if let Some(lines) = res.lines {
                if lines.len() >= 3 {
                    let a = lines[0].score_cp;
                    let b = lines[1].score_cp;
                    let c = lines[2].score_cp;
                    out = Some([a, b, c]);
                }
            }
        }
        self.eng.set_multipv_persistent(1u8);
        out
    }
}

// RAII ガードは二重可変借用を避けるため未使用。明示的に set→search→set で戻す。

#[derive(Clone, Debug)]
struct Histogram {
    bucket_size_cp: u32,
    max_cp: u32,
    counts: Vec<u64>,
    overflow: u64,
}
impl Histogram {
    fn new(max_cp: u32) -> Self {
        let counts = vec![0; (max_cp + 1) as usize];
        Self {
            bucket_size_cp: 1,
            max_cp,
            counts,
            overflow: 0,
        }
    }
    fn add(&mut self, spread_cp: u32) {
        if spread_cp <= self.max_cp {
            self.counts[spread_cp as usize] += 1;
        } else {
            self.overflow += 1;
        }
    }
    fn quantile_nearest_rank(&self, q: f64) -> u32 {
        let total = self.counts.iter().sum::<u64>();
        if total == 0 {
            return 0;
        }
        let rank = (q * (total as f64)).ceil().max(1.0) as u64;
        let mut acc = 0u64;
        for (cp, &c) in self.counts.iter().enumerate() {
            acc += c;
            if acc >= rank {
                return cp as u32;
            }
        }
        self.max_cp
    }
    fn max_observed(&self) -> u32 {
        for cp in (0..=self.max_cp as usize).rev() {
            if self.counts[cp] > 0 {
                return cp as u32;
            }
        }
        0
    }
    fn merge_into(&mut self, other: &Histogram) -> Result<()> {
        if self.bucket_size_cp != other.bucket_size_cp || self.max_cp != other.max_cp {
            return Err(anyhow!("histogram incompatible (bucket/max_cp)"));
        }
        if self.counts.len() != other.counts.len() {
            return Err(anyhow!("histogram length mismatch"));
        }
        for (a, b) in self.counts.iter_mut().zip(other.counts.iter()) {
            *a = a.checked_add(*b).context("u64 overflow in histogram counts")?;
        }
        self.overflow = self
            .overflow
            .checked_add(other.overflow)
            .context("u64 overflow in histogram overflow")?;
        Ok(())
    }
}

fn gather_env_info() -> (String, String) {
    let toolchain = std::process::Command::new("rustup")
        .args(["show", "active-toolchain"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let cpu = {
        let linux = || {
            fs::read_to_string("/proc/cpuinfo").ok().and_then(|c| {
                c.lines()
                    .find(|l| {
                        l.to_lowercase().starts_with("model name")
                            || l.to_lowercase().starts_with("hardware")
                    })
                    .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
            })
        };
        let mac = || {
            std::process::Command::new("sysctl")
                .args(["-n", "machdep.cpu.brand_string"])
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        };
        linux().or_else(mac).unwrap_or_else(|| "cpu unknown".to_string())
    };
    (cpu, toolchain)
}

fn nearest_rank_from_hist(hist: &Histogram) -> Summary {
    let samples = hist.counts.iter().sum::<u64>() as usize;
    let p50 = hist.quantile_nearest_rank(0.50);
    let p90 = hist.quantile_nearest_rank(0.90);
    let maxv = hist.max_observed();
    Summary {
        samples,
        p50_cp: p50,
        p90_cp: p90,
        max_cp: maxv,
    }
}

fn make_meta(_book_path: &str, book_sha256: &str, book_len: usize, cand_sha256: &str) -> MetaV2 {
    let ts = chrono::Utc::now().to_rfc3339();
    let (cpu, toolchain) = gather_env_info();
    let engine_commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    MetaV2 {
        ts,
        cpu,
        toolchain,
        engine_commit,
        engine_build_profile: profile.into(),
        book_sha256: book_sha256.into(),
        book_num_lines: book_len,
        cand_sha256: cand_sha256.into(),
        rng_algo: "chacha12".into(),
        rand_crate: "rand_chacha 0.3".into(),
    }
}

fn shard_indices(len: usize, shards: usize, shard_index: usize, mode: ShardMode) -> Vec<usize> {
    if shards <= 1 {
        return (0..len).collect();
    }
    match mode {
        ShardMode::Block => {
            let chunk = len.div_ceil(shards);
            let start = shard_index.saturating_mul(chunk);
            let end = (start + chunk).min(len);
            (start..end).collect()
        }
        ShardMode::Stride => (shard_index..len).step_by(shards).collect(),
    }
}

fn run_real(args: &RunArgs) -> Result<OutJsonV2> {
    if args.threads != 1 {
        eprintln!("[warn] Spec 013 recommends --threads=1 (got {})", args.threads);
    }
    let (mut book, book_sha) = load_book(&args.book)?;
    if args.shuffle_before_shard {
        if let Some(seed) = args.seed {
            // Fisher-Yates with ChaCha12Rng to avoid rand version mismatch
            let mut rng = ChaCha12Rng::seed_from_u64(seed);
            let mut i = book.len();
            while i > 1 {
                i -= 1;
                let j = (rng.next_u64() as usize) % (i + 1);
                book.swap(i, j);
            }
        }
    }
    let idxs = shard_indices(book.len(), args.shards, args.shard_index, args.shard_mode);
    let selected: Vec<String> = idxs.into_iter().map(|i| book[i].clone()).collect();
    let book_len = selected.len();

    // progress note
    if !args.quiet {
        eprintln!(
            "[info] pv_probe: workers={} threads=1 tt={:?} target={} attempts(max={})",
            args.workers,
            args.tt_mode,
            args.target_samples,
            args.max_attempts.unwrap_or((args.target_samples as f64 * 1.5).ceil() as usize)
        );
        eprintln!(
            "[info] hash per worker={}MB total~{}MB",
            args.hash_mb_per_worker,
            args.hash_mb_per_worker * args.workers
        );
    }

    // assign jobs to workers (round-robin)
    let max_attempts =
        args.max_attempts.unwrap_or((args.target_samples as f64 * 1.5).ceil() as usize);
    let mut jobs = selected;
    // Cap attempts by available positions
    if jobs.len() > max_attempts {
        jobs.truncate(max_attempts);
    }

    // spawn workers
    let start = Instant::now();
    let max_cp: u32 = 30_000;
    let bucket_size_cp = 1u32;

    let mut handles = Vec::new();
    let workers = args.workers.max(1);
    for w in 0..workers {
        let slice: Vec<String> = jobs
            .iter()
            .enumerate()
            .filter(|(i, _)| i % workers == w)
            .map(|(_, s)| s.clone())
            .collect();
        let a = args.clone();
        handles.push(std::thread::spawn(move || -> Result<(Histogram, SkippedV2, AttemptsV2)> {
            let mut r = Runner::new(&a.cand, 1, a.hash_mb_per_worker, a.tt_mode)?;
            let mut hist = Histogram::new(max_cp);
            let mut skipped = SkippedV2::default();
            let mut attempts = AttemptsV2::default();
            let limits_depth = a.depth;
            for s in slice {
                if (attempts.accepted as usize) >= a.target_samples {
                    break;
                }
                attempts.total += 1;
                let triple = if let Some(d) = limits_depth {
                    r.probe_pv3_cp_depth(&s, d, a.tt_mode)
                } else {
                    r.probe_pv3_cp_time(&s, a.ms, a.tt_mode)
                };
                match triple {
                    Some([a_cp, b_cp, c_cp]) => {
                        if [a_cp, b_cp, c_cp].iter().any(|&cp| cp.abs() >= a.mate_cp_abs_threshold)
                        {
                            skipped.mates += 1;
                            continue;
                        }
                        let min = a_cp.min(b_cp).min(c_cp);
                        let max = a_cp.max(b_cp).max(c_cp);
                        let spread = (max - min).max(0) as u32;
                        hist.add(spread);
                        attempts.accepted += 1;
                    }
                    None => {
                        skipped.short_pv += 1;
                    }
                }
            }
            Ok((hist, skipped, attempts))
        }));
    }

    // reduce
    let mut hist = Histogram::new(max_cp);
    let mut skipped = SkippedV2::default();
    let mut attempts = AttemptsV2::default();
    for h in handles {
        let (h2, s2, a2) = h.join().expect("worker join").context("worker")?;
        hist.merge_into(&h2)?;
        skipped.mates += s2.mates;
        skipped.invalid_sfen += s2.invalid_sfen;
        skipped.short_pv += s2.short_pv;
        skipped.bound_non_exact += s2.bound_non_exact;
        skipped.search_error += s2.search_error;
        attempts.total += a2.total;
        attempts.accepted += a2.accepted;
    }
    if !args.quiet {
        eprintln!(
            "[info] done: accepted={} / attempts={} in {:?}",
            attempts.accepted,
            attempts.total,
            start.elapsed()
        );
    }

    let stats = nearest_rank_from_hist(&hist);
    let cand_sha = sha256_file(&args.cand).unwrap_or_else(|_| "unknown".into());
    let meta = make_meta(&args.book, &book_sha, book_len, &cand_sha);
    let params = ParamsV2 {
        cand: args.cand.clone(),
        book: args.book.clone(),
        mode: if args.depth.is_some() {
            "fixed_depth"
        } else {
            "fixed_time"
        }
        .into(),
        ms: args.depth.is_none().then_some(args.ms),
        depth: args.depth,
        threads: args.threads,
        workers: args.workers,
        hash_mb_per_worker: args.hash_mb_per_worker,
        mate_cp_abs_threshold: args.mate_cp_abs_threshold,
        seed: args.seed,
        shards: args.shards,
        shard_index: args.shard_index,
        shard_mode: match args.shard_mode {
            ShardMode::Block => "block",
            ShardMode::Stride => "stride",
        }
        .into(),
        shuffle_before_shard: args.shuffle_before_shard,
        tt_mode: match args.tt_mode {
            TtMode::Off => "off",
            TtMode::Isolate => "isolate",
            TtMode::Carried => "carried",
        }
        .into(),
        tt_backend: "per_engine".into(),
    };
    let out = OutJsonV2 {
        format_version: 2,
        meta,
        params,
        stats,
        hist: HistV2 {
            bucket_size_cp,
            max_cp,
            counts: None,
            counts_dec: Some(hist.counts.iter().map(|v| v.to_string()).collect()),
            counts_encoding: "u64-decimal".into(),
            overflow: hist.overflow,
        },
        skipped,
        attempts,
    };
    Ok(out)
}

fn write_json_pretty<T: Serialize>(path: &str, val: &T) -> Result<()> {
    let s = serde_json::to_string_pretty(val)?;
    if path == "-" {
        println!("{}", s);
    } else {
        if let Some(p) = Path::new(path).parent() {
            if !p.as_os_str().is_empty() {
                let _ = fs::create_dir_all(p);
            }
        }
        fs::write(path, s)?;
    }
    Ok(())
}

fn validate_compat(a: &OutJsonV2, b: &OutJsonV2) -> Result<()> {
    if a.format_version != 2 || b.format_version != 2 {
        return Err(anyhow!("only v2 inputs are supported"));
    }
    let ka = (&a.params.cand, &a.params.book, &a.params.mode, a.params.ms, a.params.depth);
    let kb = (&b.params.cand, &b.params.book, &b.params.mode, b.params.ms, b.params.depth);
    if ka != kb {
        return Err(anyhow!("params(cand/book/mode/ms/depth) mismatch"));
    }
    // exclusive integrity: exactly one of ms/depth should be set
    let a_mode_ok = (a.params.ms.is_some()) ^ (a.params.depth.is_some());
    let b_mode_ok = (b.params.ms.is_some()) ^ (b.params.depth.is_some());
    if !(a_mode_ok && b_mode_ok) {
        return Err(anyhow!("params(ms/depth) exclusivity violation"));
    }
    if a.params.mate_cp_abs_threshold != b.params.mate_cp_abs_threshold {
        return Err(anyhow!("mate threshold mismatch"));
    }
    if a.hist.bucket_size_cp != 1 || b.hist.bucket_size_cp != 1 {
        return Err(anyhow!("bucket_size_cp must be 1 for exact merge"));
    }
    if a.hist.bucket_size_cp != b.hist.bucket_size_cp || a.hist.max_cp != b.hist.max_cp {
        return Err(anyhow!("histogram bucket/max mismatch"));
    }
    if a.meta.book_sha256 != b.meta.book_sha256 {
        return Err(anyhow!("book sha256 mismatch"));
    }
    if a.params.tt_mode != b.params.tt_mode {
        return Err(anyhow!("tt_mode mismatch (exact merge requires same tt mode)"));
    }
    if a.params.tt_backend != b.params.tt_backend {
        return Err(anyhow!("tt_backend mismatch"));
    }
    if a.meta.cand_sha256 != b.meta.cand_sha256 {
        return Err(anyhow!("cand sha256 mismatch"));
    }
    Ok(())
}

fn merge_real(args: &MergeArgs) -> Result<OutJsonV2> {
    if args.inputs.len() < 2 {
        return Err(anyhow!("need >=2 inputs"));
    }
    let mut it = args.inputs.iter();
    let first_path = it.next().unwrap();
    let mut merged: OutJsonV2 =
        serde_json::from_str(&fs::read_to_string(first_path)?).context("read first json")?;
    // normalize stats by recomputing from hist of first
    // load first histogram (counts_dec or counts)
    let mut hist = Histogram::new(merged.hist.max_cp);
    if let Some(ref dec) = merged.hist.counts_dec {
        for (i, s) in dec.iter().enumerate() {
            let v: u64 = s.parse().context("parse counts_dec")?;
            hist.counts[i] = v;
        }
    } else if let Some(ref nums) = merged.hist.counts {
        for (i, &v) in nums.iter().enumerate() {
            hist.counts[i] = v;
        }
    } else {
        return Err(anyhow!("no histogram counts in first input"));
    }
    hist.overflow = merged.hist.overflow;
    for p in it {
        let next: OutJsonV2 = serde_json::from_str(&fs::read_to_string(p)?)
            .with_context(|| format!("read json: {p}"))?;
        if let Err(e) = validate_compat(&merged, &next) {
            if args.warn {
                eprintln!("[warn] {}", e);
            } else {
                return Err(e);
            }
        }
        let mut add = Histogram::new(next.hist.max_cp);
        if let Some(ref dec) = next.hist.counts_dec {
            for (i, s) in dec.iter().enumerate() {
                let v: u64 = s.parse().context("parse counts_dec")?;
                add.counts[i] = v;
            }
        } else if let Some(ref nums) = next.hist.counts {
            for (i, &v) in nums.iter().enumerate() {
                add.counts[i] = v;
            }
        } else {
            return Err(anyhow!("no histogram counts in input"));
        }
        add.overflow = next.hist.overflow;
        hist.merge_into(&add)?;
        merged.skipped.mates += next.skipped.mates;
        merged.skipped.invalid_sfen += next.skipped.invalid_sfen;
        merged.skipped.short_pv += next.skipped.short_pv;
        merged.skipped.bound_non_exact += next.skipped.bound_non_exact;
        merged.skipped.search_error += next.skipped.search_error;
        merged.attempts.total += next.attempts.total;
        merged.attempts.accepted += next.attempts.accepted;
    }
    // recompute stats
    let stats = nearest_rank_from_hist(&hist);
    merged.stats = stats;
    // store as decimal strings for portability
    merged.hist.counts = None;
    merged.hist.counts_dec = Some(hist.counts.iter().map(|v| v.to_string()).collect());
    merged.hist.counts_encoding = "u64-decimal".into();
    merged.hist.overflow = hist.overflow;
    // refresh meta.ts to now
    merged.meta.ts = chrono::Utc::now().to_rfc3339();
    Ok(merged)
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Run(a)) => {
            let out = run_real(&a)?;
            if let Some(j) = &a.json {
                write_json_pretty(j, &out)?;
            }
            if let Some(m) = &a.report {
                let mut md = String::new();
                md.push_str("# PV Probe\n\n");
                md.push_str(&format!(
                    "- cand: {}\n- book: {}\n- mode: {}\n- threads: {}\n- workers: {}\n- tt: {}\n\n",
                    out.params.cand,
                    out.params.book,
                    out.params.mode,
                    out.params.threads,
                    out.params.workers,
                    out.params.tt_mode,
                ));
                md.push_str(&format!(
                    "## Summary\n- samples: {}\n- p50: {} cp\n- p90: {} cp\n- max: {} cp\n",
                    out.stats.samples, out.stats.p50_cp, out.stats.p90_cp, out.stats.max_cp
                ));
                if m == "-" {
                    println!("{}", md);
                } else {
                    fs::write(m, md)?;
                }
            }
            if a.json.is_none() && a.report.is_none() {
                eprintln!(
                    "pv_probe: samples={} p90={}cp (mode={}, tt={})",
                    out.stats.samples, out.stats.p90_cp, out.params.mode, out.params.tt_mode
                );
            }
        }
        Some(Commands::Merge(a)) => {
            let out = merge_real(&a)?;
            write_json_pretty(&a.out, &out)?;
            eprintln!(
                "[ok] merged {} files -> {} (samples={}, p90={}cp)",
                a.inputs.len(),
                a.out,
                out.stats.samples,
                out.stats.p90_cp
            );
        }
        Some(Commands::Validate(a)) => {
            if a.inputs.len() < 2 {
                return Err(anyhow!("need >=2 inputs"));
            }
            let mut ok = true;
            let mut it = a.inputs.iter();
            let first = it.next().unwrap();
            let base: OutJsonV2 = serde_json::from_str(&fs::read_to_string(first)?)?;
            for p in it {
                let next: OutJsonV2 = serde_json::from_str(&fs::read_to_string(p)?)?;
                if let Err(e) = validate_compat(&base, &next) {
                    ok = false;
                    if a.warn {
                        eprintln!("[warn] {}: {}", p, e);
                    } else {
                        return Err(anyhow!("{}: {}", p, e));
                    }
                }
            }
            if ok {
                eprintln!("[ok] compatible ({} files)", a.inputs.len());
            }
        }
        None => {
            // back-compat: behave as run with flattened args
            let out = run_real(&cli.run_args)?;
            if let Some(j) = &cli.run_args.json {
                write_json_pretty(j, &out)?;
            }
            if let Some(m) = &cli.run_args.report {
                let mut md = String::new();
                md.push_str("# PV Probe\n\n");
                md.push_str(&format!(
                    "- cand: {}\n- book: {}\n- mode: {}\n- threads: {}\n- workers: {}\n- tt: {}\n\n",
                    out.params.cand,
                    out.params.book,
                    out.params.mode,
                    out.params.threads,
                    out.params.workers,
                    out.params.tt_mode,
                ));
                md.push_str(&format!(
                    "## Summary\n- samples: {}\n- p50: {} cp\n- p90: {} cp\n- max: {} cp\n",
                    out.stats.samples, out.stats.p50_cp, out.stats.p90_cp, out.stats.max_cp
                ));
                if m == "-" {
                    println!("{}", md);
                } else {
                    fs::write(m, md)?;
                }
            }
            if cli.run_args.json.is_none() && cli.run_args.report.is_none() {
                eprintln!(
                    "pv_probe: samples={} p90={}cp (mode={}, tt={})",
                    out.stats.samples, out.stats.p90_cp, out.params.mode, out.params.tt_mode
                );
            }
        }
    }
    Ok(())
}
