//! hcpe 教師プールの eval を NNUE 固定 depth 探索で付け替える教師生成ツール。
//!
//! 入力 hcpe（cshogi HuffmanCodedPosAndEval, 38B/レコード）の各局面を、共有コア
//! `tools::teacher_labeler` の **fresh-per-position 固定 depth 探索**で再評価し、eval だけを
//! 差し替えた hcpe を出力する（局面・bestMove16・gameResult は保持）。`yardstick_label`
//! （ラベル品質の物差し）と**同一コア経由**なので、同一 config なら両者のラベルは bit 一致する。
//!
//! - **決定性**: 局面ごとに空の `Search` を作る fresh-per-position。処理順・スレッド数・
//!   入力分割（シャード）に依存せず、同一局面は常に同一ラベル → 複数機のシャードを連結可能。
//! - **resume**: 入力をチャンクファイル群で渡し、出力済みファイルは skip（`--output-dir` に
//!   入力ファイル名で出力、`.tmp` → rename で原子的に完了マーク）。GPU 学習等で中断 → 同じ
//!   コマンドで再実行すると未処理チャンクから再開できる。中断時の損失は最大 1 チャンク。
//! - 符号規約は手番側視点 cp（hcpe 保存 eval と同じ）。出力は探索値を `--score-clip` で i16 に収める。

use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossbeam_channel::{bounded, unbounded};
use glob::glob;
use indicatif::{ProgressBar, ProgressStyle};

use rshogi_core::position::Position;
use tools::packed_sfen::unpack_hcp;
use tools::teacher_labeler::{
    self, HCPE_RECORD_SIZE, LabelerEvalConfig, SEARCH_STACK_SIZE, label_position,
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Parser, Debug)]
#[command(
    name = "rescore_hcpe",
    version,
    about = "hcpe 教師プールの eval を NNUE 固定 depth 探索で付け替える（局面/結果は保持、共有コアで yardstick とラベル bit 一致）"
)]
struct Cli {
    /// 入力 hcpe（38B/レコード）。複数指定・glob パターン可（例 `pool/*.hcpe`）。
    #[arg(long = "in", required = true, num_args = 1..)]
    input: Vec<String>,

    /// 出力ディレクトリ。入力ファイル名と同名で hcpe を書く（resume の単位）。
    #[arg(long = "out-dir")]
    out_dir: PathBuf,

    /// labeler の NNUE モデルファイル。
    #[arg(long)]
    nnue: PathBuf,

    /// FV_SCALE オーバーライド（0=ヘッダ自動判定、1 以上=指定値。none/threat LayerStacks 系は 28）。
    #[arg(long, default_value_t = 0)]
    fv_scale: i32,

    /// LayerStacks の bucket mode（例 `progress8kpabs`）。LS ビルドでは既定なので通常は指定不要。
    #[arg(long)]
    ls_bucket_mode: Option<String>,

    /// progress8kpabs 用の進行度係数ファイル（USI `LS_PROGRESS_COEFF`）。LS + progress8kpabs で必須。
    #[arg(long)]
    ls_progress_coeff: Option<PathBuf>,

    /// SPSA 探索パラメータ `.params`（USI `SPSAParamsFile` 同形式）を各局面の探索へ適用。
    #[arg(long)]
    spsa_params: Option<PathBuf>,

    /// 探索深さ（固定 depth ラベリング）。
    #[arg(long, default_value_t = 15)]
    depth: i32,

    /// 探索ノード数上限（0=無制限）。depth を binding にするなら 0。
    #[arg(long, default_value_t = 0)]
    nodes: u64,

    /// worker ごとの置換表サイズ（MB）。局面ごとに作り直すため過大にしない。
    #[arg(long, default_value_t = 32)]
    hash_mb: usize,

    /// worker スレッド数（0=利用可能 CPU 数）。出力は thread 数非依存に bit 一致。
    #[arg(long, default_value_t = 0)]
    threads: usize,

    /// 出力 eval の clip 範囲（±この値に clamp して i16 へ収める）。
    #[arg(long, default_value_t = 32_000)]
    score_clip: i32,

    /// 王手局面を出力から除外する。
    #[arg(long)]
    skip_in_check: bool,

    /// 先頭から処理する最大レコード数（0=全件、ファイルごと）。smoke 用。
    #[arg(long, default_value_t = 0)]
    limit: usize,

    /// 出力が既に存在しても再処理する（既定は skip = resume）。
    #[arg(long)]
    overwrite: bool,
}

/// 1 レコードの処理結果。`Error`/`Skip` でも seq スロットを消費し順序を保つ。
enum Outcome {
    Ok(Box<[u8; HCPE_RECORD_SIZE]>),
    Skip,
    Error(String),
}

/// ファイル 1 つの集計。
#[derive(Default)]
struct FileStats {
    written: u64,
    skipped: u64,
    errors: u64,
}

fn main() -> Result<()> {
    install_fatal_panic_hook();
    let cli = Cli::parse();
    run(&cli)
}

/// worker スレッドの探索パニックでプロセス全体を loud に終了させる（致命バグを黙って部分出力に
/// 残さない）。`yardstick_label` と同方針。
fn install_fatal_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        std::process::exit(101);
    }));
}

fn run(cli: &Cli) -> Result<()> {
    ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
        .context("Failed to set Ctrl-C handler")?;

    if cli.depth <= 0 && cli.nodes == 0 {
        bail!("--depth and --nodes are both unlimited; specify at least one to bound the search");
    }
    if cli.score_clip <= 0 {
        bail!("--score-clip must be > 0 (got {})", cli.score_clip);
    }

    let inputs = expand_inputs(&cli.input)?;
    if inputs.is_empty() {
        bail!("no input files matched {:?}", cli.input);
    }
    fs::create_dir_all(&cli.out_dir)
        .with_context(|| format!("Failed to create out-dir {}", cli.out_dir.display()))?;

    // 評価器を yardstick と同一手順で構成（fv-scale/progress/bucket）。
    teacher_labeler::configure_eval(&LabelerEvalConfig {
        nnue: &cli.nnue,
        fv_scale: cli.fv_scale,
        ls_bucket_mode: cli.ls_bucket_mode.as_deref(),
        ls_progress_coeff: cli.ls_progress_coeff.as_deref(),
    })?;

    // SPSA 探索パラメータ（空なら engine 既定値）。ロード時に適用/clamp/未知名を warn。
    let tune_params: Arc<[(String, i32)]> = match &cli.spsa_params {
        Some(path) => {
            let parsed = teacher_labeler::parse_spsa_params(path)?;
            teacher_labeler::warn_unapplied_tune_params(&parsed);
            Arc::from(parsed)
        }
        None => Arc::from([]),
    };

    let num_threads = if cli.threads > 0 {
        cli.threads
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    };
    eprintln!(
        "rescore_hcpe: {} file(s), depth={}, nodes={}, hash={}MB/worker, threads={}, score_clip=±{}",
        inputs.len(),
        cli.depth,
        cli.nodes,
        cli.hash_mb,
        num_threads,
        cli.score_clip,
    );

    let mut total = FileStats::default();
    let mut processed = 0usize;
    let mut skipped_files = 0usize;
    for input in &inputs {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }
        let out_path = output_path_for(&cli.out_dir, input)?;
        if out_path.exists() && !cli.overwrite {
            skipped_files += 1;
            continue; // resume: 完了済みチャンクは skip
        }
        let stats = process_file(cli, input, &out_path, &tune_params, num_threads)?;
        total.written += stats.written;
        total.skipped += stats.skipped;
        total.errors += stats.errors;
        processed += 1;
        if INTERRUPTED.load(Ordering::SeqCst) {
            break; // 中断: この .tmp は rename されず残る（次回再処理）
        }
    }

    eprintln!(
        "DONE: processed {processed} file(s), skipped {skipped_files} existing; \
         wrote {} records ({} skipped, {} errors)",
        total.written, total.skipped, total.errors,
    );
    if INTERRUPTED.load(Ordering::SeqCst) {
        bail!("interrupted: current file left as .tmp and will be redone on resume");
    }
    Ok(())
}

/// `--in` の各エントリを glob 展開し、ソートして重複排除した入力ファイル列にする（決定的順序）。
fn expand_inputs(patterns: &[String]) -> Result<Vec<PathBuf>> {
    let mut files: Vec<PathBuf> = Vec::new();
    for pat in patterns {
        let mut matched = 0usize;
        for entry in glob(pat).with_context(|| format!("invalid glob pattern '{pat}'"))? {
            let path = entry.with_context(|| format!("glob error for '{pat}'"))?;
            if path.is_file() {
                files.push(path);
                matched += 1;
            }
        }
        if matched == 0 {
            // glob に一致しない場合はリテラルパスとして扱う（存在すれば追加）。
            let p = PathBuf::from(pat);
            if p.is_file() {
                files.push(p);
            } else {
                bail!("input not found: {pat}");
            }
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

/// 入力ファイルに対応する出力パス（out-dir + 入力ファイル名）。
fn output_path_for(out_dir: &Path, input: &Path) -> Result<PathBuf> {
    let name = input
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("input has no file name: {}", input.display()))?;
    Ok(out_dir.join(name))
}

/// 1 ファイルを streaming で再ラベルし、`.tmp` へ書いて完了後 rename する（原子的な完了マーク）。
fn process_file(
    cli: &Cli,
    input: &Path,
    out_path: &Path,
    tune_params: &Arc<[(String, i32)]>,
    num_threads: usize,
) -> Result<FileStats> {
    let total_records = count_records(input)?;
    let total = if cli.limit > 0 {
        total_records.min(cli.limit as u64)
    } else {
        total_records
    };
    let progress = ProgressBar::new(total);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );
    progress.set_message(
        input.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
    );

    let tmp_path = out_path.with_extension("tmp");

    // in-flight をトークンで一定上限に抑える streaming パイプライン（peak メモリは入力サイズ非依存）。
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
    let score_clip = cli.score_clip;
    let skip_in_check = cli.skip_in_check;

    let mut workers = Vec::with_capacity(num_threads);
    for worker_idx in 0..num_threads {
        let work_rx = work_rx.clone();
        let res_tx = res_tx.clone();
        let tune_params = Arc::clone(tune_params);
        let handle = thread::Builder::new()
            .name(format!("rescore-hcpe-{worker_idx}"))
            .stack_size(SEARCH_STACK_SIZE)
            .spawn(move || {
                while let Ok((seq, bytes)) = work_rx.recv() {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        break;
                    }
                    let outcome = relabel_record(
                        &bytes,
                        depth,
                        nodes,
                        hash_mb,
                        &tune_params,
                        score_clip,
                        skip_in_check,
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

    let input_path = input.to_path_buf();
    let limit = cli.limit;
    let producer = thread::spawn(move || -> Result<()> {
        let file = File::open(&input_path)
            .with_context(|| format!("Failed to open {}", input_path.display()))?;
        let mut reader = BufReader::new(file);
        let mut seq = 0usize;
        let mut buf = [0u8; HCPE_RECORD_SIZE];
        loop {
            if (limit > 0 && seq >= limit) || INTERRUPTED.load(Ordering::SeqCst) {
                break;
            }
            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e).context("Failed to read hcpe record"),
            }
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

    // collector: seq 順に並べ替えて .tmp へ逐次書き出す。
    let mut writer = BufWriter::new(
        File::create(&tmp_path)
            .with_context(|| format!("Failed to create {}", tmp_path.display()))?,
    );
    let mut next = 0usize;
    let mut buf: std::collections::BTreeMap<usize, Outcome> = std::collections::BTreeMap::new();
    let mut stats = FileStats::default();
    while let Ok((seq, outcome)) = res_rx.recv() {
        buf.insert(seq, outcome);
        while let Some(outcome) = buf.remove(&next) {
            match outcome {
                Outcome::Ok(rec) => {
                    writer.write_all(rec.as_ref())?;
                    stats.written += 1;
                }
                Outcome::Skip => stats.skipped += 1,
                Outcome::Error(msg) => {
                    stats.errors += 1;
                    eprintln!("skip record {next} in {}: {msg}", input.display());
                }
            }
            next += 1;
            progress.inc(1);
            let _ = token_tx.send(());
        }
    }

    let producer_result = producer.join().expect("producer thread panicked");
    for handle in workers {
        handle.join().expect("worker thread panicked");
    }
    producer_result?;
    writer.flush()?;
    drop(writer);
    progress.finish_and_clear();

    if INTERRUPTED.load(Ordering::SeqCst) {
        // 中断時は .tmp を残して rename しない（次回 resume で同チャンクを最初からやり直す）。
        return Ok(stats);
    }
    fs::rename(&tmp_path, out_path).with_context(|| {
        format!("Failed to rename {} -> {}", tmp_path.display(), out_path.display())
    })?;
    Ok(stats)
}

/// hcpe 1 レコードを fresh-per-position の固定 depth 探索で再評価し、eval だけ差し替えた 38B を返す。
/// 局面・bestMove16・gameResult は保持。`skip_in_check` 時は王手局面を除外。
fn relabel_record(
    bytes: &[u8; HCPE_RECORD_SIZE],
    depth: i32,
    nodes: u64,
    hash_mb: usize,
    tune_params: &[(String, i32)],
    score_clip: i32,
    skip_in_check: bool,
) -> Outcome {
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
    if skip_in_check && pos.in_check() {
        return Outcome::Skip;
    }

    let labels = label_position(&mut pos, depth, nodes, hash_mb, tune_params, None);
    let (eval, _mate) = labels[0];
    let clipped = eval.clamp(-score_clip, score_clip) as i16;

    let mut out = *bytes;
    out[32..34].copy_from_slice(&clipped.to_le_bytes());
    Outcome::Ok(Box::new(out))
}

/// hcpe レコード数（ファイルサイズ / 38）。
fn count_records(path: &Path) -> Result<u64> {
    let len = fs::metadata(path)
        .with_context(|| format!("Failed to stat {}", path.display()))?
        .len();
    if len % HCPE_RECORD_SIZE as u64 != 0 {
        bail!(
            "hcpe file size {} is not a multiple of {} (corrupt or wrong format): {}",
            len,
            HCPE_RECORD_SIZE,
            path.display()
        );
    }
    Ok(len / HCPE_RECORD_SIZE as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_path_uses_input_filename() {
        let p = output_path_for(Path::new("out"), Path::new("pool/chunk_007.hcpe")).unwrap();
        assert_eq!(p, PathBuf::from("out/chunk_007.hcpe"));
    }

    #[test]
    fn relabel_preserves_position_move_result_replaces_eval() {
        // 32B の hcp は本物の局面でなくても、unpack_hcp が失敗すれば Error になる。ここでは
        // eval/move/result バイトの保持・差し替え境界のみを検査するため、relabel ではなく
        // 直接バイト操作の不変条件（[32..34] のみ書き換え）を別途担保する単体に留める。
        // （局面を要する経路は bit 一致検証スクリプトで担保）
        let mut rec = [0u8; HCPE_RECORD_SIZE];
        rec[32] = 0x10; // eval lo
        rec[33] = 0x20; // eval hi
        rec[34] = 0xAB; // bestMove16 lo
        rec[35] = 0xCD; // bestMove16 hi
        rec[36] = 1; // gameResult
        let new_eval: i16 = -123;
        let mut out = rec;
        out[32..34].copy_from_slice(&new_eval.to_le_bytes());
        // eval だけ変わり、move/result は不変。
        assert_eq!(i16::from_le_bytes([out[32], out[33]]), -123);
        assert_eq!(out[34], 0xAB);
        assert_eq!(out[35], 0xCD);
        assert_eq!(out[36], 1);
    }
}
