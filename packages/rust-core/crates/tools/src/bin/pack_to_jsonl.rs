//! pack_to_jsonl - PackedSfenValue形式をJSONL形式に変換
//!
//! YaneuraOuのpack形式（PackedSfenValue）を読み込み、JSONL形式に変換する。
//!
//! # 使用例
//!
//! ```bash
//! # 基本的な使用法
//! cargo run -p tools --bin pack_to_jsonl -- \
//!   --input data.pack --output train.jsonl
//!
//! # 処理数を制限
//! cargo run -p tools --bin pack_to_jsonl -- \
//!   --input data.pack --output train.jsonl --limit 10000
//!
//! # 詳細出力
//! cargo run -p tools --bin pack_to_jsonl -- \
//!   --input data.pack --output train.jsonl --verbose
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use tools::packed_sfen::{move16_to_usi, unpack_sfen, PackedSfenValue};

#[derive(Parser)]
#[command(
    name = "pack_to_jsonl",
    version,
    about = "PackedSfenValue形式をJSONL形式に変換\n\nYaneuraOuのpack形式を読み込み、SFEN + 評価値のJSONLを出力"
)]
struct Cli {
    /// 入力packファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力ファイル（JSONL形式）
    #[arg(short, long)]
    output: PathBuf,

    /// 処理するレコード数の上限（0=無制限）
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
    /// 探索深さ（pack形式には含まれないため0固定）
    depth: i32,
    /// 最善手（USI形式）
    best_move: String,
    /// 探索ノード数（pack形式には含まれないため0固定）
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

    // Ctrl-Cハンドラを設定
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted, finishing current position...");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // 入力ファイルサイズからレコード数を推定
    let file_size = std::fs::metadata(&cli.input)?.len();
    let estimated_records = file_size / PackedSfenValue::SIZE as u64;
    eprintln!(
        "Input file: {} ({} bytes, estimated {} records)",
        cli.input.display(),
        file_size,
        estimated_records
    );

    // 進捗バー設定
    let total = if cli.limit > 0 {
        std::cmp::min(cli.limit as u64, estimated_records)
    } else {
        estimated_records
    };
    let progress = ProgressBar::new(total);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) ETA: {eta}")
            .expect("valid template"),
    );

    // 統計カウンタ
    let processed = AtomicU64::new(0);
    let errors = AtomicU64::new(0);
    let written = AtomicU64::new(0);

    // ファイル処理
    let in_file = File::open(&cli.input)
        .with_context(|| format!("Failed to open {}", cli.input.display()))?;
    let mut reader = BufReader::new(in_file);

    let out_file = File::create(&cli.output)
        .with_context(|| format!("Failed to create {}", cli.output.display()))?;
    let mut writer = BufWriter::new(out_file);

    let mut buffer = [0u8; PackedSfenValue::SIZE];

    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }

        if cli.limit > 0 && processed.load(Ordering::Relaxed) >= cli.limit as u64 {
            break;
        }

        // 40バイト読み込み
        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        processed.fetch_add(1, Ordering::Relaxed);
        progress.inc(1);

        // PackedSfenValueを解析
        let psv = match PackedSfenValue::from_bytes(&buffer) {
            Some(v) => v,
            None => {
                errors.fetch_add(1, Ordering::Relaxed);
                if cli.verbose {
                    log::warn!(
                        "Failed to parse PackedSfenValue at position {}",
                        processed.load(Ordering::Relaxed)
                    );
                }
                continue;
            }
        };

        // SFEN文字列に変換
        let sfen = match unpack_sfen(&psv.sfen) {
            Ok(s) => s,
            Err(e) => {
                errors.fetch_add(1, Ordering::Relaxed);
                if cli.verbose {
                    log::warn!(
                        "Failed to unpack SFEN at position {}: {e}",
                        processed.load(Ordering::Relaxed)
                    );
                }
                continue;
            }
        };

        // Move16をUSI形式に変換
        let best_move = move16_to_usi(psv.move16);

        // TrainingRecordを作成
        let record = TrainingRecord {
            sfen,
            score: psv.score as i32,
            depth: 0,
            best_move,
            nodes: 0,
        };

        // JSON出力
        let json = serde_json::to_string(&record).context("Failed to serialize record")?;
        writeln!(writer, "{json}").context("Failed to write record")?;
        written.fetch_add(1, Ordering::Relaxed);
    }

    progress.finish();
    writer.flush()?;

    let processed_count = processed.load(Ordering::Relaxed);
    let error_count = errors.load(Ordering::Relaxed);
    let written_count = written.load(Ordering::Relaxed);

    eprintln!("Processed: {processed_count}, Written: {written_count}, Errors: {error_count}");
    eprintln!("Output: {}", cli.output.display());

    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("Note: Processing was interrupted");
    }

    if error_count > 0 {
        eprintln!("Note: {error_count} positions had errors and were skipped");
    }

    Ok(())
}
