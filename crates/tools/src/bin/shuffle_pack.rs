//! shuffle_pack - packファイル内のレコードをシャッフル
//!
//! PackedSfenValue形式（40バイト/レコード）のpackファイルをシャッフルする。
//!
//! # 使用例
//!
//! ```bash
//! # 基本的な使用法
//! cargo run -p tools --bin shuffle_pack -- \
//!   --input data.pack --output shuffled.pack
//!
//! # シード指定（再現性のため）
//! cargo run -p tools --bin shuffle_pack -- \
//!   --input data.pack --output shuffled.pack --seed 42
//!
//! # チャンク方式（大規模ファイル用、メモリ使用量を制限）
//! cargo run -p tools --bin shuffle_pack -- \
//!   --input large.pack --output shuffled.pack --chunk-size 10000000
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use tools::packed_sfen::PackedSfenValue;

/// PackedSfenValue形式のpackファイルをシャッフル
#[derive(Parser)]
#[command(
    name = "shuffle_pack",
    version,
    about = "packファイル内のレコードをシャッフル\n\nPackedSfenValue形式（40バイト/レコード）のファイルをシャッフルして出力"
)]
struct Cli {
    /// 入力packファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力packファイル
    #[arg(short, long)]
    output: PathBuf,

    /// 乱数シード（再現性のため）
    #[arg(long)]
    seed: Option<u64>,

    /// チャンクサイズ（レコード数）
    /// 大規模ファイル用。この数のレコードをメモリに保持
    /// デフォルト: 0（全レコードをメモリに読み込む）
    #[arg(long, default_value_t = 0)]
    chunk_size: usize,
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
        eprintln!("\nInterrupted!");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // 入力ファイルサイズからレコード数を計算
    let file_size = std::fs::metadata(&cli.input)?.len();
    let record_count = file_size / PackedSfenValue::SIZE as u64;

    if file_size % PackedSfenValue::SIZE as u64 != 0 {
        eprintln!(
            "Warning: File size ({file_size}) is not a multiple of record size ({}). Trailing bytes will be ignored.",
            PackedSfenValue::SIZE
        );
    }

    eprintln!(
        "Input file: {} ({} bytes, {} records)",
        cli.input.display(),
        file_size,
        record_count
    );

    // 乱数生成器を初期化
    let mut rng = if let Some(seed) = cli.seed {
        eprintln!("Using seed: {seed}");
        ChaCha8Rng::seed_from_u64(seed)
    } else {
        ChaCha8Rng::from_os_rng()
    };

    // シャッフル方式を選択
    if cli.chunk_size > 0 && record_count > cli.chunk_size as u64 {
        eprintln!("Using chunked shuffle (chunk size: {} records)", cli.chunk_size);
        shuffle_chunked(&cli.input, &cli.output, record_count, cli.chunk_size, &mut rng)?;
    } else {
        eprintln!("Using in-memory shuffle");
        shuffle_in_memory(&cli.input, &cli.output, record_count, &mut rng)?;
    }

    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("Note: Processing was interrupted, output may be incomplete");
    } else {
        eprintln!("Output: {}", cli.output.display());
    }

    Ok(())
}

/// インメモリ方式でシャッフル
/// レコード全体をメモリに読み込み、インデックス配列をシャッフルして書き出す
fn shuffle_in_memory(
    input_path: &PathBuf,
    output_path: &PathBuf,
    record_count: u64,
    rng: &mut ChaCha8Rng,
) -> Result<()> {
    // メモリ使用量の見積もり
    let data_size = record_count * PackedSfenValue::SIZE as u64;
    let index_size = record_count * std::mem::size_of::<usize>() as u64;
    let total_mb = (data_size + index_size) / 1_000_000;
    eprintln!(
        "Estimated memory usage: {} MB (data) + {} MB (indices) = {} MB total",
        data_size / 1_000_000,
        index_size / 1_000_000,
        total_mb
    );

    // 大規模ファイルの場合は警告
    const LARGE_FILE_THRESHOLD_MB: u64 = 4000; // 4GB
    if total_mb > LARGE_FILE_THRESHOLD_MB {
        eprintln!(
            "Warning: Large memory allocation ({} MB). Consider using --chunk-size option.",
            total_mb
        );
    }

    // 進捗バー設定
    let progress = ProgressBar::new(record_count * 2); // 読み込み + 書き出し
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );
    progress.set_message("Reading...");

    // 全レコードを読み込み
    let mut data = Vec::with_capacity(record_count as usize);
    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::new(in_file);
    let mut buffer = [0u8; PackedSfenValue::SIZE];

    for _ in 0..record_count {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            return Ok(());
        }

        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        data.push(buffer);
        progress.inc(1);
    }

    let actual_count = data.len();
    eprintln!("Read {} records", actual_count);

    // インデックス配列をシャッフル
    progress.set_message("Shuffling...");
    let mut indices: Vec<usize> = (0..actual_count).collect();
    indices.shuffle(rng);

    // シャッフルされた順序で書き出し
    progress.set_message("Writing...");
    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::new(out_file);

    for &idx in &indices {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            return Ok(());
        }

        writer.write_all(&data[idx])?;
        progress.inc(1);
    }

    writer.flush()?;
    progress.finish_with_message("Done");
    eprintln!("Shuffled {} records", actual_count);

    Ok(())
}

/// チャンク方式でシャッフル
/// 大規模ファイル用。2パス方式でシャッフル:
/// 1. 各レコードをランダムなチャンクファイルに振り分け
/// 2. 各チャンクファイルを読み込み、シャッフルして出力に追記
fn shuffle_chunked(
    input_path: &PathBuf,
    output_path: &PathBuf,
    record_count: u64,
    chunk_size: usize,
    rng: &mut ChaCha8Rng,
) -> Result<()> {
    let num_chunks = (record_count as usize).div_ceil(chunk_size);

    // チャンク数のバリデーション
    if num_chunks == 0 {
        anyhow::bail!("Invalid chunk configuration: num_chunks is 0");
    }

    const MAX_CHUNKS: usize = 1000;
    if num_chunks > MAX_CHUNKS {
        anyhow::bail!(
            "Too many chunks ({num_chunks}). Maximum is {MAX_CHUNKS}. \
             Consider increasing --chunk-size (current: {chunk_size})"
        );
    }

    eprintln!("Creating {num_chunks} temporary chunks");

    // 一時ディレクトリを作成
    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    // Pass 1: 各レコードをランダムなチャンクに振り分け
    eprintln!("Pass 1: Distributing records to chunks...");
    let progress = ProgressBar::new(record_count);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) Pass 1")
            .expect("valid template"),
    );

    // チャンクファイルを開く
    let mut chunk_writers: Vec<BufWriter<File>> = (0..num_chunks)
        .map(|i| {
            let path = temp_dir.path().join(format!("chunk_{i}.pack"));
            File::create(&path)
                .map(BufWriter::new)
                .with_context(|| format!("Failed to create chunk file: {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;

    // 入力ファイルを読み込み、ランダムにチャンクに振り分け
    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::new(in_file);
    let mut buffer = [0u8; PackedSfenValue::SIZE];

    for _ in 0..record_count {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            return Ok(());
        }

        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }

        // ランダムなチャンクに書き込み
        let chunk_idx = rng.random_range(0..num_chunks);
        chunk_writers[chunk_idx].write_all(&buffer)?;
        progress.inc(1);
    }

    // チャンクファイルをフラッシュ
    for writer in &mut chunk_writers {
        writer.flush()?;
    }
    drop(chunk_writers);
    progress.finish();

    // Pass 2: 各チャンクをシャッフルして出力に追記
    eprintln!("Pass 2: Shuffling and writing chunks...");
    let progress = ProgressBar::new(num_chunks as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) Pass 2")
            .expect("valid template"),
    );

    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::new(out_file);

    for i in 0..num_chunks {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            return Ok(());
        }

        let chunk_path = temp_dir.path().join(format!("chunk_{i}.pack"));

        // チャンクファイルを読み込み
        let chunk_size = std::fs::metadata(&chunk_path)?.len();
        let chunk_records = chunk_size / PackedSfenValue::SIZE as u64;

        let chunk_file = File::open(&chunk_path)?;
        let mut chunk_reader = BufReader::new(chunk_file);

        let mut chunk_data: Vec<[u8; PackedSfenValue::SIZE]> =
            Vec::with_capacity(chunk_records as usize);
        let mut buf = [0u8; PackedSfenValue::SIZE];

        loop {
            match chunk_reader.read_exact(&mut buf) {
                Ok(()) => chunk_data.push(buf),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }

        // チャンク内をシャッフル
        chunk_data.shuffle(rng);

        // 出力ファイルに書き込み
        for record in &chunk_data {
            writer.write_all(record)?;
        }

        progress.inc(1);
    }

    writer.flush()?;
    progress.finish();

    // 一時ディレクトリは自動的にクリーンアップされる
    eprintln!("Shuffling complete");

    Ok(())
}
