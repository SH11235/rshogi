//! preprocess_psv - PSVファイルにqsearch leaf置換を適用
//!
//! PackedSfenValue形式（40バイト/レコード）のPSVファイルに対して
//! qsearch leaf置換を適用する。
//!
//! # 使用例
//!
//! ```bash
//! # 基本的な使用法（Material評価）
//! cargo run -p tools --bin preprocess_psv -- \
//!   --input data.psv --output processed.psv
//!
//! # NNUEモデルを使用
//! cargo run -p tools --bin preprocess_psv -- \
//!   --input data.psv --output processed.psv --nnue model.nnue
//!
//! # 並列処理（4スレッド）
//! cargo run -p tools --bin preprocess_psv -- \
//!   --input data.psv --output processed.psv --threads 4
//!
//! # 局面が変わった出力行の bitmap mask も生成
//! cargo run -p tools --bin preprocess_psv -- \
//!   --input data.psv --output processed.psv \
//!   --moved-mask moved.bits
//! ```
//!
//! # moved mask の契約
//!
//! `--moved-mask` を指定すると、出力 PSV の packed SFEN（各レコードの先頭32 byte）が
//! 入力と異なる行を bit 1 とした LSB-first bitmap を出力する。byte `j` の bit `k` は
//! 出力 record `j * 8 + k` に対応し、サイズは `ceil(出力 records / 8)` byte、最終 byte の
//! 未使用 bit は 0 になる。処理エラー行は skip 行と同様に破棄され、出力にも mask にも
//! 現れない (`--moved-mask` の有無で PSV 出力は変わらない)。
//! `--skip-in-check` で除外した行は PSV にも mask にも出力せず、後続 bit は出力行番号に詰める。
//! PSV と mask はそれぞれ一時ファイルへ書き、処理成功時だけ最終パスへ rename する。

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use tools::mask_io::BitmapWriter;
use tools::nnue_routing::{NativeRouting, parse_native_layer_stack_bucket_mode};

use std::cell::RefCell;

use rshogi_core::nnue::{get_network, init_nnue, load_progress_coeff_kpabs};
use rshogi_core::position::Position;
use tools::packed_sfen::{PackedSfenValue, pack_position, unpack_sfen};
use tools::qsearch_pv::{
    MaterialEvaluator, NnueStacks, QsearchResult, qsearch_with_pv, qsearch_with_pv_nnue,
};

/// PackedSfenValue形式のPSVファイルにqsearch leaf置換を適用
#[derive(Parser)]
#[command(
    name = "preprocess_psv",
    version,
    about = "PSVファイルにqsearch leaf置換を適用\n\n各局面をqsearchのPV末端局面に置換して出力"
)]
struct Cli {
    /// 入力PSVファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力PSVファイル
    #[arg(short, long)]
    output: PathBuf,

    /// packed SFEN（先頭32 byte）が変わった出力行を示すLSB-first bitmapの出力先
    #[arg(long)]
    moved_mask: Option<PathBuf>,

    /// qsearchの最大深さ（ノード制限と併用で爆発防止）
    #[arg(long, default_value_t = 16)]
    max_ply: i32,

    /// 並列処理スレッド数（0=自動）
    #[arg(short, long, default_value_t = 1)]
    threads: usize,

    /// NNUEモデルファイル（省略時はMaterial評価、--rescoreには必須）
    #[arg(long)]
    nnue: Option<PathBuf>,

    /// LayerStacks bucket mode。LayerStacks NNUE では必須。
    #[arg(long)]
    ls_bucket_mode: Option<String>,

    /// progresskpabs が推論に使う bucket 数。
    #[arg(long)]
    ls_progress_buckets: Option<usize>,

    /// progresskpabs 用の進行度係数ファイル。
    #[arg(long)]
    ls_progress_coeff: Option<PathBuf>,

    /// 処理するレコード数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: u64,

    /// 詳細出力
    #[arg(short, long)]
    verbose: bool,

    /// 手番反転時にscoreとgame_resultの符号を補正しない（デバッグ用）
    /// qsearch leaf置換で手番が変わった場合でもscoreとgame_resultを反転しない
    #[arg(long)]
    no_fix_stm_sign: bool,

    /// qsearch leaf置換後にNNUEで再評価（推奨）
    /// 元の評価値を破棄し、指定したNNUEモデルで評価し直す
    /// これにより局面とスコアの整合性が保証される
    #[arg(long)]
    rescore: bool,

    /// 王手局面をスキップ（出力から除外）
    #[arg(long)]
    skip_in_check: bool,

    /// スコアのクリップ範囲（±この値にクリップ、--rescore時のみ有効）
    #[arg(long, default_value_t = 10000)]
    score_clip: i16,
}

/// 処理中にCtrl-Cが押されたかを追跡
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// qsearchの初期alpha値
const QSEARCH_ALPHA_INIT: i32 = -30000;
/// qsearchの初期beta値
const QSEARCH_BETA_INIT: i32 = 30000;

/// チャンクサイズ（レコード数）。chunk バッファ約40MB + results バッファ約40MB = ピーク約80MB/チャンク。
const CHUNK_SIZE: usize = 1_000_000;

/// I/Oバッファサイズ（8MB）
const IO_BUF_SIZE: usize = 8 * 1024 * 1024;

/// 処理結果
enum ProcessResult {
    /// 正常に処理完了
    Ok([u8; PackedSfenValue::SIZE]),
    /// スキップ（王手局面など）
    Skip,
    /// エラー
    Error(anyhow::Error),
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ResultCounts {
    skipped: u64,
    errors: u64,
    written: u64,
    moved: u64,
}

struct TemporaryFiles {
    paths: Vec<PathBuf>,
}

impl TemporaryFiles {
    fn new() -> Self {
        Self { paths: Vec::new() }
    }

    fn track(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    /// rename 済み・失敗時に残したいパスを Drop の削除対象から外す。
    fn untrack(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
    }
}

impl Drop for TemporaryFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 処理オプション
#[derive(Clone, Copy)]
struct ProcessOptions {
    max_ply: i32,
    fix_stm_sign: bool,
    rescore: bool,
    skip_in_check: bool,
    score_clip: i16,
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    // 入力ファイルの存在確認
    if !cli.input.exists() {
        anyhow::bail!("Input file not found: {}", cli.input.display());
    }

    // --rescoreは--nnueが必須
    if cli.rescore && cli.nnue.is_none() {
        anyhow::bail!("--rescore requires --nnue option");
    }

    validate_all_output_paths(&cli)?;

    // NNUEモデルのロード
    let use_nnue = if let Some(ref nnue_path) = cli.nnue {
        if !nnue_path.exists() {
            anyhow::bail!("NNUE model file not found: {}", nnue_path.display());
        }
        init_nnue(nnue_path).context("Failed to load NNUE model")?;
        eprintln!("NNUE model loaded: {}", nnue_path.display());
        let network = get_network().context("NNUE was not initialized")?;
        let mode = cli
            .ls_bucket_mode
            .as_deref()
            .map(parse_native_layer_stack_bucket_mode)
            .transpose()?;
        if let Some(plan) = NativeRouting::prepare(
            network.layer_stack_num_buckets(),
            mode,
            cli.ls_progress_buckets,
            cli.ls_progress_coeff.is_some(),
        )
        .context("native routing: --ls-bucket-mode / --ls-progress-buckets / --ls-progress-coeff")?
        {
            plan.apply(|| {
                cli.ls_progress_coeff
                    .as_deref()
                    .map(|path| load_progress_coeff_kpabs(path).map_err(anyhow::Error::msg))
                    .transpose()
            })?;
        }
        true
    } else {
        false
    };

    // Ctrl-Cハンドラを設定
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted!");
        INTERRUPTED.store(true, Ordering::Release);
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
    let fix_stm_sign = !cli.no_fix_stm_sign;
    eprintln!("STM sign fix: {}", if fix_stm_sign { "enabled" } else { "disabled" });
    eprintln!("Rescore with NNUE: {}", if cli.rescore { "yes" } else { "no" });
    eprintln!("Skip in-check positions: {}", if cli.skip_in_check { "yes" } else { "no" });
    if let Some(path) = &cli.moved_mask {
        eprintln!("Moved mask: {}", path.display());
    }
    if cli.rescore {
        eprintln!("Score clip: ±{}", cli.score_clip);
    }

    // 処理オプションを構築
    let opts = ProcessOptions {
        max_ply: cli.max_ply,
        fix_stm_sign,
        rescore: cli.rescore,
        skip_in_check: cli.skip_in_check,
        score_clip: cli.score_clip,
    };

    // 処理実行
    process_file(&cli, process_count, use_nnue, opts)?;

    eprintln!("Output: {}", cli.output.display());
    if let Some(path) = &cli.moved_mask {
        eprintln!("Moved mask: {}", path.display());
    }

    Ok(())
}

/// ProcessResult を集計し、出力バッファに書き込む共通ハンドラ
///
fn collect_results<W: Write, M: Write>(
    original_records: &[[u8; PackedSfenValue::SIZE]],
    results: &[ProcessResult],
    writer: &mut W,
    mut mask_writer: Option<&mut BitmapWriter<M>>,
    verbose: bool,
) -> Result<ResultCounts> {
    anyhow::ensure!(
        original_records.len() == results.len(),
        "Internal error: input/result count mismatch ({} != {})",
        original_records.len(),
        results.len()
    );
    let mut counts = ResultCounts::default();
    for (original, result) in original_records.iter().zip(results) {
        match result {
            ProcessResult::Ok(new_record) => {
                writer.write_all(new_record)?;
                let moved = new_record[..32] != original[..32];
                if let Some(mask) = mask_writer.as_deref_mut() {
                    mask.push(moved)?;
                }
                counts.written += 1;
                counts.moved += u64::from(moved);
            }
            ProcessResult::Skip => {
                counts.skipped += 1;
            }
            ProcessResult::Error(error) => {
                // エラー行は skip と同様に出力・mask の両方から除外する (mask 有無で
                // PSV 出力を変えない。bit は出力行番号に対応するため詰める)。
                counts.errors += 1;
                if verbose {
                    eprintln!("Error processing record: {error}");
                }
            }
        }
    }
    Ok(counts)
}

fn temporary_path(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .with_context(|| format!("Output path has no file name: {}", path.display()))?;
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(".tmp");
    Ok(path.with_file_name(temporary_name))
}

fn validate_output_paths(cli: &Cli) -> Result<()> {
    let mut outputs = vec![cli.output.as_path()];
    outputs.extend(cli.moved_mask.as_deref());
    let mut resources = vec![cli.input.as_path()];
    resources.extend(read_only_resource_paths(cli));
    tools::output_path::ensure_safe_output_paths(&outputs, &resources)
}

fn validate_all_output_paths(cli: &Cli) -> Result<()> {
    let temporary_output = temporary_path(&cli.output)?;
    let temporary_mask = cli.moved_mask.as_deref().map(temporary_path).transpose()?;
    validate_temporary_paths(cli, &temporary_output, temporary_mask.as_deref())
}

/// 処理中に読み込むだけでディスク上に残す前提のファイル (書き込み先と衝突させない)。
fn read_only_resource_paths(cli: &Cli) -> impl Iterator<Item = &Path> {
    [cli.nnue.as_deref(), cli.ls_progress_coeff.as_deref()].into_iter().flatten()
}

fn validate_temporary_paths(cli: &Cli, tmp_output: &Path, tmp_mask: Option<&Path>) -> Result<()> {
    validate_output_paths(cli)?;
    let mut outputs = vec![cli.output.as_path(), tmp_output];
    outputs.extend(cli.moved_mask.as_deref());
    outputs.extend(tmp_mask);
    let mut resources = vec![cli.input.as_path()];
    resources.extend(read_only_resource_paths(cli));
    tools::output_path::ensure_safe_output_paths(&outputs, &resources)
}

fn open_staging_files(
    cli: &Cli,
    paths: &[&Path],
    cleanup: &mut TemporaryFiles,
) -> Result<Vec<File>> {
    let mut resources = vec![cli.input.as_path()];
    resources.extend(read_only_resource_paths(cli));
    let mut files = Vec::with_capacity(paths.len());
    let mut new_files = TemporaryFiles::new();
    for path in paths {
        let existed = path.try_exists()?;
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("Failed to open staging {}", path.display()))?;
        let others: Vec<&Path> = std::iter::once(cli.output.as_path())
            .chain(cli.moved_mask.as_deref())
            .chain(paths.iter().copied().filter(|other| other != path))
            .collect();
        tools::output_path::ensure_safe_output_file(&file, path, &resources, &others)?;
        if !existed {
            new_files.track(path.to_path_buf());
        }
        files.push(file);
    }
    // 後続の open が別の最終出力と同じ実体を作る場合も、truncate 前に拒否する。
    for (index, file) in files.iter().enumerate() {
        let others: Vec<&Path> = std::iter::once(cli.output.as_path())
            .chain(cli.moved_mask.as_deref())
            .chain(
                paths
                    .iter()
                    .enumerate()
                    .filter(|(other, _)| *other != index)
                    .map(|(_, path)| *path),
            )
            .collect();
        tools::output_path::ensure_safe_output_file(file, paths[index], &resources, &others)?;
    }
    for path in paths {
        new_files.untrack(path);
        cleanup.track(path.to_path_buf());
    }
    for file in &files {
        file.set_len(0)?;
    }
    Ok(files)
}

/// ファイルをチャンクストリーミングで処理
fn process_file(cli: &Cli, process_count: u64, use_nnue: bool, opts: ProcessOptions) -> Result<()> {
    validate_output_paths(cli)?;

    let tmp_output = temporary_path(&cli.output)?;
    let tmp_mask = cli.moved_mask.as_deref().map(temporary_path).transpose()?;
    validate_temporary_paths(cli, &tmp_output, tmp_mask.as_deref())?;
    let mut temporary_files = TemporaryFiles::new();

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
    let mut reader = BufReader::with_capacity(IO_BUF_SIZE, in_file);

    let mut staging_paths = vec![tmp_output.as_path()];
    staging_paths.extend(tmp_mask.as_deref());
    let mut files = open_staging_files(cli, &staging_paths, &mut temporary_files)?.into_iter();
    let mut writer =
        BufWriter::with_capacity(IO_BUF_SIZE, files.next().context("Missing PSV staging file")?);
    let mut mask_writer = files
        .next()
        .map(|file| BitmapWriter::new(BufWriter::with_capacity(IO_BUF_SIZE, file)));

    if use_nnue {
        eprintln!("Using NNUE evaluation (with incremental updates)");
    } else {
        eprintln!("Using Material evaluation");
    }

    // カウンタ（メインスレッドのみで加算するため通常の u64）
    let mut error_count = 0u64;
    let mut skipped_count = 0u64;
    let mut moved_count = 0u64;

    let verbose = cli.verbose;
    let mut remaining = process_count as usize;
    let mut chunk: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(CHUNK_SIZE);
    let mut total_written = 0u64;
    let mut total_processed = 0u64;
    let mut buffer = [0u8; PackedSfenValue::SIZE];

    progress.set_message("Processing...");

    // チャンク単位でストリーミング処理
    while remaining > 0 {
        if INTERRUPTED.load(Ordering::Acquire) {
            progress.abandon_with_message("Interrupted");
            drop(writer);
            // 何も公開していないので、ラッパースクリプトが後段へ進まないよう
            // 成功 (exit 0) にはしない。
            anyhow::bail!("Interrupted; output files were not updated");
        }

        // チャンクを読み込み
        chunk.clear();
        let chunk_target = remaining.min(CHUNK_SIZE);
        for _ in 0..chunk_target {
            match reader.read_exact(&mut buffer) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
            chunk.push(buffer);
        }

        if chunk.is_empty() {
            break;
        }

        remaining -= chunk.len();

        // 並列処理
        let results: Vec<ProcessResult> = if use_nnue {
            chunk
                .par_iter()
                .map(|record| {
                    if INTERRUPTED.load(Ordering::Acquire) {
                        return ProcessResult::Ok(*record);
                    }

                    // スレッドローカルでNnueStacksを管理
                    thread_local! {
                        static NNUE_STACKS: RefCell<NnueStacks> = RefCell::new(NnueStacks::new());
                    }

                    NNUE_STACKS.with(|stacks| {
                        let mut stacks = stacks.borrow_mut();
                        stacks.reset();
                        process_record_nnue(record, &mut stacks, opts)
                    })
                })
                .collect()
        } else {
            let evaluator = MaterialEvaluator;
            chunk
                .par_iter()
                .map(|record| {
                    if INTERRUPTED.load(Ordering::Acquire) {
                        return ProcessResult::Ok(*record);
                    }
                    process_record_material(record, &evaluator, opts)
                })
                .collect()
        };

        // 結果を集計・書き出し
        let chunk_count = results.len() as u64;
        let counts = collect_results(&chunk, &results, &mut writer, mask_writer.as_mut(), verbose)?;
        total_written += counts.written;
        error_count += counts.errors;
        skipped_count += counts.skipped;
        moved_count += counts.moved;
        total_processed += chunk_count;

        // チャンク処理完了後にまとめて進捗更新
        progress.inc(chunk_count);
    }

    // 最終チャンクの並列処理中に割り込まれた場合も成果物を確定しない。
    if INTERRUPTED.load(Ordering::Acquire) {
        progress.abandon_with_message("Interrupted");
        anyhow::bail!("Interrupted; output files were not updated");
    }

    writer.flush()?;
    drop(writer);
    if let Some(mask) = mask_writer {
        drop(mask.finish()?);
    }
    // flush 中に割り込まれた場合も rename 前なら公開を止める。
    if INTERRUPTED.load(Ordering::Acquire) {
        progress.abandon_with_message("Interrupted");
        anyhow::bail!("Interrupted; output files were not updated");
    }
    validate_temporary_paths(cli, &tmp_output, tmp_mask.as_deref())?;

    // 正常完了: 一時ファイルを最終出力パスに移動。
    // 前回 run の完成 mask が残っていると、PSV rename 成功 + mask rename 失敗のとき
    // 「新 PSV + 旧 mask」の組が下流の validate_mask (サイズ検査のみ) を素通りするため、
    // PSV を公開する前に旧 mask を消しておく (mask 欠落なら下流は大声で失敗する)。
    if let Some(mask) = &cli.moved_mask
        && let Err(err) = std::fs::remove_file(mask)
        && err.kind() != std::io::ErrorKind::NotFound
    {
        return Err(err).with_context(|| format!("Failed to remove stale mask {}", mask.display()));
    }
    std::fs::rename(&tmp_output, &cli.output).with_context(|| {
        format!("Failed to rename {} -> {}", tmp_output.display(), cli.output.display())
    })?;
    temporary_files.untrack(&tmp_output);
    if let (Some(tmp), Some(output)) = (&tmp_mask, &cli.moved_mask) {
        let renamed = std::fs::rename(tmp, output);
        // rename に失敗しても完成済み mask を Drop で消さず、`.tmp` に残して再試行可能にする。
        temporary_files.untrack(tmp);
        renamed.with_context(|| {
            format!(
                "Failed to rename {} -> {}; finished mask left at {}",
                tmp.display(),
                output.display(),
                tmp.display()
            )
        })?;
    }
    // EOF で早期終了した場合でも進捗バーが100%になるよう実処理件数に合わせる
    progress.set_length(total_processed);
    progress.finish_with_message("Done");

    if total_processed != process_count {
        eprintln!("Note: processed {} records (expected {})", total_processed, process_count);
    }

    let final_errors = error_count;
    let final_skipped = skipped_count;
    if final_errors > 0 {
        eprintln!("Note: {final_errors} positions had errors");
    }
    if final_skipped > 0 {
        if total_processed > 0 {
            eprintln!(
                "Skipped: {} ({:.2}%)",
                final_skipped,
                final_skipped as f64 / total_processed as f64 * 100.0
            );
        } else {
            eprintln!("Skipped: {}", final_skipped);
        }
    }

    if total_written > 0 {
        eprintln!(
            "Moved: {} ({:.2}%)",
            moved_count,
            moved_count as f64 / total_written as f64 * 100.0
        );
    } else {
        eprintln!("Moved: {moved_count}");
    }
    eprintln!("Wrote {} records", total_written);

    Ok(())
}

/// 1レコードを処理（Material評価版）
/// 注意: --rescoreオプションはNNUEモードでのみ有効
fn process_record_material(
    record: &[u8; PackedSfenValue::SIZE],
    evaluator: &MaterialEvaluator,
    opts: ProcessOptions,
) -> ProcessResult {
    // PackedSfenValueを読み込み
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(p) => p,
        None => {
            return ProcessResult::Error(anyhow::anyhow!("Failed to parse PackedSfenValue"));
        }
    };

    // PackedSfen → SFEN → Position
    let sfen = match unpack_sfen(&psv.sfen) {
        Ok(s) => s,
        Err(e) => {
            return ProcessResult::Error(anyhow::anyhow!("Failed to unpack SFEN: {e}"));
        }
    };

    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return ProcessResult::Error(anyhow::anyhow!("Failed to set SFEN: {e:?}"));
    }

    // 王手中の局面の処理
    if pos.in_check() {
        if opts.skip_in_check {
            return ProcessResult::Skip;
        }
        // 王手中はqsearchをスキップして元のレコードを返す
        return ProcessResult::Ok(*record);
    }

    // 元の手番を記録
    let original_stm = pos.side_to_move();

    // qsearch_with_pvを実行
    let result = qsearch_with_pv(
        &mut pos,
        evaluator,
        QSEARCH_ALPHA_INIT,
        QSEARCH_BETA_INIT,
        0,
        opts.max_ply,
    );

    // 結果をPackedSfenValueに変換（Material評価版はrescore非対応）
    finalize_result(&mut pos, &psv, result, original_stm, opts, None)
}

/// 1レコードを処理（NNUE評価版、差分更新）
fn process_record_nnue(
    record: &[u8; PackedSfenValue::SIZE],
    stacks: &mut NnueStacks,
    opts: ProcessOptions,
) -> ProcessResult {
    // PackedSfenValueを読み込み
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(p) => p,
        None => {
            return ProcessResult::Error(anyhow::anyhow!("Failed to parse PackedSfenValue"));
        }
    };

    // PackedSfen → SFEN → Position
    let sfen = match unpack_sfen(&psv.sfen) {
        Ok(s) => s,
        Err(e) => {
            return ProcessResult::Error(anyhow::anyhow!("Failed to unpack SFEN: {e}"));
        }
    };

    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return ProcessResult::Error(anyhow::anyhow!("Failed to set SFEN: {e:?}"));
    }

    // 王手中の局面の処理
    if pos.in_check() {
        if opts.skip_in_check {
            return ProcessResult::Skip;
        }
        // 王手中はqsearchをスキップして元のレコードを返す
        return ProcessResult::Ok(*record);
    }

    // 元の手番を記録
    let original_stm = pos.side_to_move();

    // qsearch_with_pv_nnueを実行（差分更新版）
    let result = qsearch_with_pv_nnue(
        &mut pos,
        stacks,
        QSEARCH_ALPHA_INIT,
        QSEARCH_BETA_INIT,
        0,
        opts.max_ply,
    );

    // 結果をPackedSfenValueに変換（rescore対応）
    finalize_result(&mut pos, &psv, result, original_stm, opts, Some(stacks))
}

/// qsearch結果をPackedSfenValueに変換
///
/// # Arguments
/// * `pos` - qsearch実行後の局面（まだPV進行していない）
/// * `psv` - 元のPackedSfenValue
/// * `result` - qsearchの結果
/// * `original_stm` - 元の局面の手番
/// * `opts` - 処理オプション
/// * `stacks` - NNUE評価用スタック（rescore時に使用、NoneならMaterial評価版）
fn finalize_result(
    pos: &mut Position,
    psv: &PackedSfenValue,
    result: QsearchResult,
    original_stm: rshogi_core::types::Color,
    opts: ProcessOptions,
    stacks: Option<&mut NnueStacks>,
) -> ProcessResult {
    // PVに沿って局面を進める
    for mv in &result.pv {
        let gives_check = pos.gives_check(*mv);
        let _ = pos.do_move(*mv, gives_check);
    }

    // 手番が変わったかチェック
    let stm_changed = pos.side_to_move() != original_stm;

    // スコアの決定
    let new_score = if opts.rescore {
        // --rescore: NNUEで再評価（推奨）
        // leaf位置の局面をNNUEで評価し、局面とスコアの整合性を保証
        if let Some(stacks) = stacks {
            stacks.reset();
            let raw_score = stacks.evaluate(pos);
            // スコアをクリップ
            raw_score.clamp(-opts.score_clip as i32, opts.score_clip as i32) as i16
        } else {
            // Material評価版は--rescore非対応なので元スコアを使用
            if opts.fix_stm_sign && stm_changed {
                psv.score.saturating_neg()
            } else {
                psv.score
            }
        }
    } else {
        // 元スコアを使用（従来の動作）
        // 注意: これは局面とスコアの不整合を引き起こす可能性がある
        if opts.fix_stm_sign && stm_changed {
            psv.score.saturating_neg()
        } else {
            psv.score
        }
    };

    // game_resultの決定（手番が変わった場合は反転）
    let new_game_result = if opts.fix_stm_sign && stm_changed {
        -psv.game_result
    } else {
        psv.game_result
    };

    // 新しいPackedSfenValueを作成
    let new_sfen = pack_position(pos);

    // move16は0（無効値）に設定
    // 理由: PV末端局面に置換した後、元のmoveやqsearch結果のPVは
    // 置換後局面での合法手ではない。nnue-pytorchの--smart-fen-skipping
    // オプションはmove16を使ってisCapturingMove()を判定するため、
    // 非合法手が設定されていると未定義動作やスキップ判定の破綻を招く。
    let new_move16 = 0;

    // game_plyはPV長分を加算
    // 理由: PVで局面を進めた分だけ手数が増えている
    let new_game_ply = psv.game_ply.saturating_add(result.pv.len() as u16);

    let new_psv = PackedSfenValue {
        sfen: new_sfen,
        score: new_score,
        move16: new_move16,
        game_ply: new_game_ply,
        game_result: new_game_result,
        padding: 0,
    };

    ProcessResult::Ok(new_psv.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static PROCESS_FILE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn test_cli(input: PathBuf, output: PathBuf, moved_mask: Option<PathBuf>) -> Cli {
        Cli {
            input,
            output,
            moved_mask,
            max_ply: 16,
            threads: 1,
            nnue: None,
            ls_bucket_mode: None,
            ls_progress_buckets: None,
            ls_progress_coeff: None,
            limit: 0,
            verbose: false,
            no_fix_stm_sign: false,
            rescore: false,
            skip_in_check: false,
            score_clip: 10000,
        }
    }

    #[test]
    fn moved_mask_byte_boundaries() {
        for (count, expected) in [
            (0, vec![]),
            (1, vec![1]),
            (7, vec![127]),
            (8, vec![255]),
            (9, vec![255, 1]),
        ] {
            let mut writer = BitmapWriter::new(Vec::new());
            for _ in 0..count {
                writer.push(true).unwrap();
            }
            assert_eq!(writer.finish().unwrap(), expected, "records={count}");
        }
    }

    #[test]
    fn newly_materialized_case_alias_preserves_existing_staging() {
        let dir = tempfile::tempdir().unwrap();
        let probe = dir.path().join("CaseProbe");
        std::fs::write(&probe, []).unwrap();
        if !dir.path().join("caseprobe").exists() {
            return;
        }
        let input = dir.path().join("input.psv");
        let output = dir.path().join("MASK.TMP");
        let mask = dir.path().join("mask");
        let first = temporary_path(&output).unwrap();
        let second = temporary_path(&mask).unwrap();
        std::fs::write(&input, b"input").unwrap();
        std::fs::write(&first, b"previous staging").unwrap();
        let cli = test_cli(input.clone(), output.clone(), Some(mask));
        validate_all_output_paths(&cli).unwrap();
        let mut cleanup = TemporaryFiles::new();
        assert!(open_staging_files(&cli, &[&first, &second], &mut cleanup).is_err());
        drop(cleanup);
        assert_eq!(std::fs::read(first).unwrap(), b"previous staging");
        assert_eq!(std::fs::read(input).unwrap(), b"input");
        assert!(std::fs::read(output).unwrap().is_empty());
    }

    #[test]
    fn newly_materialized_final_case_alias_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("CaseProbe"), []).unwrap();
        if !dir.path().join("caseprobe").exists() {
            return;
        }
        let input = dir.path().join("input.psv");
        let output = dir.path().join("Leaf.psv");
        let mask = dir.path().join("leaf.psv");
        std::fs::write(&input, b"input").unwrap();
        let cli = test_cli(input.clone(), output.clone(), Some(mask.clone()));
        validate_all_output_paths(&cli).unwrap();
        let first = temporary_path(&output).unwrap();
        let second = temporary_path(&mask).unwrap();
        let mut cleanup = TemporaryFiles::new();
        assert!(open_staging_files(&cli, &[&first, &second], &mut cleanup).is_err());
        drop(cleanup);
        assert_eq!(std::fs::read(input).unwrap(), b"input");
        assert!(!output.exists());
        assert!(!mask.exists());
    }

    #[test]
    fn staging_handle_rejection_preserves_existing_files() {
        for protect_final in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let input = dir.path().join("input.psv");
            let output = dir.path().join("output.psv");
            let mask = dir.path().join("mask.bits");
            let first = temporary_path(&output).unwrap();
            let second = temporary_path(&mask).unwrap();
            std::fs::write(&input, b"input").unwrap();
            std::fs::write(&output, b"final").unwrap();
            std::fs::write(&first, b"previous staging").unwrap();
            let protected = if protect_final { &output } else { &input };
            std::fs::hard_link(protected, &second).unwrap();
            let cli = test_cli(input.clone(), output.clone(), Some(mask));
            let mut cleanup = TemporaryFiles::new();
            assert!(open_staging_files(&cli, &[&first, &second], &mut cleanup).is_err());
            drop(cleanup);
            assert_eq!(std::fs::read(&first).unwrap(), b"previous staging");
            assert_eq!(std::fs::read(&input).unwrap(), b"input");
            assert_eq!(std::fs::read(&output).unwrap(), b"final");
            assert!(second.exists());
        }
    }

    #[test]
    fn staging_handles_reject_each_other_without_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.psv");
        let first = temporary_path(&output).unwrap();
        let second = dir.path().join("mask.tmp");
        std::fs::write(&input, b"input").unwrap();
        std::fs::write(&first, b"staging").unwrap();
        std::fs::hard_link(&first, &second).unwrap();
        let cli = test_cli(input, output, None);
        let mut cleanup = TemporaryFiles::new();
        assert!(open_staging_files(&cli, &[&first, &second], &mut cleanup).is_err());
        drop(cleanup);
        assert_eq!(std::fs::read(first).unwrap(), b"staging");
        assert_eq!(std::fs::read(second).unwrap(), b"staging");
    }

    #[test]
    fn publish_validation_rechecks_changed_resource_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.psv");
        let mask = dir.path().join("mask.bits");
        std::fs::write(&input, b"input").unwrap();
        let cli = test_cli(input.clone(), output, Some(mask.clone()));
        validate_all_output_paths(&cli).unwrap();
        std::fs::hard_link(&input, &mask).unwrap();
        assert!(validate_all_output_paths(&cli).is_err());
        assert_eq!(std::fs::read(input).unwrap(), b"input");
        assert_eq!(std::fs::read(mask).unwrap(), b"input");
    }

    #[test]
    fn moved_mask_is_lsb_first_and_clears_unused_bits() {
        let mut mask = BitmapWriter::new(Vec::new());
        for moved in [
            true, false, false, false, false, false, false, true, true, false,
        ] {
            mask.push(moved).unwrap();
        }

        let bytes = mask.finish().unwrap();
        assert_eq!(bytes, [0b1000_0001, 0b0000_0001]);
    }

    #[test]
    fn collect_results_uses_only_packed_sfen_and_drops_error_rows() {
        let unchanged = [0x11; PackedSfenValue::SIZE];
        let mut metadata_only = unchanged;
        metadata_only[32] = 0x22;
        let error_original = [0x66; PackedSfenValue::SIZE];
        let mut moved_original = [0x33; PackedSfenValue::SIZE];
        moved_original[32] = 0x44;
        let mut moved = moved_original;
        moved[31] = 0x55;
        // エラー行を moved 行より前に置き、bit が出力行番号へ詰まることを確認する
        let originals = [unchanged, error_original, moved_original];
        let results = [
            ProcessResult::Ok(metadata_only),
            ProcessResult::Error(anyhow::anyhow!("test error")),
            ProcessResult::Ok(moved),
        ];
        let mut output = Vec::new();
        let mut mask = BitmapWriter::new(Vec::new());

        let counts =
            collect_results(&originals, &results, &mut output, Some(&mut mask), false).unwrap();
        let mask = mask.finish().unwrap();

        assert_eq!(
            counts,
            ResultCounts {
                errors: 1,
                written: 2,
                moved: 1,
                ..Default::default()
            }
        );
        // エラー行は出力にも mask にも現れない: 出力 2 行、moved は出力 row 1
        assert_eq!(mask, [0b0000_0010]);
        assert_eq!(output.len(), PackedSfenValue::SIZE * 2);
        assert_eq!(&output[..PackedSfenValue::SIZE], &metadata_only);
        assert_eq!(&output[PackedSfenValue::SIZE..], &moved);
    }

    #[test]
    fn generated_mask_passes_consumer_validate_mask_contract() {
        // 8 の倍数でない出力件数 (10 行) の mask が、consumer 側
        // (psv_select_by_mask / psv_scatter_by_mask) の契約検証をそのまま通ること。
        let dir = tempfile::tempdir().unwrap();
        let mask_path = dir.path().join("moved.bits");
        let mut mask = BitmapWriter::new(Vec::new());
        for i in 0..10 {
            mask.push(i % 3 == 0).unwrap();
        }
        std::fs::write(&mask_path, mask.finish().unwrap()).unwrap();
        tools::mask_io::validate_mask(&mask_path, 10).unwrap();
        // サイズ不一致 (7 件なら ceil(7/8)=1 byte のはず) は拒否される
        assert!(tools::mask_io::validate_mask(&mask_path, 7).is_err());
    }

    #[test]
    fn error_rows_are_dropped_identically_without_mask() {
        let ok_record = [0x11; PackedSfenValue::SIZE];
        let error_original = [0x66; PackedSfenValue::SIZE];
        let originals = [error_original, ok_record];
        let results = [
            ProcessResult::Error(anyhow::anyhow!("test error")),
            ProcessResult::Ok(ok_record),
        ];
        let mut output = Vec::new();

        let counts = collect_results(
            &originals,
            &results,
            &mut output,
            None::<&mut BitmapWriter<Vec<u8>>>,
            false,
        )
        .unwrap();

        assert_eq!(
            counts,
            ResultCounts {
                errors: 1,
                written: 1,
                moved: 0,
                ..Default::default()
            }
        );
        assert_eq!(output, ok_record);
    }

    #[test]
    fn skipped_rows_do_not_shift_output_mask_indices() {
        let skipped = [0x10; PackedSfenValue::SIZE];
        let moved_original = [0x20; PackedSfenValue::SIZE];
        let mut moved = moved_original;
        moved[0] = 0x21;
        let unchanged = [0x30; PackedSfenValue::SIZE];
        let originals = [skipped, moved_original, unchanged];
        let results = [
            ProcessResult::Skip,
            ProcessResult::Ok(moved),
            ProcessResult::Ok(unchanged),
        ];
        let mut output = Vec::new();
        let mut mask = BitmapWriter::new(Vec::new());

        let counts =
            collect_results(&originals, &results, &mut output, Some(&mut mask), false).unwrap();
        let mask = mask.finish().unwrap();

        assert_eq!(counts.skipped, 1);
        assert_eq!(counts.written, 2);
        assert_eq!(mask, [0b0000_0001]);
        assert_eq!(output.len(), PackedSfenValue::SIZE * 2);
    }

    #[test]
    fn moved_mask_rejects_input_or_output_same_path() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        std::fs::write(&input, []).unwrap();
        let output = dir.path().join("output.psv");

        let input_collision = test_cli(input.clone(), output.clone(), Some(input.clone()));
        assert!(validate_output_paths(&input_collision).is_err());

        let output_collision = test_cli(input, output.clone(), Some(output));
        assert!(validate_output_paths(&output_collision).is_err());
    }

    #[test]
    fn nnue_model_path_is_rejected_as_write_target() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let nnue = dir.path().join("model.nnue");
        std::fs::write(&input, []).unwrap();
        std::fs::write(&nnue, [0u8]).unwrap();

        // 出力 / mask が NNUE モデルと同一実体なら拒否する。
        let mut mask_collision =
            test_cli(input.clone(), dir.path().join("output.psv"), Some(nnue.clone()));
        mask_collision.nnue = Some(nnue.clone());
        assert!(validate_output_paths(&mask_collision).is_err());

        let mut output_collision =
            test_cli(input.clone(), nnue.clone(), Some(dir.path().join("moved.bits")));
        output_collision.nnue = Some(nnue.clone());
        assert!(validate_output_paths(&output_collision).is_err());

        // progress 係数も同じ read-only resource として拒否する。
        let mut coeff_collision =
            test_cli(input.clone(), dir.path().join("output.psv"), Some(nnue.clone()));
        coeff_collision.ls_progress_coeff = Some(nnue.clone());
        assert!(validate_output_paths(&coeff_collision).is_err());

        // `.tmp` が NNUE モデル / 係数と同一実体でも拒否する (truncate / 終了時削除を防ぐ)。
        let output = dir.path().join("output.psv");
        let mask = dir.path().join("moved.bits");
        for set_resource in [
            (|cli: &mut Cli, p: PathBuf| cli.nnue = Some(p)) as fn(&mut Cli, PathBuf),
            |cli, p| cli.ls_progress_coeff = Some(p),
        ] {
            let mut cli = test_cli(input.clone(), output.clone(), Some(mask.clone()));
            let resource = temporary_path(&mask).unwrap();
            std::fs::write(&resource, [0u8]).unwrap();
            set_resource(&mut cli, resource);
            assert!(
                validate_temporary_paths(
                    &cli,
                    &temporary_path(&output).unwrap(),
                    Some(&temporary_path(&mask).unwrap())
                )
                .is_err()
            );
        }
    }

    #[test]
    fn successful_processing_renames_both_temporary_files() {
        let _lock = PROCESS_FILE_TEST_LOCK.lock().unwrap();
        INTERRUPTED.store(false, Ordering::Release);
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.psv");
        let moved_mask = dir.path().join("moved.bits");
        std::fs::write(&input, []).unwrap();
        std::fs::write(temporary_path(&output).unwrap(), b"old psv staging").unwrap();
        std::fs::write(temporary_path(&moved_mask).unwrap(), b"old mask staging").unwrap();
        let cli = test_cli(input, output.clone(), Some(moved_mask.clone()));
        let opts = ProcessOptions {
            max_ply: 16,
            fix_stm_sign: true,
            rescore: false,
            skip_in_check: false,
            score_clip: 10000,
        };

        process_file(&cli, 0, false, opts).unwrap();

        assert!(std::fs::read(&output).unwrap().is_empty());
        assert!(std::fs::read(&moved_mask).unwrap().is_empty());
        assert!(!temporary_path(&output).unwrap().exists());
        assert!(!temporary_path(&moved_mask).unwrap().exists());
    }

    #[test]
    fn failed_processing_leaves_no_final_or_partial_files() {
        let _lock = PROCESS_FILE_TEST_LOCK.lock().unwrap();
        INTERRUPTED.store(false, Ordering::Release);
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.psv");
        let moved_mask = dir.path().join("moved.bits");
        std::fs::write(&input, []).unwrap();
        std::fs::create_dir(temporary_path(&moved_mask).unwrap()).unwrap();
        let cli = test_cli(input, output.clone(), Some(moved_mask.clone()));
        let opts = ProcessOptions {
            max_ply: 16,
            fix_stm_sign: true,
            rescore: false,
            skip_in_check: false,
            score_clip: 10000,
        };

        assert!(process_file(&cli, 0, false, opts).is_err());

        assert!(!output.exists());
        assert!(!moved_mask.exists());
        assert!(!temporary_path(&output).unwrap().exists());
    }

    #[test]
    fn stale_final_mask_is_removed_before_psv_publish() {
        let _lock = PROCESS_FILE_TEST_LOCK.lock().unwrap();
        INTERRUPTED.store(false, Ordering::Release);
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.psv");
        let moved_mask = dir.path().join("moved.bits");
        std::fs::write(&input, []).unwrap();
        std::fs::write(&moved_mask, b"stale").unwrap();
        let opts = ProcessOptions {
            max_ply: 16,
            fix_stm_sign: true,
            rescore: false,
            skip_in_check: false,
            score_clip: 10000,
        };

        let cli = test_cli(input.clone(), output.clone(), Some(moved_mask.clone()));
        process_file(&cli, 0, false, opts).unwrap();
        // 前回 run の mask は新しい mask で置き換わる (0 レコードなので空)。
        assert!(std::fs::read(&moved_mask).unwrap().is_empty());

        // 旧 mask を消せない場合は PSV を公開する前に失敗する
        // (新 PSV + 旧 mask の組を作らない)。
        std::fs::write(&output, b"previous psv").unwrap();
        std::fs::remove_file(&moved_mask).unwrap();
        std::fs::create_dir(&moved_mask).unwrap();
        let error = process_file(&cli, 0, false, opts).unwrap_err();
        let message = format!("{error:#}");
        let expected = if cfg!(windows) {
            "Failed to inspect output"
        } else {
            "stale mask"
        };
        assert!(message.contains(expected), "{message}");
        assert_eq!(std::fs::read(&output).unwrap(), b"previous psv");
        assert!(!temporary_path(&output).unwrap().exists());
        assert!(!temporary_path(&moved_mask).unwrap().exists());
    }

    #[test]
    fn interruption_before_publish_removes_both_temporary_files() {
        let _lock = PROCESS_FILE_TEST_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("input.psv");
        let output = dir.path().join("output.psv");
        let moved_mask = dir.path().join("moved.bits");
        std::fs::write(&input, []).unwrap();
        let cli = test_cli(input, output.clone(), Some(moved_mask.clone()));
        let opts = ProcessOptions {
            max_ply: 16,
            fix_stm_sign: true,
            rescore: false,
            skip_in_check: false,
            score_clip: 10000,
        };
        INTERRUPTED.store(true, Ordering::Release);

        let result = process_file(&cli, 0, false, opts);
        INTERRUPTED.store(false, Ordering::Release);

        // 何も公開しなかった run を成功 (exit 0) にすると、ラッパースクリプトが
        // 欠けた成果物のまま後段へ進んでしまう。
        assert!(result.is_err());
        assert!(!output.exists());
        assert!(!moved_mask.exists());
        assert!(!temporary_path(&output).unwrap().exists());
        assert!(!temporary_path(&moved_mask).unwrap().exists());
    }
}
