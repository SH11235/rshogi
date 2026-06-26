//! ラベル品質「物差し」ステージ 1: held-out hcpe を labeler でラベル付けする。
//!
//! 固定 held-out（棋譜由来の hcpe。各局面に保存 eval=教師ラベルと gameResult=実対局結果
//! を持つ）の各局面を、与えた labeler（NNUE 評価器 + 固定 depth）の決定的探索で評価し、
//! 採点に必要な値だけを 1 行 1 局面の jsonl に書き出す。ステージ 2 (`yardstick_score`) が
//! この出力を読み、engine ごとに勝率スケールを較正して per-class の WDL logloss /
//! 参照天井 / リファレンス一致を算出する。
//!
//! 設計上の不変条件（`label_bench_positions` と同じ）:
//! - 局面ごとに `Search` を作り直し 1 スレッド固定（`set_num_threads(1)`）で探索する。
//!   これにより 1 局面の評価は他局面・処理順・`--threads` から独立し、同一入力なら
//!   出力は bit 一致する。
//! - 入力件数に対してピークメモリが線形に増えないよう streaming で処理する。producer が
//!   トークン制で in-flight 件数を一定上限に抑え、collector が入力順へ並べ替えて逐次書き出す。
//!
//! 評価値・勝敗の符号規約は **手番側視点（side-to-move view）**で統一する。hcpe の保存 eval は
//! 手番側視点 cp（PSV `score` と同じ）であり、dlshogi DataLoader の value 目標もそうなので、
//! ここでは先手視点へ変換せず手番側視点のまま素通しする（ステージ 2 もこの規約で採点する）。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossbeam_channel::{bounded, unbounded};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;

use rshogi_core::bitboard::{Bitboard, RANK_BB};
use rshogi_core::nnue::{
    LayerStackBucketMode, SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, get_layer_stack_bucket_mode,
    init_nnue, is_layer_stacks_loaded, parse_layer_stack_bucket_mode, set_fv_scale_override,
    set_layer_stack_bucket_mode, set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search, SearchInfo};
use rshogi_core::types::Color;
use tools::packed_sfen::unpack_hcp;

/// 探索用スタックサイズ（64MB）。深い探索で再帰スタックを使うため main 同等を確保する。
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

/// hcpe（cshogi HuffmanCodedPosAndEval）1 レコードのバイト長。
const HCPE_RECORD_SIZE: usize = 38;

/// 詰みとみなす絶対 cp 閾値。保存 eval の符号飽和域は較正・logloss から除外する。
const MATE_ABS: i32 = 30000;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Parser, Debug)]
#[command(
    name = "yardstick_label",
    version,
    about = "held-out hcpe を NNUE labeler の固定 depth 探索でラベル付けし採点用 jsonl を出す"
)]
struct Cli {
    /// 入力 hcpe（cshogi HuffmanCodedPosAndEval, 38B/レコード）。
    #[arg(long = "in")]
    input: PathBuf,

    /// 出力 jsonl（採点用フィールドのみ、入力順）。
    #[arg(long = "out")]
    output: PathBuf,

    /// labeler の NNUE モデルファイル。
    #[arg(long)]
    nnue: PathBuf,

    /// FV_SCALE オーバーライド（0=ヘッダ自動判定、1 以上=指定値）。評価器の native 値に
    /// 合わせて明示すること（threat/none LayerStacks 系は 28）。
    #[arg(long, default_value_t = 0)]
    fv_scale: i32,

    /// LayerStacks の bucket mode（例: `progress8kpabs`）。LS ビルドでは既定が
    /// progress8kpabs なので通常は指定不要。
    #[arg(long)]
    ls_bucket_mode: Option<String>,

    /// progress8kpabs 用の進行度係数ファイル（USI `LS_PROGRESS_COEFF` と同じ）。
    /// LayerStacks モデルで bucket mode が progress8kpabs のとき必須。
    #[arg(long)]
    ls_progress_coeff: Option<PathBuf>,

    /// 探索深さ上限（0 以下=無制限）。depth を物差しの変数にする時は `--nodes 0` で
    /// depth を binding にする。`--nodes` と両方とも無制限は不可。
    #[arg(long, default_value_t = 12)]
    depth: i32,

    /// 探索ノード数上限（0=無制限）。深さと併用し先に到達した方で停止。
    #[arg(long, default_value_t = 0)]
    nodes: u64,

    /// worker ごとの置換表サイズ（MB）。局面ごとに作り直すため過大にしない。
    #[arg(long, default_value_t = 128)]
    hash_mb: usize,

    /// worker スレッド数（0=利用可能 CPU 数）。出力は thread 数非依存に bit 一致。
    #[arg(long, default_value_t = 0)]
    threads: usize,

    /// 出力レコードに付与する source ラベル（hcpe はソースを持たないので任意指定）。
    /// 例: `floodgate` / `dlsuisho_val`。
    #[arg(long)]
    source: Option<String>,

    /// 先頭から処理する最大レコード数（0=全件）。smoke 用。
    #[arg(long, default_value_t = 0)]
    limit: usize,
}

/// ステージ 2 が読む採点用レコード。符号規約はすべて手番側視点。
#[derive(Serialize)]
struct ScoreRecord {
    /// 手番（`b`/`w`）。
    stm: char,
    /// 実対局結果（手番側視点の勝率: 勝 1.0 / 負 0.0 / 引分 0.5）。
    wdl: f64,
    /// held-out 保存 eval（手番側視点 cp、教師=リファレンスラベル）。
    eval_ref: i32,
    /// labeler の探索値（手番側視点 cp）。
    eval_label: i32,
    /// 保存 eval から決めた class（labeler 非依存に固定するため eval_ref を使う）。
    eval_band: &'static str,
    /// 入玉 class（`none`/`black_entered`/`white_entered`/`both_entered`）。
    nyugyoku: &'static str,
    /// 王手局面か。
    in_check: bool,
    /// 保存 eval が飽和域（|eval_ref| >= MATE_ABS）か。
    mate_ref: bool,
    /// labeler が詰みスコアを返したか。
    mate_label: bool,
    /// source ラベル（`--source` 指定時のみ）。
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
}

/// 1 局面の処理結果。`Error` でも seq スロットを消費するので順序は崩れない。
enum Outcome {
    Ok(String),
    Skip,
    Error(String),
}

fn main() -> Result<()> {
    install_fatal_panic_hook();
    let cli = Cli::parse();
    run(&cli)
}

/// worker スレッドの探索パニックでプロセス全体を loud に終了させる（致命バグを黙って
/// 部分出力に残さない）。`label_bench_positions` と同方針。
fn install_fatal_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::exit(101);
    }));
}

fn run(cli: &Cli) -> Result<()> {
    validate_paths(&cli.input, &cli.output)?;
    if cli.depth <= 0 && cli.nodes == 0 {
        bail!("--depth and --nodes are both unlimited; specify at least one to bound the search");
    }

    configure_eval(cli)?;

    let num_threads = if cli.threads > 0 {
        cli.threads
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    };

    let total_records = count_records(&cli.input)?;
    let total = if cli.limit > 0 {
        total_records.min(cli.limit as u64)
    } else {
        total_records
    };

    eprintln!(
        "Labeling {} ({} records) -> {} (depth={}, nodes={}, hash={}MB/worker, threads={})",
        cli.input.display(),
        total,
        cli.output.display(),
        cli.depth,
        cli.nodes,
        cli.hash_mb,
        num_threads,
    );

    ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
        .context("Failed to set Ctrl-C handler")?;

    let progress = ProgressBar::new(total);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    let stats = run_pipeline(cli, num_threads, &progress)?;

    progress.finish_with_message("Done");
    eprintln!("Wrote {} labeled records", stats.written);
    if stats.skipped > 0 {
        eprintln!("Skipped {} records (invalid wdl/board)", stats.skipped);
    }
    if stats.errors > 0 {
        eprintln!("Skipped {} records due to errors", stats.errors);
    }
    if INTERRUPTED.load(Ordering::SeqCst) {
        bail!(
            "interrupted: output truncated to the in-order prefix ({} records written)",
            stats.written
        );
    }
    Ok(())
}

struct RunStats {
    written: u64,
    skipped: u64,
    errors: u64,
}

/// producer + worker + collector のストリーミングパイプライン本体。
fn run_pipeline(cli: &Cli, num_threads: usize, progress: &ProgressBar) -> Result<RunStats> {
    let inflight_cap = (num_threads * 4).max(num_threads + 1);

    let (token_tx, token_rx) = bounded::<()>(inflight_cap);
    for _ in 0..inflight_cap {
        token_tx.send(()).expect("prime tokens");
    }
    let (work_tx, work_rx) = unbounded::<(usize, [u8; HCPE_RECORD_SIZE])>();
    let (res_tx, res_rx) = unbounded::<(usize, Outcome)>();

    let depth = cli.depth;
    let nodes = cli.nodes;
    let hash_mb = cli.hash_mb;
    let source = cli.source.clone();

    let mut workers = Vec::with_capacity(num_threads);
    for worker_idx in 0..num_threads {
        let work_rx = work_rx.clone();
        let res_tx = res_tx.clone();
        let source = source.clone();
        let handle = thread::Builder::new()
            .name(format!("yardstick-worker-{worker_idx}"))
            .stack_size(SEARCH_STACK_SIZE)
            .spawn(move || {
                while let Ok((seq, bytes)) = work_rx.recv() {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        break;
                    }
                    let outcome = process_record(&bytes, hash_mb, depth, nodes, source.as_deref());
                    if res_tx.send((seq, outcome)).is_err() {
                        break;
                    }
                }
            })
            .context("Failed to spawn worker thread")?;
        workers.push(handle);
    }
    drop(work_rx);
    drop(res_tx);

    let input_path = cli.input.clone();
    let limit = cli.limit;
    let producer = thread::spawn(move || -> Result<()> {
        let file = File::open(&input_path)
            .with_context(|| format!("Failed to open {}", input_path.display()))?;
        let mut reader = BufReader::new(file);
        let mut seq = 0usize;
        let mut buf = [0u8; HCPE_RECORD_SIZE];
        loop {
            if limit > 0 && seq >= limit {
                break;
            }
            if INTERRUPTED.load(Ordering::SeqCst) {
                break;
            }
            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e).context("Failed to read hcpe record"),
            }
            // in-flight 上限まで投入したら token を待つ（collector が 1 件書き出すと返る）。
            if token_rx.recv().is_err() {
                break;
            }
            if work_tx.send((seq, buf)).is_err() {
                break;
            }
            seq += 1;
        }
        drop(work_tx);
        Ok(())
    });

    // collector: seq 順に並べ替えて逐次書き出す。
    let out_file = File::create(&cli.output)
        .with_context(|| format!("Failed to create {}", cli.output.display()))?;
    let mut writer = BufWriter::new(out_file);
    let mut next = 0usize;
    let mut buf: std::collections::BTreeMap<usize, Outcome> = std::collections::BTreeMap::new();
    let mut written = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;

    while let Ok((seq, outcome)) = res_rx.recv() {
        buf.insert(seq, outcome);
        while let Some(outcome) = buf.remove(&next) {
            match outcome {
                Outcome::Ok(line) => {
                    writer.write_all(line.as_bytes())?;
                    writer.write_all(b"\n")?;
                    written += 1;
                }
                Outcome::Skip => skipped += 1,
                Outcome::Error(msg) => {
                    errors += 1;
                    eprintln!("skip record {next}: {msg}");
                }
            }
            next += 1;
            progress.inc(1);
            let _ = token_tx.send(());
        }
    }
    writer.flush()?;

    drop(token_tx);
    producer.join().map_err(|_| anyhow::anyhow!("producer thread panicked"))??;
    for handle in workers {
        let _ = handle.join();
    }

    Ok(RunStats {
        written,
        skipped,
        errors,
    })
}

/// hcpe 1 レコードを labeler でラベル付けし採点用 jsonl 行にする。
fn process_record(
    bytes: &[u8; HCPE_RECORD_SIZE],
    hash_mb: usize,
    depth: i32,
    nodes: u64,
    source: Option<&str>,
) -> Outcome {
    let eval_ref = i16::from_le_bytes([bytes[32], bytes[33]]) as i32;
    let game_result = bytes[36];

    let mut hcp = [0u8; 32];
    hcp.copy_from_slice(&bytes[0..32]);
    let sfen = match unpack_hcp(&hcp) {
        Ok(s) => s,
        Err(e) => return Outcome::Error(format!("unpack_hcp failed: {e}")),
    };

    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return Outcome::Error(format!("set_sfen failed: {e:?}: {sfen}"));
    }
    let stm = pos.side_to_move();

    let Some(wdl) = wdl_stm(game_result, stm) else {
        // gameResult が不正（0/1/2 以外）なレコードは採点対象外。
        return Outcome::Skip;
    };

    let nyugyoku = nyugyoku_label(&pos);
    let in_check = pos.in_check();

    let mut search = Search::new(hash_mb);
    search.set_num_threads(1);
    let mut limits = LimitsType::default();
    limits.depth = depth;
    if nodes > 0 {
        limits.nodes = nodes;
    }
    limits.set_start_time();
    let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);
    let score = result.score;

    let record = ScoreRecord {
        stm: if stm == Color::Black { 'b' } else { 'w' },
        wdl,
        eval_ref,
        eval_label: score.to_cp(),
        eval_band: eval_band(eval_ref),
        nyugyoku,
        in_check,
        mate_ref: eval_ref.abs() >= MATE_ABS,
        mate_label: score.is_mate_score(),
        source: source.map(str::to_string),
    };
    match serde_json::to_string(&record) {
        Ok(s) => Outcome::Ok(s),
        Err(e) => Outcome::Error(format!("serialize error: {e}")),
    }
}

/// gameResult（0=DRAW / 1=BLACK_WIN / 2=WHITE_WIN, 絶対視点）を手番側視点の勝率へ。
fn wdl_stm(game_result: u8, stm: Color) -> Option<f64> {
    match game_result {
        0 => Some(0.5),
        1 => Some(if stm == Color::Black { 1.0 } else { 0.0 }),
        2 => Some(if stm == Color::White { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// 入玉 class。敵陣 3 段目以内に玉がいるかで分類（`extract_bench_positions` と同義）。
fn nyugyoku_label(pos: &Position) -> &'static str {
    let black = enemy_field_ranks(Color::Black).contains(pos.king_square(Color::Black));
    let white = enemy_field_ranks(Color::White).contains(pos.king_square(Color::White));
    match (black, white) {
        (true, true) => "both_entered",
        (true, false) => "black_entered",
        (false, true) => "white_entered",
        (false, false) => "none",
    }
}

fn enemy_field_ranks(color: Color) -> Bitboard {
    match color {
        Color::Black => RANK_BB[0] | RANK_BB[1] | RANK_BB[2],
        Color::White => RANK_BB[6] | RANK_BB[7] | RANK_BB[8],
    }
}

/// |eval| の帯（`extract_bench_positions` と同義）。手番側視点でも絶対値分類なので不変。
fn eval_band(eval: i32) -> &'static str {
    match eval.abs() {
        0..=150 => "0-150",
        151..=600 => "151-600",
        601..=1500 => "601-1500",
        1501..=29_999 => "1501+",
        _ => "mate",
    }
}

/// 評価器（NNUE + LayerStacks bucket 設定）を USI エンジンと同じ手順で構成する。
/// `label_bench_positions::configure_eval` と同じく progress8kpabs で係数未指定なら弾く。
fn configure_eval(cli: &Cli) -> Result<()> {
    if !cli.nnue.exists() {
        bail!("NNUE model file not found: {}", cli.nnue.display());
    }
    if cli.fv_scale != 0 {
        set_fv_scale_override(cli.fv_scale);
        eprintln!("FV_SCALE: {}", cli.fv_scale);
    } else {
        eprintln!("FV_SCALE: auto-detect (header)");
    }
    if let Some(mode_str) = &cli.ls_bucket_mode {
        let mode = parse_layer_stack_bucket_mode(mode_str)
            .with_context(|| format!("invalid --ls-bucket-mode '{mode_str}'"))?;
        set_layer_stack_bucket_mode(mode);
        eprintln!("LS_BUCKET_MODE: {}", mode.as_str());
    }
    let mut coeff_loaded = false;
    if let Some(path) = &cli.ls_progress_coeff {
        let weights = load_progress_coeff_kpabs(path)?;
        set_layer_stack_progress_kpabs_weights(weights)
            .map_err(|e| anyhow::anyhow!("failed to set progress coeff weights: {e}"))?;
        coeff_loaded = true;
        eprintln!("LS_PROGRESS_COEFF: {}", path.display());
    }
    init_nnue(&cli.nnue).context("Failed to load NNUE model")?;
    eprintln!("NNUE model loaded: {}", cli.nnue.display());
    if is_layer_stacks_loaded()
        && get_layer_stack_bucket_mode() == LayerStackBucketMode::Progress8KPAbs
        && !coeff_loaded
    {
        bail!(
            "LS_BUCKET_MODE=progress8kpabs requires --ls-progress-coeff. \
             Without it the progress bucket selection diverges from training and labels are wrong."
        );
    }
    Ok(())
}

/// progress8kpabs 用の進行度係数ファイル（f64 配列）を読み f32 重みへ変換する。
fn load_progress_coeff_kpabs(path: &Path) -> Result<Box<[f32]>> {
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read --ls-progress-coeff {}", path.display()))?;
    let expected = SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS * std::mem::size_of::<f64>();
    if bytes.len() != expected {
        bail!("progress coeff size mismatch: got {} bytes, expected {}", bytes.len(), expected);
    }
    let weights: Vec<f32> = bytes
        .chunks_exact(std::mem::size_of::<f64>())
        .map(|chunk| f64::from_le_bytes(chunk.try_into().expect("chunk size is checked")) as f32)
        .collect();
    Ok(weights.into_boxed_slice())
}

fn validate_paths(input: &Path, output: &Path) -> Result<()> {
    if input == output {
        bail!("--in and --out must differ");
    }
    if let (Ok(a), Ok(b)) = (fs::canonicalize(input), fs::canonicalize(output))
        && a == b
    {
        bail!("--in and --out resolve to the same file");
    }
    // canonicalize は hardlink（別 path・同一 inode）を検出できない。collector が出力を
    // create で truncate するため、dev/ino でも同一性を弾いて入力破壊を防ぐ。
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(im), Ok(om)) = (fs::metadata(input), fs::metadata(output))
            && im.dev() == om.dev()
            && im.ino() == om.ino()
        {
            bail!("--in and --out are the same file (same dev/ino)");
        }
    }
    if let Ok(meta) = fs::symlink_metadata(output)
        && meta.file_type().is_symlink()
    {
        bail!("refusing to write through a symlink: {}", output.display());
    }
    if let Some(parent) = output.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create output dir {}", parent.display()))?;
    }
    Ok(())
}

/// 進捗バーの分母用に hcpe レコード数を数える（ファイルサイズ / 38）。
fn count_records(path: &Path) -> Result<u64> {
    let len = fs::metadata(path)
        .with_context(|| format!("Failed to stat {}", path.display()))?
        .len();
    if len % HCPE_RECORD_SIZE as u64 != 0 {
        bail!(
            "hcpe file size {} is not a multiple of {} (corrupt or wrong format)",
            len,
            HCPE_RECORD_SIZE
        );
    }
    Ok(len / HCPE_RECORD_SIZE as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wdl_stm_maps_absolute_result_to_side_to_move() {
        assert_eq!(wdl_stm(1, Color::Black), Some(1.0));
        assert_eq!(wdl_stm(1, Color::White), Some(0.0));
        assert_eq!(wdl_stm(2, Color::Black), Some(0.0));
        assert_eq!(wdl_stm(2, Color::White), Some(1.0));
        assert_eq!(wdl_stm(0, Color::Black), Some(0.5));
        assert_eq!(wdl_stm(0, Color::White), Some(0.5));
        assert_eq!(wdl_stm(3, Color::Black), None);
    }

    #[test]
    fn eval_band_boundaries() {
        assert_eq!(eval_band(0), "0-150");
        assert_eq!(eval_band(150), "0-150");
        assert_eq!(eval_band(-151), "151-600");
        assert_eq!(eval_band(600), "151-600");
        assert_eq!(eval_band(601), "601-1500");
        assert_eq!(eval_band(1500), "601-1500");
        assert_eq!(eval_band(1501), "1501+");
        assert_eq!(eval_band(29_999), "1501+");
        assert_eq!(eval_band(30_000), "mate");
        assert_eq!(eval_band(-32_767), "mate");
    }

    /// 玉位置と入玉 class（初期局面はどちらも自陣なので none）。
    #[test]
    fn nyugyoku_initial_position_is_none() {
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")
            .expect("startpos");
        assert_eq!(nyugyoku_label(&pos), "none");
    }
}
