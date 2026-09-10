//! compare_eval_nnue - 教師NNUEと生徒NNUEの評価値一致度を検証（並列版）
//!
//! 静的評価と探索スコアを別モードで比較する補助計器。棋力や蒸留成立性は判定しない。
//! 同一局面に対して教師NNUEと生徒NNUE（学習済み）で評価し、
//! MAE、相関係数、スコア帯別誤差を計算する。
//!
//! # 使用方法
//!
//! ```bash
//! cargo run --release -p tools --bin compare_eval_nnue -- \
//!   --input data/*.bin \
//!   --teacher-nnue path/to/teacher.bin \
//!   --student-nnue path/to/student.nnue \
//!   --engine path/to/engine-usi \
//!   --samples 10000 \
//!   --threads 8
//! ```

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;
use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tools::selfplay::engine::{EngineConfig, EngineProcess};

use rshogi_core::position::Position;
use tools::packed_sfen::{PackedSfenValue, unpack_sfen};

#[derive(Parser)]
#[command(
    name = "compare_eval_nnue",
    about = "教師NNUEと生徒NNUEの静的評価または探索スコアを比較"
)]
struct Cli {
    /// 入力packファイル（複数指定可能）
    #[arg(short, long, required = true, num_args = 1..)]
    input: Vec<PathBuf>,

    /// 教師NNUEファイル（nn.bin等）
    #[arg(long, required = true)]
    teacher_nnue: PathBuf,

    /// 生徒NNUEファイル（学習済み.nnue）
    #[arg(long, required = true)]
    student_nnue: PathBuf,

    /// USIエンジンのパス
    #[arg(short, long, required = true)]
    engine: PathBuf,

    /// サンプリングするレコード数
    #[arg(short, long, default_value_t = 10000)]
    samples: usize,

    /// search モードの探索深さ（depth 1 も探索）
    #[arg(short, long, default_value_t = 1)]
    depth: u32,

    /// 比較対象。static は rshogi の eval コマンドを使う
    #[arg(long, value_enum, default_value_t = ComparisonMode::Search)]
    mode: ComparisonMode,

    /// 起動全体・各局面の応答期限（ミリ秒）
    #[arg(long, default_value_t = 120000)]
    timeout_ms: u64,

    /// 並列スレッド数
    #[arg(short = 't', long, default_value_t = 8)]
    threads: usize,

    /// 乱数シード
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// 結果を保存するファイル
    #[arg(short, long)]
    output: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ComparisonMode {
    Search,
    Static,
}

impl ComparisonMode {
    fn label(self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::Static => "static",
        }
    }

    fn units(self) -> &'static str {
        match self {
            Self::Search => "USI cp",
            Self::Static => "rshogi model-scaled raw",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Score {
    Numeric(i32),
    Mate(String),
}

impl Score {
    fn text(&self) -> String {
        match self {
            Self::Numeric(v) => v.to_string(),
            Self::Mate(v) => format!("mate {v}"),
        }
    }
}

/// 生の cp と mate を混同せず、主PVの exact score のみ返す。
fn parse_search_score(line: &str) -> Option<Score> {
    let tokens: Vec<_> = line.split_whitespace().collect();
    if tokens.first() != Some(&"info") || tokens.get(1) == Some(&"string") {
        return None;
    }
    if tokens.windows(2).any(|w| w[0] == "multipv" && w[1] != "1")
        || tokens.contains(&"lowerbound")
        || tokens.contains(&"upperbound")
    {
        return None;
    }
    let i = tokens.iter().position(|v| *v == "score")?;
    match (*tokens.get(i + 1)?, *tokens.get(i + 2)?) {
        ("cp", value) => value.parse().ok().map(Score::Numeric),
        ("mate", value) if value == "+" || value == "-" || value.parse::<i32>().is_ok() => {
            Some(Score::Mate(value.to_string()))
        }
        _ => None,
    }
}

struct UsiEngine {
    process: EngineProcess,
    timeout: Duration,
}

impl UsiEngine {
    fn new(
        engine_path: &std::path::Path,
        eval_file: &std::path::Path,
        timeout: Duration,
    ) -> Result<Self> {
        let cfg = EngineConfig {
            path: engine_path.to_path_buf(),
            args: Vec::new(),
            threads: 1,
            hash_mb: 16,
            network_delay: None,
            network_delay2: None,
            minimum_thinking_time: None,
            slowmover: None,
            ponder: false,
            usi_options: vec![format!("EvalFile={}", eval_file.display())],
        };
        let deadline = Instant::now() + timeout;
        let mut process =
            EngineProcess::spawn_with_timeout(&cfg, eval_file.display().to_string(), timeout)?;
        // EvalFile を広告しないエンジンには初期化時の setoption が届かないため補う。
        // 届いている場合に再送すると、setoption を受けた時点でロードするエンジンでは
        // net を二重に読み込む。
        if !process.is_option_available("EvalFile") {
            process
                .write_line(&format!("setoption name EvalFile value {}", eval_file.display()))?;
            process.write_line("isready")?;
            loop {
                let line = process.recv_line_until(deadline)?;
                anyhow::ensure!(
                    !line.starts_with("info string Error"),
                    "engine initialization: {line}"
                );
                if line == "readyok" {
                    break;
                }
            }
        }
        Ok(Self { process, timeout })
    }

    fn evaluate(&mut self, sfen: &str, mode: ComparisonMode, depth: u32) -> Result<Option<Score>> {
        let deadline = Instant::now() + self.timeout;
        self.process.write_line(&format!("position sfen {sfen}"))?;
        match mode {
            ComparisonMode::Search => self.process.write_line(&format!("go depth {depth}"))?,
            ComparisonMode::Static => {
                self.process.write_line("eval")?;
                self.process.write_line("isready")?;
            }
        }
        let mut score = None;
        loop {
            let line = self.process.recv_line_until(deadline)?;
            anyhow::ensure!(!line.starts_with("info string Error"), "engine evaluation: {line}");
            match mode {
                ComparisonMode::Search => {
                    if let Some(value) = parse_search_score(&line) {
                        score = Some(value);
                    }
                    if line.split_whitespace().next() == Some("bestmove") {
                        return Ok(score);
                    }
                }
                ComparisonMode::Static => {
                    if let Some(value) = line.strip_prefix("info string Static eval: ") {
                        anyhow::ensure!(score.is_none(), "duplicate static evaluation");
                        score = Some(Score::Numeric(
                            value.trim().parse().context("invalid static evaluation")?,
                        ));
                    }
                    if line == "readyok" {
                        anyhow::ensure!(
                            score.is_some(),
                            "engine did not return rshogi static evaluation (eval unsupported or no NNUE)"
                        );
                        return Ok(score);
                    }
                }
            }
        }
    }
}

/// サンプルデータ
#[derive(Debug, Clone)]
struct Sample {
    sfen: String,
    original_score: i16,
}

/// 評価結果
#[derive(Debug, Clone)]
struct EvalResult {
    teacher_score: Option<Score>,
    student_score: Option<Score>,
}

fn main() -> Result<()> {
    run(Cli::parse())
}

fn run(cli: Cli) -> Result<()> {
    anyhow::ensure!(
        cli.threads > 0 && cli.samples > 0 && cli.timeout_ms > 0,
        "threads, samples and timeout-ms must be positive"
    );
    anyhow::ensure!(
        cli.mode != ComparisonMode::Search || cli.depth > 0,
        "search depth must be positive"
    );
    let timeout = Duration::from_millis(cli.timeout_ms);
    anyhow::ensure!(Instant::now().checked_add(timeout).is_some(), "timeout too large");

    // rayon スレッドプール設定
    rayon::ThreadPoolBuilder::new().num_threads(cli.threads).build_global().ok();

    println!("=== NNUE スコア比較（並列版） ===");
    println!("教師NNUE: {}", cli.teacher_nnue.display());
    println!("生徒NNUE: {}", cli.student_nnue.display());
    println!("サンプル数: {}", cli.samples);
    println!(
        "mode: {}, units: {}, perspective: side-to-move",
        cli.mode.label(),
        cli.mode.units()
    );
    println!(
        "effective model scale: unknown; bucket/routing: unknown (engine configured defaults)"
    );
    if cli.mode == ComparisonMode::Search {
        println!("探索深さ: {}", cli.depth);
    }
    println!("並列スレッド数: {}", cli.threads);
    println!();

    // 1. ファイル情報取得
    println!("ファイルサイズを確認中...");
    let mut file_records: Vec<(PathBuf, usize)> = Vec::new();
    let mut total_records: usize = 0;

    for path in &cli.input {
        let size = std::fs::metadata(path)
            .with_context(|| format!("Failed to get metadata: {}", path.display()))?
            .len() as usize;
        let records = size / PackedSfenValue::SIZE;
        total_records += records;
        file_records.push((path.clone(), records));
    }

    println!("総レコード数: {total_records}");

    // 2. ランダムサンプリング
    println!("サンプルを選択中...");
    let mut rng = ChaCha8Rng::seed_from_u64(cli.seed);
    let sample_indices: Vec<usize> = {
        let mut indices: Vec<usize> = (0..total_records).collect();
        indices.shuffle(&mut rng);
        indices.into_iter().take(cli.samples).collect()
    };

    let mut sorted_indices = sample_indices.clone();
    sorted_indices.sort();

    // 3. サンプル読み込み
    println!("サンプルを読み込み中...");
    let mut samples: Vec<Sample> = Vec::new();

    let mut current_file_idx = 0;
    let mut current_file_start = 0;
    let mut current_file: Option<File> = None;

    for &global_idx in &sorted_indices {
        while current_file_idx < file_records.len() {
            let (_, records) = &file_records[current_file_idx];
            if global_idx < current_file_start + records {
                break;
            }
            current_file_start += records;
            current_file_idx += 1;
            current_file = None;
        }

        if current_file_idx >= file_records.len() {
            break;
        }

        if current_file.is_none() {
            current_file = Some(File::open(&file_records[current_file_idx].0)?);
        }

        let local_idx = global_idx - current_file_start;
        let file = current_file.as_mut().unwrap();

        use std::io::Seek;
        file.seek(std::io::SeekFrom::Start((local_idx * PackedSfenValue::SIZE) as u64))?;

        let mut buffer = [0u8; PackedSfenValue::SIZE];
        file.read_exact(&mut buffer)?;

        let psv = PackedSfenValue::from_bytes(&buffer)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse PackedSfenValue"))?;

        let sfen =
            unpack_sfen(&psv.sfen).map_err(|e| anyhow::anyhow!("Failed to unpack SFEN: {e}"))?;

        // SFENの妥当性確認
        let mut pos = Position::new();
        if pos.set_sfen(&sfen).is_err() {
            continue;
        }

        samples.push(Sample {
            sfen,
            original_score: psv.score,
        });
    }

    println!("読み込み完了: {} サンプル", samples.len());
    println!();

    anyhow::ensure!(!samples.is_empty(), "no valid samples");

    // 4. サンプルをチャンクに分割して並列評価
    let chunk_size = samples.len().div_ceil(cli.threads);
    let chunks: Vec<Vec<Sample>> = samples.chunks(chunk_size).map(|c| c.to_vec()).collect();

    let progress = Arc::new(AtomicUsize::new(0));
    let total = samples.len();

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) {msg}")
            .expect("valid template"),
    );
    pb.set_message("評価中...");

    // 各チャンクを並列で処理
    let engine_path = cli.engine.clone();
    let teacher_nnue = cli.teacher_nnue.clone();
    let student_nnue = cli.student_nnue.clone();
    let depth = cli.depth;
    let progress_clone = Arc::clone(&progress);

    println!("エンジン起動中...");
    let results: Result<Vec<Vec<(Sample, EvalResult)>>> = chunks
        .into_par_iter()
        .enumerate()
        .map(|(thread_id, chunk)| {
            let mut teacher_engine = UsiEngine::new(&engine_path, &teacher_nnue, timeout)
                .context("Failed to start teacher engine")?;
            let mut student_engine = UsiEngine::new(&engine_path, &student_nnue, timeout)
                .context("Failed to start student engine")?;
            eprintln!("[スレッド{thread_id}] エンジン起動完了");

            let mut results = Vec::new();

            for sample in chunk {
                // 通信エラーは握り潰さず、子プロセスの Drop により回収する。
                let teacher_score = teacher_engine
                    .evaluate(&sample.sfen, cli.mode, depth)
                    .context("teacher evaluation")?;
                let student_score = student_engine
                    .evaluate(&sample.sfen, cli.mode, depth)
                    .context("student evaluation")?;

                results.push((
                    sample,
                    EvalResult {
                        teacher_score,
                        student_score,
                    },
                ));

                let count = progress_clone.fetch_add(1, Ordering::Relaxed) + 1;
                if count.is_multiple_of(100) {
                    pb.set_position(count as u64);
                }
            }

            Ok(results)
        })
        .collect();

    let results = results?;
    pb.finish_with_message("評価完了");
    println!();

    // 結果を結合
    let all_results: Vec<(Sample, EvalResult)> = results.into_iter().flatten().collect();

    // 5. 結果分析
    analyze_results(&all_results, &cli)?;

    Ok(())
}

fn analyze_results(results: &[(Sample, EvalResult)], cli: &Cli) -> Result<()> {
    save_results(results, cli)?;
    let unit = cli.mode.units();
    println!("=== スコア差の補助統計 ===");

    let mut teacher_scores: Vec<f64> = Vec::new();
    let mut student_scores: Vec<f64> = Vec::new();
    let mut abs_diffs: Vec<i64> = Vec::new();

    let mut band_300: Vec<i64> = Vec::new();
    let mut band_1000: Vec<i64> = Vec::new();
    let mut band_3000: Vec<i64> = Vec::new();
    let mut band_large: Vec<i64> = Vec::new();

    let mut missing = 0;
    let mut mate_count = 0;

    for (_, eval) in results {
        match (&eval.teacher_score, &eval.student_score) {
            (Some(Score::Numeric(t)), Some(Score::Numeric(s))) => {
                let (t, s) = (i64::from(*t), i64::from(*s));

                teacher_scores.push(t as f64);
                student_scores.push(s as f64);

                let diff = (s - t).abs();
                abs_diffs.push(diff);

                let abs_t = t.abs();
                if abs_t <= 300 {
                    band_300.push(diff);
                } else if abs_t <= 1000 {
                    band_1000.push(diff);
                } else if abs_t <= 3000 {
                    band_3000.push(diff);
                } else {
                    band_large.push(diff);
                }
            }
            (Some(Score::Mate(_)), _) | (_, Some(Score::Mate(_))) => mate_count += 1,
            _ => {
                missing += 1;
            }
        }
    }

    println!("比較可能サンプル数: {}", teacher_scores.len());
    println!("評価失敗: {missing}");
    println!("詰みスコア除外: {mate_count}");
    println!();

    if teacher_scores.is_empty() {
        println!("ERROR: 比較可能なサンプルがありません");
        return Ok(());
    }

    // MAE
    let mae: f64 = abs_diffs.iter().map(|&d| d as f64).sum::<f64>() / abs_diffs.len() as f64;

    // 相関係数
    let n = teacher_scores.len() as f64;
    let mean_t: f64 = teacher_scores.iter().sum::<f64>() / n;
    let mean_s: f64 = student_scores.iter().sum::<f64>() / n;

    let mut cov = 0.0;
    let mut var_t = 0.0;
    let mut var_s = 0.0;

    for i in 0..teacher_scores.len() {
        let dt = teacher_scores[i] - mean_t;
        let ds = student_scores[i] - mean_s;
        cov += dt * ds;
        var_t += dt * dt;
        var_s += ds * ds;
    }

    let correlation = if var_t > 0.0 && var_s > 0.0 {
        cov / (var_t.sqrt() * var_s.sqrt())
    } else {
        0.0
    };

    // 統計値
    let mut sorted_abs = abs_diffs.clone();
    sorted_abs.sort();
    let median_abs = sorted_abs[sorted_abs.len() / 2];
    let p95_abs = sorted_abs[sorted_abs.len() * 95 / 100];
    let p99_abs = sorted_abs[sorted_abs.len() * 99 / 100];

    println!("=== 全体統計 ===");
    println!("MAE (平均絶対誤差): {mae:.1} {unit}");
    println!("相関係数: {correlation:.4}");
    println!("絶対誤差 中央値: {median_abs} {unit}");
    println!("絶対誤差 P95: {p95_abs} {unit}");
    println!("絶対誤差 P99: {p99_abs} {unit}");
    println!();

    fn calc_mae(diffs: &[i64]) -> f64 {
        if diffs.is_empty() {
            return 0.0;
        }
        diffs.iter().map(|&d| d as f64).sum::<f64>() / diffs.len() as f64
    }

    println!("=== スコア帯別MAE ===");
    println!("|score| <= 300:  N={:5}, MAE={:.1} {unit}", band_300.len(), calc_mae(&band_300));
    println!(
        "300 < |score| <= 1000: N={:5}, MAE={:.1} {unit}",
        band_1000.len(),
        calc_mae(&band_1000)
    );
    println!(
        "1000 < |score| <= 3000: N={:5}, MAE={:.1} {unit}",
        band_3000.len(),
        calc_mae(&band_3000)
    );
    println!(
        "|score| > 3000: N={:5}, MAE={:.1} {unit}",
        band_large.len(),
        calc_mae(&band_large)
    );
    println!();

    println!("これらの補助統計だけで棋力や蒸留成立性は判定できません。");
    Ok(())
}

fn save_results(results: &[(Sample, EvalResult)], cli: &Cli) -> Result<()> {
    if let Some(path) = &cli.output {
        let mut file = File::create(path)?;
        writeln!(
            file,
            "sfen\tteacher_score\tstudent_score\tdiff\toriginal_score\tmode\tunits\tperspective\tmodel_scale\tbucket_routing\tteacher_model\tstudent_model\tdepth"
        )?;
        for (sample, eval) in results {
            let teacher = eval.teacher_score.as_ref().map(Score::text).unwrap_or_default();
            let student = eval.student_score.as_ref().map(Score::text).unwrap_or_default();
            let diff = match (&eval.teacher_score, &eval.student_score) {
                (Some(Score::Numeric(t)), Some(Score::Numeric(s))) => {
                    (i64::from(*s) - i64::from(*t)).to_string()
                }
                _ => String::new(),
            };
            writeln!(
                file,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\tside-to-move\tunknown\tunknown\t{}\t{}\t{}",
                sample.sfen,
                teacher,
                student,
                diff,
                sample.original_score,
                cli.mode.label(),
                cli.mode.units(),
                cli.teacher_nnue.display(),
                cli.student_nnue.display(),
                if cli.mode == ComparisonMode::Search {
                    cli.depth.to_string()
                } else {
                    String::new()
                }
            )?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;

    /// script の書き込みと fork を直列化する。並列テストでは、あるテストが script の
    /// write fd を開いている間に別テストが fork すると、子が write fd を継承したまま
    /// exec 前の窓に入り、その script の exec が ETXTBSY で落ちる (`O_CLOEXEC` は fork では
    /// 閉じず exec 時にしか閉じない)。書き込みと spawn の両方が同じ lock を取る。
    static FORK_WRITE_LOCK: Mutex<()> = Mutex::new(());

    /// tmp へ書き込み → close → chmod → atomic rename で、自スレッドの write fd が
    /// exec と重なる経路も塞ぐ。
    fn write_script(path: &std::path::Path, body: &str) {
        let _guard = FORK_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = path.with_extension("sh.tmp");
        {
            let mut f = std::fs::File::create(&tmp).unwrap();
            f.write_all(body.as_bytes()).unwrap();
            f.sync_all().unwrap();
        }
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(&tmp, path).unwrap();
    }

    fn mock(dir: &std::path::Path, special: &str) -> PathBuf {
        let path = dir.join("mock.sh");
        write_script(
            &path,
            &format!(
                r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
{special}
    usi) echo 'option name EvalFile type string default none'; echo usiok ;;
    isready) echo readyok ;;
    eval) echo 'info string Static eval: -123'; echo 'info string SFEN: test' ;;
    'go depth '*) echo 'info depth 1 score cp 37'; echo 'bestmove 7g7f' ;;
    quit) exit 0 ;;
  esac
done
"#
            ),
        );
        path
    }

    /// engine を起動する処理を [`FORK_WRITE_LOCK`] 下で行う。
    fn with_fork_lock<T>(f: impl FnOnce() -> T) -> T {
        let _guard = FORK_WRITE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        f()
    }

    fn engine(path: &std::path::Path) -> Result<UsiEngine> {
        with_fork_lock(|| {
            UsiEngine::new(path, std::path::Path::new("model.bin"), Duration::from_millis(300))
        })
    }

    #[test]
    fn typed_scores_keep_mates_and_large_cp_separate() {
        assert_eq!(parse_search_score("info score cp 31000"), Some(Score::Numeric(31000)));
        for value in ["+", "-", "3", "-5"] {
            assert_eq!(
                parse_search_score(&format!("info score mate {value}")),
                Some(Score::Mate(value.into()))
            );
        }
        assert_eq!(parse_search_score("info score cp 100 lowerbound"), None);
        assert_eq!(parse_search_score("info multipv 2 score cp 999"), None);
        assert_eq!(parse_search_score("info string score cp 999"), None);
    }

    #[test]
    fn ready_and_bestmove_eof_are_errors() {
        for special in [
            "isready) exit 3 ;;",
            "'go depth '*) echo 'info score cp 7'; exit 4 ;;",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = mock(dir.path(), special);
            let outcome = engine(&path)
                .and_then(|mut engine| engine.evaluate("test", ComparisonMode::Search, 1));
            let error = format!("{:#}", outcome.expect_err("EOF must fail"));
            assert!(error.contains("exited unexpectedly"), "{error}");
        }
    }

    /// EvalFile を送った回数を log file に記録する mock。`advertise` が空なら EvalFile を広告しない。
    fn logging_mock(dir: &std::path::Path, advertise: &str) -> (PathBuf, PathBuf) {
        let log = dir.join("setoption.log");
        let path = dir.join("mock.sh");
        write_script(
            &path,
            &format!(
                r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    'setoption name EvalFile'*) echo "$line" >> '{log}' ;;
    usi) {advertise} echo 'option name USI_Hash type spin default 16'; echo usiok ;;
    isready) echo readyok ;;
    eval) echo 'info string Static eval: -123'; echo 'info string SFEN: test' ;;
    quit) exit 0 ;;
  esac
done
"#,
                log = log.display(),
            ),
        );
        (path, log)
    }

    fn eval_file_sends(log: &std::path::Path) -> usize {
        std::fs::read_to_string(log).map(|s| s.lines().count()).unwrap_or(0)
    }

    #[test]
    fn eval_file_is_sent_exactly_once_whether_advertised_or_not() {
        // 広告あり: 初期化で届くので補完しない (再送すると net を二重ロードする)
        let dir = tempfile::tempdir().unwrap();
        let (path, log) =
            logging_mock(dir.path(), "echo 'option name EvalFile type string default none';");
        let mut advertised = engine(&path).expect("advertised engine must initialize");
        advertised.evaluate("test", ComparisonMode::Static, 1).expect("static eval");
        assert_eq!(eval_file_sends(&log), 1);

        // 広告なし: 初期化で落ちるので補完が要る
        let dir = tempfile::tempdir().unwrap();
        let (path, log) = logging_mock(dir.path(), "");
        let mut unadvertised = engine(&path).expect("unadvertised engine must initialize");
        unadvertised.evaluate("test", ComparisonMode::Static, 1).expect("static eval");
        assert_eq!(eval_file_sends(&log), 1);
    }

    #[test]
    fn first_ready_error_is_not_hidden_by_later_ready() {
        let dir = tempfile::tempdir().unwrap();
        let path = mock(
            dir.path(),
            "isready) if [ -z \"$seen_ready\" ]; then echo 'info string Error: model load failed'; seen_ready=1; fi; echo readyok ;;",
        );
        let error = format!("{:#}", engine(&path).err().expect("first ready error must fail"));
        assert!(error.contains("model load failed"), "{error}");
    }

    #[test]
    fn static_barrier_errors_and_deadlines() {
        for special in [
            "eval) echo 'info string Error: No NNUE network loaded' ;;",
            "eval) : ;;",
            "eval) exit 5 ;;",
            "eval) while :; do echo 'info string waiting'; sleep 0.01; done ;;",
            "'go depth '*) while :; do echo 'info depth 1'; sleep 0.01; done ;;",
            "usi) while :; do echo 'info string loading'; sleep 0.01; done ;;",
            "isready) while :; do echo 'info string loading'; sleep 0.01; done ;;",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = mock(dir.path(), special);
            let start = Instant::now();
            let mode = if special.starts_with("'go") {
                ComparisonMode::Search
            } else {
                ComparisonMode::Static
            };
            let result = engine(&path).and_then(|mut engine| engine.evaluate("test", mode, 1));
            assert!(result.is_err(), "{special}");
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "deadline/cleanup exceeded: {special}"
            );
        }
    }

    #[test]
    fn stderr_tail_is_retained_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = mock(
            dir.path(),
            "isready) i=0; while [ $i -lt 100 ]; do echo \"old-$i\" >&2; i=$((i+1)); done; echo diagnostic-one >&2; echo diagnostic-two >&2; sleep 0.05; exit 6 ;;",
        );
        let error = format!("{:#}", engine(&path).err().unwrap());
        assert!(error.contains("diagnostic-one") && error.contains("diagnostic-two"), "{error}");
        assert!(!error.contains("old-0"), "{error}");
    }

    #[test]
    fn static_and_search_complete_workflow_preserves_units_and_rows() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = mock(dir.path(), "");
        let mut pos = Position::new();
        pos.set_sfen("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")?;
        let record = PackedSfenValue {
            sfen: tools::packed_sfen::pack_position(&pos),
            score: 42,
            move16: 0,
            game_ply: 1,
            game_result: 0,
            padding: 0,
        };
        let input = dir.path().join("input.bin");
        std::fs::write(&input, record.to_bytes())?;
        for mode in [ComparisonMode::Static, ComparisonMode::Search] {
            let output = dir.path().join(format!("{}.tsv", mode.label()));
            with_fork_lock(|| {
                run(Cli {
                    input: vec![input.clone()],
                    teacher_nnue: "teacher.bin".into(),
                    student_nnue: "student.bin".into(),
                    engine: path.clone(),
                    samples: 1,
                    depth: 1,
                    mode,
                    timeout_ms: 1000,
                    threads: 1,
                    seed: 42,
                    output: Some(output.clone()),
                })
            })?;
            let text = std::fs::read_to_string(output)?;
            let value = if mode == ComparisonMode::Static {
                "-123"
            } else {
                "37"
            };
            assert!(
                text.contains(&format!(
                    "\t{value}\t{value}\t0\t42\t{}\t{}\tside-to-move\tunknown\tunknown",
                    mode.label(),
                    mode.units()
                )),
                "{text}"
            );
        }
        Ok(())
    }

    #[test]
    fn mate_rows_are_saved_without_numeric_difference() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let output = dir.path().join("mates.tsv");
        let cli = Cli {
            input: vec![],
            teacher_nnue: "t.bin".into(),
            student_nnue: "s.bin".into(),
            engine: "engine".into(),
            samples: 1,
            depth: 1,
            mode: ComparisonMode::Search,
            timeout_ms: 1000,
            threads: 1,
            seed: 42,
            output: Some(output.clone()),
        };
        analyze_results(
            &[(
                Sample {
                    sfen: "test".into(),
                    original_score: 8,
                },
                EvalResult {
                    teacher_score: Some(Score::Mate("+".into())),
                    student_score: Some(Score::Numeric(31000)),
                },
            )],
            &cli,
        )?;
        assert!(
            std::fs::read_to_string(output)?.contains("test\tmate +\t31000\t\t8\tsearch\tUSI cp")
        );
        Ok(())
    }
}
