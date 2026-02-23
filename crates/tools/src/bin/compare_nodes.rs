//! compare_nodes - 2つのUSIエンジン間でノード数を深度別に比較するツール
//!
//! YaneuraOu との alignment 調査や、同一エンジンの A/B テストに使用する。
//! 複数局面を並列処理し、結果をタイムスタンプ付きディレクトリに保存する。
//!
//! # 使用方法
//!
//! rshogi vs YaneuraOu（depth 20、100局面）:
//! ```bash
//! cargo run --release -p tools --bin compare_nodes -- \
//!   --engine-a ./target/release/rshogi-usi \
//!   --engine-b /mnt/nvme1/development/YaneuraOu/source/YaneuraOu-by-gcc \
//!   --options-a "Threads=1" \
//!   --options-b "FV_SCALE=24,Threads=1,PvInterval=0" \
//!   --hash 512 \
//!   --eval-a /mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin \
//!   --eval-b /mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu \
//!   --sfens start_sfens_ply24.txt \
//!   --depth 20 \
//!   --sample 100 \
//!   --workers 8
//! ```
//!
//! 単一SFEN文字列を直接指定して調査（--sfens の代わりに --sfen を使用）:
//! ```bash
//! cargo run --release -p tools --bin compare_nodes -- \
//!   --engine-a ./target/release/rshogi-usi \
//!   --engine-b /mnt/nvme1/development/YaneuraOu/source/YaneuraOu-by-gcc \
//!   --options-a "Threads=1" \
//!   --options-b "FV_SCALE=24,Threads=1,PvInterval=0" \
//!   --hash 256 \
//!   --eval-a /mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu/suisho5.bin \
//!   --eval-b /mnt/nvme1/development/rshogi/eval/halfkp_256x2-32-32_crelu \
//!   --sfen "l6nl/1r1sgkgs1/p3pp1p1/2pp2p1p/1p1n3P1/P1P2PP1P/1PSPP1N2/2GK2SR1/LN3G2L b Bb 29" \
//!   --depth 18 \
//!   --workers 1
//! ```

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;
use serde::Serialize;

#[derive(Parser)]
#[command(
    name = "compare_nodes",
    about = "2つのUSIエンジン間でノード数を深度別に比較する"
)]
struct Cli {
    /// エンジンAのバイナリパス
    #[arg(long)]
    engine_a: PathBuf,

    /// エンジンBのバイナリパス
    #[arg(long)]
    engine_b: PathBuf,

    /// エンジンA固有のUSIオプション（カンマ区切り、例: "Threads=1,FV_SCALE=24"）
    #[arg(long, value_delimiter = ',')]
    options_a: Vec<String>,

    /// エンジンB固有のUSIオプション（カンマ区切り、例: "FV_SCALE=24,Threads=1,PvInterval=0"）
    #[arg(long, value_delimiter = ',')]
    options_b: Vec<String>,

    /// 置換表サイズ（MB）— 両エンジン共通
    #[arg(long, default_value_t = 64)]
    hash: u32,

    /// エンジンAの評価関数パス（"EvalFile" として設定）
    #[arg(long)]
    eval_a: Option<PathBuf>,

    /// エンジンBの評価関数パス（"EvalDir" として設定、YaneuraOu等のディレクトリ指定に対応）
    #[arg(long)]
    eval_b: Option<PathBuf>,

    /// SFENファイルのパス（1行1局面）。--sfen と排他
    #[arg(long, conflicts_with = "sfen")]
    sfens: Option<PathBuf>,

    /// SFEN文字列を直接指定（1局面）。--sfens と排他
    #[arg(long, conflicts_with = "sfens")]
    sfen: Option<String>,

    /// 探索深度
    #[arg(long, default_value_t = 10)]
    depth: u32,

    /// ランダムサンプル数（0=全件）
    #[arg(long, default_value_t = 0)]
    sample: usize,

    /// 並列ワーカー数（デフォルト: 利用可能コア数 / 2）
    #[arg(long)]
    workers: Option<usize>,

    /// 乱数シード
    #[arg(long, default_value_t = 42)]
    seed: u64,

    /// 出力ディレクトリの親（デフォルト: results/）
    #[arg(long, default_value = "results")]
    output_base: PathBuf,

    /// エンジンを局面間で使い回す（TT を蓄積させる対局内モードの再現）。
    /// 有効時は逐次処理（workers=1 相当）になる。
    ///
    /// # 背景
    ///
    /// 対局フレームワークは対局開始時にのみ `usinewgame + isready` を送り、
    /// 着手間では TT をリセットしない。そのため対局中は TT が蓄積し続け、
    /// クリーン TT 状態（新規プロセス）とは探索挙動が異なる。
    ///
    /// # 用途
    ///
    /// - 対局中の TT 蓄積が bestmove 選択に与える影響の定量的計測
    /// - 新規局面（クリーン TT）と連続対局局面の探索差異の分析
    #[arg(long, default_value_t = false)]
    reuse_engine: bool,
}

// ---------------------------------------------------------------------------
// データ構造
// ---------------------------------------------------------------------------

/// 特定深度の探索情報
#[derive(Debug, Clone, Serialize)]
struct DepthInfo {
    depth: u32,
    nodes: u64,
    score_cp: Option<i32>,
    score_mate: Option<i32>,
    nps: Option<u64>,
    pv: String,
}

/// search_depth の戻り値
struct SearchResult {
    depths: Vec<DepthInfo>,
    bestmove: String,
}

/// 1局面の比較結果
#[derive(Debug, Serialize)]
struct PositionResult {
    index: usize,
    sfen: String,
    a_depths: Vec<DepthInfo>,
    b_depths: Vec<DepthInfo>,
    a_bestmove: String,
    b_bestmove: String,
    bestmove_match: bool,
    final_nodes_diff: i64,
    final_nodes_ratio: Option<f64>,
    /// 局面の処理時間（秒）
    elapsed_secs: f64,
}

/// メタデータ
#[derive(Serialize)]
struct Meta {
    timestamp: String,
    engine_a: String,
    engine_b: String,
    options_a: Vec<String>,
    options_b: Vec<String>,
    hash_mb: u32,
    eval_a: Option<String>,
    eval_b: Option<String>,
    sfens_file: String,
    depth: u32,
    workers: usize,
    sample: usize,
    seed: u64,
    total_positions: usize,
    reuse_engine: bool,
}

// ---------------------------------------------------------------------------
// USIエンジンラッパー
// ---------------------------------------------------------------------------

struct UsiEngine {
    child: Child,
    stdin: BufWriter<std::process::ChildStdin>,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for UsiEngine {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        // プロセス終了を待つ（最大300ms）
        for _ in 0..30 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        let _ = self.child.kill();
    }
}

impl UsiEngine {
    /// エンジンを起動して初期化する
    ///
    /// `eval_option` — 評価関数オプション（例: `("EvalFile", "/path/to/nn.bin")` や `("EvalDir", "/path/to/eval/")`）
    fn new(
        engine_path: &Path,
        hash_mb: u32,
        eval_option: Option<(&str, &Path)>,
        options: &[String],
    ) -> Result<Self> {
        let mut child = Command::new(engine_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("エンジン起動失敗: {}", engine_path.display()))?;

        let stdin = BufWriter::new(child.stdin.take().expect("stdin"));
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));

        let mut engine = Self {
            child,
            stdin,
            stdout,
        };

        engine.send("usi")?;
        engine.wait_for("usiok")?;

        // 共通オプション
        engine.send(&format!("setoption name USI_Hash value {hash_mb}"))?;
        if let Some((opt_name, eval_path)) = eval_option {
            engine.send(&format!("setoption name {} value {}", opt_name, eval_path.display()))?;
        }

        // エンジン固有オプション
        for opt in options {
            if let Some((name, value)) = opt.split_once('=') {
                engine.send(&format!("setoption name {} value {}", name.trim(), value.trim()))?;
            } else {
                // ボタン型オプション（値なし）
                engine.send(&format!("setoption name {}", opt.trim()))?;
            }
        }

        engine.send("isready")?;
        engine.wait_for("readyok")?;

        Ok(engine)
    }

    fn send(&mut self, cmd: &str) -> Result<()> {
        writeln!(self.stdin, "{cmd}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn wait_for(&mut self, expected: &str) -> Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            self.stdout.read_line(&mut line)?;
            if line.trim().starts_with(expected) {
                break;
            }
        }
        Ok(())
    }

    /// go depth N で探索し、深度別の情報を収集
    fn search_depth(&mut self, sfen: &str, depth: u32) -> Result<SearchResult> {
        // USIプロトコルの position コマンドを構築
        // "sfen ..." で始まる行はそのまま、それ以外は "sfen " を付加
        let pos_cmd = if sfen.starts_with("sfen ") || sfen == "startpos" {
            format!("position {sfen}")
        } else {
            format!("position sfen {sfen}")
        };
        self.send(&pos_cmd)?;
        self.send(&format!("go depth {depth}"))?;

        let mut depth_map: BTreeMap<u32, DepthInfo> = BTreeMap::new();
        let mut line = String::new();

        let bestmove = loop {
            line.clear();
            self.stdout.read_line(&mut line).context("エンジン出力の読み取りに失敗")?;
            let trimmed = line.trim();

            if trimmed.starts_with("info") {
                // multipv > 1 の行はスキップ
                if has_multipv_gt1(trimmed) {
                    continue;
                }
                if let Some(di) = parse_info_line(trimmed) {
                    depth_map.insert(di.depth, di);
                }
            } else if trimmed.starts_with("bestmove") {
                break trimmed.split_whitespace().nth(1).unwrap_or("none").to_string();
            }
        };

        Ok(SearchResult {
            depths: depth_map.into_values().collect(),
            bestmove,
        })
    }
}

// ---------------------------------------------------------------------------
// info行パーサ
// ---------------------------------------------------------------------------

/// multipv 2以上か判定
fn has_multipv_gt1(line: &str) -> bool {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for i in 0..tokens.len().saturating_sub(1) {
        if tokens[i] == "multipv"
            && let Ok(v) = tokens[i + 1].parse::<u32>()
        {
            return v > 1;
        }
    }
    false
}

/// info行から DepthInfo をパース。depth フィールドがない行は None を返す。
fn parse_info_line(line: &str) -> Option<DepthInfo> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut depth: Option<u32> = None;
    let mut nodes: u64 = 0;
    let mut score_cp: Option<i32> = None;
    let mut score_mate: Option<i32> = None;
    let mut nps: Option<u64> = None;
    let mut pv_start: Option<usize> = None;

    let mut i = 0;
    while i < tokens.len() {
        match tokens[i] {
            "depth" if i + 1 < tokens.len() => {
                depth = tokens[i + 1].parse().ok();
                i += 2;
            }
            "nodes" if i + 1 < tokens.len() => {
                nodes = tokens[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "score" if i + 2 < tokens.len() => match tokens[i + 1] {
                "cp" => {
                    score_cp = tokens[i + 2].parse().ok();
                    i += 3;
                }
                "mate" => {
                    score_mate = tokens[i + 2].parse().ok();
                    i += 3;
                }
                _ => i += 1,
            },
            "nps" if i + 1 < tokens.len() => {
                nps = tokens[i + 1].parse().ok();
                i += 2;
            }
            "pv" => {
                pv_start = Some(i + 1);
                break;
            }
            _ => i += 1,
        }
    }

    let d = depth?;
    let pv = pv_start.map(|start| tokens[start..].join(" ")).unwrap_or_default();

    Some(DepthInfo {
        depth: d,
        nodes,
        score_cp,
        score_mate,
        nps,
        pv,
    })
}

// ---------------------------------------------------------------------------
// SFEN読み込み
// ---------------------------------------------------------------------------

fn load_sfens(path: &Path) -> Result<Vec<(usize, String)>> {
    let file = File::open(path)
        .with_context(|| format!("SFENファイルを開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut sfens = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim().to_string();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        sfens.push((i + 1, trimmed));
    }
    if sfens.is_empty() {
        anyhow::bail!("SFENファイルに有効な局面がありません: {}", path.display());
    }
    Ok(sfens)
}

// ---------------------------------------------------------------------------
// 局面処理
// ---------------------------------------------------------------------------

/// エンジン起動パラメータ
struct EngineParams {
    path: PathBuf,
    hash: u32,
    eval_opt_name: &'static str,
    eval_path: Option<PathBuf>,
    options: Vec<String>,
}

impl EngineParams {
    fn spawn(&self) -> Result<UsiEngine> {
        let eval_option = self.eval_path.as_deref().map(|p| (self.eval_opt_name, p));
        UsiEngine::new(&self.path, self.hash, eval_option, &self.options)
    }
}

fn process_position(
    params_a: &EngineParams,
    params_b: &EngineParams,
    index: usize,
    sfen: &str,
    depth: u32,
) -> Result<PositionResult> {
    let start = std::time::Instant::now();

    // シェルスクリプト同様、局面ごとにエンジンを新規起動して完全にクリーンな状態で探索
    let mut engine_a = params_a
        .spawn()
        .with_context(|| format!("エンジンA起動失敗: position {index}"))?;
    let mut engine_b = params_b
        .spawn()
        .with_context(|| format!("エンジンB起動失敗: position {index}"))?;

    engine_a.send("usinewgame")?;
    engine_b.send("usinewgame")?;

    let result_a = engine_a
        .search_depth(sfen, depth)
        .with_context(|| format!("エンジンA探索失敗: position {index}"))?;
    let result_b = engine_b
        .search_depth(sfen, depth)
        .with_context(|| format!("エンジンB探索失敗: position {index}"))?;

    let elapsed_secs = start.elapsed().as_secs_f64();

    let final_nodes_a = result_a.depths.last().map(|d| d.nodes).unwrap_or(0);
    let final_nodes_b = result_b.depths.last().map(|d| d.nodes).unwrap_or(0);
    let final_nodes_diff = final_nodes_a as i64 - final_nodes_b as i64;
    let final_nodes_ratio = if final_nodes_b > 0 {
        Some(final_nodes_a as f64 / final_nodes_b as f64)
    } else {
        None
    };
    let bestmove_match = result_a.bestmove == result_b.bestmove;

    Ok(PositionResult {
        index,
        sfen: sfen.to_string(),
        a_depths: result_a.depths,
        b_depths: result_b.depths,
        a_bestmove: result_a.bestmove,
        b_bestmove: result_b.bestmove,
        bestmove_match,
        final_nodes_diff,
        final_nodes_ratio,
        elapsed_secs,
    })
}

/// エンジンを使い回しながら局面を逐次処理する（TT 蓄積モード）。
///
/// 対局フレームワークが対局中に行う処理を再現する:
/// - エンジンを1回だけ起動し全局面で共有する（対局開始時の起動に相当）
/// - 先頭局面の前に `usinewgame` を1回送る
/// - 局面間に `usinewgame` も `isready` も送らない（TT は蓄積し続ける）
///
/// 注意: 並列化はせず逐次処理のみ。sfens の順序が TT 蓄積の内容に影響する。
fn process_positions_reuse(
    params_a: &EngineParams,
    params_b: &EngineParams,
    sfens: &[(usize, String)],
    depth: u32,
    pb: &indicatif::ProgressBar,
) -> Vec<PositionResult> {
    let mut engine_a = match params_a.spawn() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("エンジンA起動失敗: {e}");
            return vec![];
        }
    };
    let mut engine_b = match params_b.spawn() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("エンジンB起動失敗: {e}");
            return vec![];
        }
    };

    // 対局開始時と同様に usinewgame を1回送る
    let _ = engine_a.send("usinewgame");
    let _ = engine_b.send("usinewgame");

    let mut results = Vec::with_capacity(sfens.len());

    for (index, sfen) in sfens {
        let start = std::time::Instant::now();

        let result_a = match engine_a.search_depth(sfen, depth) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("position {index} エンジンA探索失敗: {e}");
                pb.inc(1);
                continue;
            }
        };
        let result_b = match engine_b.search_depth(sfen, depth) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("position {index} エンジンB探索失敗: {e}");
                pb.inc(1);
                continue;
            }
        };

        let elapsed_secs = start.elapsed().as_secs_f64();
        let final_nodes_a = result_a.depths.last().map(|d| d.nodes).unwrap_or(0);
        let final_nodes_b = result_b.depths.last().map(|d| d.nodes).unwrap_or(0);
        let final_nodes_diff = final_nodes_a as i64 - final_nodes_b as i64;
        let final_nodes_ratio = if final_nodes_b > 0 {
            Some(final_nodes_a as f64 / final_nodes_b as f64)
        } else {
            None
        };
        let bestmove_match = result_a.bestmove == result_b.bestmove;

        results.push(PositionResult {
            index: *index,
            sfen: sfen.clone(),
            a_depths: result_a.depths,
            b_depths: result_b.depths,
            a_bestmove: result_a.bestmove,
            b_bestmove: result_b.bestmove,
            bestmove_match,
            final_nodes_diff,
            final_nodes_ratio,
            elapsed_secs,
        });

        pb.inc(1);
    }

    results
}

// ---------------------------------------------------------------------------
// サマリ出力
// ---------------------------------------------------------------------------

fn write_summary(writer: &mut dyn Write, results: &[PositionResult], cli: &Cli) -> Result<()> {
    writeln!(writer, "=== ノード数比較サマリ ===")?;
    writeln!(writer, "エンジンA: {}", cli.engine_a.display())?;
    if !cli.options_a.is_empty() {
        writeln!(writer, "  オプション: {}", cli.options_a.join(", "))?;
    }
    writeln!(writer, "エンジンB: {}", cli.engine_b.display())?;
    if !cli.options_b.is_empty() {
        writeln!(writer, "  オプション: {}", cli.options_b.join(", "))?;
    }
    writeln!(writer, "深度: {}, 局面数: {}", cli.depth, results.len())?;
    writeln!(
        writer,
        "モード: {}",
        if cli.reuse_engine {
            "エンジン使い回し（TT蓄積・逐次）"
        } else {
            "局面ごと新規起動（TTリセット・並列）"
        }
    )?;
    if let Some(eval) = &cli.eval_a {
        writeln!(writer, "EvalFile(A): {}", eval.display())?;
    }
    if let Some(eval) = &cli.eval_b {
        writeln!(writer, "EvalDir(B): {}", eval.display())?;
    }
    writeln!(writer, "Hash: {} MB", cli.hash)?;
    let total_secs: f64 = results.iter().map(|r| r.elapsed_secs).sum();
    let avg_secs = if results.is_empty() {
        0.0
    } else {
        total_secs / results.len() as f64
    };
    writeln!(writer, "所要時間: {:.1}s (平均 {:.1}s/局面)", total_secs, avg_secs)?;
    writeln!(writer)?;

    // 深度別統計
    writeln!(writer, "--- 深度別ノード数統計 ---")?;
    writeln!(
        writer,
        "{:>5} {:>12} {:>12} {:>12} {:>12} {:>8}",
        "depth", "A_avg", "B_avg", "A_total", "B_total", "ratio"
    )?;
    writeln!(writer, "{}", "-".repeat(65))?;

    for d in 1..=cli.depth {
        let mut a_total: u64 = 0;
        let mut b_total: u64 = 0;
        let mut count: u64 = 0;

        for r in results {
            let a_nodes =
                r.a_depths.iter().find(|di| di.depth == d).map(|di| di.nodes).unwrap_or(0);
            let b_nodes =
                r.b_depths.iter().find(|di| di.depth == d).map(|di| di.nodes).unwrap_or(0);
            a_total += a_nodes;
            b_total += b_nodes;
            count += 1;
        }

        if count == 0 {
            continue;
        }

        let a_avg = a_total / count;
        let b_avg = b_total / count;
        let ratio = if b_total > 0 {
            a_total as f64 / b_total as f64
        } else {
            f64::NAN
        };

        writeln!(
            writer,
            "{:>5} {:>12} {:>12} {:>12} {:>12} {:>7.3}x",
            d, a_avg, b_avg, a_total, b_total, ratio
        )?;
    }
    writeln!(writer)?;

    // bestmove一致率
    let matches = results.iter().filter(|r| r.bestmove_match).count();
    writeln!(
        writer,
        "--- bestmove 一致率: {}/{} ({:.1}%) ---",
        matches,
        results.len(),
        matches as f64 / results.len() as f64 * 100.0
    )?;
    writeln!(writer)?;

    // 最終深度ノード数倍率の分布
    writeln!(writer, "--- 最終深度ノード数倍率(A/B)分布 ---")?;
    let mut bucket_low = 0; // < 0.9
    let mut bucket_mid_low = 0; // 0.9 <= A/B < 1.0
    let mut bucket_exact = 0; // A==B (完全一致)
    let mut bucket_mid_high = 0; // 1.0 < A/B < 1.1
    let mut bucket_high = 0; // >= 1.1
    let mut no_ratio = 0;

    for r in results {
        if r.final_nodes_diff == 0 {
            bucket_exact += 1;
        } else {
            match r.final_nodes_ratio {
                Some(ratio) => {
                    if ratio < 0.9 {
                        bucket_low += 1;
                    } else if ratio < 1.0 {
                        bucket_mid_low += 1;
                    } else if ratio < 1.1 {
                        bucket_mid_high += 1;
                    } else {
                        bucket_high += 1;
                    }
                }
                None => no_ratio += 1,
            }
        }
    }

    writeln!(writer, "  A/B < 0.9:              {:>4} 局面", bucket_low)?;
    writeln!(writer, "  0.9 <= A/B < 1.0:       {:>4} 局面", bucket_mid_low)?;
    writeln!(writer, "  A/B = 1.0 (完全一致):   {:>4} 局面", bucket_exact)?;
    writeln!(writer, "  1.0 < A/B < 1.1:        {:>4} 局面", bucket_mid_high)?;
    writeln!(writer, "  1.1 <= A/B:             {:>4} 局面", bucket_high)?;
    if no_ratio > 0 {
        writeln!(writer, "  (B=0で計算不能):        {:>4} 局面", no_ratio)?;
    }
    writeln!(writer)?;

    // 乖離が大きい局面トップ10
    let mut sorted: Vec<&PositionResult> = results.iter().collect();
    sorted
        .sort_by(|a, b| b.final_nodes_diff.unsigned_abs().cmp(&a.final_nodes_diff.unsigned_abs()));
    let top_n = sorted.len().min(10);
    writeln!(writer, "--- 乖離が大きい局面 (top {top_n}) ---")?;
    for r in &sorted[..top_n] {
        let a_nodes = r.a_depths.last().map(|d| d.nodes).unwrap_or(0);
        let b_nodes = r.b_depths.last().map(|d| d.nodes).unwrap_or(0);
        let ratio_str = match r.final_nodes_ratio {
            Some(ratio) => format!("{ratio:.3}x"),
            None => "N/A".to_string(),
        };
        let bm = if r.bestmove_match {
            r.a_bestmove.to_string()
        } else {
            format!("{} vs {}", r.a_bestmove, r.b_bestmove)
        };
        writeln!(
            writer,
            "#{} | final: A={a_nodes} B={b_nodes} diff={} ratio={ratio_str} | {:.1}s | bestmove: {bm}",
            r.index, r.final_nodes_diff, r.elapsed_secs
        )?;
        writeln!(writer, "  sfen {}", r.sfen)?;
        // 深度別の乖離を表示
        let depth_count = r.a_depths.len().min(r.b_depths.len());
        for i in 0..depth_count {
            let a = &r.a_depths[i];
            let b = &r.b_depths[i];
            let diff = a.nodes as i64 - b.nodes as i64;
            let marker = if diff != 0 { " *" } else { "" };
            writeln!(
                writer,
                "  d{:>2}: A={:<10} B={:<10} diff={:<+10}{marker}",
                a.depth, a.nodes, b.nodes, diff
            )?;
        }
        writeln!(writer)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// メイン
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    let cli = Cli::parse();

    let workers = cli
        .workers
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get() / 2).unwrap_or(1))
        .max(1);

    // SFEN読み込み（--sfen または --sfens のいずれか必須）
    let (mut sfens, sfens_source) = if let Some(sfen_str) = &cli.sfen {
        let trimmed = sfen_str.trim().to_string();
        (vec![(1, trimmed)], "(直接指定)".to_string())
    } else if let Some(sfens_path) = &cli.sfens {
        let loaded = load_sfens(sfens_path)?;
        let source = format!("{} (ファイル内 {} 件中)", sfens_path.display(), loaded.len());
        (loaded, source)
    } else {
        anyhow::bail!("--sfens または --sfen のいずれかを指定してください");
    };
    let total_loaded = sfens.len();

    // サンプリング
    if cli.sample > 0 && cli.sample < sfens.len() {
        let mut rng = ChaCha8Rng::seed_from_u64(cli.seed);
        sfens.shuffle(&mut rng);
        sfens.truncate(cli.sample);
        sfens.sort_by_key(|(idx, _)| *idx);
    }

    println!("=== compare_nodes ===");
    println!("エンジンA: {}", cli.engine_a.display());
    if !cli.options_a.is_empty() {
        println!("  オプション: {}", cli.options_a.join(", "));
    }
    println!("エンジンB: {}", cli.engine_b.display());
    if !cli.options_b.is_empty() {
        println!("  オプション: {}", cli.options_b.join(", "));
    }
    if total_loaded == 1 {
        println!("局面数: 1 {sfens_source}");
    } else {
        println!("局面数: {} {sfens_source}", sfens.len());
    }
    println!("深度: {}, Hash: {} MB, ワーカー: {}", cli.depth, cli.hash, workers);
    if let Some(eval) = &cli.eval_a {
        println!("EvalFile(A): {}", eval.display());
    }
    if let Some(eval) = &cli.eval_b {
        println!("EvalDir(B): {}", eval.display());
    }
    println!();

    // 出力ディレクトリ作成
    let timestamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let output_dir = cli.output_base.join(&timestamp);
    fs::create_dir_all(&output_dir)
        .with_context(|| format!("出力ディレクトリ作成失敗: {}", output_dir.display()))?;

    // meta.json 書き出し
    let meta = Meta {
        timestamp: Local::now().to_rfc3339(),
        engine_a: cli.engine_a.display().to_string(),
        engine_b: cli.engine_b.display().to_string(),
        options_a: cli.options_a.clone(),
        options_b: cli.options_b.clone(),
        hash_mb: cli.hash,
        eval_a: cli.eval_a.as_ref().map(|p| p.display().to_string()),
        eval_b: cli.eval_b.as_ref().map(|p| p.display().to_string()),
        sfens_file: cli
            .sfens
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(直接指定)".to_string()),
        depth: cli.depth,
        workers,
        sample: cli.sample,
        seed: cli.seed,
        total_positions: sfens.len(),
        reuse_engine: cli.reuse_engine,
    };
    {
        let meta_file = File::create(output_dir.join("meta.json"))?;
        serde_json::to_writer_pretty(BufWriter::new(meta_file), &meta)?;
    }

    let total = sfens.len();

    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({per_sec}) {msg}",
            )
            .expect("valid template"),
    );
    pb.set_message("探索中...");

    let params_a = Arc::new(EngineParams {
        path: cli.engine_a.clone(),
        hash: cli.hash,
        eval_opt_name: "EvalFile",
        eval_path: cli.eval_a.clone(),
        options: cli.options_a.clone(),
    });
    let params_b = Arc::new(EngineParams {
        path: cli.engine_b.clone(),
        hash: cli.hash,
        eval_opt_name: "EvalDir",
        eval_path: cli.eval_b.clone(),
        options: cli.options_b.clone(),
    });
    let depth = cli.depth;

    let results: Vec<PositionResult> = if cli.reuse_engine {
        // TT蓄積モード: エンジンを使い回して逐次処理（--reuse-engine）。
        process_positions_reuse(&params_a, &params_b, &sfens, depth, &pb)
    } else {
        // 通常モード: 局面ごとに新規プロセスを起動して並列処理。
        // 各局面は対局開始時（usinewgame + isready）と同等のクリーンな TT から探索する。
        rayon::ThreadPoolBuilder::new().num_threads(workers).build_global().ok();
        sfens
            .par_iter()
            .filter_map(|(index, sfen)| {
                match process_position(&params_a, &params_b, *index, sfen, depth) {
                    Ok(result) => {
                        pb.inc(1);
                        Some(result)
                    }
                    Err(e) => {
                        eprintln!("position {index} エラー: {e}");
                        pb.inc(1);
                        None
                    }
                }
            })
            .collect()
    };

    pb.finish_with_message("完了");
    println!();

    // results.jsonl 書き出し
    {
        let jsonl_file = File::create(output_dir.join("results.jsonl"))?;
        let mut writer = BufWriter::new(jsonl_file);
        for result in &results {
            serde_json::to_writer(&mut writer, result)?;
            writer.write_all(b"\n")?;
        }
    }

    // summary.txt 書き出し + stdout表示
    {
        let summary_file = File::create(output_dir.join("summary.txt"))?;
        let mut file_writer = BufWriter::new(summary_file);
        write_summary(&mut file_writer, &results, &cli)?;
    }

    // stdoutにも表示
    write_summary(&mut std::io::stdout().lock(), &results, &cli)?;

    println!();
    println!("結果保存先: {}", output_dir.display());

    Ok(())
}
