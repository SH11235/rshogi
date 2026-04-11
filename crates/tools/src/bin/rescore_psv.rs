//! rescore_psv - PSVファイルの評価値を再評価
//!
//! PackedSfenValueのスコアを別の評価関数やエンジンで再計算する。
//! NNUEモデルによる内部評価と、外部USIエンジンによる評価の両方をサポート。
//!
//! # 使用例
//!
//! ```bash
//! # NNUE静的評価で再スコア（高速）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input data.psv --output-dir rescored/ \
//!   --nnue path/to/nn.bin
//!
//! # 複数ファイルを処理（glob パターン）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input "data/*.bin" --output-dir rescored/ \
//!   --nnue path/to/nn.bin
//!
//! # qsearch評価で再スコア（より正確）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input data.psv --output-dir rescored/ \
//!   --nnue path/to/nn.bin --use-qsearch
//!
//! # qsearch leaf置換も同時に実行
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input data.psv --output-dir rescored/ \
//!   --nnue path/to/nn.bin --apply-qsearch-leaf
//!
//! # 深さ指定探索で再スコア（最も正確だが低速）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input "data/*.bin" --output-dir rescored/ \
//!   --nnue path/to/nn.bin \
//!   --search-depth 8 \
//!   --hash-mb 256 \
//!   --threads 4
//!
//! # 外部USIエンジン（DLshogi系等）で再スコア（知識蒸留用）
//! cargo run --release -p tools --bin rescore_psv -- \
//!   --input data.psv --output-dir rescored/ \
//!   --engine /path/to/dlshogi_aoba/usi/bin/usi \
//!   --engine-nodes 1 \
//!   --usi-option "DNN_Model=/path/to/model.onnx" \
//!   --usi-option "UCT_Threads=1" \
//!   --usi-option "DNN_Batch_Size=8"
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use glob::glob;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::cell::RefCell;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

use rshogi_core::nnue::init_nnue;
use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search};
use tools::packed_sfen::{PackedSfenValue, pack_position, unpack_sfen};
use tools::qsearch_pv::{NnueStacks, qsearch_with_pv_nnue};

/// 探索用スタックサイズ（64MB）
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

/// PSVファイルの評価値を再評価
#[derive(Parser)]
#[command(
    name = "rescore_psv",
    version,
    about = "PSVファイルの評価値を再評価\n\n内部NNUE評価または外部USIエンジンで局面を再評価するツール"
)]
struct Cli {
    /// 入力PSVファイル（複数指定可、globパターン対応）
    /// 例: --input file1.bin --input file2.bin
    /// 例: --input "data/*.bin"
    #[arg(short, long, required = true, num_args = 1..)]
    input: Vec<String>,

    /// 出力ディレクトリ（入力ファイル名で出力）
    #[arg(short, long)]
    output_dir: PathBuf,

    /// NNUEモデルファイル（--engine未使用時に必須）
    #[arg(long)]
    nnue: Option<PathBuf>,

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

    // --- 外部USIエンジンモード ---
    /// 外部USIエンジンのパス（DLshogi系等）
    /// 指定すると内部NNUEの代わりに外部エンジンで評価
    #[arg(long)]
    engine: Option<PathBuf>,

    /// エンジンの探索ノード数（--engine使用時、0=depth 1）
    #[arg(long, default_value_t = 1)]
    engine_nodes: u64,

    /// USIオプション（"Name=Value"形式、複数指定可）
    /// 例: --usi-option "DNN_Model=model.onnx" --usi-option "UCT_Threads=1"
    #[arg(long = "usi-option")]
    usi_options: Vec<String>,

    /// エンジン応答のタイムアウト（秒）
    /// DLエンジンの初回TensorRTビルド等に対応するため長めに設定
    #[arg(long, default_value_t = 600)]
    engine_timeout: u64,

    /// 並列エンジンプロセス数（--engine使用時、デフォルト1）
    /// DL系: 2-4程度（GPU VRAM制限）、NNUE系: CPUコア数まで
    #[arg(long, default_value_t = 1)]
    engine_threads: usize,

    // --- AobaZero ONNX 直接推論モード ---
    /// AobaZero ONNXモデルパス（USIを介さず直接GPU推論）
    /// dlshogi_aoba のカスタム特徴量フォーマット専用。
    /// 標準 dlshogi モデルには使用不可。
    #[arg(long)]
    onnx_model: Option<PathBuf>,

    // --- 標準 dlshogi ONNX 直接推論モード ---
    /// 標準dlshogi ONNXモデルパス（DL水匠等、57ch features2）
    /// AobaZero モデルには使用不可。
    #[arg(long)]
    dlshogi_onnx_model: Option<PathBuf>,

    /// ONNX推論バッチサイズ（--onnx-model/--dlshogi-onnx-model使用時）
    #[arg(long, default_value_t = 256)]
    onnx_batch_size: usize,

    /// ONNX推論の GPU ID（-1=CPU）
    #[arg(long, default_value_t = 0)]
    onnx_gpu_id: i32,

    /// 引き分け手数（--onnx-model使用時の手数特徴量調整、0=調整なし）
    #[arg(long, default_value_t = 0)]
    onnx_draw_ply: i32,

    /// 勝率→cp変換のスケール（--onnx-model/--dlshogi-onnx-model使用時、bullet-shogiの--scaleと合わせる）
    #[arg(long, default_value_t = 600.0)]
    onnx_eval_scale: f32,

    /// ORT profiling出力先ディレクトリ（指定するとsession.run()の内訳をJSONで出力）
    #[arg(long)]
    ort_profile: Option<PathBuf>,
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

    let use_engine = cli.engine.is_some();
    let use_onnx = cli.onnx_model.is_some();
    let use_dlshogi_onnx = cli.dlshogi_onnx_model.is_some();

    // 排他チェック
    if use_onnx && (use_dlshogi_onnx || use_engine || cli.use_qsearch || cli.search_depth.is_some())
    {
        anyhow::bail!(
            "--onnx-model is mutually exclusive with --dlshogi-onnx-model, --engine, --use-qsearch, --search-depth"
        );
    }
    if use_dlshogi_onnx && (use_engine || cli.use_qsearch || cli.search_depth.is_some()) {
        anyhow::bail!(
            "--dlshogi-onnx-model is mutually exclusive with --engine, --use-qsearch, --search-depth"
        );
    }
    if use_engine && (cli.use_qsearch || cli.search_depth.is_some()) {
        anyhow::bail!("--engine is mutually exclusive with --use-qsearch and --search-depth");
    }
    if cli.use_qsearch && cli.search_depth.is_some() {
        anyhow::bail!("--use-qsearch and --search-depth are mutually exclusive");
    }
    if !use_engine && !use_onnx && !use_dlshogi_onnx && cli.nnue.is_none() {
        anyhow::bail!(
            "--nnue is required when --engine/--onnx-model/--dlshogi-onnx-model is not specified"
        );
    }
    #[cfg(not(feature = "aobazero-onnx"))]
    if use_onnx {
        anyhow::bail!(
            "--onnx-model requires the 'aobazero-onnx' feature.\n\
             Rebuild with: cargo build --release -p tools --features aobazero-onnx --bin rescore_psv"
        );
    }
    #[cfg(not(feature = "dlshogi-onnx"))]
    if use_dlshogi_onnx {
        anyhow::bail!(
            "--dlshogi-onnx-model requires the 'dlshogi-onnx' feature.\n\
             Rebuild with: cargo build --release -p tools --features dlshogi-onnx --bin rescore_psv"
        );
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

    // NNUEモデルのロード（NNUE内部評価モードのみ）
    if !use_engine && !use_onnx && !use_dlshogi_onnx {
        let nnue = cli.nnue.as_ref().unwrap();
        if !nnue.exists() {
            anyhow::bail!("NNUE model file not found: {}", nnue.display());
        }
        init_nnue(nnue).context("Failed to load NNUE model")?;
        eprintln!("NNUE model loaded: {}", nnue.display());
    }

    // Ctrl-Cハンドラを設定
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted!");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // rayon スレッドプール設定（NNUE並列モードのみ）
    if !use_engine && !use_onnx && cli.search_depth.is_none() && cli.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.threads)
            .build_global()
            .unwrap_or_else(|e| {
                eprintln!("Warning: Failed to set thread count: {e}");
            });
    }

    // 外部USIエンジンの起動
    let engine_threads = if use_engine {
        cli.engine_threads.max(1)
    } else {
        0
    };
    let mut engines: Vec<UsiEngine> = Vec::new();
    if use_engine {
        let engine_path = cli.engine.as_ref().unwrap();
        let timeout = std::time::Duration::from_secs(cli.engine_timeout);
        for i in 0..engine_threads {
            eprintln!("--- Engine instance {}/{} ---", i + 1, engine_threads);
            engines.push(UsiEngine::new(engine_path, &cli.usi_options, timeout)?);
        }
    }

    // 処理設定の表示
    eprintln!(
        "Mode: {}",
        if use_onnx {
            format!(
                "AobaZero ONNX direct inference (batch={}, gpu={})",
                cli.onnx_batch_size, cli.onnx_gpu_id
            )
        } else if use_dlshogi_onnx {
            format!(
                "dlshogi ONNX direct inference (batch={}, gpu={})",
                cli.onnx_batch_size, cli.onnx_gpu_id
            )
        } else if use_engine {
            format!("external USI engine (nodes={}, threads={})", cli.engine_nodes, engine_threads)
        } else if let Some(depth) = cli.search_depth {
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
    if !use_engine {
        eprintln!(
            "qsearch leaf replacement: {}",
            if cli.apply_qsearch_leaf && cli.search_depth.is_none() {
                "enabled"
            } else {
                "disabled"
            }
        );
    }
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

        // 出力ファイルが既に存在し、入力と同じレコード数ならスキップ（完了済み）
        let input_file_size = fs::metadata(input_path)?.len();
        let input_record_count = input_file_size / PackedSfenValue::SIZE as u64;
        if output_path.exists() {
            let out_size = fs::metadata(&output_path)?.len();
            let out_records = out_size / PackedSfenValue::SIZE as u64;
            if out_records >= input_record_count && out_size % PackedSfenValue::SIZE as u64 == 0 {
                eprintln!(
                    "=== [{}/{}] Skipping (complete: {} records): {} ===",
                    file_idx + 1,
                    total_files,
                    out_records,
                    output_path.display()
                );
                continue;
            }
            if out_records > 0 {
                eprintln!(
                    "=== [{}/{}] Resuming ({}/{} records): {} ===",
                    file_idx + 1,
                    total_files,
                    out_records,
                    input_record_count,
                    input_path.display()
                );
            }
        }

        eprintln!(
            "=== [{}/{}] Processing: {} ===",
            file_idx + 1,
            total_files,
            input_path.display()
        );

        let file_size = input_file_size;
        let record_count = input_record_count;

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
        #[cfg(feature = "aobazero-onnx")]
        if use_onnx {
            process_file_with_onnx(&cli, input_path, &output_path, process_count)?;
        }
        #[cfg(feature = "dlshogi-onnx")]
        if use_dlshogi_onnx {
            process_file_with_dlshogi_onnx(&cli, input_path, &output_path, process_count)?;
        }
        if !use_onnx && !use_dlshogi_onnx {
            if !engines.is_empty() {
                process_file_with_engine(
                    &cli,
                    &mut engines,
                    input_path,
                    &output_path,
                    process_count,
                )?;
            } else if cli.search_depth.is_some() {
                process_file_with_search(&cli, input_path, &output_path, process_count)?;
            } else {
                process_file(&cli, input_path, &output_path, process_count)?;
            }
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

    // エンジン終了
    for mut eng in engines {
        let _ = eng.quit();
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

    // チャンクストリーミング: 読み込み → rayon 並列処理 → 書き出しをチャンク単位で繰り返す
    // 全レコードをメモリに溜めず、ピークメモリ = チャンクサイズ分のみ
    const CHUNK_SIZE: usize = 1_000_000;

    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, in_file);

    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);

    let error_count = AtomicU64::new(0);
    let processed_count = AtomicU64::new(0);
    let clipped_count = AtomicU64::new(0);
    let skipped_count = AtomicU64::new(0);

    let max_ply = cli.max_ply;
    let use_qsearch = cli.use_qsearch;
    let apply_leaf = cli.apply_qsearch_leaf;
    let score_clip = cli.score_clip;
    let skip_in_check = cli.skip_in_check;
    let source_fv_scale = cli.source_fv_scale;
    let target_fv_scale = cli.target_fv_scale;
    let verbose = cli.verbose;

    progress.set_message("Processing...");
    let mut total_read = 0u64;
    let mut total_written = 0u64;

    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            break;
        }

        // チャンク読み込み
        let want = (CHUNK_SIZE as u64).min(process_count.saturating_sub(total_read)) as usize;
        let mut chunk: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(want);
        let mut buffer = [0u8; PackedSfenValue::SIZE];

        for _ in 0..want {
            match reader.read_exact(&mut buffer) {
                Ok(()) => chunk.push(buffer),
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }

        if chunk.is_empty() {
            break;
        }
        total_read += chunk.len() as u64;

        // rayon 並列処理 → 即座に書き出し
        let results: Vec<Option<[u8; PackedSfenValue::SIZE]>> = chunk
            .par_iter()
            .map(|record| {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    return None;
                }

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
                        Some(new_record)
                    }
                    ProcessResult::Skip => {
                        skipped_count.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    ProcessResult::Error(e) => {
                        error_count.fetch_add(1, Ordering::Relaxed);
                        if verbose {
                            eprintln!("Error processing record: {e}");
                        }
                        None
                    }
                }
            })
            .collect();

        // チャンク結果を即座に書き出し
        for record in results.iter().flatten() {
            writer.write_all(record)?;
            total_written += 1;
        }
        progress.inc(results.len() as u64);
    }

    writer.flush()?;
    progress.finish_with_message("Done");

    let final_errors = error_count.load(Ordering::SeqCst);
    let final_clipped = clipped_count.load(Ordering::SeqCst);
    let final_skipped = skipped_count.load(Ordering::SeqCst);
    if final_errors > 0 {
        eprintln!("Note: {final_errors} positions had errors");
    }
    if final_skipped > 0 && total_read > 0 {
        eprintln!(
            "Skipped (in check): {} ({:.2}%)",
            final_skipped,
            final_skipped as f64 / total_read as f64 * 100.0
        );
    }
    if total_read > 0 {
        eprintln!(
            "Clipped scores: {} ({:.2}%)",
            final_clipped,
            final_clipped as f64 / total_read as f64 * 100.0
        );
    }

    eprintln!("Wrote {total_written} records");

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

    // rx のドレインが完了した時点でワーカーは既に終了しているため、join は即座に返る
    for handle in handles {
        let _ = handle.join();
    }

    progress_arc.finish_with_message("Done");

    // ソート済み結果から直接書き出し（中間 Vec を排除）
    eprintln!("Writing output...");
    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);

    let mut written = 0u64;
    for (_, result) in results_with_index {
        match result {
            ProcessResult::Ok(record, clipped) => {
                if clipped {
                    clipped_count.fetch_add(1, Ordering::Relaxed);
                }
                writer.write_all(&record)?;
                written += 1;
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

    writer.flush()?;

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
    eprintln!("Wrote {written} records");

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

    let search_result = search.go(&mut pos, limits, None::<fn(&rshogi_core::search::SearchInfo)>);

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

// ============================================================
// 外部USIエンジンによるリスコア
// ============================================================

/// 外部USIエンジンの管理構造体
struct UsiEngine {
    child: Child,
    stdin: BufWriter<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl UsiEngine {
    /// USIエンジンを起動し、初期化する
    fn new(
        engine_path: &std::path::Path,
        usi_options: &[String],
        _timeout: std::time::Duration,
    ) -> Result<Self> {
        eprintln!("Starting USI engine: {}", engine_path.display());

        let mut child = Command::new(engine_path)
            .current_dir(engine_path.parent().unwrap_or(std::path::Path::new(".")))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("Failed to start engine: {}", engine_path.display()))?;

        let stdin = BufWriter::new(child.stdin.take().expect("stdin"));
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));

        let mut engine = Self {
            child,
            stdin,
            stdout,
        };

        // USIハンドシェイク
        engine.send_command("usi")?;
        engine.wait_for("usiok")?;

        // USIオプション設定
        for opt in usi_options {
            if let Some((name, value)) = opt.split_once('=') {
                engine.send_command(&format!("setoption name {name} value {value}"))?;
            } else {
                eprintln!("Warning: invalid USI option format (expected Name=Value): {opt}");
            }
        }

        // isready/readyok（TensorRTビルド等で長時間かかる場合あり）
        eprintln!("Waiting for engine ready (TensorRT build may take a while)...");
        engine.send_command("isready")?;
        engine.wait_for("readyok")?;
        eprintln!("Engine ready.");

        // ウォームアップ: 初期局面で評価して GPU/TRT ランタイムを安定させる
        eprintln!("Warming up engine...");
        engine.send_command("usinewgame")?;
        engine.send_command(
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        )?;
        engine.send_command("go nodes 1")?;
        engine.wait_for_bestmove()?;
        // 2回目: DLshogi系は初回goがスレッドプール初期化を含む場合がある
        engine.send_command(
            "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
        )?;
        engine.send_command("go nodes 1")?;
        engine.wait_for_bestmove()?;
        eprintln!("Warmup complete.");

        Ok(engine)
    }

    fn send_command(&mut self, cmd: &str) -> Result<()> {
        writeln!(self.stdin, "{cmd}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn wait_for(&mut self, expected: &str) -> Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                anyhow::bail!("Engine process closed stdout while waiting for '{expected}'");
            }
            if line.trim() == expected {
                break;
            }
        }
        Ok(())
    }

    /// bestmove行まで読み飛ばす
    fn wait_for_bestmove(&mut self) -> Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                anyhow::bail!("Engine process closed stdout while waiting for bestmove");
            }
            if line.trim().starts_with("bestmove") {
                break;
            }
        }
        Ok(())
    }

    /// 局面を評価し、score cp 値を返す
    fn evaluate_position(&mut self, sfen: &str, nodes: u64) -> Result<Option<i32>> {
        self.send_command(&format!("position sfen {sfen}"))?;
        if nodes > 0 {
            self.send_command(&format!("go nodes {nodes}"))?;
        } else {
            self.send_command("go depth 1")?;
        }

        let mut score: Option<i32> = None;
        let mut line = String::new();

        loop {
            line.clear();
            let n = self.stdout.read_line(&mut line)?;
            if n == 0 {
                anyhow::bail!("Engine process closed stdout during evaluation");
            }
            let trimmed = line.trim();

            // score cp / score mate を抽出（最後のinfo行のものを採用）
            if trimmed.starts_with("info") {
                if let Some(cp_idx) = trimmed.find("score cp") {
                    let rest = &trimmed[cp_idx + 9..];
                    let end_idx = rest.find(' ').unwrap_or(rest.len());
                    if let Ok(cp) = rest[..end_idx].parse::<i32>() {
                        score = Some(cp);
                    }
                } else if let Some(mate_idx) = trimmed.find("score mate") {
                    let rest = &trimmed[mate_idx + 11..];
                    let end_idx = rest.find(' ').unwrap_or(rest.len());
                    if let Ok(mate_in) = rest[..end_idx].parse::<i32>() {
                        score = Some(if mate_in > 0 { 30000 } else { -30000 });
                    }
                }
            }

            if trimmed.starts_with("bestmove") {
                if trimmed.contains("resign") && score.is_none() {
                    score = Some(-30000);
                }
                break;
            }
        }

        Ok(score)
    }

    fn quit(&mut self) -> Result<()> {
        let _ = self.send_command("quit");
        let _ = self.child.wait();
        Ok(())
    }
}

/// エンジン処理結果（インデックス付き）
struct EngineProcessResult {
    index: usize,
    score: Option<i32>,
    psv: PackedSfenValue,
}

/// 外部USIエンジンでファイルを処理（複数エンジン並列対応）
fn process_file_with_engine(
    cli: &Cli,
    engines: &mut [UsiEngine],
    input_path: &PathBuf,
    output_path: &PathBuf,
    process_count: u64,
) -> Result<()> {
    let num_engines = engines.len();

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

    // 全レコードを読み込み（SFEN展開・フィルタリング含む）
    progress.set_message("Reading...");
    let mut records: Vec<(usize, PackedSfenValue, String)> = Vec::new(); // (global_index, psv, sfen)
    let mut buffer = [0u8; PackedSfenValue::SIZE];
    let mut skipped_count: u64 = 0;
    let mut read_errors: u64 = 0;

    for global_idx in 0..process_count as usize {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }
        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let psv = match PackedSfenValue::from_bytes(&buffer) {
            Some(p) => p,
            None => {
                read_errors += 1;
                progress.inc(1);
                continue;
            }
        };
        let sfen = match unpack_sfen(&psv.sfen) {
            Ok(s) => s,
            Err(_) => {
                read_errors += 1;
                progress.inc(1);
                continue;
            }
        };
        if cli.skip_in_check {
            let mut pos = Position::new();
            if pos.set_sfen(&sfen).is_ok() && pos.in_check() {
                skipped_count += 1;
                progress.inc(1);
                continue;
            }
        }
        records.push((global_idx, psv, sfen));
    }

    let actual_count = records.len();
    eprintln!(
        "Read {} records ({} skipped, {} errors)",
        actual_count, skipped_count, read_errors
    );

    if actual_count == 0 {
        progress.finish_with_message("Done (empty)");
        File::create(output_path)?;
        return Ok(());
    }

    // レコードをチャンクに分割
    let chunk_size = actual_count.div_ceil(num_engines).max(1);
    let chunks: Vec<Vec<(usize, PackedSfenValue, String)>> =
        records.chunks(chunk_size).map(|c| c.to_vec()).collect();

    eprintln!("Using {} engine process(es), chunk_size={}", chunks.len(), chunk_size);

    // 各チャンクをワーカースレッドで処理
    let score_clip = cli.score_clip;
    let engine_nodes = cli.engine_nodes;
    let verbose = cli.verbose;

    let error_count = AtomicU64::new(read_errors);
    let clipped_count = AtomicU64::new(0);

    let (tx, rx) = mpsc::channel::<EngineProcessResult>();
    let progress_arc = std::sync::Arc::new(progress);

    progress_arc.set_message("Processing...");

    std::thread::scope(|s| {
        let mut handles = Vec::new();

        for (engine, chunk) in engines.iter_mut().zip(chunks.into_iter()) {
            let tx = tx.clone();
            let progress = std::sync::Arc::clone(&progress_arc);
            let error_count = &error_count;
            let _clipped_count = &clipped_count;

            handles.push(s.spawn(move || {
                // usinewgame でリセット
                let _ = engine.send_command("usinewgame");

                for (global_idx, psv, sfen) in &chunk {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        break;
                    }

                    match engine.evaluate_position(sfen, engine_nodes) {
                        Ok(score) => {
                            if score.is_none() {
                                error_count.fetch_add(1, Ordering::Relaxed);
                                if verbose {
                                    eprintln!("No score returned for: {sfen}");
                                }
                            }
                            let _ = tx.send(EngineProcessResult {
                                index: *global_idx,
                                score,
                                psv: *psv,
                            });
                        }
                        Err(e) => {
                            error_count.fetch_add(1, Ordering::Relaxed);
                            if verbose {
                                eprintln!("Engine error for {sfen}: {e}");
                            }
                            // スコアなしで送信（エンジン死亡時はループを抜ける）
                            let _ = tx.send(EngineProcessResult {
                                index: *global_idx,
                                score: None,
                                psv: *psv,
                            });
                            break;
                        }
                    }

                    progress.inc(1);
                }
            }));
        }

        drop(tx); // 全ワーカーの送信側をドロップ

        // 結果を収集
        let mut results: Vec<EngineProcessResult> = rx.into_iter().collect();
        results.sort_by_key(|r| r.index);

        // 出力レコードを構築
        let mut processed_records: Vec<[u8; PackedSfenValue::SIZE]> =
            Vec::with_capacity(results.len());

        for r in &results {
            if let Some(raw_score) = r.score {
                let clipped = raw_score.abs() > score_clip as i32;
                let new_score = raw_score.clamp(-score_clip as i32, score_clip as i32) as i16;
                if clipped {
                    clipped_count.fetch_add(1, Ordering::Relaxed);
                }
                let new_psv = PackedSfenValue {
                    sfen: r.psv.sfen,
                    score: new_score,
                    move16: 0,
                    game_ply: r.psv.game_ply,
                    game_result: r.psv.game_result,
                    padding: 0,
                };
                processed_records.push(new_psv.to_bytes());
            }
        }

        // ワーカースレッド完了待ち
        for h in handles {
            let _ = h.join();
        }

        progress_arc.finish_with_message("Done");

        let final_errors = error_count.load(Ordering::SeqCst);
        let final_clipped = clipped_count.load(Ordering::SeqCst);
        let total = actual_count as u64 + skipped_count + read_errors;
        if final_errors > 0 {
            eprintln!("Note: {final_errors} positions had errors");
        }
        if skipped_count > 0 {
            eprintln!(
                "Skipped (in check): {skipped_count} ({:.2}%)",
                skipped_count as f64 / total as f64 * 100.0
            );
        }
        if total > 0 {
            eprintln!(
                "Clipped scores: {final_clipped} ({:.2}%)",
                final_clipped as f64 / total as f64 * 100.0
            );
        }

        // 出力ファイルに書き込み
        eprintln!("Writing output...");
        let out_file = File::create(output_path)
            .with_context(|| format!("Failed to create {}", output_path.display()))
            .unwrap();
        let mut writer = BufWriter::new(out_file);

        for record in &processed_records {
            writer.write_all(record).unwrap();
        }

        writer.flush().unwrap();
        eprintln!("Wrote {} records", processed_records.len());
    });

    Ok(())
}

// ============================================================
// ONNX 直接推論モード (共通パイプライン)
// ============================================================

/// ort のエラーを anyhow に変換 (ort::Error は Send+Sync を満たさないため)
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
fn onnx_ort_err(e: ort::Error) -> anyhow::Error {
    anyhow::anyhow!("ONNX Runtime error: {e}")
}

/// ストリーミング読み込み + rayon 並列特徴量構築 + ゼロコピー GPU 推論
///
/// AobaZero / 標準 dlshogi の両方で共通のパイプライン処理。
/// `build_features` クロージャで特徴量構築の差異を吸収する。
#[cfg(any(feature = "aobazero-onnx", feature = "dlshogi-onnx"))]
#[allow(clippy::too_many_arguments)]
fn process_file_with_onnx_pipeline<F>(
    model_name: &str,
    onnx_path: &std::path::Path,
    input_path: &std::path::Path,
    output_path: &std::path::Path,
    process_count: u64,
    batch_size: usize,
    gpu_id: i32,
    score_clip: i16,
    eval_scale: f32,
    skip_in_check: bool,
    f1_size: usize,
    f2_size: usize,
    input1_channels: usize,
    input2_channels: usize,
    profile_path: Option<&std::path::Path>,
    build_features: F,
) -> Result<()>
where
    F: Fn(&Position, &mut [f32], &mut [f32], &PackedSfenValue) + Send + Sync,
{
    use ort::ep::ExecutionProvider;
    use ort::session::Session;
    use ort::value::TensorRef;

    // ORT_DYLIB_PATH 事前検証（load-dynamic feature）
    //
    // ort クレートは load-dynamic feature 有効時、ORT_DYLIB_PATH 環境変数で指定された
    // ライブラリを dlopen する。未設定時はシステムパスから "libonnxruntime.so" を探すが、
    // 見つからない場合にハングする（エラーではなくブロックする）。
    //
    // ort の default features には download-binaries（ビルド時に CPU 版ランタイムを
    // 自動ダウンロード）が含まれるが、load-dynamic とは #[cfg] で排他のため共存不可。
    // また download-binaries で取得されるのは CPU 版のみで GPU 版は提供されない。
    //
    // したがって load-dynamic を使い、ランタイムはユーザーが自前で用意する設計とする。
    // GPU 推論がデフォルト。暗黙の CPU フォールバックは CUDA EP チェックで防止する。
    //
    // ORT_DYLIB_PATH は GPU/CPU どちらのモードでも必須。
    // 未設定時に ort がシステムパスから探すとハングするため。
    match std::env::var("ORT_DYLIB_PATH") {
        Ok(path) if !path.is_empty() => {
            if !std::path::Path::new(&path).is_file() {
                anyhow::bail!(
                    "ORT_DYLIB_PATH is set to '{path}' but the file does not exist \
                     (or is not a regular file).\n\
                     Download ONNX Runtime from:\n  \
                     https://github.com/microsoft/onnxruntime/releases"
                );
            }
            eprintln!("ORT_DYLIB_PATH: {path}");
        }
        _ => {
            let mode_hint = if gpu_id >= 0 {
                "GPU inference requires a GPU-enabled ONNX Runtime (onnxruntime-linux-x64-gpu-*)."
            } else {
                "CPU inference requires an ONNX Runtime library (GPU or CPU build)."
            };
            anyhow::bail!(
                "ORT_DYLIB_PATH environment variable is not set.\n\
                 {mode_hint}\n\
                 Download from: https://github.com/microsoft/onnxruntime/releases\n\
                 Example:\n  \
                 ORT_DYLIB_PATH=/path/to/libonnxruntime.so cargo run ..."
            );
        }
    }

    // ONNX Runtime セッション初期化
    eprintln!("Loading {model_name} ONNX model: {}", onnx_path.display());

    let builder = Session::builder()
        .map_err(onnx_ort_err)?
        .with_optimization_level(ort::session::builder::GraphOptimizationLevel::All)
        .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?
        .with_intra_threads(1)
        .map_err(|e| anyhow::anyhow!("ORT builder error: {e}"))?;

    let mut builder = if let Some(path) = profile_path {
        eprintln!("ORT profiling enabled: {}", path.display());
        builder
            .with_profiling(path)
            .map_err(|e| anyhow::anyhow!("ORT profiling error: {e}"))?
    } else {
        builder
    };

    let mut session = if gpu_id >= 0 {
        eprintln!("Using CUDA GPU {gpu_id}");

        // CUDA EP が ORT ライブラリに含まれているか事前チェック（サイレント CPU フォールバック防止）
        let cuda_ep =
            ort::execution_providers::CUDAExecutionProvider::default().with_device_id(gpu_id);
        match cuda_ep.is_available() {
            Ok(true) => eprintln!("CUDA execution provider: available"),
            Ok(false) => {
                anyhow::bail!(
                    "CUDAExecutionProvider is NOT available in the loaded ONNX Runtime library.\n\
                     The library may be a CPU-only build.\n\
                     Check ORT_DYLIB_PATH points to a GPU-enabled onnxruntime.\n\
                     To use CPU inference instead, omit --onnx-gpu-id."
                );
            }
            Err(e) => {
                eprintln!("WARNING: Failed to check CUDA EP availability: {e}");
            }
        }

        let ep = cuda_ep.build().error_on_failure();
        builder
            .with_execution_providers([ep])
            .map_err(|e| {
                anyhow::anyhow!("CUDA EP registration failed (is onnxruntime-gpu installed?): {e}")
            })?
            .commit_from_file(onnx_path)
            .map_err(onnx_ort_err)?
    } else {
        eprintln!("Using CPU");
        builder.commit_from_file(onnx_path).map_err(onnx_ort_err)?
    };

    eprintln!("{model_name} ONNX model loaded. Batch size: {batch_size}");

    // 進捗バー
    let progress = ProgressBar::new(process_count);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );
    progress.set_message("Processing...");

    // ストリーミング読み込み + バッチ処理
    let in_file = File::open(input_path)
        .with_context(|| format!("Failed to open {}", input_path.display()))?;
    let mut reader = BufReader::new(in_file);

    // レジューム対応: 出力ファイルに既存レコードがあればスキップして追記
    let resume_count = if output_path.exists() {
        let out_size = fs::metadata(output_path)?.len();
        let records = out_size / PackedSfenValue::SIZE as u64;
        // 不完全レコードがあれば切り捨て
        let clean_size = records * PackedSfenValue::SIZE as u64;
        if out_size != clean_size {
            let f = File::options().write(true).open(output_path)?;
            f.set_len(clean_size)?;
        }
        records
    } else {
        0
    };

    let out_file = File::options()
        .create(true)
        .append(true)
        .open(output_path)
        .with_context(|| format!("Failed to open {}", output_path.display()))?;
    let mut writer = BufWriter::new(out_file);

    // 既存レコード分の入力をスキップ
    let mut remaining = process_count;
    if resume_count > 0 {
        let skip = resume_count.min(remaining);
        let mut skip_buf = [0u8; PackedSfenValue::SIZE];
        for _ in 0..skip {
            if reader.read_exact(&mut skip_buf).is_err() {
                break;
            }
            remaining -= 1;
        }
        eprintln!("Resuming: skipped {skip} already-processed records");
    }

    let mut f1_buf = vec![0.0f32; batch_size * f1_size];
    let mut f2_buf = vec![0.0f32; batch_size * f2_size];
    let mut buffer = [0u8; PackedSfenValue::SIZE];
    let mut skipped_count: u64 = 0;
    let mut error_count: u64 = 0;
    let mut clipped_count: u64 = 0;
    let mut total_processed: u64 = 0;

    loop {
        // バッチ分のレコードをストリーム読み込み
        let mut batch_records: Vec<(PackedSfenValue, String)> = Vec::with_capacity(batch_size);
        while batch_records.len() < batch_size && remaining > 0 {
            if INTERRUPTED.load(Ordering::SeqCst) {
                remaining = 0;
                break;
            }
            remaining -= 1;
            match reader.read_exact(&mut buffer) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    remaining = 0;
                    break;
                }
                Err(e) => return Err(e.into()),
            }
            let psv = match PackedSfenValue::from_bytes(&buffer) {
                Some(p) => p,
                None => {
                    error_count += 1;
                    continue;
                }
            };
            let sfen = match unpack_sfen(&psv.sfen) {
                Ok(s) => s,
                Err(_) => {
                    error_count += 1;
                    continue;
                }
            };
            if skip_in_check {
                let mut pos = Position::new();
                if pos.set_sfen(&sfen).is_ok() && pos.in_check() {
                    skipped_count += 1;
                    progress.inc(1);
                    continue;
                }
            }
            batch_records.push((psv, sfen));
        }

        let actual_batch = batch_records.len();
        if actual_batch == 0 {
            break;
        }

        // rayon 並列で特徴量構築
        f1_buf[..actual_batch * f1_size].fill(0.0);
        f2_buf[..actual_batch * f2_size].fill(0.0);

        let batch_errors = AtomicU64::new(0);
        let f1_slices: Vec<&mut [f32]> =
            f1_buf[..actual_batch * f1_size].chunks_mut(f1_size).collect();
        let f2_slices: Vec<&mut [f32]> =
            f2_buf[..actual_batch * f2_size].chunks_mut(f2_size).collect();

        f1_slices.into_par_iter().zip(f2_slices).zip(batch_records.par_iter()).for_each(
            |((f1, f2), (psv, sfen))| {
                let mut pos = Position::new();
                if pos.set_sfen(sfen).is_err() {
                    batch_errors.fetch_add(1, Ordering::Relaxed);
                    return;
                }
                build_features(&pos, f1, f2, psv);
            },
        );

        error_count += batch_errors.load(Ordering::Relaxed);

        // ゼロコピーでテンソル参照を作成し推論（CPU→ORT のメモリコピーを排除）
        let shape1: [usize; 4] = [actual_batch, input1_channels, 9, 9];
        let input1 = TensorRef::<f32>::from_array_view((shape1, &f1_buf[..actual_batch * f1_size]))
            .map_err(onnx_ort_err)?;

        let shape2: [usize; 4] = [actual_batch, input2_channels, 9, 9];
        let input2 = TensorRef::<f32>::from_array_view((shape2, &f2_buf[..actual_batch * f2_size]))
            .map_err(onnx_ort_err)?;

        let outputs = session
            .run(ort::inputs![
                "input1" => input1,
                "input2" => input2,
            ])
            .map_err(onnx_ort_err)?;

        let (_, values) =
            outputs["output_value"].try_extract_tensor::<f32>().map_err(onnx_ort_err)?;

        // 結果を書き出し（テンソルから直接読み取り、to_vec() コピーを排除）
        for (i, (psv, _)) in batch_records.iter().enumerate() {
            let winrate = values[i];
            let clamped = winrate.clamp(0.001, 0.999);
            let logit = (clamped / (1.0 - clamped)).ln();
            let raw_score = (logit * eval_scale) as i32;
            let clipped = raw_score.abs() > score_clip as i32;
            let new_score = raw_score.clamp(-(score_clip as i32), score_clip as i32) as i16;
            if clipped {
                clipped_count += 1;
            }
            let new_psv = PackedSfenValue {
                sfen: psv.sfen,
                score: new_score,
                move16: 0,
                game_ply: psv.game_ply,
                game_result: psv.game_result,
                padding: 0,
            };
            writer.write_all(&new_psv.to_bytes())?;
        }
        total_processed += actual_batch as u64;
        progress.inc(actual_batch as u64);
    }

    if profile_path.is_some() {
        match session.end_profiling() {
            Ok(path) => eprintln!("ORT profile saved: {path}"),
            Err(e) => eprintln!("ORT profile error: {e}"),
        }
    }

    writer.flush()?;
    progress.finish_with_message("Done");

    // 統計情報
    let total = total_processed + skipped_count + error_count;
    if error_count > 0 {
        eprintln!("Note: {error_count} positions had errors");
    }
    if skipped_count > 0 && total > 0 {
        eprintln!(
            "Skipped (in check): {skipped_count} ({:.2}%)",
            skipped_count as f64 / total as f64 * 100.0
        );
    }
    if total > 0 {
        eprintln!(
            "Clipped scores: {clipped_count} ({:.2}%)",
            clipped_count as f64 / total as f64 * 100.0
        );
    }
    eprintln!("Wrote {total_processed} records");

    Ok(())
}

// ============================================================
// AobaZero ONNX 直接推論モード
// ============================================================

#[cfg(feature = "aobazero-onnx")]
fn process_file_with_onnx(
    cli: &Cli,
    input_path: &std::path::Path,
    output_path: &std::path::Path,
    process_count: u64,
) -> Result<()> {
    use tools::aobazero_features::{
        FEATURES1_SIZE, FEATURES2_SIZE, INPUT1_CHANNELS, INPUT2_CHANNELS, make_input_features,
    };

    let draw_ply = cli.onnx_draw_ply;
    process_file_with_onnx_pipeline(
        "AobaZero",
        cli.onnx_model.as_ref().unwrap(),
        input_path,
        output_path,
        process_count,
        cli.onnx_batch_size,
        cli.onnx_gpu_id,
        cli.score_clip,
        cli.onnx_eval_scale,
        cli.skip_in_check,
        FEATURES1_SIZE,
        FEATURES2_SIZE,
        INPUT1_CHANNELS,
        INPUT2_CHANNELS,
        cli.ort_profile.as_deref(),
        move |pos, f1, f2, psv| {
            make_input_features(pos, f1, f2, psv.game_ply as i32, draw_ply);
        },
    )
}

// ============================================================
// 標準 dlshogi ONNX 直接推論モード
// ============================================================

#[cfg(feature = "dlshogi-onnx")]
fn process_file_with_dlshogi_onnx(
    cli: &Cli,
    input_path: &std::path::Path,
    output_path: &std::path::Path,
    process_count: u64,
) -> Result<()> {
    use tools::dlshogi_features::{
        FEATURES1_SIZE, FEATURES2_SIZE, INPUT1_CHANNELS, INPUT2_CHANNELS, make_input_features,
    };

    process_file_with_onnx_pipeline(
        "dlshogi",
        cli.dlshogi_onnx_model.as_ref().unwrap(),
        input_path,
        output_path,
        process_count,
        cli.onnx_batch_size,
        cli.onnx_gpu_id,
        cli.score_clip,
        cli.onnx_eval_scale,
        cli.skip_in_check,
        FEATURES1_SIZE,
        FEATURES2_SIZE,
        INPUT1_CHANNELS,
        INPUT2_CHANNELS,
        cli.ort_profile.as_deref(),
        |pos, f1, f2, _psv| {
            make_input_features(pos, f1, f2);
        },
    )
}
