//! compare_eval_nnue - 教師NNUEと生徒NNUEの評価値一致度を検証（並列版）
//!
//! 蒸留学習の成立性を確認するツール。
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
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use engine_core::position::Position;
use tools::packed_sfen::{unpack_sfen, PackedSfenValue};

#[derive(Parser)]
#[command(
    name = "compare_eval_nnue",
    about = "教師NNUEと生徒NNUEの評価値一致度を検証（蒸留成立性チェック）"
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

    /// 評価時の探索深さ（1=静的評価のみ）
    #[arg(short, long, default_value_t = 1)]
    depth: u32,

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

/// USIエンジンラッパー
struct UsiEngine {
    child: Child,
    stdin: BufWriter<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl UsiEngine {
    fn new(engine_path: &std::path::Path, eval_file: &std::path::Path) -> Result<Self> {
        let mut child = Command::new(engine_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("Failed to start engine: {}", engine_path.display()))?;

        let stdin = BufWriter::new(child.stdin.take().expect("stdin"));
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));

        let mut engine = Self {
            child,
            stdin,
            stdout,
        };

        engine.send_command("usi")?;
        engine.wait_for("usiok")?;

        let eval_file_str = eval_file.to_string_lossy();
        engine.send_command(&format!("setoption name EvalFile value {eval_file_str}"))?;
        engine.send_command("setoption name Threads value 1")?;
        engine.send_command("setoption name USI_Hash value 16")?;

        engine.send_command("isready")?;
        engine.wait_for("readyok")?;

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
            self.stdout.read_line(&mut line)?;
            if line.trim() == expected {
                break;
            }
        }
        Ok(())
    }

    /// 探索による評価（go depthコマンド使用）
    fn evaluate_search(&mut self, sfen: &str, depth: u32) -> Result<Option<i32>> {
        self.send_command(&format!("position sfen {sfen}"))?;
        self.send_command(&format!("go depth {depth}"))?;

        let mut score: Option<i32> = None;
        let mut line = String::new();

        loop {
            line.clear();
            self.stdout.read_line(&mut line)?;
            let trimmed = line.trim();

            if trimmed.starts_with("info") && trimmed.contains("score cp") {
                if let Some(cp_idx) = trimmed.find("score cp") {
                    let rest = &trimmed[cp_idx + 9..];
                    if let Some(end_idx) = rest.find(' ').or(Some(rest.len())) {
                        if let Ok(cp) = rest[..end_idx].parse::<i32>() {
                            score = Some(cp);
                        }
                    }
                }
            }

            if trimmed.starts_with("info") && trimmed.contains("score mate") {
                if let Some(mate_idx) = trimmed.find("score mate") {
                    let rest = &trimmed[mate_idx + 11..];
                    if let Some(end_idx) = rest.find(' ').or(Some(rest.len())) {
                        if let Ok(mate_in) = rest[..end_idx].parse::<i32>() {
                            score = Some(if mate_in > 0 { 31999 } else { -31999 });
                        }
                    }
                }
            }

            if trimmed.starts_with("bestmove") {
                break;
            }
        }

        Ok(score)
    }

    fn quit(&mut self) -> Result<()> {
        self.send_command("quit")?;
        self.child.wait()?;
        Ok(())
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
    teacher_score: Option<i32>,
    student_score: Option<i32>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // rayon スレッドプール設定
    rayon::ThreadPoolBuilder::new().num_threads(cli.threads).build_global().ok();

    println!("=== 蒸留成立性チェッカー（並列版） ===");
    println!("教師NNUE: {}", cli.teacher_nnue.display());
    println!("生徒NNUE: {}", cli.student_nnue.display());
    println!("サンプル数: {}", cli.samples);
    println!("評価深さ: {} (1=静的評価)", cli.depth);
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
    let results: Vec<Vec<(Sample, EvalResult)>> = chunks
        .into_par_iter()
        .enumerate()
        .map(|(thread_id, chunk)| {
            let mut teacher_engine = UsiEngine::new(&engine_path, &teacher_nnue)
                .expect("Failed to start teacher engine");
            let mut student_engine = UsiEngine::new(&engine_path, &student_nnue)
                .expect("Failed to start student engine");
            eprintln!("[スレッド{thread_id}] エンジン起動完了");

            let mut results = Vec::new();

            for sample in chunk {
                // go depth で評価（Rust製エンジンはevalコマンド非対応）
                let teacher_score =
                    teacher_engine.evaluate_search(&sample.sfen, depth).ok().flatten();
                let student_score =
                    student_engine.evaluate_search(&sample.sfen, depth).ok().flatten();

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

            teacher_engine.quit().ok();
            student_engine.quit().ok();

            results
        })
        .collect();

    pb.finish_with_message("評価完了");
    println!();

    // 結果を結合
    let all_results: Vec<(Sample, EvalResult)> = results.into_iter().flatten().collect();

    // 5. 結果分析
    analyze_results(&all_results, &cli.output)?;

    Ok(())
}

fn analyze_results(results: &[(Sample, EvalResult)], output_path: &Option<PathBuf>) -> Result<()> {
    println!("=== 蒸留成立性分析 ===");

    let mut teacher_scores: Vec<f64> = Vec::new();
    let mut student_scores: Vec<f64> = Vec::new();
    let mut abs_diffs: Vec<i32> = Vec::new();

    let mut band_300: Vec<i32> = Vec::new();
    let mut band_1000: Vec<i32> = Vec::new();
    let mut band_3000: Vec<i32> = Vec::new();
    let mut band_large: Vec<i32> = Vec::new();

    let mut missing = 0;
    let mut mate_count = 0;

    for (_, eval) in results {
        match (eval.teacher_score, eval.student_score) {
            (Some(t), Some(s)) => {
                if t.abs() >= 30000 || s.abs() >= 30000 {
                    mate_count += 1;
                    continue;
                }

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
    println!("MAE (平均絶対誤差): {mae:.1} cp");
    println!("相関係数: {correlation:.4}");
    println!("絶対誤差 中央値: {median_abs} cp");
    println!("絶対誤差 P95: {p95_abs} cp");
    println!("絶対誤差 P99: {p99_abs} cp");
    println!();

    fn calc_mae(diffs: &[i32]) -> f64 {
        if diffs.is_empty() {
            return 0.0;
        }
        diffs.iter().map(|&d| d as f64).sum::<f64>() / diffs.len() as f64
    }

    println!("=== スコア帯別MAE ===");
    println!("|score| <= 300:  N={:5}, MAE={:.1} cp", band_300.len(), calc_mae(&band_300));
    println!(
        "300 < |score| <= 1000: N={:5}, MAE={:.1} cp",
        band_1000.len(),
        calc_mae(&band_1000)
    );
    println!(
        "1000 < |score| <= 3000: N={:5}, MAE={:.1} cp",
        band_3000.len(),
        calc_mae(&band_3000)
    );
    println!("|score| > 3000: N={:5}, MAE={:.1} cp", band_large.len(), calc_mae(&band_large));
    println!();

    println!("=== 蒸留成立性判定 ===");

    if correlation >= 0.95 {
        println!("相関係数: ✓ 優秀 (≥0.95)");
    } else if correlation >= 0.90 {
        println!("相関係数: △ 良好 (0.90-0.95)");
    } else if correlation >= 0.80 {
        println!("相関係数: △ 要改善 (0.80-0.90)");
    } else {
        println!("相関係数: ✗ 不十分 (<0.80) - 蒸留が成立していない可能性");
    }

    if mae <= 100.0 {
        println!("MAE: ✓ 優秀 (≤100cp)");
    } else if mae <= 200.0 {
        println!("MAE: △ 良好 (100-200cp)");
    } else if mae <= 300.0 {
        println!("MAE: △ 要改善 (200-300cp)");
    } else {
        println!("MAE: ✗ 大きい (>300cp) - 評価値の再現性が低い");
    }

    println!();
    if correlation >= 0.95 && mae <= 100.0 {
        println!("→ 蒸留は成立している。Materialに勝てない原因は「目的不一致」の可能性");
    } else if correlation >= 0.90 && mae <= 200.0 {
        println!("→ 蒸留はほぼ成立。微調整で改善の余地あり");
    } else {
        println!("→ 蒸留が不十分。スケール/シリアライズ/量子化/特徴量の確認が必要");
    }

    // 結果をファイルに保存
    if let Some(path) = output_path {
        println!();
        println!("結果を保存中: {}", path.display());

        let mut file = File::create(path)?;
        writeln!(file, "sfen\tteacher_score\tstudent_score\tdiff\toriginal_score")?;
        for (sample, eval) in results {
            if let (Some(t), Some(s)) = (eval.teacher_score, eval.student_score) {
                writeln!(
                    file,
                    "{}\t{}\t{}\t{}\t{}",
                    sample.sfen,
                    t,
                    s,
                    s - t,
                    sample.original_score
                )?;
            }
        }
        println!("保存完了");
    }

    Ok(())
}
