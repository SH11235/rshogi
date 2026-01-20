//! filter_teacher_data - 教師データのフィルタリングツール
//!
//! PackedSfenValue形式（40バイト/レコード）の教師データをフィルタリングする。
//! 王手除外、極端なスコアの除外、スコアクリップなどの前処理を行う。
//!
//! # 使用例
//!
//! ```bash
//! # 王手局面を除外
//! cargo run -p tools --release --bin filter_teacher_data -- \
//!   --input shuffled.bin --output filtered.bin --filter-in-check
//!
//! # 複合フィルタ（王手除外 + 極端値除外 + 手数下限）
//! cargo run -p tools --release --bin filter_teacher_data -- \
//!   --input shuffled.bin --output filtered.bin \
//!   --filter-in-check --score-abs-max 12000 --ply-min 16
//!
//! # スコアクリップのみ（除外フィルタなし）
//! cargo run -p tools --release --bin filter_teacher_data -- \
//!   --input shuffled.bin --output clipped.bin --score-clip 6000
//!
//! # 統計のみ出力（全件統計、高速シーケンシャル）
//! cargo run -p tools --release --bin filter_teacher_data -- \
//!   --input shuffled.bin --stats-only --target-scale 508
//!
//! # ヒストグラム付き統計（P50/P90/P99が必要な場合）
//! cargo run -p tools --release --bin filter_teacher_data -- \
//!   --input shuffled.bin --stats-only --histogram
//!
//! # フィルタ適用後の統計（出力なし）
//! # Note: フィルタありの場合は並列処理になり、全件統計より遅くなります
//! cargo run -p tools --release --bin filter_teacher_data -- \
//!   --input shuffled.bin --stats-only --filter-in-check --target-scale 508
//! ```

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde::Serialize;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use engine_core::position::Position;
use tools::packed_sfen::{unpack_sfen, PackedSfenValue};

/// 教師データのフィルタリングツール
#[derive(Parser)]
#[command(
    name = "filter_teacher_data",
    version,
    about = "教師データのフィルタリングと前処理\n\n王手除外、スコアフィルタ、クリップなどを適用"
)]
struct Cli {
    /// 入力packファイル
    #[arg(short, long)]
    input: PathBuf,

    /// 出力packファイル（--stats-only時は省略可）
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// 王手局面を除外
    #[arg(long)]
    filter_in_check: bool,

    /// 絶対値がこの値を超えるスコアの局面を除外（正の値のみ）
    #[arg(long, value_parser = parse_positive_i16)]
    score_abs_max: Option<i16>,

    /// スコアをこの範囲にクリップ（±この値、正の値のみ）
    #[arg(long, value_parser = parse_positive_i16)]
    score_clip: Option<i16>,

    /// 手数がこの値未満の局面を除外
    #[arg(long)]
    ply_min: Option<u16>,

    /// 統計情報をJSON形式で出力
    #[arg(long)]
    stats: Option<PathBuf>,

    /// 統計のみ出力（出力ファイル書き込みをスキップ）
    #[arg(long)]
    stats_only: bool,

    /// ヒストグラムを作成（P50/P90/P99計算用、メモリ使用量増加）
    #[arg(long)]
    histogram: bool,

    /// 並列処理スレッド数（0=自動、王手フィルタ時のみ有効）
    #[arg(short, long, default_value_t = 0)]
    threads: usize,

    /// 処理するレコード数の上限（0=無制限）
    #[arg(long, default_value_t = 0)]
    limit: u64,

    /// 飽和率計算用のスケール値（sigmoid(score/scale)で計算、正の値のみ）
    #[arg(long, value_parser = parse_positive_f64)]
    target_scale: Option<f64>,
}

/// 正の整数をパースするバリデータ（i16）
fn parse_positive_i16(s: &str) -> Result<i16, String> {
    let val: i16 = s.parse().map_err(|_| format!("'{s}' is not a valid i16 number"))?;
    if val <= 0 {
        Err(format!("value must be positive, got {val}"))
    } else {
        Ok(val)
    }
}

/// 正の浮動小数点数をパースするバリデータ
fn parse_positive_f64(s: &str) -> Result<f64, String> {
    let val: f64 = s.parse().map_err(|_| format!("'{s}' is not a valid number"))?;
    if val <= 0.0 {
        Err(format!("target-scale must be positive, got {val}"))
    } else {
        Ok(val)
    }
}

/// 処理中にCtrl-Cが押されたかを追跡
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// フィルタ条件
#[derive(Clone, Copy)]
struct FilterOptions {
    filter_in_check: bool,
    score_abs_max: Option<i16>,
    score_clip: Option<i16>,
    ply_min: Option<u16>,
    target_scale: Option<f64>,
}

/// 統計情報
#[derive(Debug, Default, Serialize)]
struct Statistics {
    /// 入力レコード数
    total_records: u64,
    /// 出力レコード数
    output_records: u64,

    // フィルタ別の除外数
    filtered_in_check: u64,
    filtered_score_abs_max: u64,
    filtered_ply_min: u64,
    decode_errors: u64,

    // スコア分布（フィルタ前）
    score_distribution: ScoreDistribution,
    // スコア分布（フィルタ後、クリップ前）
    score_distribution_filtered: ScoreDistribution,
    // スコア分布（クリップ後）
    score_distribution_clipped: ScoreDistribution,

    // 飽和率統計
    saturation_stats: Option<SaturationStats>,
}

/// 飽和率統計（sigmoid(score/scale) の分布）
/// 注: クリップ後のスコアを使用して計算
#[derive(Debug, Default, Serialize)]
struct SaturationStats {
    scale: f64,
    count: u64,
    saturated_low: u64,
    saturated_high: u64,
    saturation_rate: f64,
}

/// スコア分布統計
#[derive(Debug, Serialize)]
struct ScoreDistribution {
    count: u64,
    min: i16,
    max: i16,
    sum: i64,
    mate_like: u64,
    high_score: u64,
    #[serde(skip)]
    histogram: Option<Vec<u64>>,
}

impl Default for ScoreDistribution {
    fn default() -> Self {
        Self::new(false)
    }
}

impl ScoreDistribution {
    fn new(with_histogram: bool) -> Self {
        Self {
            count: 0,
            min: i16::MAX,
            max: i16::MIN,
            sum: 0,
            mate_like: 0,
            high_score: 0,
            histogram: if with_histogram {
                Some(vec![0u64; 65536])
            } else {
                None
            },
        }
    }

    fn add(&mut self, score: i16) {
        self.count += 1;
        self.min = self.min.min(score);
        self.max = self.max.max(score);
        self.sum += score as i64;

        let abs_score = score.unsigned_abs();
        if abs_score >= MATE_LIKE_THRESHOLD {
            self.mate_like += 1;
        }
        if abs_score >= HIGH_SCORE_THRESHOLD {
            self.high_score += 1;
        }

        // ヒストグラムに追加（有効な場合のみ）
        if let Some(ref mut hist) = self.histogram {
            let idx = (score as i32 + 32768) as usize;
            if idx < 65536 {
                hist[idx] += 1;
            }
        }
    }

    fn percentile(&self, p: f64) -> Option<i16> {
        let hist = self.histogram.as_ref()?;
        if self.count == 0 {
            return None; // データがない場合はパーセンタイルも存在しない
        }
        let target = (self.count as f64 * p / 100.0).ceil() as u64;
        let mut cumulative = 0u64;
        for (i, &count) in hist.iter().enumerate() {
            cumulative += count;
            if cumulative >= target {
                return Some((i as i32 - 32768) as i16);
            }
        }
        Some(self.max)
    }

    fn average(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum as f64 / self.count as f64
        }
    }
}

/// フィルタ結果（並列処理用）
/// score_before は decode 成功時に常に含まれる（before filter 統計用）
///
/// 型パラメータ O で出力の有無を制御:
/// - FilterResult<[u8; 40]>: 出力あり（~50 bytes、ヒープ割り当てなし）
/// - FilterResult<()>: stats-only（~10 bytes、出力なし）
enum FilterResult<O> {
    /// フィルタを通過
    Pass {
        /// 出力バイト列（stats-only 時は () で実質 0 バイト）
        output: O,
        /// フィルタ前のスコア（before filter 統計用）
        score_before: i16,
        /// フィルタ後・クリップ前のスコア（after filter, before clip 統計用）
        /// 現状は score_before と同値だが、将来の拡張（スコア正規化等）に備えて分離
        score_unclipped: i16,
        /// クリップ後のスコア（clipped 統計用、飽和率計算にも使用）
        score_clipped: i16,
        /// 飽和判定結果（target_scale 指定時のみ有効）
        saturated_low: bool,
        saturated_high: bool,
    },
    /// 王手で除外
    FilteredInCheck { score_before: i16 },
    /// スコア絶対値超過で除外
    FilteredScoreAbsMax { score_before: i16 },
    /// 手数下限未満で除外
    FilteredPlyMin { score_before: i16 },
    /// デコードエラー（score_before なし）
    DecodeError,
}

/// 出力あり用の型エイリアス
type FilterResultWithOutput = FilterResult<[u8; PackedSfenValue::SIZE]>;
/// stats-only 用の型エイリアス（出力なし、省メモリ）
type FilterResultStatsOnly = FilterResult<()>;

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    // 入力ファイルの存在確認
    if !cli.input.exists() {
        anyhow::bail!("Input file not found: {}", cli.input.display());
    }
    if !cli.input.is_file() {
        anyhow::bail!("Input path is not a regular file: {}", cli.input.display());
    }

    // --stats-only でない場合は出力ファイルが必須
    if !cli.stats_only && cli.output.is_none() {
        anyhow::bail!("Output file is required (use --output or --stats-only)");
    }

    // --stats-only + フィルタ併用時は警告（出力なしでフィルタ適用統計を見たい場合に有用）
    if cli.stats_only
        && (cli.filter_in_check || cli.score_abs_max.is_some() || cli.ply_min.is_some())
    {
        eprintln!(
            "Note: --stats-only with filters computes stats on filtered stream (no output written).\n\
             For fastest full-file stats, omit filter options."
        );
    }

    // --histogram + --filter-in-check の組み合わせは遅いので注意喚起
    if cli.histogram && cli.filter_in_check {
        eprintln!(
            "Note: --histogram with --filter-in-check is slower.\n\
             Consider running histogram on filtered output with --stats-only."
        );
    }

    // score_abs_max < score_clip の場合は意図しない可能性があるので警告
    if let (Some(abs_max), Some(clip)) = (cli.score_abs_max, cli.score_clip) {
        if abs_max < clip {
            eprintln!(
                "Note: --score-abs-max ({abs_max}) < --score-clip ({clip}).\n\
                 Records with |score| > {abs_max} will be dropped before clipping."
            );
        }
    }

    // Ctrl-Cハンドラを設定
    ctrlc::set_handler(|| {
        eprintln!("\nInterrupted!");
        INTERRUPTED.store(true, Ordering::SeqCst);
    })
    .context("Failed to set Ctrl-C handler")?;

    // スレッド数を設定（王手フィルタ時のみ有効）
    if cli.threads > 0 && cli.filter_in_check {
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

    // 設定表示
    eprintln!("Input: {} ({} bytes, {} records)", cli.input.display(), file_size, record_count);
    eprintln!("Processing {process_count} records");
    eprintln!();
    eprintln!("Filter settings:");
    eprintln!("  - In-check filter: {}", cli.filter_in_check);
    if let Some(v) = cli.score_abs_max {
        eprintln!("  - Score abs max: {v}");
    }
    if let Some(v) = cli.score_clip {
        eprintln!("  - Score clip: ±{v}");
    }
    if let Some(v) = cli.ply_min {
        eprintln!("  - Ply min: {v}");
    }
    if let Some(v) = cli.target_scale {
        eprintln!("  - Target scale (for saturation calc): {v}");
    }
    if cli.stats_only {
        eprintln!("  - Stats only mode (no output file)");
    }
    if cli.histogram {
        eprintln!("  - Histogram enabled (P50/P90/P99 available)");
    }

    // 処理モード選択
    let mode = if cli.filter_in_check {
        "parallel (in-check filter)"
    } else {
        "sequential (fast path)"
    };
    eprintln!("  - Processing mode: {mode}");

    let opts = FilterOptions {
        filter_in_check: cli.filter_in_check,
        score_abs_max: cli.score_abs_max,
        score_clip: cli.score_clip,
        ply_min: cli.ply_min,
        target_scale: cli.target_scale,
    };

    // 処理実行（ハイブリッド方式）
    let stats = if cli.filter_in_check {
        // 王手フィルタあり：並列処理
        process_file_parallel(&cli, process_count, opts)?
    } else {
        // 王手フィルタなし：高速シーケンシャル処理
        process_file_sequential(&cli, process_count, opts)?
    };

    // 統計表示
    print_statistics(&stats, &opts, cli.histogram);

    // 統計をJSONで保存
    if let Some(ref stats_path) = cli.stats {
        let json = serde_json::to_string_pretty(&stats)?;
        std::fs::write(stats_path, json)?;
        eprintln!("\nStatistics saved to: {}", stats_path.display());
    }

    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("\nNote: Processing was interrupted, output may be incomplete");
    }

    Ok(())
}

/// チャンクサイズ（レコード数）
/// 1M records = 40MB: メモリ効率と並列化のバランスを考慮
const CHUNK_SIZE: usize = 1_000_000;

/// 詰み近似と判定するスコアの絶対値閾値
const MATE_LIKE_THRESHOLD: u16 = 30_000;
/// 高スコアと判定するスコアの絶対値閾値
const HIGH_SCORE_THRESHOLD: u16 = 10_000;

/// 飽和率計算: 低飽和と判定する閾値
const SATURATION_LOW_THRESHOLD: f64 = 0.05;
/// 飽和率計算: 高飽和と判定する閾値
const SATURATION_HIGH_THRESHOLD: f64 = 0.95;

/// 高速シーケンシャル処理（王手フィルタなし）
/// メインスレッドでストリーミング処理、ヒストグラムはオプション
fn process_file_sequential(
    cli: &Cli,
    process_count: u64,
    opts: FilterOptions,
) -> Result<Statistics> {
    let progress = ProgressBar::new(process_count);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    let in_file = File::open(&cli.input)
        .with_context(|| format!("Failed to open {}", cli.input.display()))?;
    let mut reader = BufReader::with_capacity(64 * 1024 * 1024, in_file);

    let mut writer: Option<BufWriter<File>> = if !cli.stats_only {
        if let Some(ref output_path) = cli.output {
            let out_file = File::create(output_path)
                .with_context(|| format!("Failed to create {}", output_path.display()))?;
            Some(BufWriter::with_capacity(64 * 1024 * 1024, out_file))
        } else {
            None
        }
    } else {
        None
    };

    // 統計を初期化（ヒストグラムはオプション）
    let mut stats = Statistics {
        score_distribution: ScoreDistribution::new(cli.histogram),
        score_distribution_filtered: ScoreDistribution::new(cli.histogram),
        score_distribution_clipped: ScoreDistribution::new(cli.histogram),
        ..Default::default()
    };

    let mut total_saturated_low = 0u64;
    let mut total_saturated_high = 0u64;
    let mut total_read = 0u64;
    let mut total_written = 0u64;

    let mut buffer = [0u8; PackedSfenValue::SIZE];

    progress.set_message("Processing...");

    while total_read < process_count {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            break;
        }

        // レコードを読み込み
        match reader.read_exact(&mut buffer) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        total_read += 1;

        // PackedSfenValueを読み込み
        let psv = match PackedSfenValue::from_bytes(&buffer) {
            Some(p) => p,
            None => {
                stats.decode_errors += 1;
                continue;
            }
        };

        // フィルタ前のスコア統計
        stats.score_distribution.add(psv.score);

        // 手数フィルタ
        if let Some(ply_min) = opts.ply_min {
            if psv.game_ply < ply_min {
                stats.filtered_ply_min += 1;
                continue;
            }
        }

        // スコア絶対値フィルタ
        if let Some(score_abs_max) = opts.score_abs_max {
            if psv.score.unsigned_abs() > score_abs_max as u16 {
                stats.filtered_score_abs_max += 1;
                continue;
            }
        }

        // フィルタ後のスコア統計
        stats.score_distribution_filtered.add(psv.score);

        // スコアクリップ
        let final_score = if let Some(clip) = opts.score_clip {
            psv.score.clamp(-clip, clip)
        } else {
            psv.score
        };

        // クリップ後のスコア統計
        stats.score_distribution_clipped.add(final_score);

        // 飽和率計算
        if let Some(scale) = opts.target_scale {
            let target = 1.0 / (1.0 + (-final_score as f64 / scale).exp());
            if target < SATURATION_LOW_THRESHOLD {
                total_saturated_low += 1;
            } else if target > SATURATION_HIGH_THRESHOLD {
                total_saturated_high += 1;
            }
        }

        // 出力
        if let Some(ref mut w) = writer {
            let new_psv = PackedSfenValue {
                sfen: psv.sfen,
                score: final_score,
                move16: psv.move16,
                game_ply: psv.game_ply,
                game_result: psv.game_result,
                padding: psv.padding,
            };
            w.write_all(&new_psv.to_bytes())?;
            total_written += 1;
        } else {
            total_written += 1; // stats-only でもカウント
        }

        if total_read.is_multiple_of(100_000) {
            progress.set_position(total_read);
        }
    }

    progress.finish_with_message("Done");

    if let Some(ref mut w) = writer {
        w.flush()?;
    }

    // 統計を設定
    stats.total_records = total_read;
    stats.output_records = total_written;

    if let Some(scale) = opts.target_scale {
        let count = stats.score_distribution_clipped.count;
        stats.saturation_stats = Some(SaturationStats {
            scale,
            count,
            saturated_low: total_saturated_low,
            saturated_high: total_saturated_high,
            saturation_rate: if count > 0 {
                (total_saturated_low + total_saturated_high) as f64 / count as f64 * 100.0
            } else {
                0.0
            },
        });
    }

    if let Some(ref output_path) = cli.output {
        if !cli.stats_only {
            eprintln!("\nWrote {} records to {}", total_written, output_path.display());
        }
    }

    Ok(stats)
}

/// 並列処理（王手フィルタあり）
/// 王手判定は並列、ヒストグラムはメインスレッドで集計
fn process_file_parallel(cli: &Cli, process_count: u64, opts: FilterOptions) -> Result<Statistics> {
    let progress = ProgressBar::new(process_count);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    let in_file = File::open(&cli.input)
        .with_context(|| format!("Failed to open {}", cli.input.display()))?;
    let mut reader = BufReader::with_capacity(64 * 1024 * 1024, in_file);

    let mut writer: Option<BufWriter<File>> = if !cli.stats_only {
        if let Some(ref output_path) = cli.output {
            let out_file = File::create(output_path)
                .with_context(|| format!("Failed to create {}", output_path.display()))?;
            Some(BufWriter::with_capacity(64 * 1024 * 1024, out_file))
        } else {
            None
        }
    } else {
        None
    };

    // 統計を初期化（ヒストグラムはオプション）
    let mut stats = Statistics {
        score_distribution: ScoreDistribution::new(cli.histogram),
        score_distribution_filtered: ScoreDistribution::new(cli.histogram),
        score_distribution_clipped: ScoreDistribution::new(cli.histogram),
        ..Default::default()
    };

    let mut total_saturated_low = 0u64;
    let mut total_saturated_high = 0u64;
    let mut total_read = 0u64;
    let mut total_written = 0u64;

    let mut chunk: Vec<[u8; PackedSfenValue::SIZE]> = Vec::with_capacity(CHUNK_SIZE);
    let mut buffer = [0u8; PackedSfenValue::SIZE];

    let collect_outputs = writer.is_some();

    progress.set_message("Processing...");

    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            progress.abandon_with_message("Interrupted");
            break;
        }

        // チャンクを読み込み
        chunk.clear();
        for _ in 0..CHUNK_SIZE {
            if total_read >= process_count {
                break;
            }

            match reader.read_exact(&mut buffer) {
                Ok(()) => {
                    chunk.push(buffer);
                    total_read += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }

        if chunk.is_empty() {
            break;
        }

        // チャンク内を並列処理（王手判定）
        // 出力あり/stats-only で異なる型を使い分け
        if collect_outputs {
            // 出力あり: FilterResultWithOutput（ヒープ割り当てなし、~50 bytes/要素）
            let results: Vec<FilterResultWithOutput> = chunk
                .par_iter()
                .map(|record| process_record_with_output(record, opts))
                .collect();

            for result in results {
                match result {
                    FilterResult::Pass {
                        output,
                        score_before,
                        score_unclipped,
                        score_clipped,
                        saturated_low,
                        saturated_high,
                    } => {
                        stats.score_distribution.add(score_before);
                        stats.score_distribution_filtered.add(score_unclipped);
                        stats.score_distribution_clipped.add(score_clipped);
                        if saturated_low {
                            total_saturated_low += 1;
                        }
                        if saturated_high {
                            total_saturated_high += 1;
                        }
                        if let Some(ref mut w) = writer {
                            w.write_all(&output)?;
                        }
                        total_written += 1;
                    }
                    FilterResult::FilteredInCheck { score_before } => {
                        stats.score_distribution.add(score_before);
                        stats.filtered_in_check += 1;
                    }
                    FilterResult::FilteredScoreAbsMax { score_before } => {
                        stats.score_distribution.add(score_before);
                        stats.filtered_score_abs_max += 1;
                    }
                    FilterResult::FilteredPlyMin { score_before } => {
                        stats.score_distribution.add(score_before);
                        stats.filtered_ply_min += 1;
                    }
                    FilterResult::DecodeError => {
                        stats.decode_errors += 1;
                    }
                }
            }
        } else {
            // stats-only: FilterResultStatsOnly（~10 bytes/要素、省メモリ）
            let results: Vec<FilterResultStatsOnly> =
                chunk.par_iter().map(|record| process_record_stats_only(record, opts)).collect();

            for result in results {
                match result {
                    FilterResult::Pass {
                        output: _,
                        score_before,
                        score_unclipped,
                        score_clipped,
                        saturated_low,
                        saturated_high,
                    } => {
                        stats.score_distribution.add(score_before);
                        stats.score_distribution_filtered.add(score_unclipped);
                        stats.score_distribution_clipped.add(score_clipped);
                        if saturated_low {
                            total_saturated_low += 1;
                        }
                        if saturated_high {
                            total_saturated_high += 1;
                        }
                        total_written += 1;
                    }
                    FilterResult::FilteredInCheck { score_before } => {
                        stats.score_distribution.add(score_before);
                        stats.filtered_in_check += 1;
                    }
                    FilterResult::FilteredScoreAbsMax { score_before } => {
                        stats.score_distribution.add(score_before);
                        stats.filtered_score_abs_max += 1;
                    }
                    FilterResult::FilteredPlyMin { score_before } => {
                        stats.score_distribution.add(score_before);
                        stats.filtered_ply_min += 1;
                    }
                    FilterResult::DecodeError => {
                        stats.decode_errors += 1;
                    }
                }
            }
        }

        progress.set_position(total_read);
    }

    progress.finish_with_message("Done");

    if let Some(ref mut w) = writer {
        w.flush()?;
    }

    // 統計を設定
    stats.total_records = total_read;
    stats.output_records = total_written;

    if let Some(scale) = opts.target_scale {
        let count = stats.score_distribution_clipped.count;
        stats.saturation_stats = Some(SaturationStats {
            scale,
            count,
            saturated_low: total_saturated_low,
            saturated_high: total_saturated_high,
            saturation_rate: if count > 0 {
                (total_saturated_low + total_saturated_high) as f64 / count as f64 * 100.0
            } else {
                0.0
            },
        });
    }

    if let Some(ref output_path) = cli.output {
        if !cli.stats_only {
            eprintln!("\nWrote {} records to {}", total_written, output_path.display());
        }
    }

    Ok(stats)
}

/// Pass 時の共通データ
struct PassData {
    psv: PackedSfenValue,
    score_before: i16,
    score_unclipped: i16,
    score_clipped: i16,
    saturated_low: bool,
    saturated_high: bool,
}

/// 共通のフィルタ処理を行い、Pass 時の追加情報を返す
fn process_record_common<O>(
    record: &[u8; PackedSfenValue::SIZE],
    opts: FilterOptions,
) -> Result<PassData, FilterResult<O>> {
    // PackedSfenValueを読み込み
    let psv = match PackedSfenValue::from_bytes(record) {
        Some(p) => p,
        None => return Err(FilterResult::DecodeError),
    };

    let score_before = psv.score;

    // 手数フィルタ（最も軽量なので最初）
    if let Some(ply_min) = opts.ply_min {
        if psv.game_ply < ply_min {
            return Err(FilterResult::FilteredPlyMin { score_before });
        }
    }

    // スコア絶対値フィルタ
    if let Some(score_abs_max) = opts.score_abs_max {
        if psv.score.unsigned_abs() > score_abs_max as u16 {
            return Err(FilterResult::FilteredScoreAbsMax { score_before });
        }
    }

    // 王手フィルタ（重いので最後）
    let sfen = match unpack_sfen(&psv.sfen) {
        Ok(s) => s,
        Err(_) => return Err(FilterResult::DecodeError),
    };

    let mut pos = Position::new();
    if pos.set_sfen(&sfen).is_err() {
        return Err(FilterResult::DecodeError);
    }

    if pos.in_check() {
        return Err(FilterResult::FilteredInCheck { score_before });
    }

    // フィルタ後・クリップ前のスコア（現状は score_before と同値）
    let score_unclipped = psv.score;

    // スコアクリップ
    let score_clipped = if let Some(clip) = opts.score_clip {
        score_unclipped.clamp(-clip, clip)
    } else {
        score_unclipped
    };

    // 飽和率計算
    let (saturated_low, saturated_high) = if let Some(scale) = opts.target_scale {
        let target = 1.0 / (1.0 + (-score_clipped as f64 / scale).exp());
        (target < SATURATION_LOW_THRESHOLD, target > SATURATION_HIGH_THRESHOLD)
    } else {
        (false, false)
    };

    Ok(PassData {
        psv,
        score_before,
        score_unclipped,
        score_clipped,
        saturated_low,
        saturated_high,
    })
}

/// 1レコードを並列処理（出力あり用）
/// ヒープ割り当てなしでインライン配列を使用
fn process_record_with_output(
    record: &[u8; PackedSfenValue::SIZE],
    opts: FilterOptions,
) -> FilterResultWithOutput {
    match process_record_common(record, opts) {
        Err(result) => result,
        Ok(data) => {
            let new_psv = PackedSfenValue {
                sfen: data.psv.sfen,
                score: data.score_clipped,
                move16: data.psv.move16,
                game_ply: data.psv.game_ply,
                game_result: data.psv.game_result,
                padding: data.psv.padding,
            };
            FilterResult::Pass {
                output: new_psv.to_bytes(),
                score_before: data.score_before,
                score_unclipped: data.score_unclipped,
                score_clipped: data.score_clipped,
                saturated_low: data.saturated_low,
                saturated_high: data.saturated_high,
            }
        }
    }
}

/// 1レコードを並列処理（stats-only 用）
/// 出力バイト列を生成せず、省メモリ
fn process_record_stats_only(
    record: &[u8; PackedSfenValue::SIZE],
    opts: FilterOptions,
) -> FilterResultStatsOnly {
    match process_record_common(record, opts) {
        Err(result) => result,
        Ok(data) => FilterResult::Pass {
            output: (),
            score_before: data.score_before,
            score_unclipped: data.score_unclipped,
            score_clipped: data.score_clipped,
            saturated_low: data.saturated_low,
            saturated_high: data.saturated_high,
        },
    }
}

/// 統計を表示
fn print_statistics(stats: &Statistics, opts: &FilterOptions, histogram_enabled: bool) {
    eprintln!("\n========== Statistics ==========");
    eprintln!();
    eprintln!("Records:");
    eprintln!("  Input:  {}", stats.total_records);
    eprintln!("  Output: {}", stats.output_records);
    let removed = stats.total_records - stats.output_records;
    let removed_pct = if stats.total_records > 0 {
        removed as f64 / stats.total_records as f64 * 100.0
    } else {
        0.0
    };
    eprintln!("  Removed: {removed} ({removed_pct:.2}%)");

    eprintln!();
    eprintln!("Filter breakdown:");
    if opts.filter_in_check {
        let pct = if stats.total_records > 0 {
            stats.filtered_in_check as f64 / stats.total_records as f64 * 100.0
        } else {
            0.0
        };
        eprintln!("  In-check:      {} ({pct:.2}%)", stats.filtered_in_check);
    }
    if opts.score_abs_max.is_some() {
        let pct = if stats.total_records > 0 {
            stats.filtered_score_abs_max as f64 / stats.total_records as f64 * 100.0
        } else {
            0.0
        };
        eprintln!("  Score abs max: {} ({pct:.2}%)", stats.filtered_score_abs_max);
    }
    if opts.ply_min.is_some() {
        let pct = if stats.total_records > 0 {
            stats.filtered_ply_min as f64 / stats.total_records as f64 * 100.0
        } else {
            0.0
        };
        eprintln!("  Ply min:       {} ({pct:.2}%)", stats.filtered_ply_min);
    }
    if stats.decode_errors > 0 {
        let pct = if stats.total_records > 0 {
            stats.decode_errors as f64 / stats.total_records as f64 * 100.0
        } else {
            0.0
        };
        eprintln!("  Decode errors: {} ({pct:.2}%)", stats.decode_errors);
    }

    // スコア分布（フィルタ前）
    let dist = &stats.score_distribution;
    if dist.count > 0 {
        eprintln!();
        eprintln!("Score distribution (before filter):");
        eprintln!("  Count: {}", dist.count);
        eprintln!("  Min: {}, Max: {}", dist.min, dist.max);
        eprintln!("  Average: {:.1}", dist.average());
        if histogram_enabled {
            if let (Some(p50), Some(p90), Some(p99)) =
                (dist.percentile(50.0), dist.percentile(90.0), dist.percentile(99.0))
            {
                eprintln!("  P50: {p50}, P90: {p90}, P99: {p99}");
            }
        }
        let mate_pct = dist.mate_like as f64 / dist.count as f64 * 100.0;
        let high_pct = dist.high_score as f64 / dist.count as f64 * 100.0;
        eprintln!(
            "  |score| >= {MATE_LIKE_THRESHOLD} (mate-like): {} ({mate_pct:.2}%)",
            dist.mate_like
        );
        eprintln!(
            "  |score| >= {HIGH_SCORE_THRESHOLD} (high):      {} ({high_pct:.2}%)",
            dist.high_score
        );
    }

    // スコア分布（フィルタ後）
    let dist_filtered = &stats.score_distribution_filtered;
    if dist_filtered.count > 0 && dist_filtered.count != dist.count {
        eprintln!();
        eprintln!("Score distribution (after filter, before clip):");
        eprintln!("  Count: {}", dist_filtered.count);
        eprintln!("  Min: {}, Max: {}", dist_filtered.min, dist_filtered.max);
        eprintln!("  Average: {:.1}", dist_filtered.average());
        if histogram_enabled {
            if let (Some(p50), Some(p90), Some(p99)) = (
                dist_filtered.percentile(50.0),
                dist_filtered.percentile(90.0),
                dist_filtered.percentile(99.0),
            ) {
                eprintln!("  P50: {p50}, P90: {p90}, P99: {p99}");
            }
        }
        let mate_pct = dist_filtered.mate_like as f64 / dist_filtered.count as f64 * 100.0;
        let high_pct = dist_filtered.high_score as f64 / dist_filtered.count as f64 * 100.0;
        eprintln!(
            "  |score| >= {MATE_LIKE_THRESHOLD} (mate-like): {} ({mate_pct:.2}%)",
            dist_filtered.mate_like
        );
        eprintln!(
            "  |score| >= {HIGH_SCORE_THRESHOLD} (high):      {} ({high_pct:.2}%)",
            dist_filtered.high_score
        );
    }

    // スコア分布（クリップ後）
    let dist_clipped = &stats.score_distribution_clipped;
    if dist_clipped.count > 0 && opts.score_clip.is_some() {
        eprintln!();
        eprintln!("Score distribution (after clip):");
        eprintln!("  Count: {}", dist_clipped.count);
        eprintln!("  Min: {}, Max: {}", dist_clipped.min, dist_clipped.max);
        eprintln!("  Average: {:.1}", dist_clipped.average());
        if histogram_enabled {
            if let (Some(p50), Some(p90), Some(p99)) = (
                dist_clipped.percentile(50.0),
                dist_clipped.percentile(90.0),
                dist_clipped.percentile(99.0),
            ) {
                eprintln!("  P50: {p50}, P90: {p90}, P99: {p99}");
            }
        }
        let high_pct = dist_clipped.high_score as f64 / dist_clipped.count as f64 * 100.0;
        eprintln!(
            "  |score| >= {HIGH_SCORE_THRESHOLD} (high):      {} ({high_pct:.2}%)",
            dist_clipped.high_score
        );
    }

    // 飽和率統計
    if let Some(ref sat) = stats.saturation_stats {
        eprintln!();
        let score_desc = if opts.score_clip.is_some() {
            "clipped score"
        } else {
            "post-filter score"
        };
        eprintln!("Saturation stats (scale={}, computed on {score_desc}):", sat.scale);
        eprintln!("  Count: {}", sat.count);
        let low_pct = if sat.count > 0 {
            sat.saturated_low as f64 / sat.count as f64 * 100.0
        } else {
            0.0
        };
        let high_pct = if sat.count > 0 {
            sat.saturated_high as f64 / sat.count as f64 * 100.0
        } else {
            0.0
        };
        eprintln!(
            "  Saturated low  (target < {SATURATION_LOW_THRESHOLD}): {} ({low_pct:.2}%)",
            sat.saturated_low
        );
        eprintln!(
            "  Saturated high (target > {SATURATION_HIGH_THRESHOLD}): {} ({high_pct:.2}%)",
            sat.saturated_high
        );
        eprintln!("  Total saturation rate: {:.2}%", sat.saturation_rate);
    }

    eprintln!();
    eprintln!("================================");
}
