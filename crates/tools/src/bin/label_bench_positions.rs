//! ベンチ局面の ground truth ラベラー
//!
//! `extract_bench_positions` が出力する `label_bench*.jsonl` の各レコードを読み、
//! `sfen` を rshogi の alpha-beta 探索で深く評価して `eval_deep` 等を追記する。
//! 入力 JSON の全フィールドはそのまま残し、探索結果フィールドだけを足す。
//!
//! 設計上の不変条件:
//! - 局面ごとに `Search` を作り直す。`clear_tt` + `clear_histories` では time-management
//!   継続用フィールド（`best_previous_score` 等）が前 `go` の値を持ち越し、処理順で結果が
//!   変わってしまうため、完全な隔離には新規 `Search` が要る。これにより 1 局面の評価は
//!   他局面・処理順・`--threads` から独立し、同一入力なら出力が bit 一致する。
//! - 探索は 1 スレッド固定（`set_num_threads(1)`）。worker 並列は局面の振り分けに
//!   使うだけで、各局面の探索自体は決定的。
//! - 入力件数に対してピークメモリが線形に増えないよう streaming で処理する。
//!   producer がトークン制で in-flight 件数を一定上限に抑え、collector が入力順へ
//!   並べ替えて逐次書き出す（reorder buffer は in-flight 上限でバウンド）。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossbeam_channel::{bounded, unbounded};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::{Value as JsonValue, json};

use rshogi_core::nnue::{
    LayerStackBucketMode, SHOGI_PROGRESS_KP_ABS_NUM_WEIGHTS, get_layer_stack_bucket_mode,
    init_nnue, is_layer_stacks_loaded, parse_layer_stack_bucket_mode, set_fv_scale_override,
    set_layer_stack_bucket_mode, set_layer_stack_progress_kpabs_weights,
};
use rshogi_core::position::Position;
use rshogi_core::search::{LimitsType, Search, SearchInfo};
use rshogi_core::types::{Color, Value};

/// 探索用スタックサイズ（64MB）。深い探索で再帰スタックを使うため main 同等を確保する。
const SEARCH_STACK_SIZE: usize = 64 * 1024 * 1024;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[derive(Parser, Debug)]
#[command(
    name = "label_bench_positions",
    version,
    about = "ベンチ局面 jsonl を深い探索でラベル付けし eval_deep を追記する"
)]
struct Cli {
    /// 入力 jsonl（`label_bench*.jsonl` 等、各行に `sfen` を含む JSON オブジェクト）
    #[arg(long = "in")]
    input: PathBuf,

    /// 出力 jsonl（入力レコード + 探索結果フィールド）
    #[arg(long = "out")]
    output: PathBuf,

    /// NNUE モデルファイル（ground truth 評価器）
    #[arg(long)]
    nnue: PathBuf,

    /// FV_SCALE オーバーライド（0=ヘッダ自動判定、1 以上=指定値）。
    /// 評価器の native 値に合わせて明示指定すること（例: bullet v100 系は 28）。
    #[arg(long, default_value_t = 0)]
    fv_scale: i32,

    /// LayerStacks の bucket mode（例: `progress8kpabs`）。LS ビルドでは既定が
    /// progress8kpabs なので通常は指定不要。
    #[arg(long)]
    ls_bucket_mode: Option<String>,

    /// progress8kpabs 用の進行度係数ファイル（USI `LS_PROGRESS_COEFF` と同じ）。
    /// bucket mode が progress8kpabs（LS ビルドの既定）のとき必須。
    #[arg(long)]
    ls_progress_coeff: Option<PathBuf>,

    /// 探索深さ上限（0 以下=無制限）。`--nodes` と両方とも無制限は不可。
    #[arg(long, default_value_t = 25)]
    depth: i32,

    /// 探索ノード数上限（0=無制限）。深さと併用し、先に到達した方で停止する。
    /// `--depth` と両方 0 は不可。
    #[arg(long, default_value_t = 500_000)]
    nodes: u64,

    /// worker ごとの置換表サイズ（MB）。局面ごとに作り直すため過大にしない。
    #[arg(long, default_value_t = 128)]
    hash_mb: usize,

    /// worker スレッド数（0=利用可能 CPU 数）
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

/// 1 局面の処理結果。`Error` でも seq スロットを消費するので順序は崩れない。
enum Outcome {
    Ok(String),
    Error(String),
}

fn main() -> Result<()> {
    install_fatal_panic_hook();
    let cli = Cli::parse();
    run(&cli)
}

/// worker スレッドの探索パニックでプロセス全体を loud に終了させる。
///
/// 正当な局面で `search.go` はパニックしない（不正 SFEN は `set_sfen` の `Err` で弾かれる）。
/// パニックは設定/ビルド不整合等の致命バグなので、部分的に壊れた巨大出力を黙って残すより
/// 即時に非ゼロ終了する方が安全。collector が来ない seq を待ち続けるハングもプロセス終了で回避する。
fn install_fatal_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        // 101 は Rust 既定のパニック終了コード。
        std::process::exit(101);
    }));
}

fn run(cli: &Cli) -> Result<()> {
    validate_paths(&cli.input, &cli.output)?;

    // 両方 0 だと時間管理探索（時間 0）に落ちて停止条件が無くなるため弾く。
    if cli.depth <= 0 && cli.nodes == 0 {
        bail!("--depth and --nodes are both unlimited; specify at least one to bound the search");
    }

    configure_eval(cli)?;

    let num_threads = if cli.threads > 0 {
        cli.threads
    } else {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
    };

    eprintln!(
        "Labeling {} -> {} (depth={}, nodes={}, hash={}MB/worker, threads={})",
        cli.input.display(),
        cli.output.display(),
        cli.depth,
        cli.nodes,
        cli.hash_mb,
        num_threads,
    );

    ctrlc::set_handler(|| INTERRUPTED.store(true, Ordering::SeqCst))
        .context("Failed to set Ctrl-C handler")?;

    let total = count_nonempty_lines(&cli.input)?;
    let progress = ProgressBar::new(total);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );

    let stats = run_pipeline(cli, num_threads, &progress)?;

    progress.finish_with_message("Done");
    eprintln!("Wrote {} labeled records", stats.written);
    if stats.errors > 0 {
        eprintln!("Skipped {} records due to errors", stats.errors);
    }
    if INTERRUPTED.load(Ordering::SeqCst) {
        eprintln!("Interrupted: output may be truncated to the in-order prefix");
    }
    Ok(())
}

struct RunStats {
    written: u64,
    errors: u64,
}

/// producer + worker + collector のストリーミングパイプライン本体。
fn run_pipeline(cli: &Cli, num_threads: usize, progress: &ProgressBar) -> Result<RunStats> {
    // in-flight（acquire 済みで未書き出し）件数の上限。reorder buffer もこの値でバウンドする。
    // 下限を num_threads+1 にするのは、num_threads と等しいと全トークンを worker が握って
    // collector が次の seq を永久に待つデッドロックになり得るため。
    let inflight_cap = (num_threads * 4).max(num_threads + 1);

    let (token_tx, token_rx) = bounded::<()>(inflight_cap);
    for _ in 0..inflight_cap {
        token_tx.send(()).expect("prime tokens");
    }
    let (work_tx, work_rx) = unbounded::<(usize, String)>();
    let (res_tx, res_rx) = unbounded::<(usize, Outcome)>();

    let depth = cli.depth;
    let nodes = cli.nodes;
    let hash_mb = cli.hash_mb;

    let mut workers = Vec::with_capacity(num_threads);
    for worker_idx in 0..num_threads {
        let work_rx = work_rx.clone();
        let res_tx = res_tx.clone();
        let handle = thread::Builder::new()
            .name(format!("label-worker-{worker_idx}"))
            .stack_size(SEARCH_STACK_SIZE)
            .spawn(move || {
                while let Ok((seq, line)) = work_rx.recv() {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        break;
                    }
                    // パニックは install_fatal_panic_hook でプロセスごと落とす（捕捉して継続すると
                    // thread-local の汚染持ち越しや seq gap によるパイプライン deadlock を招くため）。
                    let outcome = process_line(&line, hash_mb, depth, nodes);
                    if res_tx.send((seq, outcome)).is_err() {
                        break;
                    }
                }
            })
            .context("Failed to spawn worker thread")?;
        workers.push(handle);
    }
    // worker が clone を保持しているので、main 側の元 sender は手放す。
    drop(work_rx);
    drop(res_tx);

    let input_path = cli.input.clone();
    let producer = thread::spawn(move || -> Result<()> {
        let file = File::open(&input_path)
            .with_context(|| format!("Failed to open {}", input_path.display()))?;
        let reader = BufReader::new(file);
        let mut seq = 0usize;
        for line in reader.lines() {
            if INTERRUPTED.load(Ordering::SeqCst) {
                break;
            }
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            // 書き出し時に release されるトークンを取得（in-flight 上限の backpressure）。
            if token_rx.recv().is_err() {
                break;
            }
            if work_tx.send((seq, line)).is_err() {
                break;
            }
            seq += 1;
        }
        Ok(())
    });

    let output_path = &cli.output;
    let out_file = File::create(output_path)
        .with_context(|| format!("Failed to create {}", output_path.display()))?;
    let mut writer = BufWriter::with_capacity(8 * 1024 * 1024, out_file);

    let mut next = 0usize;
    let mut buf: BTreeMap<usize, Outcome> = BTreeMap::new();
    let mut written = 0u64;
    let mut errors = 0u64;

    for (seq, outcome) in res_rx {
        buf.insert(seq, outcome);
        while let Some(out) = buf.remove(&next) {
            match out {
                Outcome::Ok(line) => {
                    writer.write_all(line.as_bytes())?;
                    writer.write_all(b"\n")?;
                    written += 1;
                }
                Outcome::Error(msg) => {
                    errors += 1;
                    eprintln!("skip record {next}: {msg}");
                }
            }
            next += 1;
            progress.inc(1);
            // 1 件書き出すごとにトークンを返し、producer の読み進みを許可する。
            let _ = token_tx.send(());
        }
    }
    writer.flush()?;

    producer.join().map_err(|_| anyhow::anyhow!("producer thread panicked"))??;
    for handle in workers {
        let _ = handle.join();
    }

    Ok(RunStats { written, errors })
}

/// 1 行の JSON を探索ラベル付きにする。元フィールドは保持し探索結果だけ追記する。
fn process_line(line: &str, hash_mb: usize, depth: i32, nodes: u64) -> Outcome {
    let mut value: JsonValue = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return Outcome::Error(format!("json parse error: {e}")),
    };
    let Some(obj) = value.as_object_mut() else {
        return Outcome::Error("record is not a JSON object".to_string());
    };
    let Some(sfen) = obj.get("sfen").and_then(JsonValue::as_str).map(str::to_string) else {
        return Outcome::Error("record has no string `sfen` field".to_string());
    };

    let mut pos = Position::new();
    if let Err(e) = pos.set_sfen(&sfen) {
        return Outcome::Error(format!("set_sfen failed: {e:?}: {sfen}"));
    }
    let stm = pos.side_to_move();

    // 局面ごとに新規 Search（不変条件はファイル冒頭参照）。1 スレッド固定で決定的。
    let mut search = Search::new(hash_mb);
    search.set_num_threads(1);

    let mut limits = LimitsType::default();
    limits.depth = depth;
    if nodes > 0 {
        limits.nodes = nodes;
    }
    limits.set_start_time();

    let result = search.go(&mut pos, limits, None::<fn(&SearchInfo)>);

    let (eval_deep, mate_deep) = black_view_label(result.score, stm);
    let label = DeepLabel {
        eval_deep,
        mate_deep,
        bestmove_deep: result.best_move.to_usi(),
        pv_deep: result.pv.iter().map(|m| m.to_usi()).collect(),
        depth_deep: result.depth,
        nodes_deep: result.nodes,
    };
    insert_deep_fields(obj, &label);

    match serde_json::to_string(&value) {
        Ok(s) => Outcome::Ok(s),
        Err(e) => Outcome::Error(format!("serialize error: {e}")),
    }
}

/// 探索ラベル。`eval_deep` は先手視点 USI cp。
struct DeepLabel {
    eval_deep: i32,
    mate_deep: Option<i32>,
    bestmove_deep: String,
    pv_deep: Vec<String>,
    depth_deep: i32,
    nodes_deep: u64,
}

/// 既存フィールドを保ったまま探索結果フィールドを追記する。
fn insert_deep_fields(obj: &mut serde_json::Map<String, JsonValue>, label: &DeepLabel) {
    obj.insert("eval_deep".to_string(), json!(label.eval_deep));
    if let Some(mate) = label.mate_deep {
        obj.insert("mate_deep".to_string(), json!(mate));
    } else {
        // 既ラベル jsonl の再ラベル時、非詰みなら旧 mate_deep を消す（他フィールドは上書き
        // されるのに mate_deep だけ前回の詰み手数が残る不整合を防ぐ）。
        obj.remove("mate_deep");
    }
    obj.insert("bestmove_deep".to_string(), json!(label.bestmove_deep));
    obj.insert("pv_deep".to_string(), json!(label.pv_deep));
    obj.insert("depth_deep".to_string(), json!(label.depth_deep));
    obj.insert("nodes_deep".to_string(), json!(label.nodes_deep));
}

/// 探索スコア（手番視点）を先手視点へ変換する。`eval_cp_black` と同じ規約に揃える。
///
/// 戻り値は (先手視点 USI cp, 詰みなら符号付き手数)。詰み手数は先手視点で、
/// 正なら先手が詰ます、負なら先手が詰まされる。
fn black_view_label(score: Value, stm: Color) -> (i32, Option<i32>) {
    let black = if stm == Color::White { -score } else { score };
    let eval = black.to_cp();
    let mate = if black.is_mate_score() {
        let ply = black.mate_ply();
        Some(if black.is_win() { ply } else { -ply })
    } else {
        None
    };
    (eval, mate)
}

/// 評価器（NNUE + LayerStacks bucket 設定）を USI エンジンと同じ手順で構成する。
///
/// 設定はすべて評価時に参照されるグローバル状態なので init_nnue 前に適用しておく。
/// progress8kpabs で係数未指定だと bucket 選択が学習時と食い違い、ラベルが静かに
/// 狂うため、エンジン同様にここで弾く（防御的すり替えはしない＝エラーで停止）。
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
        let mode = parse_layer_stack_bucket_mode(mode_str).with_context(|| {
            format!("invalid --ls-bucket-mode '{mode_str}' (expected progress8kpabs)")
        })?;
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

    // progress bucket は LayerStacks のときだけ使う。非 LS モデル (HalfKP 等) では係数不要なので、
    // USI エンジンと同じく LS ロード時のみ係数必須を課す（bucket mode の getter は値に依らず
    // Progress8KPAbs を返すため、is_layer_stacks_loaded で実ネットワークを判定する）。
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
/// USI エンジンの `LS_PROGRESS_COEFF` ハンドラと同じ検証・変換を行う。
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
    // canonicalize は hardlink（別 path・同一 inode）を検出できない。出力 create が入力を
    // truncate する事故を防ぐため、dev/ino でも同一性を弾く。
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

/// 進捗バーの分母用に非空行数を数える（全件を貯めず streaming）。
fn count_nonempty_lines(path: &Path) -> Result<u64> {
    let file = File::open(path).with_context(|| format!("Failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut count = 0u64;
    for line in reader.lines() {
        if !line?.trim().is_empty() {
            count += 1;
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_view_keeps_black_to_move_score() {
        let (eval, mate) = black_view_label(Value::new(90), Color::Black);
        assert_eq!(eval, 100); // 内部 90 ≒ 歩1枚 → USI 100cp
        assert_eq!(mate, None);
    }

    #[test]
    fn black_view_flips_white_to_move_score() {
        let (eval, mate) = black_view_label(Value::new(90), Color::White);
        assert_eq!(eval, -100);
        assert_eq!(mate, None);
    }

    #[test]
    fn black_view_mate_for_black() {
        // 先手番で先手が5手詰め
        let (_eval, mate) = black_view_label(Value::mate_in(5), Color::Black);
        assert_eq!(mate, Some(5));
    }

    #[test]
    fn black_view_mate_against_black() {
        // 先手番で先手が3手で詰まされる
        let (_eval, mate) = black_view_label(Value::mated_in(3), Color::Black);
        assert_eq!(mate, Some(-3));
    }

    #[test]
    fn black_view_white_wins_is_black_loss() {
        // 後手番で手番側（後手）が5手詰め → 先手視点は詰まされ
        let (_eval, mate) = black_view_label(Value::mate_in(5), Color::White);
        assert_eq!(mate, Some(-5));
    }

    fn sample_label(mate: Option<i32>) -> DeepLabel {
        DeepLabel {
            eval_deep: 123,
            mate_deep: mate,
            bestmove_deep: "7g7f".to_string(),
            pv_deep: vec!["7g7f".to_string(), "3c3d".to_string()],
            depth_deep: 25,
            nodes_deep: 500_000,
        }
    }

    fn augment(line: &str, label: &DeepLabel) -> String {
        let mut value: JsonValue = serde_json::from_str(line).expect("valid json");
        insert_deep_fields(value.as_object_mut().expect("object"), label);
        serde_json::to_string(&value).expect("serialize")
    }

    #[test]
    fn insert_preserves_original_fields_and_adds_deep() {
        let out = augment(r#"{"sfen":"x","ply":4,"eval_cp_black":20}"#, &sample_label(None));
        let v: JsonValue = serde_json::from_str(&out).expect("valid json");
        let obj = v.as_object().expect("object");
        assert_eq!(obj.get("sfen").and_then(JsonValue::as_str), Some("x"));
        assert_eq!(obj.get("ply").and_then(JsonValue::as_i64), Some(4));
        assert_eq!(obj.get("eval_cp_black").and_then(JsonValue::as_i64), Some(20));
        assert_eq!(obj.get("eval_deep").and_then(JsonValue::as_i64), Some(123));
        assert_eq!(obj.get("depth_deep").and_then(JsonValue::as_i64), Some(25));
        assert_eq!(obj.get("nodes_deep").and_then(JsonValue::as_u64), Some(500_000));
        assert_eq!(obj.get("bestmove_deep").and_then(JsonValue::as_str), Some("7g7f"));
        assert_eq!(obj.get("pv_deep").and_then(JsonValue::as_array).map(Vec::len), Some(2));
        // 詰みでない局面に mate_deep を生やさない
        assert!(!obj.contains_key("mate_deep"));
    }

    #[test]
    fn insert_emits_mate_field_only_when_present() {
        let out = augment(r#"{"sfen":"x"}"#, &sample_label(Some(-3)));
        let v: JsonValue = serde_json::from_str(&out).expect("valid json");
        assert_eq!(v.as_object().unwrap().get("mate_deep").and_then(JsonValue::as_i64), Some(-3));
    }

    #[test]
    fn insert_clears_stale_mate_field_on_relabel() {
        // 既に mate_deep を持つ（前回詰みラベル）レコードを非詰みで再ラベルすると消える
        let out = augment(r#"{"sfen":"x","mate_deep":5}"#, &sample_label(None));
        let v: JsonValue = serde_json::from_str(&out).expect("valid json");
        assert!(!v.as_object().unwrap().contains_key("mate_deep"));
    }

    #[test]
    fn insert_is_deterministic() {
        let line = r#"{"sfen":"x","ply":4}"#;
        let a = augment(line, &sample_label(None));
        let b = augment(line, &sample_label(None));
        assert_eq!(a, b);
    }
}
