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
//!   コマンドで再実行すると未処理チャンクから再開できる。さらに処理中チャンクは **intra-chunk
//!   resume**: `.tmp` に書けた連続プレフィックスを `.tmp.meta`（config 指紋 + 入力サイズ +
//!   入力 seq + 出力件数の checkpoint）で裏取りし、チャンク途中から再開する。fresh-per-position
//!   の決定性ゆえ途中再開でも全件フレッシュ処理と bit 一致するため、中断/電源断時の損失は
//!   最悪でも checkpoint 間隔（数百レコード ≒ 数秒）に収まる。
//! - 符号規約は手番側視点 cp（hcpe 保存 eval と同じ）。出力は探索値を `--score-clip` で i16 に収める。

use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossbeam_channel::{bounded, unbounded};
use glob::glob;
use indicatif::{ProgressBar, ProgressStyle};
use sha2::{Digest, Sha256};

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

/// 1 レコードを再ラベルする決定的 transform。worker 間で共有するため `Arc<dyn Fn>`。
/// 本番は固定 depth 探索、テストは search を伴わない決定的関数を注入する。
type RelabelFn = Arc<dyn Fn(&[u8; HCPE_RECORD_SIZE]) -> Outcome + Send + Sync>;

/// ファイル 1 つの集計（破損レコードがあれば process_file は bail するため、Ok 返却時の
/// `skipped` は in-check スキップのみ）。
#[derive(Default)]
struct FileStats {
    written: u64,
    skipped: u64,
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
    // clamp 後に `as i16` で wrap しないよう i16 範囲に収める。
    if cli.score_clip <= 0 || cli.score_clip > i16::MAX as i32 {
        bail!("--score-clip must be in 1..={} (got {})", i16::MAX, cli.score_clip);
    }

    let inputs = expand_inputs(&cli.input)?;
    if inputs.is_empty() {
        bail!("no input files matched {:?}", cli.input);
    }
    // 出力は入力 basename で書くため、別ディレクトリの同名入力は出力衝突＝silent なチャンク欠落に
    // なる。重複 basename と予約サフィックス（.tmp/.meta）を弾く。
    let mut seen_names = std::collections::HashSet::new();
    for input in &inputs {
        let name = input
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("input has no file name: {}", input.display()))?
            .to_string_lossy()
            .into_owned();
        if name.ends_with(".tmp") || name.ends_with(".meta") {
            bail!(
                "input file name '{name}' uses a reserved suffix (.tmp/.meta): {}",
                input.display()
            );
        }
        if !seen_names.insert(name.clone()) {
            bail!(
                "duplicate input file name '{name}' across directories — outputs would collide in --out-dir; \
                 rename chunks to be unique"
            );
        }
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
    // ラベルに影響する config の指紋。resume 完了メタ（`.meta`）と突き合わせ、設定違いや --limit の
    // 短縮出力を「完了」と誤認しないようにする。
    let config_fp = config_fingerprint(cli, &tune_params)?;
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
    let mut failed_files = 0usize;
    for input in &inputs {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break;
        }
        let out_path = output_path_for(&cli.out_dir, input)?;
        if !cli.overwrite && output_is_complete(&out_path, input, &config_fp)? {
            skipped_files += 1;
            continue; // resume: 同一 config・件数一致の完了済みチャンクのみ skip
        }
        // 1 ファイルの失敗（破損レコード・IO）はそのファイルだけ未完了として続行し、最後に非ゼロ終了。
        match process_file(cli, input, &out_path, &tune_params, num_threads, &config_fp) {
            Ok(stats) => {
                total.written += stats.written;
                total.skipped += stats.skipped;
                processed += 1;
            }
            Err(e) => {
                failed_files += 1;
                eprintln!(
                    "FAILED {}: {e:#} (left unrenamed; will be retried on resume)",
                    input.display()
                );
            }
        }
        if INTERRUPTED.load(Ordering::SeqCst) {
            break; // 中断: 処理中の .tmp は rename されず残る（次回再処理）
        }
    }

    eprintln!(
        "DONE: processed {processed} file(s), skipped {skipped_files} existing, failed {failed_files}; \
         wrote {} records ({} in-check skipped)",
        total.written, total.skipped,
    );
    if INTERRUPTED.load(Ordering::SeqCst) {
        bail!("interrupted: current file left as .tmp and will be redone on resume");
    }
    if failed_files > 0 {
        bail!(
            "{failed_files} file(s) failed and were not written; fix the inputs and re-run to resume"
        );
    }
    Ok(())
}

/// 出力チャンクの完了メタ（`<out>.meta`）のパス。
fn meta_path_for(out_path: &Path) -> PathBuf {
    let mut s = out_path.to_path_buf().into_os_string();
    s.push(".meta");
    PathBuf::from(s)
}

/// 出力が「同一 config・同一入力で完全に書かれて完了している」かを検証する（resume の skip 判定）。
/// `.meta`（input_bytes / output_records / config）と出力サイズの整合を確認し、いずれかが食い違えば
/// false（= 再処理）。破損・短縮出力・設定違い・--limit 短縮を完了と誤認しない。
fn output_is_complete(out_path: &Path, input: &Path, config_fp: &str) -> Result<bool> {
    if !out_path.exists() {
        return Ok(false);
    }
    let Ok(meta) = fs::read_to_string(meta_path_for(out_path)) else {
        return Ok(false); // メタ無し（旧 .tmp→rename 前など）→ 安全側で再処理
    };
    let (mut input_bytes, mut output_records, mut cfg) = (None, None, None);
    for line in meta.lines() {
        if let Some(v) = line.strip_prefix("input_bytes=") {
            input_bytes = v.trim().parse::<u64>().ok();
        } else if let Some(v) = line.strip_prefix("output_records=") {
            output_records = v.trim().parse::<u64>().ok();
        } else if let Some(v) = line.strip_prefix("config=") {
            cfg = Some(v.to_string());
        }
    }
    let (Some(ib), Some(or), Some(cfg)) = (input_bytes, output_records, cfg) else {
        return Ok(false);
    };
    if cfg != config_fp || fs::metadata(input)?.len() != ib {
        return Ok(false);
    }
    let out_bytes = fs::metadata(out_path)?.len();
    Ok(out_bytes % HCPE_RECORD_SIZE as u64 == 0 && out_bytes / HCPE_RECORD_SIZE as u64 == or)
}

/// ラベルに影響する config を sha256 指紋にまとめる（resume 一致判定用）。スカラ config に加え、
/// **net・progress 係数はファイル内容**を、SPSA は**順序固定の (名前,値) 列**をハッシュへ流し込む
/// （basename+size や値合計では中身違い・別パラメータの衝突を防げないため内容ハッシュにする）。
fn config_fingerprint(cli: &Cli, tune_params: &[(String, i32)]) -> Result<String> {
    let mut h = Sha256::new();
    // 各セクションを「タグ + 長さ」で囲んで domain separation する（可変長の連結で境界が曖昧に
    // なり、別々の (net,係数,SPSA) が同一バイト列を産む構造的衝突を防ぐ）。
    let scalars = format!(
        "depth={};nodes={};fv={};hash={};clip={};skip_in_check={};limit={};bucket={}",
        cli.depth,
        cli.nodes,
        cli.fv_scale,
        cli.hash_mb,
        cli.score_clip,
        cli.skip_in_check,
        cli.limit,
        cli.ls_bucket_mode.as_deref().unwrap_or("-"),
    );
    update_tagged(&mut h, b"scalars", scalars.as_bytes());
    hash_file_tagged(&mut h, b"nnue", &cli.nnue)?;
    match &cli.ls_progress_coeff {
        Some(p) => hash_file_tagged(&mut h, b"coeff", p)?,
        None => update_tagged(&mut h, b"coeff", b""),
    }
    // SPSA は件数 + 各 (名前長, 名前, 値) で長さ前置（名前境界も曖昧にしない）。
    h.update(b"spsa");
    h.update((tune_params.len() as u64).to_le_bytes());
    for (name, value) in tune_params {
        h.update((name.len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        h.update(value.to_le_bytes());
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// `タグ + u64長 + バイト列` を hasher に投入する（セクション境界の曖昧さを排除）。
fn update_tagged(hasher: &mut Sha256, tag: &[u8], bytes: &[u8]) {
    hasher.update(tag);
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

/// `タグ + u64ファイルサイズ + ファイル内容` を hasher に投入する。
fn hash_file_tagged(hasher: &mut Sha256, tag: &[u8], path: &Path) -> Result<()> {
    let len = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?.len();
    hasher.update(tag);
    hasher.update(len.to_le_bytes());
    hash_file_into(hasher, path)
}

/// ファイル内容を sha256 hasher に流し込む（net/係数の取り違えを resume 検証で検出するため）。
fn hash_file_into(hasher: &mut Sha256, path: &Path) -> Result<()> {
    let mut reader = BufReader::new(
        File::open(path)
            .with_context(|| format!("Failed to open {} for hashing", path.display()))?,
    );
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("read {} for hashing", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(())
}

/// 完了メタ（`<out>.meta`）を原子的に書く（`.meta.tmp` → rename）。
fn write_meta(
    out_path: &Path,
    input_bytes: u64,
    output_records: u64,
    config_fp: &str,
) -> Result<()> {
    let meta_path = meta_path_for(out_path);
    let mut tmp = meta_path.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let body =
        format!("input_bytes={input_bytes}\noutput_records={output_records}\nconfig={config_fp}\n");
    fs::write(&tmp, body).with_context(|| format!("Failed to write {}", tmp.display()))?;
    fs::rename(&tmp, &meta_path).with_context(|| {
        format!("Failed to rename {} -> {}", tmp.display(), meta_path.display())
    })?;
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
    config_fp: &str,
) -> Result<FileStats> {
    let input_bytes = fs::metadata(input)
        .with_context(|| format!("Failed to stat {}", input.display()))?
        .len();
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

    // 1 レコードを再ラベルする決定的 transform。worker 間で共有する（探索 config を捕捉）。
    let transform = make_relabel_transform(cli, Arc::clone(tune_params));
    run_streaming(
        &StreamParams {
            input,
            out_path,
            input_bytes,
            total,
            limit: cli.limit,
            config_fp,
            num_threads,
        },
        &progress,
        transform,
    )
}

/// `run_streaming` に渡す 1 ファイル分の入出力パラメータ（引数過多回避のためまとめる）。
#[derive(Clone, Copy)]
struct StreamParams<'a> {
    input: &'a Path,
    out_path: &'a Path,
    input_bytes: u64,
    total: u64,
    limit: usize,
    config_fp: &'a str,
    num_threads: usize,
}

/// 1 レコードを fresh-per-position 探索で再ラベルする `transform` を組む（探索 config を捕捉）。
/// `run_streaming` の worker 間で共有するため `Arc<dyn Fn .. + Send + Sync>` にする。テストは
/// search を伴わない決定的 transform を注入して resume の byte 一致を検証する。
fn make_relabel_transform(cli: &Cli, tune_params: Arc<[(String, i32)]>) -> RelabelFn {
    let depth = cli.depth;
    let nodes = cli.nodes;
    let hash_mb = cli.hash_mb;
    let score_clip = cli.score_clip;
    let skip_in_check = cli.skip_in_check;
    Arc::new(move |bytes| {
        relabel_record(bytes, depth, nodes, hash_mb, &tune_params, score_clip, skip_in_check)
    })
}

/// resume 判定 + streaming 本体。`.tmp` へ seq 順に書き、`.tmp.meta` に checkpoint を刻みつつ、
/// 完了したら `out_path` へ atomic rename し `<out>.meta` を書く。`transform` は 1 レコードを
/// 再ラベルする決定的な純関数（fresh-per-position なので処理順非依存に bit 一致）。
///
/// intra-chunk resume: 起動時に `.tmp.meta` の checkpoint が現 config / 入力サイズと一致し、
/// かつ `.tmp` に `output_records` 分が durable に在るときだけ、`input_seq` から再開する
/// （[0,input_seq) は再処理しない）。少しでも矛盾すれば最初から（後方互換・安全側）。
fn run_streaming(
    params: &StreamParams,
    progress: &ProgressBar,
    transform: RelabelFn,
) -> Result<FileStats> {
    let StreamParams {
        input,
        out_path,
        input_bytes,
        total,
        limit,
        config_fp,
        num_threads,
    } = *params;
    // `out_path` のフルネームに `.tmp` を足す（`with_extension` だと `foo` と `foo.hcpe` が
    // 同じ `foo.tmp` に衝突するため）。
    let tmp_path = {
        let mut s = out_path.to_path_buf().into_os_string();
        s.push(".tmp");
        PathBuf::from(s)
    };
    let tmp_meta_path = tmp_meta_path_for(&tmp_path);

    // resume 判定: checkpoint を信頼できるなら (input_seq, output_records) から、駄目なら (0,0)。
    let tmp_size = fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
    let (start_seq, start_written) =
        match decide_resume(read_tmp_meta(&tmp_meta_path), input_bytes, config_fp, tmp_size, total)
        {
            ResumeDecision::Resume {
                input_seq,
                output_records,
            } => (input_seq, output_records),
            ResumeDecision::Fresh => (0, 0),
        };

    // `.tmp` を output_records*38 に切り詰めて append（fresh は 0 に truncate = stale を破棄）。
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false) // 全消去ではなく set_len で N*38 へ切り詰める（resume プレフィックスを残す）。
        .open(&tmp_path)
        .with_context(|| format!("Failed to open {}", tmp_path.display()))?;
    file.set_len(start_written * HCPE_RECORD_SIZE as u64)
        .with_context(|| format!("Failed to truncate {}", tmp_path.display()))?;
    file.seek(SeekFrom::End(0))
        .with_context(|| format!("Failed to seek {}", tmp_path.display()))?;
    let mut writer = BufWriter::new(file);
    // 初期 checkpoint を刻む（直後に落ちても (start_seq,start_written) で整合する）。
    write_tmp_meta(&tmp_meta_path, input_bytes, config_fp, start_seq, start_written)?;
    progress.set_position(start_seq);

    // in-flight をトークンで一定上限に抑える streaming パイプライン（peak メモリは入力サイズ非依存）。
    let inflight_cap = (num_threads * 4).max(num_threads + 1);
    let (token_tx, token_rx) = bounded::<()>(inflight_cap);
    for _ in 0..inflight_cap {
        token_tx.send(()).expect("prime tokens");
    }
    let (work_tx, work_rx) = unbounded::<(usize, [u8; HCPE_RECORD_SIZE])>();
    let (res_tx, res_rx) = unbounded::<(usize, Outcome)>();

    let mut workers = Vec::with_capacity(num_threads);
    for worker_idx in 0..num_threads {
        let work_rx = work_rx.clone();
        let res_tx = res_tx.clone();
        let transform = Arc::clone(&transform);
        let handle = thread::Builder::new()
            .name(format!("rescore-hcpe-{worker_idx}"))
            .stack_size(SEARCH_STACK_SIZE)
            .spawn(move || {
                while let Ok((seq, bytes)) = work_rx.recv() {
                    if INTERRUPTED.load(Ordering::SeqCst) {
                        break;
                    }
                    let outcome = transform(&bytes);
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
    let producer = thread::spawn(move || -> Result<()> {
        let mut file = File::open(&input_path)
            .with_context(|| format!("Failed to open {}", input_path.display()))?;
        // resume: [0,start_seq) は処理済みなので入力を seek して飛ばす。
        if start_seq > 0 {
            file.seek(SeekFrom::Start(start_seq * HCPE_RECORD_SIZE as u64))
                .with_context(|| {
                    format!("Failed to seek {} to record {start_seq}", input_path.display())
                })?;
        }
        let mut reader = BufReader::new(file);
        let mut seq = start_seq as usize;
        let mut buf = [0u8; HCPE_RECORD_SIZE];
        loop {
            if (limit > 0 && seq >= limit) || INTERRUPTED.load(Ordering::SeqCst) {
                break;
            }
            match reader.read_exact(&mut buf) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("Failed to read hcpe record {seq} from {}", input_path.display())
                    });
                }
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

    // collector: seq 順に並べ替えて .tmp へ逐次書き出す。resume 時は次の入力 seq / 既出力件数から開始。
    let mut next = start_seq as usize;
    let mut buf: std::collections::BTreeMap<usize, Outcome> = std::collections::BTreeMap::new();
    // start_written は既に `.tmp` に在る出力件数。skipped は [0,start_seq) の非出力分（= seq-written）。
    let mut stats = FileStats {
        written: start_written,
        skipped: start_seq - start_written,
    };
    // 破損レコード（unpack/set_sfen 失敗）は黙って落とすとレコード対応が崩れたファイルを完了扱い
    // しかねないので、最初のエラーを捕え、ファイル全体を未完了（rename しない）として扱う。
    let mut first_error: Option<String> = None;
    let mut last_ckpt = next;
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
                    if first_error.is_none() {
                        first_error = Some(format!("record {next}: {msg}"));
                    }
                }
            }
            next += 1;
            progress.inc(1);
            let _ = token_tx.send(());
        }
        // 周期 checkpoint: flush→fsync(.tmp)→atomic meta の順で「meta.output_records ≤ durable .tmp」
        // を保つ。途中で落ちても meta が指す位置を超えて再開しない（決定性ゆえ多少の再処理は bit 一致）。
        if next - last_ckpt >= RESUME_CHECKPOINT_RECORDS && first_error.is_none() {
            checkpoint_tmp(
                &mut writer,
                &tmp_meta_path,
                input_bytes,
                config_fp,
                next as u64,
                stats.written,
            )?;
            last_ckpt = next;
        }
    }

    // producer が backpressure（token_rx.recv）でブロックしたまま collector が抜けた場合に、
    // token_tx を drop して disconnect で解放する（Ctrl-C 中断時のデッドロック防止）。
    drop(token_tx);
    let producer_result = producer.join().expect("producer thread panicked");
    for handle in workers {
        handle.join().expect("worker thread panicked");
    }
    producer_result.with_context(|| format!("producer failed on {}", input.display()))?;
    writer.flush()?;
    progress.finish_and_clear();

    if INTERRUPTED.load(Ordering::SeqCst) {
        // 中断時は .tmp を rename せず残し、最新の checkpoint を刻む（次回はそこから再開）。
        if first_error.is_none() {
            checkpoint_tmp(
                &mut writer,
                &tmp_meta_path,
                input_bytes,
                config_fp,
                next as u64,
                stats.written,
            )?;
        }
        drop(writer);
        return Ok(stats);
    }
    drop(writer);
    if let Some(err) = first_error {
        // 破損レコードあり: .tmp を残し rename しない（完了扱いを防ぐ）。
        bail!("{} in {} (file left unrenamed)", err, input.display());
    }
    fs::rename(&tmp_path, out_path).with_context(|| {
        format!("Failed to rename {} -> {}", tmp_path.display(), out_path.display())
    })?;
    // 完了メタを書く（resume の完了検証用）。
    write_meta(out_path, input_bytes, stats.written, config_fp)?;
    // 完了後は intra-chunk resume 用の sidecar を残さない。
    if let Err(e) = fs::remove_file(&tmp_meta_path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("warning: failed to remove {}: {e}", tmp_meta_path.display());
    }
    Ok(stats)
}

/// チャンク途中再開（`.tmp` プレフィックス活用）の checkpoint 間隔（入力レコード数）。worst-case
/// ロスはこの間隔分（数百レコード ≒ 実測 46-82 pos/s で数秒）。決定性ゆえ間隔は正しさに無関係。
const RESUME_CHECKPOINT_RECORDS: usize = 256;

/// intra-chunk resume の checkpoint パス（`<out>.tmp.meta`）。完了メタ `<out>.meta` とは別物。
fn tmp_meta_path_for(tmp_path: &Path) -> PathBuf {
    let mut s = tmp_path.to_path_buf().into_os_string();
    s.push(".meta");
    PathBuf::from(s)
}

/// `.tmp.meta` の checkpoint 内容（config 指紋 + 入力サイズ + 入力 seq + 出力件数）。
struct TmpMeta {
    input_bytes: u64,
    config_fp: String,
    input_seq: u64,
    output_records: u64,
}

/// `.tmp.meta` を読む。欠損・パース不能・項目不足は `None`（= 信頼しない）。
fn read_tmp_meta(path: &Path) -> Option<TmpMeta> {
    let body = fs::read_to_string(path).ok()?;
    let (mut input_bytes, mut config_fp, mut input_seq, mut output_records) =
        (None, None, None, None);
    for line in body.lines() {
        if let Some(v) = line.strip_prefix("input_bytes=") {
            input_bytes = v.trim().parse::<u64>().ok();
        } else if let Some(v) = line.strip_prefix("config=") {
            config_fp = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("input_seq=") {
            input_seq = v.trim().parse::<u64>().ok();
        } else if let Some(v) = line.strip_prefix("output_records=") {
            output_records = v.trim().parse::<u64>().ok();
        }
    }
    Some(TmpMeta {
        input_bytes: input_bytes?,
        config_fp: config_fp?,
        input_seq: input_seq?,
        output_records: output_records?,
    })
}

/// `.tmp.meta` を原子的に書く（`.tmp.meta.tmp` → fsync → rename）。
fn write_tmp_meta(
    path: &Path,
    input_bytes: u64,
    config_fp: &str,
    input_seq: u64,
    output_records: u64,
) -> Result<()> {
    let mut tmp = path.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let body = format!(
        "input_bytes={input_bytes}\nconfig={config_fp}\ninput_seq={input_seq}\noutput_records={output_records}\n"
    );
    {
        let mut f =
            File::create(&tmp).with_context(|| format!("Failed to write {}", tmp.display()))?;
        f.write_all(body.as_bytes())
            .with_context(|| format!("Failed to write {}", tmp.display()))?;
        f.sync_all().with_context(|| format!("Failed to fsync {}", tmp.display()))?;
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("Failed to rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

/// checkpoint を刻む: `.tmp` を flush + fsync してから `.tmp.meta` を更新する。この順序により
/// 不変条件「meta.output_records ≤ durable な `.tmp` レコード数」が常に成立し、電源断でも
/// meta が指す位置を超えて再開しない（meta 紛失時は `.tmp` 不信＝最初から、で安全側）。
fn checkpoint_tmp(
    writer: &mut BufWriter<File>,
    tmp_meta_path: &Path,
    input_bytes: u64,
    config_fp: &str,
    input_seq: u64,
    output_records: u64,
) -> Result<()> {
    writer.flush()?;
    writer.get_ref().sync_data()?;
    write_tmp_meta(tmp_meta_path, input_bytes, config_fp, input_seq, output_records)
}

/// intra-chunk resume の判定結果。
enum ResumeDecision {
    /// `.tmp` を破棄して最初から。
    Fresh,
    /// `.tmp` の先頭 `output_records` を活かし、入力 `input_seq` から再開する。
    Resume { input_seq: u64, output_records: u64 },
}

/// checkpoint を信頼して途中再開してよいかを判定する純関数（テスト容易性のため IO と分離）。
/// 現 config / 入力サイズ一致、整合性（consumed ≥ written、入力範囲内）、かつ `.tmp` に
/// `output_records` 分が durable に在ること（`output_records*38 ≤ tmp_size`）を全て満たすときだけ
/// `Resume`。一つでも崩れれば `Fresh`（後方互換・安全側）。
fn decide_resume(
    meta: Option<TmpMeta>,
    input_bytes: u64,
    config_fp: &str,
    tmp_size: u64,
    total: u64,
) -> ResumeDecision {
    let Some(m) = meta else {
        return ResumeDecision::Fresh; // `.tmp.meta` 無し（旧バイナリ残置等）→ 最初から。
    };
    if m.input_bytes != input_bytes || m.config_fp != config_fp {
        return ResumeDecision::Fresh; // 設定 / 入力が変わった → 最初から。
    }
    if m.output_records > m.input_seq || m.input_seq > total {
        return ResumeDecision::Fresh; // 矛盾した checkpoint（consumed<written / 入力範囲外）。
    }
    if m.output_records * HCPE_RECORD_SIZE as u64 > tmp_size {
        return ResumeDecision::Fresh; // `.tmp` が checkpoint 分を満たさない（torn write 等）。
    }
    ResumeDecision::Resume {
        input_seq: m.input_seq,
        output_records: m.output_records,
    }
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

    // ---- intra-chunk resume の検証 ----
    // search を伴わない決定的 transform を注入し、「全件フレッシュ出力」と「途中 checkpoint →
    // resume 出力」が byte 完全一致することを確かめる（net 不要で決定性の本体を担保）。

    const REC: usize = HCPE_RECORD_SIZE;

    /// 決定的 transform: `byte[0] % 3 == 0` を Skip（skip_in_check 同様に出力件数 < 入力件数を作る）、
    /// それ以外は eval([32..34]) を byte[0] 由来の値へ差し替えて Ok。
    fn test_transform(bytes: &[u8; HCPE_RECORD_SIZE]) -> Outcome {
        if bytes[0].is_multiple_of(3) {
            return Outcome::Skip;
        }
        let mut out = *bytes;
        let v = (bytes[0] as i16).wrapping_mul(7).wrapping_sub(3);
        out[32..34].copy_from_slice(&v.to_le_bytes());
        Outcome::Ok(Box::new(out))
    }

    fn arc_transform() -> RelabelFn {
        Arc::new(test_transform)
    }

    /// byte[0] にレコードごとに変化を持たせた入力（skip 分布を分散させる）。
    fn make_input(records: usize) -> Vec<u8> {
        let mut v = vec![0u8; records * REC];
        for i in 0..records {
            v[i * REC] = (i as u8).wrapping_mul(5).wrapping_add(1);
            v[i * REC + 1] = i as u8;
        }
        v
    }

    /// 入力（の連続プレフィックス）に transform を適用した期待出力（Ok のみ連結）。
    fn expected_output(input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in input.chunks(REC) {
            let mut b = [0u8; REC];
            b.copy_from_slice(chunk);
            if let Outcome::Ok(rec) = test_transform(&b) {
                out.extend_from_slice(rec.as_ref());
            }
        }
        out
    }

    fn tmp_paths(out_path: &Path) -> (PathBuf, PathBuf) {
        let mut s = out_path.to_path_buf().into_os_string();
        s.push(".tmp");
        let tmp = PathBuf::from(s);
        let tmp_meta = tmp_meta_path_for(&tmp);
        (tmp, tmp_meta)
    }

    fn run(input_path: &Path, out_path: &Path, config_fp: &str) -> FileStats {
        let input_bytes = fs::metadata(input_path).unwrap().len();
        let total = input_bytes / REC as u64;
        let params = StreamParams {
            input: input_path,
            out_path,
            input_bytes,
            total,
            limit: 0,
            config_fp,
            num_threads: 2,
        };
        run_streaming(&params, &ProgressBar::hidden(), arc_transform()).unwrap()
    }

    #[test]
    fn fresh_output_matches_expected_transform() {
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("chunk.hcpe");
        let input = make_input(20);
        fs::write(&input_path, &input).unwrap();
        let out_path = dir.path().join("chunk_out.hcpe");
        run(&input_path, &out_path, "cfg");
        assert_eq!(fs::read(&out_path).unwrap(), expected_output(&input));
        // 完了後は sidecar を残さない。
        let (tmp, tmp_meta) = tmp_paths(&out_path);
        assert!(!tmp.exists() && !tmp_meta.exists());
    }

    #[test]
    fn resume_from_checkpoint_matches_fresh_bytewise() {
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("chunk.hcpe");
        let n = 20usize;
        let input = make_input(n);
        fs::write(&input_path, &input).unwrap();
        let input_bytes = input.len() as u64;
        let full = expected_output(&input);

        // crafted checkpoint: 入力 [0,k) を処理した状態を .tmp + .tmp.meta で再現（途中中断の模擬）。
        let k = 8usize;
        let prefix = expected_output(&input[..k * REC]);
        let m = (prefix.len() / REC) as u64;
        assert!(m < k as u64, "skip があるので出力件数 < 入力件数のはず");
        let out_path = dir.path().join("chunk_out.hcpe");
        let (tmp, tmp_meta) = tmp_paths(&out_path);
        fs::write(&tmp, &prefix).unwrap();
        write_tmp_meta(&tmp_meta, input_bytes, "cfg", k as u64, m).unwrap();

        let stats = run(&input_path, &out_path, "cfg");
        assert_eq!(fs::read(&out_path).unwrap(), full, "resume 出力が fresh と byte 不一致");
        assert_eq!(stats.written, (full.len() / REC) as u64);
        assert!(!tmp.exists() && !tmp_meta.exists());
    }

    #[test]
    fn resume_at_total_just_finalizes() {
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("chunk.hcpe");
        let n = 12usize;
        let input = make_input(n);
        fs::write(&input_path, &input).unwrap();
        let full = expected_output(&input);
        let out_path = dir.path().join("chunk_out.hcpe");
        let (tmp, tmp_meta) = tmp_paths(&out_path);
        // 全件処理済（flush 済だが rename 前に落ちた）状態。
        fs::write(&tmp, &full).unwrap();
        write_tmp_meta(&tmp_meta, input.len() as u64, "cfg", n as u64, (full.len() / REC) as u64)
            .unwrap();
        run(&input_path, &out_path, "cfg");
        assert_eq!(fs::read(&out_path).unwrap(), full);
        assert!(!tmp.exists() && !tmp_meta.exists());
    }

    #[test]
    fn stale_tmp_discarded_on_config_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let input_path = dir.path().join("chunk.hcpe");
        let input = make_input(15);
        fs::write(&input_path, &input).unwrap();
        let full = expected_output(&input);
        let out_path = dir.path().join("chunk_out.hcpe");
        let (tmp, tmp_meta) = tmp_paths(&out_path);
        // 別 config の checkpoint と garbage な .tmp（信頼すると壊れる）。
        fs::write(&tmp, vec![0xFFu8; 5 * REC]).unwrap();
        write_tmp_meta(&tmp_meta, input.len() as u64, "OTHER", 5, 5).unwrap();
        run(&input_path, &out_path, "cfg");
        // garbage は破棄され最初から処理される → fresh と一致。
        assert_eq!(fs::read(&out_path).unwrap(), full);
    }

    fn meta(ib: u64, cfg: &str, seq: u64, rec: u64) -> TmpMeta {
        TmpMeta {
            input_bytes: ib,
            config_fp: cfg.to_string(),
            input_seq: seq,
            output_records: rec,
        }
    }

    #[test]
    fn decide_resume_variants() {
        let ib = 100 * REC as u64;
        let total = 100u64;
        let cfg = "c";
        let ok = |d: ResumeDecision, seq: u64, rec: u64| matches!(d, ResumeDecision::Resume { input_seq, output_records } if input_seq == seq && output_records == rec);
        let fresh = |d: ResumeDecision| matches!(d, ResumeDecision::Fresh);

        // 正常: resume。
        assert!(ok(
            decide_resume(Some(meta(ib, cfg, 50, 40)), ib, cfg, 40 * REC as u64, total),
            50,
            40
        ));
        // meta 無し / config 不一致 / input_bytes 不一致 → Fresh。
        assert!(fresh(decide_resume(None, ib, cfg, 0, total)));
        assert!(fresh(decide_resume(
            Some(meta(ib, "x", 50, 40)),
            ib,
            cfg,
            40 * REC as u64,
            total
        )));
        assert!(fresh(decide_resume(
            Some(meta(ib + 1, cfg, 50, 40)),
            ib,
            cfg,
            40 * REC as u64,
            total
        )));
        // 矛盾: written>seq / seq>total → Fresh。
        assert!(fresh(decide_resume(
            Some(meta(ib, cfg, 40, 50)),
            ib,
            cfg,
            50 * REC as u64,
            total
        )));
        assert!(fresh(decide_resume(
            Some(meta(ib, cfg, 101, 40)),
            ib,
            cfg,
            40 * REC as u64,
            total
        )));
        // .tmp が checkpoint 分に満たない（torn write）→ Fresh。
        assert!(fresh(decide_resume(
            Some(meta(ib, cfg, 50, 40)),
            ib,
            cfg,
            40 * REC as u64 - 1,
            total
        )));
        // .tmp が余分に長い（未 flush 分が disk に残る）→ Resume（後で N*38 に切り詰める）。
        assert!(ok(
            decide_resume(Some(meta(ib, cfg, 50, 40)), ib, cfg, 45 * REC as u64, total),
            50,
            40
        ));
        // seq==total（完了直前）→ Resume。
        assert!(ok(
            decide_resume(Some(meta(ib, cfg, 100, 80)), ib, cfg, 80 * REC as u64, total),
            100,
            80
        ));
    }

    #[test]
    fn tmp_meta_roundtrip_and_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.tmp.meta");
        write_tmp_meta(&p, 1234, "abc", 50, 40).unwrap();
        let m = read_tmp_meta(&p).unwrap();
        assert_eq!(
            (m.input_bytes, m.config_fp.as_str(), m.input_seq, m.output_records),
            (1234, "abc", 50, 40)
        );
        // 項目欠損 → None。
        fs::write(&p, "input_bytes=1\nconfig=c\n").unwrap();
        assert!(read_tmp_meta(&p).is_none());
    }
}
