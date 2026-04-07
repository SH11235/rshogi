//! shuffle_psv - PSVファイル内のレコードをシャッフル
//!
//! PackedSfenValue形式（40バイト/レコード）のPSVファイルをシャッフルする。
//!
//! # 使用例
//!
//! ```bash
//! # 基本的な使用法
//! cargo run -p tools --bin shuffle_psv -- \
//!   --input data.psv --output shuffled.psv
//!
//! # シード指定（再現性のため）
//! cargo run -p tools --bin shuffle_psv -- \
//!   --input data.psv --output shuffled.psv --seed 42
//!
//! # チャンク方式（大規模ファイル用、メモリ使用量を制限）
//! cargo run -p tools --bin shuffle_psv -- \
//!   --input large.psv --output shuffled.psv --chunk-size 10000000
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use tools::packed_sfen::PackedSfenValue;

const RECORD_SIZE: usize = PackedSfenValue::SIZE;
/// I/O バッファサイズ（8 MB）
const BUF_SIZE: usize = 8 * 1024 * 1024;

/// PackedSfenValue形式のPSVファイルをシャッフル
#[derive(Parser)]
#[command(
    name = "shuffle_psv",
    version,
    about = "PSVファイル内のレコードをシャッフル\n\nPackedSfenValue形式（40バイト/レコード）のファイルをシャッフルして出力"
)]
struct Cli {
    /// 入力PSVファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力PSVファイル
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
    let record_count = file_size / RECORD_SIZE as u64;

    if file_size % RECORD_SIZE as u64 != 0 {
        eprintln!(
            "Warning: File size ({file_size}) is not a multiple of record size ({RECORD_SIZE}). \
             Trailing bytes will be ignored.",
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

/// バイト列を `[u8; RECORD_SIZE]` のミュータブルスライスとして再解釈する。
///
/// # Safety
/// - `buf` の長さは `RECORD_SIZE` の倍数でなければならない。
/// - `[u8; RECORD_SIZE]` のアラインメントは 1 なので、アラインメント条件は自明に満たされる。
fn bytes_as_records_mut(buf: &mut [u8]) -> &mut [[u8; RECORD_SIZE]] {
    assert!(buf.len().is_multiple_of(RECORD_SIZE));
    // Safety: u8 配列のアラインメントは 1 で、長さは RECORD_SIZE の倍数を事前確認済み
    unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast(), buf.len() / RECORD_SIZE) }
}

/// インメモリ方式でシャッフル
///
/// 全レコードをバイト列として一括読み込みし、Fisher-Yates で in-place シャッフル。
/// インデックス配列を使わないため、メモリ使用量 = データサイズのみ。
fn shuffle_in_memory(
    input_path: &PathBuf,
    output_path: &PathBuf,
    record_count: u64,
    rng: &mut ChaCha8Rng,
) -> Result<()> {
    let data_size = record_count as usize * RECORD_SIZE;
    let data_mb = data_size / 1_000_000;
    eprintln!("Estimated memory usage: {data_mb} MB");

    const LARGE_FILE_THRESHOLD_MB: usize = 4000;
    if data_mb > LARGE_FILE_THRESHOLD_MB {
        eprintln!(
            "Warning: Large memory allocation ({data_mb} MB). Consider using --chunk-size option.",
        );
    }

    let progress = ProgressBar::new(3);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
            .expect("valid template"),
    );

    // 一括読み込み
    progress.set_message("Reading...");
    let mut buf = vec![0u8; data_size];
    {
        let file = File::open(input_path)
            .with_context(|| format!("Failed to open {}", input_path.display()))?;
        let mut reader = BufReader::with_capacity(BUF_SIZE, file);
        reader.read_exact(&mut buf)?;
    }
    progress.inc(1);

    // Fisher-Yates in-place シャッフル（インデックス配列不要）
    progress.set_message("Shuffling...");
    let records = bytes_as_records_mut(&mut buf);
    records.shuffle(rng);
    progress.inc(1);

    // 一括書き出し
    progress.set_message("Writing...");
    {
        let file = File::create(output_path)
            .with_context(|| format!("Failed to create {}", output_path.display()))?;
        let mut writer = BufWriter::with_capacity(BUF_SIZE, file);
        writer.write_all(&buf)?;
        writer.flush()?;
    }
    progress.inc(1);

    progress.finish_with_message("Done");
    eprintln!("Shuffled {} records", record_count);

    Ok(())
}

/// チャンク方式でシャッフル
///
/// 大規模ファイル用。2パス方式:
/// 1. 各レコードをランダムなチャンクファイルに振り分け
/// 2. 各チャンクを rayon で並列に読み込み・シャッフルし、順次出力に書き出し
fn shuffle_chunked(
    input_path: &PathBuf,
    output_path: &PathBuf,
    record_count: u64,
    chunk_size: usize,
    rng: &mut ChaCha8Rng,
) -> Result<()> {
    let num_chunks = (record_count as usize).div_ceil(chunk_size);

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

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    // Pass 1: 各レコードをランダムなチャンクに振り分け
    eprintln!("Pass 1: Distributing records to chunks...");
    let progress = ProgressBar::new(record_count);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) Pass 1")
            .expect("valid template"),
    );

    let mut chunk_writers: Vec<BufWriter<File>> = (0..num_chunks)
        .map(|i| {
            let path = temp_dir.path().join(format!("chunk_{i}.tmp"));
            File::create(&path)
                .map(|f| BufWriter::with_capacity(BUF_SIZE / num_chunks.clamp(1, 64), f))
                .with_context(|| format!("Failed to create chunk file: {}", path.display()))
        })
        .collect::<Result<Vec<_>>>()?;

    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::with_capacity(BUF_SIZE, in_file);
    let mut buffer = [0u8; RECORD_SIZE];

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

        let chunk_idx = rng.random_range(0..num_chunks);
        chunk_writers[chunk_idx].write_all(&buffer)?;
        progress.inc(1);
    }

    for writer in &mut chunk_writers {
        writer.flush()?;
    }
    drop(chunk_writers);
    progress.finish();

    // Pass 2: 各チャンクを rayon で並列に読み込み・シャッフル → 順次書き出し
    eprintln!("Pass 2: Shuffling chunks in parallel and writing...");
    let progress = ProgressBar::new(num_chunks as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) Pass 2")
            .expect("valid template"),
    );

    // チャンクごとに独立した RNG シードを事前生成（再現性のため）
    let chunk_seeds: Vec<u64> = (0..num_chunks).map(|_| rng.random()).collect();

    let chunk_paths: Vec<PathBuf> = (0..num_chunks)
        .map(|i| temp_dir.path().join(format!("chunk_{i}.tmp")))
        .collect();

    // 各チャンクを並列に読み込み＋シャッフル
    let shuffled_chunks: Vec<Result<Vec<u8>>> = chunk_paths
        .par_iter()
        .zip(chunk_seeds.par_iter())
        .map(|(path, &seed)| {
            let file_size = std::fs::metadata(path)?.len() as usize;
            if file_size == 0 {
                return Ok(Vec::new());
            }

            let mut buf = vec![0u8; file_size];
            let file = File::open(path)?;
            let mut reader = BufReader::with_capacity(BUF_SIZE, file);
            reader.read_exact(&mut buf)?;

            let records = bytes_as_records_mut(&mut buf);
            let mut chunk_rng = ChaCha8Rng::seed_from_u64(seed);
            records.shuffle(&mut chunk_rng);

            Ok(buf)
        })
        .collect();

    // 順次書き出し
    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(BUF_SIZE, out_file);

    for chunk_result in shuffled_chunks {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            return Ok(());
        }

        let buf = chunk_result?;
        if !buf.is_empty() {
            writer.write_all(&buf)?;
        }
        progress.inc(1);
    }

    writer.flush()?;
    progress.finish();

    eprintln!("Shuffling complete");

    Ok(())
}
