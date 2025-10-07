use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{bail, ensure, Context, Result};
use clap::{Parser, ValueEnum};
use engine_core::evaluation::evaluate::MaterialEvaluator;
use engine_core::search::parallel::{ParallelSearcher, StopController};
use engine_core::search::{SearchLimitsBuilder, SearchResult, TranspositionTable};
use engine_core::shogi::Position;
use serde::Serialize;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "lazy_smp_benchmark",
    disable_help_subcommand = true,
    about = "Lazy SMP 並列探索のスケーリングを計測するシンプルなベンチマーク",
    version
)]
struct Args {
    /// 計測するスレッド数（カンマ区切り）
    #[arg(long, default_value = "1,2,4", value_name = "LIST")]
    threads: String,

    /// 1回の探索に与える時間（ミリ秒）
    #[arg(long = "fixed-total-ms", default_value_t = 200, value_name = "MS")]
    fixed_total_ms: u64,

    /// 各スレッド数での繰り返し回数
    #[arg(long, default_value_t = 3)]
    iterations: u32,

    /// ベンチ対象のSFENファイル（1行1局面）。未指定時は組み込みセットを使用
    #[arg(long, value_name = "FILE")]
    sfens: Option<PathBuf>,

    /// トランスポジションテーブルのサイズ（MB）（ベンチは32〜64MBを推奨）
    #[arg(long = "tt-mb", default_value_t = 64, value_name = "MB")]
    tt_mb: usize,

    /// 深さ制限（任意）
    #[arg(long = "depth", value_name = "PLY")]
    depth: Option<u8>,

    /// ノード制限（任意）
    #[arg(long = "nodes", value_name = "NODES")]
    nodes: Option<u64>,

    /// MultiPV 数
    #[arg(long, default_value_t = 1, value_name = "K")]
    multipv: u8,

    /// ヘルパースレッドのジッタ設定
    #[arg(long, value_enum, default_value = "auto")]
    jitter: JitterMode,

    /// JSON 出力ファイル（"-" で標準出力）
    #[arg(long, value_name = "FILE")]
    json: Option<String>,

    /// stdout でのサマリ出力を抑制
    #[arg(long, default_value_t = false)]
    quiet: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum, Serialize)]
enum JitterMode {
    Auto,
    On,
    Off,
}

#[derive(Debug, Serialize)]
struct ThreadReport {
    threads: usize,
    searches: u32,
    total_nodes: u64,
    total_qnodes: u64,
    total_elapsed_ms: f64,
    avg_nps: f64,
    max_depth: u32,
    max_seldepth: u32,
    mean_helper_share_pct: Option<f64>,
    efficiency_pct: Option<f64>,
}

#[derive(Debug, Serialize)]
struct ReportSettings {
    threads: Vec<usize>,
    fixed_total_ms: u64,
    iterations: u32,
    positions: usize,
    tt_mb: usize,
    depth: Option<u8>,
    nodes: Option<u64>,
    multipv: u8,
    jitter: JitterMode,
}

#[derive(Debug, Serialize)]
struct BenchmarkReport {
    settings: ReportSettings,
    results: Vec<ThreadReport>,
}

struct Aggregate {
    threads: usize,
    searches: u32,
    total_nodes: u128,
    total_qnodes: u128,
    total_elapsed: f64,
    max_depth: u32,
    max_seldepth: u32,
    helper_sum: f64,
    helper_count: u64,
}

impl Aggregate {
    fn new(threads: usize) -> Self {
        Self {
            threads,
            searches: 0,
            total_nodes: 0,
            total_qnodes: 0,
            total_elapsed: 0.0,
            max_depth: 0,
            max_seldepth: 0,
            helper_sum: 0.0,
            helper_count: 0,
        }
    }

    fn add(&mut self, result: &SearchResult) {
        self.searches += 1;
        self.total_nodes += result.nodes as u128;
        self.total_qnodes += result.stats.qnodes as u128;
        self.total_elapsed += result.stats.elapsed.as_secs_f64();
        self.max_depth = self.max_depth.max(result.depth);
        self.max_seldepth = self.max_seldepth.max(result.seldepth);
        if let Some(h) = result.stats.helper_share_pct {
            self.helper_sum += h;
            self.helper_count += 1;
        }
    }

    fn finalize(&self, baseline_nps: Option<f64>) -> ThreadReport {
        let avg_nps = if self.total_elapsed > 0.0 {
            (self.total_nodes as f64) / self.total_elapsed
        } else {
            0.0
        };
        let efficiency_pct = baseline_nps.map(|base| {
            if base <= f64::EPSILON {
                0.0
            } else {
                (avg_nps / (base * self.threads as f64)) * 100.0
            }
        });
        ThreadReport {
            threads: self.threads,
            searches: self.searches,
            total_nodes: (self.total_nodes.min(u128::from(u64::MAX))) as u64,
            total_qnodes: (self.total_qnodes.min(u128::from(u64::MAX))) as u64,
            total_elapsed_ms: self.total_elapsed * 1000.0,
            avg_nps,
            max_depth: self.max_depth,
            max_seldepth: self.max_seldepth,
            mean_helper_share_pct: if self.helper_count > 0 {
                Some(self.helper_sum / self.helper_count as f64)
            } else {
                None
            },
            efficiency_pct,
        }
    }
}

fn main() -> Result<()> {
    env_logger::builder().format_timestamp(None).init();
    let args = Args::parse();

    let threads = parse_threads(&args.threads)?;
    ensure!(!threads.is_empty(), "threads list must not be empty");

    let positions = load_positions(args.sfens.as_ref())?;
    ensure!(!positions.is_empty(), "no positions available for benchmarking");

    let mut reports = Vec::new();
    let mut aggregates = Vec::new();

    for &threads_count in &threads {
        if !args.quiet {
            let total_runs = args.iterations as usize * positions.len();
            println!(
                "[threads={}] running {} searches ({} iterations x {} positions)...",
                threads_count,
                total_runs,
                args.iterations,
                positions.len()
            );
        }
        let mut aggregate = Aggregate::new(threads_count);
        run_benchmark_for_threads(threads_count, &positions, &args, &mut aggregate)?;
        aggregates.push(aggregate);
    }

    let baseline_nps = aggregates.first().map(|agg| {
        if agg.total_elapsed > 0.0 {
            (agg.total_nodes as f64) / agg.total_elapsed
        } else {
            0.0
        }
    });

    for aggregate in &aggregates {
        reports.push(aggregate.finalize(baseline_nps));
    }

    if !args.quiet {
        println!("\n=== Summary ===");
        for report in &reports {
            let efficiency = report
                .efficiency_pct
                .map(|v| format!("{v:.1}%"))
                .unwrap_or_else(|| "N/A".to_string());
            println!(
                "threads={:>2} | searches={:>4} | avg_nps={:>10.0} | elapsed={:>7.1} ms | max_depth={:>2} | helper_share={}",
                report.threads,
                report.searches,
                report.avg_nps,
                report.total_elapsed_ms,
                report.max_depth,
                report
                    .mean_helper_share_pct
                    .map(|h| format!("{h:.2}%"))
                    .unwrap_or_else(|| "N/A".into())
            );
            println!("             efficiency vs baseline: {}", efficiency);
        }
    }

    if let Some(json_path) = args.json.as_ref() {
        let settings = ReportSettings {
            threads: threads.clone(),
            fixed_total_ms: args.fixed_total_ms,
            iterations: args.iterations,
            positions: positions.len(),
            tt_mb: args.tt_mb,
            depth: args.depth,
            nodes: args.nodes,
            multipv: args.multipv,
            jitter: args.jitter,
        };
        let payload = BenchmarkReport {
            settings,
            results: reports,
        };
        write_json(json_path, &payload)?;
    }

    Ok(())
}

fn run_benchmark_for_threads(
    threads: usize,
    positions: &[Position],
    args: &Args,
    aggregate: &mut Aggregate,
) -> Result<()> {
    let evaluator = Arc::new(MaterialEvaluator);
    let tt = Arc::new(TranspositionTable::new(args.tt_mb));
    let stop_ctrl = Arc::new(StopController::new());
    let mut searcher = ParallelSearcher::<MaterialEvaluator>::new(
        Arc::clone(&evaluator),
        Arc::clone(&tt),
        threads,
        Arc::clone(&stop_ctrl),
    );

    let mut session_counter: u64 = 1;
    for iter in 0..args.iterations {
        for pos_template in positions {
            let mut pos = pos_template.clone();
            let session_id = ((threads as u64) << 32) | ((iter as u64) << 16) | session_counter;
            session_counter = session_counter.wrapping_add(1);

            let stop_flag = Arc::new(AtomicBool::new(false));
            stop_ctrl.publish_session(Some(&stop_flag), session_id);

            let mut builder = SearchLimitsBuilder::default()
                .session_id(session_id)
                .stop_flag(Arc::clone(&stop_flag))
                .multipv(args.multipv);

            builder = builder.fixed_time_ms(args.fixed_total_ms);
            if let Some(depth) = args.depth {
                builder = builder.depth(depth);
            }
            if let Some(nodes) = args.nodes {
                builder = builder.nodes(nodes);
            }
            match args.jitter {
                JitterMode::Auto => {}
                JitterMode::On => {
                    builder = builder.jitter_override(true);
                }
                JitterMode::Off => {
                    builder = builder.jitter_override(false);
                }
            }

            let mut limits = builder.build();
            // SearchLimitsBuilder::build は start_time を Instant::now() で初期化済みだが、
            // 念のため再設定して計測の一貫性を保持する。
            limits.start_time = std::time::Instant::now();

            let result = searcher.search(&mut pos, limits);
            aggregate.add(&result);
        }
    }

    Ok(())
}

fn parse_threads(spec: &str) -> Result<Vec<usize>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in spec.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: usize =
            trimmed.parse().with_context(|| format!("invalid thread count '{trimmed}'"))?;
        if value == 0 {
            bail!("thread count must be >= 1");
        }
        if seen.insert(value) {
            out.push(value);
        }
    }
    Ok(out)
}

fn load_positions(path: Option<&PathBuf>) -> Result<Vec<Position>> {
    if let Some(p) = path {
        parse_positions_file(p)
    } else {
        Ok(default_positions())
    }
}

fn parse_positions_file(path: &PathBuf) -> Result<Vec<Position>> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read positions file: {}", path.display()))?;
    let mut positions = Vec::new();
    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let sfen = line.strip_prefix("sfen ").unwrap_or(line).trim();
        let pos = parse_single_position(sfen)
            .with_context(|| format!("invalid SFEN at line {}", idx + 1))?;
        positions.push(pos);
    }
    ensure!(!positions.is_empty(), "positions file did not contain any valid SFEN lines");
    Ok(positions)
}

fn parse_single_position(sfen: &str) -> Result<Position> {
    if sfen.eq_ignore_ascii_case("startpos") {
        return Ok(Position::startpos());
    }
    Position::from_sfen(sfen).map_err(|e| anyhow::anyhow!("{e:?}"))
}

fn default_positions() -> Vec<Position> {
    const DEFAULT_SFENS: &[&str] = &[
        "startpos",
        "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        "ln1g1g1nl/1ks2r3/1pppp1bpp/p6p1/9/P1P4P1/1P1PPPP1P/1BK1GS1R1/LNSG3NL b Pp 1",
        "l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w GR5pnsg 1",
        "8l/7p1/6gk1/5Sp1p/9/5G1PP/7K1/9/7NL b RBG2S2N2L13P2rbgsnl 1",
    ];
    DEFAULT_SFENS.iter().filter_map(|s| parse_single_position(s).ok()).collect()
}

fn write_json(path: &str, payload: &BenchmarkReport) -> Result<()> {
    let json = serde_json::to_string_pretty(payload)?;
    if path == "-" {
        println!("{}", json);
    } else {
        fs::write(path, json).with_context(|| format!("failed to write JSON report to {path}"))?;
    }
    Ok(())
}
