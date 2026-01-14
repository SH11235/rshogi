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

use std::cell::RefCell;

use engine_core::nnue::init_nnue;
use engine_core::position::Position;
use tools::packed_sfen::{pack_position, unpack_sfen, PackedSfenValue};
use tools::qsearch_pv::{
    qsearch_with_pv, qsearch_with_pv_nnue, MaterialEvaluator, NnueStacks, QsearchResult,
};

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

    /// qsearchの最大深さ（ノード制限と併用で爆発防止）
    #[arg(long, default_value_t = 16)]
    max_ply: i32,

    /// 並列処理スレッド数（0=自動）
    #[arg(short, long, default_value_t = 1)]
    threads: usize,

    /// NNUEモデルファイル（省略時はMaterial評価、--rescoreには必須）
    #[arg(long)]
    nnue: Option<PathBuf>,

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

/// 処理結果
enum ProcessResult {
    /// 正常に処理完了
    Ok([u8; PackedSfenValue::SIZE]),
    /// スキップ（王手局面など）
    Skip,
    /// エラー
    Error(anyhow::Error),
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

    // NNUEモデルのロード
    let use_nnue = if let Some(ref nnue_path) = cli.nnue {
        if !nnue_path.exists() {
            anyhow::bail!("NNUE model file not found: {}", nnue_path.display());
        }
        init_nnue(nnue_path).context("Failed to load NNUE model")?;
        eprintln!("NNUE model loaded: {}", nnue_path.display());
        true
    } else {
        false
    };

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
    let fix_stm_sign = !cli.no_fix_stm_sign;
    eprintln!("STM sign fix: {}", if fix_stm_sign { "enabled" } else { "disabled" });
    eprintln!("Rescore with NNUE: {}", if cli.rescore { "yes" } else { "no" });
    eprintln!("Skip in-check positions: {}", if cli.skip_in_check { "yes" } else { "no" });
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

    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("Note: Processing was interrupted, output may be incomplete");
    } else {
        eprintln!("Output: {}", cli.output.display());
    }

    Ok(())
}

/// ファイルを処理
fn process_file(cli: &Cli, process_count: u64, use_nnue: bool, opts: ProcessOptions) -> Result<()> {
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
    eprintln!("Read {actual_count} records");

    // カウンタ
    let error_count = AtomicU64::new(0);
    let processed_count = AtomicU64::new(0);
    let skipped_count = AtomicU64::new(0);

    // qsearch leaf置換を並列で適用
    progress.set_message("Processing...");
    let verbose = cli.verbose;

    // 処理関数を選択
    let processed_records: Vec<[u8; PackedSfenValue::SIZE]> = if use_nnue {
        eprintln!("Using NNUE evaluation (with incremental updates)");
        records
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
                    process_record_nnue(record, &mut stacks, opts)
                });

                match result {
                    ProcessResult::Ok(new_record) => {
                        processed_count.fetch_add(1, Ordering::Relaxed);
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
            .collect()
    } else {
        eprintln!("Using Material evaluation");
        let evaluator = MaterialEvaluator;
        records
            .par_iter()
            .filter_map(|record| {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    return Some(*record);
                }

                let result = process_record_material(record, &evaluator, opts);

                match result {
                    ProcessResult::Ok(new_record) => {
                        processed_count.fetch_add(1, Ordering::Relaxed);
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
            .collect()
    };

    progress.finish_with_message("Done");

    let final_errors = error_count.load(Ordering::SeqCst);
    let final_skipped = skipped_count.load(Ordering::SeqCst);
    if final_errors > 0 {
        eprintln!("Note: {final_errors} positions had errors");
    }
    if final_skipped > 0 {
        eprintln!(
            "Skipped: {} ({:.2}%)",
            final_skipped,
            final_skipped as f64 / actual_count as f64 * 100.0
        );
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
    original_stm: engine_core::types::Color,
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
