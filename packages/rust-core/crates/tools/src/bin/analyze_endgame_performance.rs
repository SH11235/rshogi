//! 終盤性能分析ツール
//!
//! 終盤局面での各エンジンタイプの探索性能を測定します。
//! 改良点:
//! - 複数局面の一括入力（--sfen-file）
//! - 反復実行（--repeat）と平均値算出
//! - JSON出力（--json-out）
//! - NNUE系で重み未指定時の警告

use anyhow::Result;
use clap::Parser;
use engine_core::{
    engine::controller::{Engine, EngineType},
    search::SearchLimits,
    Position,
};
use log::info;
use serde::Serialize;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::time::Instant;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Analyze endgame performance for different engine types"
)]
struct Args {
    /// SFEN string of the position to analyze
    #[arg(short, long)]
    sfen: String,

    /// Load SFENs from a file (one per line; lines may start with 'sfen ')
    #[arg(long)]
    sfen_file: Option<String>,

    /// Time limit per engine in milliseconds
    #[arg(short, long, default_value = "10000")]
    time_ms: u64,

    /// NNUE weights file path (optional)
    #[arg(short = 'w', long)]
    weights: Option<String>,

    /// Repeat count per position (take mean over repeats)
    #[arg(long, default_value = "1")]
    repeat: usize,

    /// Output JSON path (summary)
    #[arg(long)]
    json_out: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
struct EngineStats {
    best_move: Option<String>,
    score: i32,
    depth: u32,
    nodes: u64,
    nps: f64,
    time_ms: u64,
}

#[derive(Debug, Serialize, Clone)]
struct PositionReport {
    sfen: String,
    time_ms: u64,
    per_engine: std::collections::BTreeMap<String, EngineStats>,
}

fn analyze_engine_type(
    engine_type: EngineType,
    position: &mut Position,
    time_limit_ms: u64,
    weights_path: Option<&str>,
) -> Result<EngineStats> {
    info!("Analyzing {:?} engine...", engine_type);

    let mut engine = Engine::new(engine_type);

    // Load NNUE weights if needed and provided
    if matches!(engine_type, EngineType::Nnue | EngineType::EnhancedNnue) {
        match weights_path {
            Some(path) => {
                engine
                    .load_nnue_weights(path)
                    .map_err(|e| anyhow::anyhow!("Failed to load NNUE weights: {}", e))?;
                info!("Loaded NNUE weights from {}", path);
            }
            None => {
                eprintln!(
                    "[warn] NNUE/EnhancedNnue selected but --weights not provided; results may be meaningless"
                );
            }
        }
    }

    let limits = SearchLimits::builder().fixed_time_ms(time_limit_ms).build();

    let start = Instant::now();
    let result = engine.search(position, limits);
    let elapsed = start.elapsed();

    let nps = result.stats.nodes as f64 / elapsed.as_secs_f64();
    info!("Results for {:?}:", engine_type);
    info!("  Best move: {:?}", result.best_move);
    info!("  Score: {}", result.score);
    info!("  Depth reached: {}", result.stats.depth);
    info!("  Nodes searched: {}", result.stats.nodes);
    info!("  NPS: {:.0}", nps);
    info!("  Time: {:?}", elapsed);
    info!("");

    Ok(EngineStats {
        best_move: result.best_move.map(|m| format!("{}", m)),
        score: result.score,
        depth: result.stats.depth as u32,
        nodes: result.stats.nodes,
        nps,
        time_ms: (elapsed.as_secs_f64() * 1000.0) as u64,
    })
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    // Load SFENs
    let mut sfens: Vec<String> = Vec::new();
    if let Some(file) = args.sfen_file.as_ref() {
        let rd = BufReader::new(File::open(file)?);
        for line in rd.lines() {
            let mut s = line?;
            if s.trim().is_empty() { continue; }
            if let Some(idx) = s.find("sfen ") { s = s[idx+5..].to_string(); }
            sfens.push(s);
        }
    }
    if !args.sfen.is_empty() { sfens.push(args.sfen.clone()); }
    if sfens.is_empty() {
        return Err(anyhow::anyhow!("no SFEN provided (use --sfen or --sfen-file)"));
    }

    // Test each engine type
    let engine_types = [
        EngineType::Material,
        EngineType::Enhanced,
        EngineType::Nnue,
        EngineType::EnhancedNnue,
    ];

    let mut reports: Vec<PositionReport> = Vec::new();
    for sfen in sfens {
        let position = Position::from_sfen(&sfen)
            .map_err(|e| anyhow::anyhow!("Failed to parse SFEN: {}", e))?;
        info!("Analyzing position: {} (ply={})", sfen, position.ply);

        let mut agg: std::collections::BTreeMap<String, Vec<EngineStats>> = Default::default();
        for &engine_type in &engine_types {
            for _ in 0..args.repeat {
                let mut pos_clone = position.clone();
                match analyze_engine_type(
                    engine_type,
                    &mut pos_clone,
                    args.time_ms,
                    args.weights.as_deref(),
                ) {
                    Ok(stat) => {
                        agg.entry(format!("{:?}", engine_type))
                            .or_default()
                            .push(stat);
                    }
                    Err(e) => eprintln!("Error analyzing {:?}: {}", engine_type, e),
                }
            }
        }

        // reduce to means
        let mut per_engine: std::collections::BTreeMap<String, EngineStats> = Default::default();
        for (k, v) in agg {
            let n = v.len() as f64;
            let mean = |f: fn(&EngineStats) -> f64| v.iter().map(f).sum::<f64>() / n;
            let depth = (v.iter().map(|x| x.depth as f64).sum::<f64>() / n).round() as u32;
            let nodes = v.iter().map(|x| x.nodes as f64).sum::<f64>() / n;
            per_engine.insert(
                k,
                EngineStats {
                    best_move: v.last().and_then(|x| x.best_move.clone()),
                    score: (mean(|x| x.score as f64)).round() as i32,
                    depth,
                    nodes: nodes as u64,
                    nps: mean(|x| x.nps),
                    time_ms: mean(|x| x.time_ms as f64) as u64,
                },
            );
        }

        reports.push(PositionReport {
            sfen,
            time_ms: args.time_ms,
            per_engine,
        });
    }

    if let Some(path) = args.json_out.as_ref() {
        let mut f = File::create(path)?;
        write!(f, "{}", serde_json::to_string_pretty(&reports)?)?;
        eprintln!("[info] wrote JSON: {}", path);
    }

    Ok(())
}
