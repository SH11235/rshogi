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

    /// 反復深化の中間 depth を 1 回の探索で捕捉し depth ごとに別ファイルへ出す（L0 用）。
    /// 例 `--capture-depths 9,12,15`。指定時 `--out PATH.jsonl` は `PATH_d9.jsonl` 等の N
    /// ファイルになり、`--depth` は最大 capture depth へ上書きされる。`--nodes 0`（depth 固定）
    /// では捕捉した中間 depth のスコアは単独固定 depth 探索と bit 一致する（反復深化の depth d
    /// までの挙動は最終 depth に依存しないため）→ N depth を探索 1 回ぶんのコストで採れる。
    /// `--nodes` でノード制限すると共有ノード予算により単独探索とズレうる。
    #[arg(long)]
    capture_depths: Option<String>,
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
/// `Ok` は出力ファイルごとの 1 行（capture-depths 時は depth 数、通常は 1）。
enum Outcome {
    Ok(Vec<String>),
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
    // capture-depths 指定時は depth ごとの N 出力、未指定時は --out 単一。探索深さは capture の
    // 最大 depth（中間 depth は反復深化の副産物として 1 回の探索で捕捉する）。
    let targets: Option<Vec<i32>> =
        cli.capture_depths.as_deref().map(parse_capture_depths).transpose()?;
    let outputs: Vec<PathBuf> = match &targets {
        Some(ds) => capture_output_paths(&cli.output, ds),
        None => vec![cli.output.clone()],
    };
    let effective_depth = match &targets {
        // parse_capture_depths が昇順を保証するので最大 depth = 末尾。
        Some(ds) => *ds.last().expect("non-empty capture depths"),
        None => cli.depth,
    };
    for out in &outputs {
        validate_paths(&cli.input, out)?;
    }
    if effective_depth <= 0 && cli.nodes == 0 {
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
        "Labeling {} ({} records) -> {} file(s) (depth={}, nodes={}, hash={}MB/worker, threads={})",
        cli.input.display(),
        total,
        outputs.len(),
        effective_depth,
        cli.nodes,
        cli.hash_mb,
        num_threads,
    );
    for out in &outputs {
        eprintln!("  out: {}", out.display());
    }

    ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
        .context("Failed to set Ctrl-C handler")?;

    let progress = ProgressBar::new(total);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    let stats =
        run_pipeline(cli, &outputs, targets.as_deref(), effective_depth, num_threads, &progress)?;

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
/// `outputs` は出力ファイル（capture-depths 時は depth 数、通常は 1）、`targets` は capture
/// する depth 列（昇順）、`depth` は実探索深さ（= capture の最大 depth）。
fn run_pipeline(
    cli: &Cli,
    outputs: &[PathBuf],
    targets: Option<&[i32]>,
    depth: i32,
    num_threads: usize,
    progress: &ProgressBar,
) -> Result<RunStats> {
    let inflight_cap = (num_threads * 4).max(num_threads + 1);

    let (token_tx, token_rx) = bounded::<()>(inflight_cap);
    for _ in 0..inflight_cap {
        token_tx.send(()).expect("prime tokens");
    }
    let (work_tx, work_rx) = unbounded::<(usize, [u8; HCPE_RECORD_SIZE])>();
    let (res_tx, res_rx) = unbounded::<(usize, Outcome)>();

    let nodes = cli.nodes;
    let hash_mb = cli.hash_mb;
    let source = cli.source.clone();
    let targets_owned: Option<Vec<i32>> = targets.map(<[i32]>::to_vec);

    let mut workers = Vec::with_capacity(num_threads);
    for worker_idx in 0..num_threads {
        let work_rx = work_rx.clone();
        let res_tx = res_tx.clone();
        let source = source.clone();
        let targets = targets_owned.clone();
        let handle = thread::Builder::new()
            .name(format!("yardstick-worker-{worker_idx}"))
            .stack_size(SEARCH_STACK_SIZE)
            .spawn(move || {
                while let Ok((seq, bytes)) = work_rx.recv() {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        break;
                    }
                    let outcome = process_record(
                        &bytes,
                        hash_mb,
                        depth,
                        nodes,
                        source.as_deref(),
                        targets.as_deref(),
                    );
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

    // collector: seq 順に並べ替えて逐次書き出す。出力は depth ごとに分かれた N 個の writer。
    let mut writers: Vec<BufWriter<File>> = outputs
        .iter()
        .map(|p| {
            File::create(p)
                .with_context(|| format!("Failed to create {}", p.display()))
                .map(BufWriter::new)
        })
        .collect::<Result<_>>()?;
    let mut next = 0usize;
    let mut buf: std::collections::BTreeMap<usize, Outcome> = std::collections::BTreeMap::new();
    let mut written = 0u64;
    let mut skipped = 0u64;
    let mut errors = 0u64;

    while let Ok((seq, outcome)) = res_rx.recv() {
        buf.insert(seq, outcome);
        while let Some(outcome) = buf.remove(&next) {
            match outcome {
                // lines[i] は writers[i]（= depth i）へ。process_record が outputs と同数の
                // 行を返す不変条件なので zip で 1 対 1 に書き出す（崩れると silent な行ズレに
                // なるため debug ビルドで検出する）。
                Outcome::Ok(lines) => {
                    debug_assert_eq!(lines.len(), writers.len());
                    for (writer, line) in writers.iter_mut().zip(&lines) {
                        writer.write_all(line.as_bytes())?;
                        writer.write_all(b"\n")?;
                    }
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
    for writer in &mut writers {
        writer.flush()?;
    }

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
/// `targets` 指定時は 1 回の探索で各 depth の中間スコアを捕捉し depth 数だけの行を返す。
fn process_record(
    bytes: &[u8; HCPE_RECORD_SIZE],
    hash_mb: usize,
    depth: i32,
    nodes: u64,
    source: Option<&str>,
    targets: Option<&[i32]>,
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

    // 出力 1 行を組み立てる（eval_label / mate_label 以外は depth 間で共有）。
    let stm_char = if stm == Color::Black { 'b' } else { 'w' };
    let make_line = |eval_label: i32, mate_label: bool| -> Result<String, String> {
        let record = ScoreRecord {
            stm: stm_char,
            wdl,
            eval_ref,
            eval_label,
            eval_band: eval_band(eval_ref),
            nyugyoku,
            in_check,
            mate_ref: eval_ref.abs() >= MATE_ABS,
            mate_label,
            source: source.map(str::to_string),
        };
        serde_json::to_string(&record).map_err(|e| format!("serialize error: {e}"))
    };

    let lines: Result<Vec<String>, String> = match targets {
        // capture mode: 1 回の探索で各 target depth の反復深化中間スコアを捕捉する。
        // 反復深化は depth 1,2,… と単調増加するので、各 target に「target 以下で最後に完了した
        // depth」のスコアが残る（target に到達すれば exact、早期終了（詰み等）なら最深 depth）。
        Some(targets) => {
            let mut captured: Vec<Option<(i32, bool)>> = vec![None; targets.len()];
            let result = {
                let cap = &mut captured;
                let on_info = |info: &SearchInfo| {
                    if info.multi_pv != 1 {
                        return;
                    }
                    for (slot, &td) in cap.iter_mut().zip(targets) {
                        if info.depth <= td {
                            *slot = Some((info.score.to_cp(), info.score.is_mate_score()));
                        }
                    }
                };
                search.go(&mut pos, limits, Some(on_info))
            };
            let fallback = (result.score.to_cp(), result.score.is_mate_score());
            captured
                .into_iter()
                .map(|c| {
                    let (eval, mate) = c.unwrap_or(fallback);
                    make_line(eval, mate)
                })
                .collect()
        }
        None => {
            let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);
            make_line(result.score.to_cp(), result.score.is_mate_score()).map(|s| vec![s])
        }
    };
    match lines {
        Ok(lines) => Outcome::Ok(lines),
        Err(e) => Outcome::Error(e),
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

/// `--capture-depths` の "9,12,15" を昇順・重複排除した正の depth 列に。
fn parse_capture_depths(s: &str) -> Result<Vec<i32>> {
    let mut depths: Vec<i32> = Vec::new();
    for tok in s.split(',') {
        let tok = tok.trim();
        if tok.is_empty() {
            continue;
        }
        let d: i32 =
            tok.parse().with_context(|| format!("invalid --capture-depths entry '{tok}'"))?;
        if d <= 0 {
            bail!("--capture-depths entries must be > 0 (got {d})");
        }
        depths.push(d);
    }
    if depths.is_empty() {
        bail!("--capture-depths is empty");
    }
    depths.sort_unstable();
    depths.dedup();
    Ok(depths)
}

/// `--out base.jsonl` と depth 列から、depth ごとの出力パス `base_d<depth>.jsonl` を作る。
/// 拡張子が無ければ末尾に `_d<depth>` を付ける。
fn capture_output_paths(base: &Path, depths: &[i32]) -> Vec<PathBuf> {
    let parent = base.parent();
    let stem = base.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = base.extension().map(|e| e.to_string_lossy().into_owned());
    depths
        .iter()
        .map(|d| {
            let name = match &ext {
                Some(ext) => format!("{stem}_d{d}.{ext}"),
                None => format!("{stem}_d{d}"),
            };
            match parent {
                Some(p) if !p.as_os_str().is_empty() => p.join(name),
                _ => PathBuf::from(name),
            }
        })
        .collect()
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

    #[test]
    fn parse_capture_depths_sorts_dedups_validates() {
        assert_eq!(parse_capture_depths("15,9,12,9").unwrap(), vec![9, 12, 15]);
        assert_eq!(parse_capture_depths(" 9 , 12 ").unwrap(), vec![9, 12]);
        assert!(parse_capture_depths("").is_err());
        assert!(parse_capture_depths("9,0").is_err());
        assert!(parse_capture_depths("9,x").is_err());
    }

    #[test]
    fn capture_output_paths_inserts_depth_suffix() {
        let p = capture_output_paths(Path::new("runs/threat.jsonl"), &[9, 12]);
        assert_eq!(
            p,
            vec![
                PathBuf::from("runs/threat_d9.jsonl"),
                PathBuf::from("runs/threat_d12.jsonl")
            ]
        );
        let p2 = capture_output_paths(Path::new("threat"), &[15]);
        assert_eq!(p2, vec![PathBuf::from("threat_d15")]);
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
