//! rescore_pack - packファイルの評価値をNNUEで再評価
//!
//! qsearch leaf置換後の局面に対して、指定したNNUEモデルで評価値を再計算する。
//! これにより「入力局面とラベル（スコア）の整合性」を確保する。
//!
//! # 使用例
//!
//! ```bash
//! # NNUE静的評価で再スコア（高速）
//! cargo run --release -p tools --bin rescore_pack -- \
//!   --input data.pack --output-dir rescored/ \
//!   --nnue path/to/nn.bin
//!
//! # 複数ファイルを処理（glob パターン）
//! cargo run --release -p tools --bin rescore_pack -- \
//!   --input "data/*.bin" --output-dir rescored/ \
//!   --nnue path/to/nn.bin
//!
//! # qsearch評価で再スコア（より正確）
//! cargo run --release -p tools --bin rescore_pack -- \
//!   --input data.pack --output-dir rescored/ \
//!   --nnue path/to/nn.bin --use-qsearch
//!
//! # qsearch leaf置換も同時に実行
//! cargo run --release -p tools --bin rescore_pack -- \
//!   --input data.pack --output-dir rescored/ \
//!   --nnue path/to/nn.bin --apply-qsearch-leaf
//!
//! # 深さ指定探索で再スコア（最も正確だが低速）
//! cargo run --release -p tools --bin rescore_pack -- \
//!   --input "data/*.bin" --output-dir rescored/ \
//!   --nnue path/to/nn.bin \
//!   --search-depth 8 \
//!   --hash-mb 256 \
//!   --threads 4
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use glob::glob;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

use engine_core::nnue::init_nnue;
use engine_core::position::Position;
use engine_core::search::{LimitsType, Search};
use tools::packed_sfen::{pack_position, unpack_sfen, PackedSfenValue};
use tools::qsearch_pv::{qsearch_with_pv_nnue, NnueStacks};

/// 探索用スタックサイズ（64MB）
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

/// packファイルの評価値をNNUEで再評価
#[derive(Parser)]
#[command(
    name = "rescore_pack",
    version,
    about = "packファイルの評価値をNNUEで再評価\n\n局面とスコアの整合性を確保するためのツール"
)]
struct Cli {
    /// 入力packファイル（複数指定可、globパターン対応）
    /// 例: --input file1.bin --input file2.bin
    /// 例: --input "data/*.bin"
    #[arg(short, long, required = true, num_args = 1..)]
    input: Vec<String>,

    /// 出力ディレクトリ（入力ファイル名で出力）
    #[arg(short, long)]
    output_dir: PathBuf,

    /// NNUEモデルファイル（必須）
    #[arg(long)]
    nnue: PathBuf,

    /// qsearch評価を使用（デフォルトは静的評価）
    #[arg(long)]
    use_qsearch: bool,

    /// 深さ指定探索を使用（--use-qsearchと排他）
    /// 指定した深さでalpha-beta探索を実行し、その結果をスコアとして使用
    #[arg(long)]
    search_depth: Option<i32>,

    /// 置換表サイズ（MB）、--search-depth使用時のみ有効
    #[arg(long, default_value_t = 64)]
    hash_mb: usize,

    /// 探索ノード数の上限（0=無制限）、--search-depth使用時のみ有効
    /// 複雑な局面での探索時間爆発を防ぐため、100万〜1000万程度を推奨
    #[arg(long, default_value_t = 0)]
    max_nodes: u64,

    /// 1局面あたりの探索時間上限（ミリ秒、0=無制限）、--search-depth使用時のみ有効
    /// 複雑な局面での探索時間爆発を防ぐため、1000〜10000程度を推奨
    #[arg(long, default_value_t = 0)]
    max_time: i64,

    /// qsearch leaf置換も同時に適用
    #[arg(long)]
    apply_qsearch_leaf: bool,

    /// qsearchの最大深さ
    #[arg(long, default_value_t = 16)]
    max_ply: i32,

    /// 並列処理スレッド数（0=自動）
    #[arg(short, long, default_value_t = 0)]
    threads: usize,

    /// 処理するレコード数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: u64,

    /// スコアのクリップ範囲（±この値にクリップ）
    #[arg(long, default_value_t = 10000)]
    score_clip: i16,

    /// 王手局面をスキップ（出力から除外）
    #[arg(long)]
    skip_in_check: bool,

    /// 入力NNUEのFV_SCALE（nn.bin=24, nnue-pytorch形式=16）
    /// 注意: スコアはセンチポーン単位で出力されるため、通常は変換不要
    #[arg(long, default_value_t = 24)]
    source_fv_scale: i32,

    /// 出力スコアのFV_SCALE
    /// デフォルトではsource_fv_scaleと同じ（変換なし）
    /// nnue-pytorchはセンチポーン単位のスコアをそのまま使用するため、
    /// 通常は変換不要。16に変更すると1.5倍のスケーリングが適用される。
    #[arg(long, default_value_t = 24)]
    target_fv_scale: i32,

    /// 詳細出力
    #[arg(short, long)]
    verbose: bool,

    /// 処理完了後に入力ファイルを削除
    /// ディスク容量節約のため、各ファイルの処理完了後に入力を削除
    #[arg(long)]
    delete_input: bool,
}

/// 処理中にCtrl-Cが押されたかを追跡
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// qsearchの初期alpha値
const QSEARCH_ALPHA_INIT: i32 = -30000;
/// qsearchの初期beta値
const QSEARCH_BETA_INIT: i32 = 30000;

/// 入力パターンをglobで展開してファイルリストを取得
fn expand_input_patterns(patterns: &[String]) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for pattern in patterns {
        // まず通常のファイルとして存在するか確認
        let path = PathBuf::from(pattern);
        if path.exists() && path.is_file() {
            files.push(path);
            continue;
        }

        // globパターンとして展開
        let matches: Vec<_> = glob(pattern)
            .with_context(|| format!("Invalid glob pattern: {pattern}"))?
            .filter_map(|entry| entry.ok())
            .filter(|p| p.is_file())
            .collect();

        if matches.is_empty() {
            // ファイルが見つからない場合はエラー
            anyhow::bail!("No files found matching pattern: {pattern}");
        }

        files.extend(matches);
    }

    // 重複を除去してソート
    files.sort();
    files.dedup();

    Ok(files)
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    // --use-qsearch と --search-depth は排他
    if cli.use_qsearch && cli.search_depth.is_some() {
        anyhow::bail!("--use-qsearch and --search-depth are mutually exclusive");
    }

    // --search-depth 指定時に --apply-qsearch-leaf が有効なら警告
    if cli.search_depth.is_some() && cli.apply_qsearch_leaf {
        eprintln!("Warning: --apply-qsearch-leaf is ignored when --search-depth is specified");
    }

    // 入力ファイルをglobパターンで展開
    let input_files = expand_input_patterns(&cli.input)?;
    if input_files.is_empty() {
        anyhow::bail!("No input files found matching the patterns");
    }

    eprintln!("Found {} input file(s)", input_files.len());

    // 出力ディレクトリの作成
    if !cli.output_dir.exists() {
        fs::create_dir_all(&cli.output_dir).with_context(|| {
            format!("Failed to create output directory: {}", cli.output_dir.display())
        })?;
    }

    // NNUEモデルのロード
    if !cli.nnue.exists() {
        anyhow::bail!("NNUE model file not found: {}", cli.nnue.display());
    }
    init_nnue(&cli.nnue).context("Failed to load NNUE model")?;
    eprintln!("NNUE model loaded: {}", cli.nnue.display());

    // Ctrl-Cハンドラを設定
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted!");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // rayon スレッドプール設定（process_file で使用、process_file_with_search では独自スレッド管理）
    if cli.search_depth.is_none() && cli.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
            .unwrap_or_else(|e| {
                eprintln!("Warning: Failed to set thread count: {e}");
            });
    }

    // 処理設定の表示
    eprintln!(
        "Mode: {}",
        if let Some(depth) = cli.search_depth {
            format!("depth {depth} search")
        } else if cli.use_qsearch {
            "qsearch evaluation".to_string()
        } else {
            "static NNUE evaluation".to_string()
        }
    );
    if cli.search_depth.is_some() {
        eprintln!("Hash size: {} MB", cli.hash_mb);
        if cli.max_nodes > 0 {
            eprintln!("Max nodes: {} (per position)", cli.max_nodes);
        } else {
            eprintln!("Max nodes: unlimited");
        }
        if cli.max_time > 0 {
            eprintln!("Max time: {} ms (per position)", cli.max_time);
        } else {
            eprintln!("Max time: unlimited");
        }
    }
    eprintln!(
        "qsearch leaf replacement: {}",
        if cli.apply_qsearch_leaf && cli.search_depth.is_none() {
            "enabled"
        } else {
            "disabled"
        }
    );
    eprintln!("Score clip: ±{}", cli.score_clip);
    eprintln!("Skip in-check positions: {}", if cli.skip_in_check { "yes" } else { "no" });
    eprintln!(
        "FV_SCALE conversion: {} -> {} (factor: {:.3})",
        cli.source_fv_scale,
        cli.target_fv_scale,
        cli.source_fv_scale as f64 / cli.target_fv_scale as f64
    );
    eprintln!("Output directory: {}", cli.output_dir.display());
    if cli.delete_input {
        eprintln!("Delete input after processing: yes");
    }
    eprintln!();

    // 各ファイルを処理
    let total_files = input_files.len();
    for (file_idx, input_path) in input_files.iter().enumerate() {
        if INTERRUPTED.load(Ordering::SeqCst) {
            eprintln!("Processing interrupted");
            break;
        }

        // 出力ファイルパスを生成
        let output_path =
            cli.output_dir.join(input_path.file_name().ok_or_else(|| {
                anyhow::anyhow!("Invalid input file name: {}", input_path.display())
            })?);

        // 入力と出力が同じパスの場合はエラー（--delete-input でデータ消失を防ぐ）
        if input_path.canonicalize().ok() == output_path.canonicalize().ok() {
            anyhow::bail!(
                "Input and output paths are the same: {}. Use a different --output-dir.",
                input_path.display()
            );
        }

        eprintln!(
            "=== [{}/{}] Processing: {} ===",
            file_idx + 1,
            total_files,
            input_path.display()
        );

        // ファイルサイズからレコード数を計算
        let file_size = fs::metadata(input_path)?.len();
        let record_count = file_size / PackedSfenValue::SIZE as u64;

        if file_size % PackedSfenValue::SIZE as u64 != 0 {
            eprintln!(
                "Warning: File size ({file_size}) is not a multiple of record size ({})",
                PackedSfenValue::SIZE
            );
        }

        let process_count = if cli.limit > 0 && cli.limit < record_count {
            cli.limit
        } else {
            record_count
        };

        eprintln!("Records: {record_count}, Processing: {process_count}");

        // 必要メモリの概算と警告（入力バッファ + 出力バッファ）
        let required_memory_mb =
            (process_count as usize * PackedSfenValue::SIZE * 2) / (1024 * 1024);
        if required_memory_mb > 1024 {
            eprintln!(
                "Warning: Estimated memory usage: {} GB. Ensure sufficient RAM is available.",
                required_memory_mb / 1024
            );
        }

        // 処理実行
        if cli.search_depth.is_some() {
            process_file_with_search(&cli, input_path, &output_path, process_count)?;
        } else {
            process_file(&cli, input_path, &output_path, process_count)?;
        }

        if !INTERRUPTED.load(Ordering::SeqCst) {
            eprintln!("Output: {}", output_path.display());

            // 処理完了後に入力ファイルを削除
            if cli.delete_input {
                let output_size = fs::metadata(&output_path).map(|m| m.len()).unwrap_or(0);
                if output_size > 0 {
                    fs::remove_file(input_path).with_context(|| {
                        format!("Failed to delete input file: {}", input_path.display())
                    })?;
                    eprintln!("Deleted input: {}", input_path.display());
                } else {
                    eprintln!(
                        "Warning: Output file is empty or missing, keeping input file: {}",
                        input_path.display()
                    );
                }
            }
        }
        eprintln!();
    }

    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("Note: Processing was interrupted, some outputs may be incomplete");
    } else {
        eprintln!("All {} file(s) processed successfully", total_files);
    }

    Ok(())
}

/// ファイルを処理
fn process_file(
    cli: &Cli,
    input_path: &PathBuf,
    output_path: &PathBuf,
    process_count: u64,
) -> Result<()> {
    // 進捗バー設定
    let progress = ProgressBar::new(process_count);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    // 入力ファイルを読み込み
    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
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
    eprintln!("Read {actual_count} records");

    if actual_count == 0 {
        eprintln!("Warning: No records to process, creating empty output file");
        progress.finish_with_message("Done (empty)");
        File::create(output_path)?;
        return Ok(());
    }

    // エラーカウンタ
    let error_count = AtomicU64::new(0);
    let processed_count = AtomicU64::new(0);
    let clipped_count = AtomicU64::new(0);

    // 処理
    progress.set_message("Processing...");
    let max_ply = cli.max_ply;
    let use_qsearch = cli.use_qsearch;
    let apply_leaf = cli.apply_qsearch_leaf;
    let score_clip = cli.score_clip;
    let skip_in_check = cli.skip_in_check;
    let source_fv_scale = cli.source_fv_scale;
    let target_fv_scale = cli.target_fv_scale;
    let verbose = cli.verbose;

    // スキップカウンタ
    let skipped_count = AtomicU64::new(0);

    let processed_records: Vec<[u8; PackedSfenValue::SIZE]> = records
        .par_iter()
        .filter_map(|record| {
            if INTERRUPTED.load(Ordering::SeqCst) {
                return Some(*record);
            }

            // スレッドローカルでNnueStacksを管理
            thread_local! {
                static NNUE_STACKS: RefCell<NnueStacks> = RefCell::new(NnueStacks::new());
            }

            let result = NNUE_STACKS.with(|stacks| {
                let mut stacks = stacks.borrow_mut();
                stacks.reset();
                process_record(
                    record,
                    &mut stacks,
                    max_ply,
                    use_qsearch,
                    apply_leaf,
                    score_clip,
                    skip_in_check,
                    source_fv_scale,
                    target_fv_scale,
                )
            });

            match result {
                ProcessResult::Ok(new_record, clipped) => {
                    processed_count.fetch_add(1, Ordering::Relaxed);
                    if clipped {
                        clipped_count.fetch_add(1, Ordering::Relaxed);
                    }
                    progress.inc(1);
                    Some(new_record)
                }
                ProcessResult::Skip => {
                    skipped_count.fetch_add(1, Ordering::Relaxed);
                    progress.inc(1);
                    None
                }
                ProcessResult::Error(e) => {
                    error_count.fetch_add(1, Ordering::Relaxed);
                    if verbose {
                        eprintln!("Error processing record: {e}");
                    }
                    progress.inc(1);
                    None
                }
            }
        })
        .collect();

    progress.finish_with_message("Done");

    let final_errors = error_count.load(Ordering::SeqCst);
    let final_clipped = clipped_count.load(Ordering::SeqCst);
    let final_skipped = skipped_count.load(Ordering::SeqCst);
    if final_errors > 0 {
        eprintln!("Note: {final_errors} positions had errors");
    }
    if final_skipped > 0 {
        eprintln!(
            "Skipped (in check): {} ({:.2}%)",
            final_skipped,
            final_skipped as f64 / actual_count as f64 * 100.0
        );
    }
    eprintln!(
        "Clipped scores: {} ({:.2}%)",
        final_clipped,
        final_clipped as f64 / actual_count as f64 * 100.0
    );

    // 出力ファイルに書き込み
    eprintln!("Writing output...");
    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
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

/// 処理結果
enum ProcessResult {
    /// 正常に処理完了（新レコード, クリップされたか）
    Ok([u8; PackedSfenValue::SIZE], bool),
    /// スキップ（王手局面など）
    Skip,
    /// エラー
    Error(anyhow::Error),
}

/// 1レコードを処理
fn process_record(
    record: &[u8; PackedSfenValue::SIZE],
    stacks: &mut NnueStacks,
    max_ply: i32,
    use_qsearch: bool,
    apply_leaf: bool,
    score_clip: i16,
    skip_in_check: bool,
    source_fv_scale: i32,
    target_fv_scale: i32,
) -> ProcessResult {
    // PackedSfenValueを読み込み
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(p) => p,
        None => return ProcessResult::Error(anyhow::anyhow!("Failed to parse PackedSfenValue")),
    };

    // PackedSfen → SFEN → Position
    let sfen = match unpack_sfen(&psv.sfen) {
        Ok(s) => s,
        Err(e) => return ProcessResult::Error(anyhow::anyhow!("Failed to unpack SFEN: {e}")),
    };

    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return ProcessResult::Error(anyhow::anyhow!("Failed to set SFEN: {e:?}"));
    }

    // 王手局面をスキップ
    if skip_in_check && pos.in_check() {
        return ProcessResult::Skip;
    }

    // 元の手番を記録
    let original_stm = pos.side_to_move();

    // qsearch leaf置換を適用する場合
    let (final_sfen, stm_changed) = if apply_leaf && !pos.in_check() {
        let result = qsearch_with_pv_nnue(
            &mut pos,
            stacks,
            QSEARCH_ALPHA_INIT,
            QSEARCH_BETA_INIT,
            0,
            max_ply,
        );

        // PVに沿って局面を進める
        for mv in &result.pv {
            let gives_check = pos.gives_check(*mv);
            let _ = pos.do_move(*mv, gives_check);
        }

        let stm_changed = pos.side_to_move() != original_stm;
        let new_sfen = pack_position(&pos);
        (new_sfen, stm_changed)
    } else {
        (psv.sfen, false)
    };

    // NNUEで評価
    stacks.reset();
    let raw_score = if use_qsearch && !pos.in_check() {
        // qsearch評価
        let result = qsearch_with_pv_nnue(
            &mut pos,
            stacks,
            QSEARCH_ALPHA_INIT,
            QSEARCH_BETA_INIT,
            0,
            max_ply,
        );
        result.value
    } else {
        // 静的評価
        stacks.evaluate(&pos)
    };

    // STM視点で統一（エンジン評価は常にSTM視点）
    // game_resultは元のまま使用（または手番変更時に反転）
    let new_game_result = if stm_changed {
        -psv.game_result
    } else {
        psv.game_result
    };

    // FV_SCALE補正: source_fv_scale -> target_fv_scale
    // nn.bin (FV_SCALE=24) の評価値を nnue-pytorch (FV_SCALE=16) 用に変換
    // 補正式: scaled_score = raw_score * source_fv_scale / target_fv_scale
    // 例: source=24, target=16 -> factor=1.5
    let scaled_score = raw_score * source_fv_scale / target_fv_scale;

    // スコアをクリップ
    let clipped = scaled_score.abs() > score_clip as i32;
    let new_score = scaled_score.clamp(-score_clip as i32, score_clip as i32) as i16;

    // 新しいPackedSfenValueを作成
    let new_psv = PackedSfenValue {
        sfen: final_sfen,
        score: new_score,
        move16: 0, // 無効値
        game_ply: psv.game_ply,
        game_result: new_game_result,
        padding: 0,
    };

    ProcessResult::Ok(new_psv.to_bytes(), clipped)
}

/// 深さ指定探索でファイルを処理
///
/// 探索は重いため、rayon並列処理ではなく、複数のワーカースレッドが
/// それぞれ独自のSearchインスタンスを持ってチャンク単位で処理する。
fn process_file_with_search(
    cli: &Cli,
    input_path: &PathBuf,
    output_path: &PathBuf,
    process_count: u64,
) -> Result<()> {
    let search_depth = cli.search_depth.expect("search_depth should be Some");

    // 進捗バー設定
    let progress = ProgressBar::new(process_count);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    // 入力ファイルを読み込み
    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
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
    eprintln!("Read {actual_count} records");

    // 空ファイルガード（chunks(0)でpanicを防ぐ）
    if actual_count == 0 {
        eprintln!("Warning: No records to process, creating empty output file");
        File::create(output_path)?;
        return Ok(());
    }

    // スレッド数を決定（0なら利用可能なCPU数）
    let num_threads = if cli.threads > 0 {
        cli.threads
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    };
    eprintln!("Using {num_threads} worker threads for search");

    // メモリ使用量の警告（各スレッドが独自の置換表を持つ）
    let total_hash_mb = cli.hash_mb * num_threads;
    eprintln!(
        "Total hash table size: {} MB ({} MB × {} threads)",
        total_hash_mb, cli.hash_mb, num_threads
    );
    if total_hash_mb > 4096 {
        eprintln!(
            "Warning: Large memory allocation ({} GB). Consider reducing --hash-mb or --threads.",
            total_hash_mb / 1024
        );
    }

    // レコードをチャンクに分割（chunk_sizeは最低1を保証）
    let chunk_size = records.len().div_ceil(num_threads).max(1);
    let chunks: Vec<Vec<[u8; PackedSfenValue::SIZE]>> =
        records.chunks(chunk_size).map(|chunk| chunk.to_vec()).collect();

    // 設定値をキャプチャ
    let hash_mb = cli.hash_mb;
    let max_nodes = cli.max_nodes;
    let max_time = cli.max_time;
    let score_clip = cli.score_clip;
    let skip_in_check = cli.skip_in_check;
    let source_fv_scale = cli.source_fv_scale;
    let target_fv_scale = cli.target_fv_scale;
    let verbose = cli.verbose;

    // カウンタ
    let error_count = AtomicU64::new(0);
    let clipped_count = AtomicU64::new(0);
    let skipped_count = AtomicU64::new(0);

    // 結果収集用チャネル
    let (tx, rx) = mpsc::channel::<SearchProcessResult>();

    // 進捗カウンタ（スレッド間共有）
    let progress_arc = std::sync::Arc::new(progress);

    progress_arc.set_message("Processing...");

    // ワーカースレッドを起動
    let handles: Vec<_> = chunks
        .into_iter()
        .enumerate()
        .map(|(chunk_idx, chunk)| {
            let tx = tx.clone();
            let progress = std::sync::Arc::clone(&progress_arc);

            thread::Builder::new()
                .stack_size(SEARCH_STACK_SIZE)
                .spawn(move || {
                    // 各ワーカースレッドで独自のSearchインスタンスを作成
                    // 各ワーカーは1スレッドで探索（マルチスレッド探索を無効化）
                    let mut search = Search::new(hash_mb);
                    search.set_num_threads(1);

                    for (record_idx, record) in chunk.iter().enumerate() {
                        if INTERRUPTED.load(Ordering::SeqCst) {
                            break;
                        }

                        let result = process_record_with_search(
                            record,
                            &mut search,
                            search_depth,
                            max_nodes,
                            max_time,
                            score_clip,
                            skip_in_check,
                            source_fv_scale,
                            target_fv_scale,
                        );

                        let global_idx = chunk_idx * chunk_size + record_idx;
                        let send_result = SearchProcessResult {
                            index: global_idx,
                            result,
                        };

                        if tx.send(send_result).is_err() {
                            break;
                        }

                        progress.inc(1);
                    }
                })
                .expect("Failed to spawn worker thread")
        })
        .collect();

    // 送信側をドロップ（全ワーカーが終了したらチャネルがクローズされる）
    drop(tx);

    // 結果を収集（順序を保持するためにインデックス付きで受け取る）
    let mut results_with_index: Vec<(usize, ProcessResult)> = Vec::with_capacity(actual_count);
    for search_result in rx {
        results_with_index.push((search_result.index, search_result.result));
    }

    // インデックスでソート
    results_with_index.sort_by_key(|(idx, _)| *idx);

    // 結果を処理
    let mut processed_records: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(actual_count);
    for (_, result) in results_with_index {
        match result {
            ProcessResult::Ok(record, clipped) => {
                if clipped {
                    clipped_count.fetch_add(1, Ordering::Relaxed);
                }
                processed_records.push(record);
            }
            ProcessResult::Skip => {
                skipped_count.fetch_add(1, Ordering::Relaxed);
            }
            ProcessResult::Error(e) => {
                error_count.fetch_add(1, Ordering::Relaxed);
                if verbose {
                    eprintln!("Error processing record: {e}");
                }
            }
        }
    }

    // ワーカースレッドの終了を待機
    for handle in handles {
        let _ = handle.join();
    }

    progress_arc.finish_with_message("Done");

    let final_errors = error_count.load(Ordering::SeqCst);
    let final_clipped = clipped_count.load(Ordering::SeqCst);
    let final_skipped = skipped_count.load(Ordering::SeqCst);
    if final_errors > 0 {
        eprintln!("Note: {final_errors} positions had errors");
    }
    if final_skipped > 0 {
        eprintln!(
            "Skipped (in check): {final_skipped} ({:.2}%)",
            final_skipped as f64 / actual_count as f64 * 100.0
        );
    }
    eprintln!(
        "Clipped scores: {final_clipped} ({:.2}%)",
        final_clipped as f64 / actual_count as f64 * 100.0
    );

    // 出力ファイルに書き込み
    eprintln!("Writing output...");
    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
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

/// 探索結果（インデックス付き）
struct SearchProcessResult {
    index: usize,
    result: ProcessResult,
}

/// 深さ指定探索で1レコードを処理
fn process_record_with_search(
    record: &[u8; PackedSfenValue::SIZE],
    search: &mut Search,
    depth: i32,
    max_nodes: u64,
    max_time: i64,
    score_clip: i16,
    skip_in_check: bool,
    source_fv_scale: i32,
    target_fv_scale: i32,
) -> ProcessResult {
    // PackedSfenValueを読み込み
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(p) => p,
        None => return ProcessResult::Error(anyhow::anyhow!("Failed to parse PackedSfenValue")),
    };

    // PackedSfen → SFEN → Position
    let sfen = match unpack_sfen(&psv.sfen) {
        Ok(s) => s,
        Err(e) => return ProcessResult::Error(anyhow::anyhow!("Failed to unpack SFEN: {e}")),
    };

    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return ProcessResult::Error(anyhow::anyhow!("Failed to set SFEN: {e:?}"));
    }

    // 王手局面をスキップ
    if skip_in_check && pos.in_check() {
        return ProcessResult::Skip;
    }

    // 探索を実行
    let mut limits = LimitsType::default();
    limits.depth = depth;
    if max_nodes > 0 {
        limits.nodes = max_nodes;
    }
    if max_time > 0 {
        limits.movetime = max_time;
    }
    limits.set_start_time();

    let search_result = search.go(&mut pos, limits, None::<fn(&engine_core::search::SearchInfo)>);

    // 探索結果のスコアを取得（STM視点）
    let raw_score: i32 = search_result.score.into();

    // FV_SCALE補正
    let scaled_score = raw_score * source_fv_scale / target_fv_scale;

    // スコアをクリップ
    let clipped = scaled_score.abs() > score_clip as i32;
    let new_score = scaled_score.clamp(-score_clip as i32, score_clip as i32) as i16;

    // 新しいPackedSfenValueを作成（局面は変更しない）
    let new_psv = PackedSfenValue {
        sfen: psv.sfen,
        score: new_score,
        move16: 0, // 無効値
        game_ply: psv.game_ply,
        game_result: psv.game_result,
        padding: 0,
    };

    ProcessResult::Ok(new_psv.to_bytes(), clipped)
}
