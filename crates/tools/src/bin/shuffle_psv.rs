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
use log::{info, warn};
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

    /// メモリ不足でも強制続行
    #[arg(long)]
    force: bool,
}

/// 処理中にCtrl-Cが押されたかを追跡
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// /proc/meminfo から MemAvailable をバイト単位で取得する。
fn get_mem_available() -> Option<u64> {
    let content = std::fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemAvailable:") {
            let kb_str = rest.trim().strip_suffix("kB")?.trim();
            let kb: u64 = kb_str.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// バイト数を人間が読みやすい形式にフォーマットする。
fn format_size(bytes: usize) -> String {
    const GIB: usize = 1024 * 1024 * 1024;
    const MIB: usize = 1024 * 1024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    }
}

/// メモリ充足チェック。required_bytes が利用可能メモリの 80% を超える場合はエラーで停止する。
/// --force 指定時は警告のみで続行。
///
/// 80% 閾値は、OS・他プロセス・バッファキャッシュ等に 20% の余裕を確保するため。
const MEMORY_VALIDATION_RATIO: f64 = 0.8;

fn check_memory(required_bytes: usize, force: bool) -> Result<()> {
    if let Some(mem_available) = get_mem_available() {
        let threshold = (mem_available as f64 * MEMORY_VALIDATION_RATIO) as usize;
        info!(
            "  required: {} / available: {} ({:.0}% threshold: {})",
            format_size(required_bytes),
            format_size(mem_available as usize),
            MEMORY_VALIDATION_RATIO * 100.0,
            format_size(threshold),
        );
        if required_bytes > threshold {
            if force {
                warn!("メモリ不足ですが --force が指定されているため続行します");
            } else {
                anyhow::bail!(
                    "メモリ不足: {} 必要ですが、利用可能メモリは {} です。\n\
                     対処法:\n\
                     - --chunk-size を小さくする\n\
                     - --force で強制続行（swap 使用の可能性あり）",
                    format_size(required_bytes),
                    format_size(mem_available as usize),
                );
            }
        }
    }
    Ok(())
}

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
        warn!(
            "File size ({file_size}) is not a multiple of record size ({RECORD_SIZE}). \
             Trailing bytes will be ignored.",
        );
    }

    info!(
        "Input file: {} ({} bytes, {} records)",
        cli.input.display(),
        file_size,
        record_count
    );

    // 乱数生成器を初期化
    let mut rng = if let Some(seed) = cli.seed {
        info!("Using seed: {seed}");
        ChaCha8Rng::seed_from_u64(seed)
    } else {
        ChaCha8Rng::from_os_rng()
    };

    // シャッフル方式を選択
    if cli.chunk_size > 0 && record_count > cli.chunk_size as u64 {
        info!("Using chunked shuffle (chunk size: {} records)", cli.chunk_size,);
        shuffle_chunked(
            &cli.input,
            &cli.output,
            record_count,
            cli.chunk_size,
            cli.force,
            &mut rng,
        )?;
    } else {
        let data_bytes = record_count as usize * RECORD_SIZE;
        info!("Using in-memory shuffle ({})", format_size(data_bytes));
        check_memory(data_bytes, cli.force)?;
        shuffle_in_memory(&cli.input, &cli.output, record_count, &mut rng)?;
    }

    if INTERRUPTED.load(Ordering::SeqCst) {
        warn!("Processing was interrupted, output may be incomplete");
    } else {
        info!("Output: {}", cli.output.display());
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

/// 進捗バーのスタイル
fn progress_style(suffix: &str) -> ProgressStyle {
    ProgressStyle::default_bar()
        .template(&format!(
            "[{{elapsed_precise}}] {{bar:40.cyan/blue}} {{pos}}/{{len}} ({{per_sec}}) {suffix}"
        ))
        .expect("valid template")
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
    info!("Estimated memory usage: {data_mb} MB");

    const LARGE_FILE_THRESHOLD_MB: usize = 4000;
    if data_mb > LARGE_FILE_THRESHOLD_MB {
        warn!("Large memory allocation ({data_mb} MB). Consider using --chunk-size option.");
    }

    // 読み込み（レコード単位の進捗表示）
    let progress = ProgressBar::new(record_count);
    progress.set_style(progress_style("Reading..."));

    let mut buf = vec![0u8; data_size];
    {
        let file = File::open(input_path)
            .with_context(|| format!("Failed to open {}", input_path.display()))?;
        let mut reader = BufReader::with_capacity(BUF_SIZE, file);
        // チャンク単位で読み込みつつ進捗更新
        let mut offset = 0;
        while offset < data_size {
            if INTERRUPTED.load(Ordering::SeqCst) {
                progress.abandon_with_message("Interrupted");
                return Ok(());
            }
            let end = (offset + BUF_SIZE).min(data_size);
            reader.read_exact(&mut buf[offset..end])?;
            let records_read = (end - offset) / RECORD_SIZE;
            progress.inc(records_read as u64);
            offset = end;
        }
    }
    progress.finish_with_message("Read complete");

    // Fisher-Yates in-place シャッフル（インデックス配列不要）
    info!("Shuffling...");
    let records = bytes_as_records_mut(&mut buf);
    records.shuffle(rng);

    // 書き出し（レコード単位の進捗表示）
    let progress = ProgressBar::new(record_count);
    progress.set_style(progress_style("Writing..."));
    {
        let file = File::create(output_path)
            .with_context(|| format!("Failed to create {}", output_path.display()))?;
        let mut writer = BufWriter::with_capacity(BUF_SIZE, file);
        let mut offset = 0;
        while offset < buf.len() {
            if INTERRUPTED.load(Ordering::SeqCst) {
                progress.abandon_with_message("Interrupted");
                return Ok(());
            }
            let end = (offset + BUF_SIZE).min(buf.len());
            writer.write_all(&buf[offset..end])?;
            let records_written = (end - offset) / RECORD_SIZE;
            progress.inc(records_written as u64);
            offset = end;
        }
        writer.flush()?;
    }
    progress.finish_with_message("Done");

    info!("Shuffled {} records", record_count);
    Ok(())
}

/// チャンク方式でシャッフル
///
/// 大規模ファイル用。2パス方式:
/// 1. 各レコードをランダムなチャンクファイルに振り分け
/// 2. チャンクをバッチ（コア数単位）で並列に読み込み・シャッフル → 即座に書き出し
///    メモリ使用量はバッチサイズ分に制限される
fn shuffle_chunked(
    input_path: &PathBuf,
    output_path: &PathBuf,
    record_count: u64,
    chunk_size: usize,
    force: bool,
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

    // ランダム振り分け後の期待チャンクサイズで見積もる。
    // chunk_size そのものではなく record_count / num_chunks を使うことで、
    // record_count が chunk_size をわずかに超える場合の過大見積もりを防ぐ。
    let expected_chunk_records = (record_count as usize).div_ceil(num_chunks);
    let expected_chunk_bytes = expected_chunk_records * RECORD_SIZE;
    info!(
        "Creating {num_chunks} temporary chunks (expected max chunk: {})",
        format_size(expected_chunk_bytes),
    );
    check_memory(expected_chunk_bytes, force)?;

    let temp_dir = tempfile::tempdir().context("Failed to create temp directory")?;

    // Pass 1: 各レコードをランダムなチャンクに振り分け
    info!("Pass 1: Distributing records to chunks...");
    let progress = ProgressBar::new(record_count);
    progress.set_style(progress_style("Pass 1"));

    // チャンクライターのバッファサイズ: 合計 BUF_SIZE を上限にチャンク数で按分
    // num_chunks > 64 の場合、個別バッファは 128KB まで縮小（合計は最大 125MB）
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

    // Pass 2: チャンクをバッチ並列でシャッフル → 即座に書き出し
    // メモリ使用量 = バッチサイズ（コア数）× チャンクサイズ に制限
    info!("Pass 2: Shuffling chunks in batches and writing...");
    let progress = ProgressBar::new(num_chunks as u64);
    progress.set_style(progress_style("Pass 2"));

    let chunk_seeds: Vec<u64> = (0..num_chunks).map(|_| rng.random()).collect();

    let chunk_paths: Vec<PathBuf> = (0..num_chunks)
        .map(|i| temp_dir.path().join(format!("chunk_{i}.tmp")))
        .collect();

    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(BUF_SIZE, out_file);

    // バッチサイズをメモリ上限で制限する。
    // 各チャンクは最大 chunk_size * RECORD_SIZE バイトをメモリに展開するため、
    // 同時にロードするチャンク数 × チャンクサイズが利用可能メモリを超えないようにする。
    let max_chunk_bytes = chunk_size * RECORD_SIZE;
    // OS・他プロセス・バッファキャッシュ等のための余裕を 30% 確保し、
    // 利用可能メモリの 70% を上限とする。
    const MEMORY_BUDGET_RATIO: f64 = 0.7;
    let batch_size = {
        let num_threads = rayon::current_num_threads();
        // 非 Linux 環境では MemAvailable を取得できないため、制限なし（スレッド数で決定）
        let mem_limit = get_mem_available().unwrap_or(u64::MAX);
        let usable = (mem_limit as f64 * MEMORY_BUDGET_RATIO) as usize;
        let by_memory = (usable / max_chunk_bytes).max(1);
        let batch = num_threads.min(by_memory);
        info!(
            "  batch_size: {} (threads: {}, memory allows: {} chunks × {} = {})",
            batch,
            num_threads,
            by_memory,
            format_size(max_chunk_bytes),
            format_size(by_memory * max_chunk_bytes),
        );
        batch
    };
    for batch_start in (0..num_chunks).step_by(batch_size) {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            return Ok(());
        }

        let batch_end = (batch_start + batch_size).min(num_chunks);
        let batch_paths = &chunk_paths[batch_start..batch_end];
        let batch_seeds = &chunk_seeds[batch_start..batch_end];

        // バッチ内を並列に読み込み + シャッフル
        let shuffled: Vec<Result<Vec<u8>>> = batch_paths
            .par_iter()
            .zip(batch_seeds.par_iter())
            .map(|(path, &seed)| {
                let file_size = std::fs::metadata(path)?.len() as usize;
                if file_size == 0 {
                    return Ok(Vec::new());
                }

                let mut buf = vec![0u8; file_size];
                let file = File::open(path)?;
                let mut r = BufReader::with_capacity(BUF_SIZE, file);
                r.read_exact(&mut buf)?;

                let records = bytes_as_records_mut(&mut buf);
                let mut chunk_rng = ChaCha8Rng::seed_from_u64(seed);
                records.shuffle(&mut chunk_rng);

                Ok(buf)
            })
            .collect();

        // バッチ結果を即座に書き出し（メモリ解放）
        for chunk_result in shuffled {
            let buf = chunk_result?;
            if !buf.is_empty() {
                writer.write_all(&buf)?;
            }
            progress.inc(1);
        }
    }

    writer.flush()?;
    progress.finish();

    info!("Shuffling complete");
    Ok(())
}
