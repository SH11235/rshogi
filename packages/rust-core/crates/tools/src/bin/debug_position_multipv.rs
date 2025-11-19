use anyhow::{Context, Result};
use clap::Parser;
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use serde_json::to_writer_pretty;
use std::fs::File;
use std::io::BufWriter;
use std::path::PathBuf;
use tools::usi_multipv::{run_multipv_analysis, AnalysisOutput, EngineConfig, PositionSpec, Score, SearchConfig};

#[derive(Parser, Debug)]
#[command(
    name = "debug_position_multipv",
    about = "Analyze a position with MultiPV search"
)]
struct Args {
    /// SFEN string of the position to analyze (no leading `sfen`), or full `position sfen ... moves ...`
    #[arg(long)]
    sfen: String,

    /// Time limit(s) per search in milliseconds (can be specified multiple times)
    #[arg(long = "time-ms", value_name = "TIME_MS")]
    time_ms: Vec<u64>,

    /// MultiPV count
    #[arg(long)]
    multipv: u8,

    /// Path to engine-usi binary
    #[arg(long = "engine-path", default_value = "target/release/engine-usi")]
    engine_path: String,

    /// Threads setoption value
    #[arg(long, default_value_t = 1)]
    threads: u32,

    /// Hash size in MB (USI_Hash)
    #[arg(long = "hash-mb", default_value_t = 256)]
    hash_mb: u32,

    /// Engine type (enhanced|enhanced_nnue|nnue|material)
    #[arg(long = "engine-type")]
    engine_type: Option<String>,

    /// Profile preset (base|short|gates|custom)
    #[arg(long)]
    profile: Option<String>,

    /// Optional JSON output path
    #[arg(long = "out-json")]
    out_json: Option<PathBuf>,

    /// Optional tag for analysis
    #[arg(long)]
    tag: Option<String>,

    /// Optional raw USI log path
    #[arg(long = "raw-log")]
    raw_log: Option<String>,
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    if args.time_ms.is_empty() {
        eprintln!("error: at least one --time-ms <TIME_MS> must be specified");
        std::process::exit(1);
    }

    let position = if args.sfen.trim_start().starts_with("position ") {
        PositionSpec::FullCommand(args.sfen.clone())
    } else if args.sfen.trim_start().starts_with("sfen ") {
        PositionSpec::FullCommand(args.sfen.clone())
    } else {
        PositionSpec::BareSfen(args.sfen.clone())
    };

    let engine_cfg = EngineConfig {
        engine_path: args.engine_path.clone(),
        engine_type: args.engine_type.clone(),
        threads: args.threads,
        hash_mb: args.hash_mb,
        profile: args.profile.clone(),
        extra_env: {
            let mut envs = Vec::new();
            if let Some(ref profile) = args.profile {
                match profile.as_str() {
                    // short: Quiet SEE Guard ON, capture futility scale 強め
                    "short" => {
                        envs.push(("SHOGI_QUIET_SEE_GUARD".to_string(), "1".to_string()));
                        envs.push(("SHOGI_CAPTURE_FUT_SCALE".to_string(), "120".to_string()));
                    }
                    // gates: Quiet SEE Guard OFF
                    "gates" => {
                        envs.push(("SHOGI_QUIET_SEE_GUARD".to_string(), "0".to_string()));
                    }
                    // base/rootfull/custom/その他: env 変更なし
                    _ => {}
                }
            }
            envs
        },
    };

    // ベースとなる SearchConfig（time_ms / tag / raw_log_path は後でジョブごとに差し替え）
    let base_search_cfg = SearchConfig {
        time_ms: 0,
        multipv: args.multipv,
        position,
        tag: None,
        raw_log_path: None,
    };

    // time-ms が 1 つだけなら従来どおり単発実行
    if args.time_ms.len() == 1 {
        let t = args.time_ms[0];
        let mut search_cfg = base_search_cfg.clone();
        search_cfg.time_ms = t;
        search_cfg.tag = args.tag.clone();
        search_cfg.raw_log_path = args.raw_log.clone();

        let result = run_multipv_analysis(&engine_cfg, &search_cfg)?;
        print_result(&result);

        if let Some(path) = args.out_json {
            let file = File::create(path)?;
            let writer = BufWriter::new(file);
            to_writer_pretty(writer, &result)?;
        }
        return Ok(());
    }

    // 複数 time-ms をまとめて並列実行
    let times = args.time_ms.clone();
    let logical_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let reserved_for_os = 1usize;
    let usable_cpus = logical_cpus.saturating_sub(reserved_for_os).max(1);
    let engine_threads = args.threads.max(1) as usize;
    let max_parallel_engines = (usable_cpus / engine_threads).max(1);
    let parallel_workers = max_parallel_engines.min(times.len()).max(1);

    let jobs: Vec<(usize, u64)> = times.iter().copied().enumerate().collect();

    let pool = ThreadPoolBuilder::new()
        .num_threads(parallel_workers)
        .build()
        .context("failed to build rayon thread pool")?;

    let mut indexed_results =
        pool.install(|| -> Result<Vec<(usize, AnalysisOutput)>> {
            jobs.par_iter()
                .map(|(idx, time_ms)| {
                    let mut cfg = base_search_cfg.clone();
                    cfg.time_ms = *time_ms;
                    // バッチ時は tag / raw-log に time_ms を付けて区別しやすくする
                    cfg.tag = args
                        .tag
                        .as_ref()
                        .map(|t| format!("{t}-t{time_ms}ms"));
                    cfg.raw_log_path = args
                        .raw_log
                        .as_ref()
                        .map(|base| format!("{base}.t{time_ms}ms"));

                    let result = run_multipv_analysis(&engine_cfg, &cfg)
                        .with_context(|| format!("failed analysis for time-ms={time_ms}"))?;
                    Ok((*idx, result))
                })
                .collect()
        })?;

    indexed_results.sort_by_key(|(idx, _)| *idx);
    let results: Vec<AnalysisOutput> =
        indexed_results.into_iter().map(|(_, r)| r).collect();

    for (i, result) in results.iter().enumerate() {
        if i > 0 {
            println!();
        }
        print_result(result);
    }

    if let Some(path) = args.out_json {
        let file = File::create(path)?;
        let writer = BufWriter::new(file);
        to_writer_pretty(writer, &results)?;
    }

    Ok(())
}

fn print_result(result: &AnalysisOutput) {
    println!("SFEN: {}", result.sfen);
    if let Some(ref eng) = result.engine_type {
        println!("Engine: {}", eng);
    } else {
        println!("Engine: (default)");
    }
    println!(
        "Time: {} ms (actual {:.3}s)",
        result.time_ms,
        (result.actual_ms as f64) / 1000.0
    );
    println!("Depth: {}", result.depth);
    println!("Nodes: {}", result.nodes);
    if let Some(ref tag) = result.tag {
        println!("Tag: {}", tag);
    }

    println!();
    println!("=== MultiPV ===");
    for line in &result.multipv {
        let score_str = match line.score {
            Score::Cp(v) => {
                if v >= 0 {
                    format!("+{}", v)
                } else {
                    v.to_string()
                }
            }
            Score::Mate(v) => format!("mate{}", v),
        };
        let pv_str = line.pv.join(" ");
        println!(
            "#{}: score={} depth={} pv={}",
            line.rank, score_str, line.depth, pv_str
        );
    }
}
