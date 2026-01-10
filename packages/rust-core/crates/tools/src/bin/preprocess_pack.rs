//! preprocess_pack - packファイルにqsearch leaf置換を適用
//!
//! PackedSfenValue形式（40バイト/レコード）のpackファイルに対して
//! qsearch leaf置換を適用する。
//!
//! # 使用例
//!
//! ```bash
//! # 基本的な使用法（Material評価）
//! cargo run -p tools --bin preprocess_pack -- \
//!   --input data.pack --output processed.pack
//!
//! # NNUEモデルを使用
//! cargo run -p tools --bin preprocess_pack -- \
//!   --input data.pack --output processed.pack --nnue model.nnue
//!
//! # 並列処理（4スレッド）
//! cargo run -p tools --bin preprocess_pack -- \
//!   --input data.pack --output processed.pack --threads 4
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use engine_core::position::Position;
use tools::packed_sfen::{move_to_move16, pack_position, unpack_sfen, PackedSfenValue};
use tools::qsearch_pv::{qsearch_with_pv, Evaluator, MaterialEvaluator};

/// PackedSfenValue形式のpackファイルにqsearch leaf置換を適用
#[derive(Parser)]
#[command(
    name = "preprocess_pack",
    version,
    about = "packファイルにqsearch leaf置換を適用\n\n各局面をqsearchのPV末端局面に置換して出力"
)]
struct Cli {
    /// 入力packファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力packファイル
    #[arg(short, long)]
    output: PathBuf,

    /// qsearchの最大深さ
    #[arg(long, default_value_t = 32)]
    max_ply: i32,

    /// 並列処理スレッド数（0=自動）
    #[arg(short, long, default_value_t = 1)]
    threads: usize,

    /// NNUEモデルファイル（省略時はMaterial評価）
    #[arg(long)]
    nnue: Option<PathBuf>,

    /// 処理するレコード数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: u64,

    /// 詳細出力
    #[arg(short, long)]
    verbose: bool,
}

/// 処理中にCtrl-Cが押されたかを追跡
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// qsearchの初期alpha値
const QSEARCH_ALPHA_INIT: i32 = -30000;
/// qsearchの初期beta値
const QSEARCH_BETA_INIT: i32 = 30000;

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    // 入力ファイルの存在確認
    if !cli.input.exists() {
        anyhow::bail!("Input file not found: {}", cli.input.display());
    }

    // NNUEモデルの確認
    if let Some(ref nnue_path) = cli.nnue {
        if !nnue_path.exists() {
            anyhow::bail!("NNUE model file not found: {}", nnue_path.display());
        }
        eprintln!("Note: NNUE evaluation is not yet implemented, using Material evaluation");
    }

    // Ctrl-Cハンドラを設定
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted!");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // スレッド数を設定
    if cli.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
            .unwrap_or_else(|e| {
                eprintln!("Warning: Failed to set thread count: {e}");
            });
    }

    // 入力ファイルサイズからレコード数を計算
    let file_size = std::fs::metadata(&cli.input)?.len();
    let record_count = file_size / PackedSfenValue::SIZE as u64;

    if file_size % PackedSfenValue::SIZE as u64 != 0 {
        eprintln!(
            "Warning: File size ({file_size}) is not a multiple of record size ({}). Trailing bytes will be ignored.",
            PackedSfenValue::SIZE
        );
    }

    let process_count = if cli.limit > 0 && cli.limit < record_count {
        cli.limit
    } else {
        record_count
    };

    eprintln!(
        "Input file: {} ({} bytes, {} records)",
        cli.input.display(),
        file_size,
        record_count
    );
    eprintln!("Processing {} records with {} thread(s)", process_count, cli.threads);
    eprintln!("Max ply: {}", cli.max_ply);

    // 処理実行
    process_file(&cli, process_count)?;

    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("Note: Processing was interrupted, output may be incomplete");
    } else {
        eprintln!("Output: {}", cli.output.display());
    }

    Ok(())
}

/// ファイルを処理
fn process_file(cli: &Cli, process_count: u64) -> Result<()> {
    // 進捗バー設定
    let progress = ProgressBar::new(process_count);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    // 入力ファイルを読み込み
    let in_file = File::open(&cli.input)
        .with_context(|| format!("Failed to open {}", cli.input.display()))?;
    let mut reader = BufReader::new(in_file);

    // 全レコードを読み込み
    let mut records: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(process_count as usize);
    let mut buffer = [0u8; PackedSfenValue::SIZE];

    progress.set_message("Reading...");
    for _ in 0..process_count {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            return Ok(());
        }

        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        records.push(buffer);
    }

    let actual_count = records.len();
    eprintln!("Read {} records", actual_count);

    // エラーカウンタ
    let error_count = AtomicU64::new(0);
    let processed_count = AtomicU64::new(0);

    // qsearch leaf置換を並列で適用
    progress.set_message("Processing...");
    let evaluator = MaterialEvaluator;
    let max_ply = cli.max_ply;
    let verbose = cli.verbose;

    let processed_records: Vec<[u8; PackedSfenValue::SIZE]> = records
        .par_iter()
        .map(|record| {
            if INTERRUPTED.load(Ordering::SeqCst) {
                return *record; // 中断時は元のレコードを返す
            }

            let result = process_record(record, &evaluator, max_ply);

            match result {
                Ok(new_record) => {
                    processed_count.fetch_add(1, Ordering::Relaxed);
                    progress.inc(1);
                    new_record
                }
                Err(e) => {
                    error_count.fetch_add(1, Ordering::Relaxed);
                    if verbose {
                        eprintln!("Error processing record: {e}");
                    }
                    progress.inc(1);
                    *record // エラー時は元のレコードを返す
                }
            }
        })
        .collect();

    progress.finish_with_message("Done");

    let final_errors = error_count.load(Ordering::SeqCst);
    if final_errors > 0 {
        eprintln!("Note: {final_errors} positions had errors and were skipped");
    }

    // 出力ファイルに書き込み
    eprintln!("Writing output...");
    let out_file = File::create(&cli.output)
        .with_context(|| format!("Failed to create {}", cli.output.display()))?;
    let mut writer = BufWriter::new(out_file);

    for record in &processed_records {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }
        writer.write_all(record)?;
    }

    writer.flush()?;
    eprintln!("Wrote {} records", processed_records.len());

    Ok(())
}

/// 1レコードを処理
fn process_record<E: Evaluator>(
    record: &[u8; PackedSfenValue::SIZE],
    evaluator: &E,
    max_ply: i32,
) -> Result<[u8; PackedSfenValue::SIZE]> {
    // PackedSfenValueを読み込み
    let psv = PackedSfenValue::from_bytes(record)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse PackedSfenValue"))?;

    // PackedSfen → SFEN → Position
    let sfen = unpack_sfen(&psv.sfen).map_err(|e| anyhow::anyhow!("Failed to unpack SFEN: {e}"))?;

    let mut pos = Position::new();
    pos.set_sfen(&sfen).map_err(|e| anyhow::anyhow!("Failed to set SFEN: {e:?}"))?;

    // qsearch_with_pvを実行
    let result =
        qsearch_with_pv(&mut pos, evaluator, QSEARCH_ALPHA_INIT, QSEARCH_BETA_INIT, 0, max_ply);

    // PVに沿って局面を進める
    for mv in &result.pv {
        let gives_check = pos.gives_check(*mv);
        let _ = pos.do_move(*mv, gives_check);
    }

    // 新しいPackedSfenValueを作成
    let new_sfen = pack_position(&pos);
    let new_move16 = if let Some(&first_mv) = result.pv.first() {
        move_to_move16(first_mv)
    } else {
        psv.move16 // PVが空の場合は元の手を維持
    };

    let new_psv = PackedSfenValue {
        sfen: new_sfen,
        score: result.value as i16,
        move16: new_move16,
        game_ply: psv.game_ply,
        game_result: psv.game_result,
        padding: 0,
    };

    Ok(new_psv.to_bytes())
}
