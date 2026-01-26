//! 教師データ生成ツール
//!
//! SFENファイルから局面を読み込み、エンジンで探索して評価値付きの教師データを生成する。
//!
//! # 使用例
//!
//! ```bash
//! # 深さ10で教師データを生成
//! cargo run -p tools --bin generate_training_data -- \
//!   --input sfens.txt --output train.jsonl --depth 10
//!
//! # ノード数100000で教師データを生成（並列4スレッド）
//! cargo run -p tools --bin generate_training_data -- \
//!   --input sfens.txt --output train.jsonl --nodes 100000 --threads 4
//!
//! # NNUEモデルを指定して生成
//! cargo run -p tools --bin generate_training_data -- \
//!   --input sfens.txt --output train.jsonl --depth 8 --nnue model.nnue
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde::Serialize;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rshogi_core::nnue::init_nnue;
use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search, SearchInfo};

#[derive(Parser)]
#[command(
    name = "generate-training-data",
    version,
    about = "SFENファイルから教師データを生成\n\nエンジンで探索して評価値・最善手を出力"
)]
struct Cli {
    /// 入力SFENファイル（1行1SFEN）
    #[arg(short, long)]
    input: PathBuf,

    /// 出力ファイル（JSONL形式）
    #[arg(short, long)]
    output: PathBuf,

    /// 探索深さ（0=使用しない）
    #[arg(long, default_value_t = 0)]
    depth: i32,

    /// 探索ノード数（0=使用しない）
    #[arg(long, default_value_t = 0)]
    nodes: u64,

    /// 探索時間（ミリ秒、0=使用しない）
    #[arg(long, default_value_t = 0)]
    movetime: i64,

    /// 並列スレッド数
    #[arg(short, long, default_value_t = 1)]
    threads: usize,

    /// NNUEモデルファイル（省略時は組み込みまたは駒得評価）
    #[arg(long)]
    nnue: Option<PathBuf>,

    /// 置換表サイズ（MB）
    #[arg(long, default_value_t = 256)]
    hash: usize,

    /// 処理する局面数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: usize,

    /// 詳細出力
    #[arg(short, long)]
    verbose: bool,
}

/// 教師データの1レコード
#[derive(Serialize)]
struct TrainingRecord {
    /// SFEN文字列
    sfen: String,
    /// 評価値（センチポーン）
    score: i32,
    /// 探索深さ
    depth: i32,
    /// 最善手（USI形式）
    best_move: String,
    /// 探索ノード数
    nodes: u64,
}

/// 処理中にCtrl-Cが押されたかを追跡
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    // 入力ファイルの存在確認
    if !cli.input.exists() {
        anyhow::bail!("Input file not found: {}", cli.input.display());
    }

    // 探索制限の検証
    if cli.depth == 0 && cli.nodes == 0 && cli.movetime == 0 {
        anyhow::bail!("At least one of --depth, --nodes, or --movetime must be specified");
    }

    // Ctrl-Cハンドラを設定
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted, finishing current positions...");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // NNUEモデルの読み込み
    if let Some(nnue_path) = &cli.nnue {
        if !nnue_path.exists() {
            anyhow::bail!("NNUE file not found: {}", nnue_path.display());
        }
        match init_nnue(nnue_path) {
            Ok(()) => {
                eprintln!("Loaded NNUE model: {}", nnue_path.display());
            }
            Err(e) => {
                eprintln!("Warning: Failed to load NNUE model: {e}");
                eprintln!("Using fallback evaluation");
            }
        }
    }

    // 入力SFENを読み込み
    let sfens = load_sfens(&cli.input, cli.limit)?;
    let total = sfens.len();
    eprintln!("Loaded {total} positions from {}", cli.input.display());

    if total == 0 {
        eprintln!("No positions to process");
        return Ok(());
    }

    // 進捗バー設定
    let progress = ProgressBar::new(total as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) ETA: {eta}")
            .expect("valid template"),
    );

    // 統計カウンタ
    let processed = AtomicU64::new(0);
    let errors = AtomicU64::new(0);

    // 並列処理用のスレッドプールを設定
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cli.threads)
        .build()
        .context("Failed to build thread pool")?;

    // 結果を収集
    let results: Vec<Option<TrainingRecord>> = pool.install(|| {
        sfens
            .par_iter()
            .map(|sfen| {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    return None;
                }

                let result = process_position(sfen, &cli);
                processed.fetch_add(1, Ordering::Relaxed);
                progress.inc(1);

                match result {
                    Ok(record) => Some(record),
                    Err(e) => {
                        errors.fetch_add(1, Ordering::Relaxed);
                        if cli.verbose {
                            log::warn!("Failed to process position: {e}");
                        }
                        None
                    }
                }
            })
            .collect()
    });

    progress.finish();

    // 結果を出力
    let out_file = File::create(&cli.output)
        .with_context(|| format!("Failed to create {}", cli.output.display()))?;
    let mut writer = BufWriter::new(out_file);
    let mut written = 0usize;

    for record in results.into_iter().flatten() {
        let json = serde_json::to_string(&record).context("Failed to serialize record")?;
        writeln!(writer, "{json}").context("Failed to write record")?;
        written += 1;
    }

    writer.flush()?;

    let processed_count = processed.load(Ordering::Relaxed);
    let error_count = errors.load(Ordering::Relaxed);

    eprintln!("Processed: {processed_count}, Written: {written}, Errors: {error_count}");
    eprintln!("Output: {}", cli.output.display());

    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("Note: Processing was interrupted");
    }

    Ok(())
}

/// SFENファイルを読み込む
fn load_sfens(path: &PathBuf, limit: usize) -> Result<Vec<String>> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut sfens = Vec::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        sfens.push(trimmed.to_string());

        if limit > 0 && sfens.len() >= limit {
            break;
        }
    }

    Ok(sfens)
}

/// 1局面を処理して教師データを生成
fn process_position(sfen: &str, cli: &Cli) -> Result<TrainingRecord> {
    // 局面を設定
    let mut pos = Position::new();
    pos.set_sfen(sfen).map_err(|e| anyhow::anyhow!("Failed to parse SFEN: {e}"))?;

    // 探索エンジンを作成（各スレッドで独立したインスタンス）
    let mut search = Search::new(cli.hash);

    // 探索制限を設定
    let mut limits = LimitsType::default();
    if cli.depth > 0 {
        limits.depth = cli.depth;
    }
    if cli.nodes > 0 {
        limits.nodes = cli.nodes;
    }
    if cli.movetime > 0 {
        limits.movetime = cli.movetime;
    }

    // 探索実行
    let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);

    // 結果を記録
    Ok(TrainingRecord {
        sfen: sfen.to_string(),
        score: result.score.raw(),
        depth: result.depth,
        best_move: result.best_move.to_usi(),
        nodes: result.nodes,
    })
}
