use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Local;
use clap::Parser;
use engine_core::position::{Position, SFEN_HIRATE};
use engine_core::types::{Color, Move, PieceType, Square};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const ENGINE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const ENGINE_QUIT_TIMEOUT: Duration = Duration::from_millis(300);
const ENGINE_QUIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
// 残り約40手を想定して1手あたりの持ち時間を配分する。
const TIME_ALLOCATION_MOVES: u64 = 40;
const MIN_THINK_MS: u64 = 10;

/// engine-usi 同士の自己対局ハーネス。時間管理と info ログ収集を最小限に実装する。
///
/// # よく使うコマンド例
///
/// - 1秒秒読みで数をこなす（infoログなし、デフォルト出力先）:
///   `cargo run -p engine-usi --bin engine_selfplay -- --games 10 --max-moves 300 --byoyomi 1000`
///
/// - 5秒秒読み + network-delay2=1120、infoログ付きで指定パスに出力:
///   `cargo run -p engine-usi --bin engine_selfplay -- --games 2 --max-moves 300 --byoyomi 5000 --network-delay2 1120 --log-info --out runs/selfplay/byoyomi5s.jsonl`
///
/// - 特定SFENの再現（startposファイルを用意して1局だけ）:
///   `cargo run -p engine-usi --bin engine_selfplay -- --games 1 --max-moves 300 --byoyomi 5000 --startpos-file sfen.txt --log-info`
///
/// `--out` 未指定時は `runs/selfplay/<timestamp>-selfplay.jsonl` に書き出し、infoは同名 `.info.jsonl` を生成する。
///
#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "engine-usi selfplay harness (engine vs engine)"
)]
struct Cli {
    /// Number of games to run
    #[arg(long, default_value_t = 1)]
    games: u32,

    /// Maximum plies per game before declaring a draw
    #[arg(long, default_value_t = 512)]
    max_moves: u32,

    /// Initial time for Black in milliseconds
    #[arg(long, default_value_t = 0)]
    btime: u64,

    /// Initial time for White in milliseconds
    #[arg(long, default_value_t = 0)]
    wtime: u64,

    /// Increment for Black in milliseconds
    #[arg(long, default_value_t = 0)]
    binc: u64,

    /// Increment for White in milliseconds
    #[arg(long, default_value_t = 0)]
    winc: u64,

    /// Byoyomi time per move in milliseconds
    #[arg(long, default_value_t = 0)]
    byoyomi: u64,

    /// Safety margin used when detecting timeouts
    #[arg(long, default_value_t = 1000)]
    timeout_margin_ms: u64,

    /// NetworkDelay USI option (if available)
    #[arg(long)]
    network_delay: Option<i64>,

    /// NetworkDelay2 USI option (if available)
    #[arg(long)]
    network_delay2: Option<i64>,

    /// MinimumThinkingTime USI option (if available)
    #[arg(long)]
    minimum_thinking_time: Option<i64>,

    /// SlowMover USI option (if available)
    #[arg(long)]
    slowmover: Option<i32>,

    /// Enable USI_Ponder (if available)
    #[arg(long, default_value_t = false)]
    ponder: bool,

    /// Threads USI option (default for both sides)
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Threads for Black (overrides --threads)
    #[arg(long)]
    threads_black: Option<usize>,

    /// Threads for White (overrides --threads)
    #[arg(long)]
    threads_white: Option<usize>,

    /// Hash/USI_Hash size (MiB)
    #[arg(long, default_value_t = 1024)]
    hash_mb: u32,

    /// Path to engine-usi binary used when per-side paths are not set
    #[arg(long)]
    engine_path: Option<PathBuf>,

    /// Path to engine-usi binary for Black (overrides engine_path)
    #[arg(long)]
    engine_path_black: Option<PathBuf>,

    /// Path to engine-usi binary for White (overrides engine_path)
    #[arg(long)]
    engine_path_white: Option<PathBuf>,

    /// Common extra arguments passed to engine processes
    #[arg(long, num_args = 1..)]
    engine_args: Option<Vec<String>>,

    /// Extra arguments for Black (overrides engine_args when set)
    #[arg(long, num_args = 1..)]
    engine_args_black: Option<Vec<String>>,

    /// Extra arguments for White (overrides engine_args when set)
    #[arg(long, num_args = 1..)]
    engine_args_white: Option<Vec<String>>,

    /// USI options to set (format: "Name=Value", can be specified multiple times)
    #[arg(long = "usi-option", num_args = 1..)]
    usi_options: Option<Vec<String>>,

    /// USI options for Black (overrides usi_options when set)
    #[arg(long = "usi-option-black", num_args = 1..)]
    usi_options_black: Option<Vec<String>>,

    /// USI options for White (overrides usi_options when set)
    #[arg(long = "usi-option-white", num_args = 1..)]
    usi_options_white: Option<Vec<String>>,

    /// Start position file (USI position lines, one per line)
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// Single start position specified as SFEN or full USI position command
    #[arg(long)]
    sfen: Option<String>,

    /// Output path (defaults to runs/selfplay/<timestamp>-selfplay.jsonl)
    #[arg(long)]
    out: Option<PathBuf>,

    /// Enable info log output
    #[arg(long, default_value_t = false)]
    log_info: bool,

    /// Flush game log on every move (safer, but slower)
    #[arg(long, default_value_t = false)]
    flush_each_move: bool,

    /// 評価値行を別ファイルに書き出す（startpos moves 行 + 評価値列）
    #[arg(long, default_value_t = false)]
    emit_eval_file: bool,

    /// ノード数などの簡易メトリクスを各対局ごとに JSONL で出力
    #[arg(long, default_value_t = false)]
    emit_metrics: bool,
}

#[derive(Serialize, Deserialize)]
struct MetaLog {
    #[serde(rename = "type")]
    kind: String,
    timestamp: String,
    settings: MetaSettings,
    engine_cmd: EngineCommandMeta,
    start_positions: Vec<String>,
    output: String,
    info_log: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct MetaSettings {
    games: u32,
    max_moves: u32,
    btime: u64,
    wtime: u64,
    binc: u64,
    winc: u64,
    byoyomi: u64,
    timeout_margin_ms: u64,
    threads: usize,
    threads_black: usize,
    threads_white: usize,
    hash_mb: u32,
    network_delay: Option<i64>,
    network_delay2: Option<i64>,
    minimum_thinking_time: Option<i64>,
    slowmover: Option<i32>,
    ponder: bool,
    #[serde(default)]
    flush_each_move: bool,
    #[serde(default)]
    emit_eval_file: bool,
    #[serde(default)]
    emit_metrics: bool,
    startpos_file: Option<String>,
    sfen: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct EngineCommandMeta {
    path_black: String,
    path_white: String,
    source_black: String,
    source_white: String,
    args_black: Vec<String>,
    args_white: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    usi_options_black: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    usi_options_white: Vec<String>,
}

/// バイナリの発見元を含む解決結果。
#[derive(Clone)]
struct ResolvedEnginePath {
    path: PathBuf,
    source: &'static str,
}

/// 先手と後手のエンジンバイナリパスの解決結果。
/// 各プレイヤーに異なるエンジンバイナリを使用できるようにする。
struct ResolvedEnginePaths {
    /// 先手（Black）のエンジンバイナリパス
    black: ResolvedEnginePath,
    /// 後手（White）のエンジンバイナリパス
    white: ResolvedEnginePath,
}

#[derive(Serialize)]
struct MoveLog {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    ply: u32,
    side_to_move: char,
    sfen_before: String,
    move_usi: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_move_usi: Option<String>,
    engine: &'static str,
    elapsed_ms: u64,
    think_limit_ms: u64,
    timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    eval: Option<EvalLog>,
}

#[derive(Serialize)]
struct ResultLog<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    outcome: &'a str,
    reason: &'a str,
    plies: u32,
}

#[derive(Serialize)]
struct MetricsLog {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    plies: u32,
    nodes_black: u64,
    nodes_white: u64,
    nodes_first60: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_cp_black: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_cp_white: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_mate_black: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_mate_white: Option<i32>,
    outcome: String,
    reason: String,
}

/// 対局セッション全体のサマリ
#[derive(Serialize)]
struct SummaryLog {
    #[serde(rename = "type")]
    kind: &'static str,
    timestamp: String,
    total_games: u32,
    black_wins: u32,
    white_wins: u32,
    draws: u32,
    black_win_rate: f64,
    white_win_rate: f64,
    draw_rate: f64,
    engine_black: EngineSummary,
    engine_white: EngineSummary,
    time_control: TimeControlSummary,
}

#[derive(Serialize)]
struct EngineSummary {
    path: String,
    name: String,
    usi_options: Vec<String>,
    threads: usize,
}

#[derive(Serialize)]
struct TimeControlSummary {
    btime: u64,
    wtime: u64,
    binc: u64,
    winc: u64,
    byoyomi: u64,
}

#[derive(Default)]
struct MetricsCollector {
    nodes_black: u64,
    nodes_white: u64,
    nodes_first60: u64,
    last_cp_black: Option<i32>,
    last_cp_white: Option<i32>,
    last_mate_black: Option<i32>,
    last_mate_white: Option<i32>,
}

impl MetricsCollector {
    fn update(&mut self, side: Color, eval: Option<&EvalLog>, ply: u32) {
        let Some(eval) = eval else { return };
        if let Some(nodes) = eval.nodes {
            if side == Color::Black {
                self.nodes_black = self.nodes_black.saturating_add(nodes);
            } else {
                self.nodes_white = self.nodes_white.saturating_add(nodes);
            }
            if ply <= 60 {
                self.nodes_first60 = self.nodes_first60.saturating_add(nodes);
            }
        }
        if let Some(mate) = eval.score_mate {
            if side == Color::Black {
                self.last_mate_black = Some(mate);
                self.last_cp_black = None;
            } else {
                self.last_mate_white = Some(mate);
                self.last_cp_white = None;
            }
        } else if let Some(cp) = eval.score_cp {
            if side == Color::Black {
                self.last_cp_black = Some(cp);
                self.last_mate_black = None;
            } else {
                self.last_cp_white = Some(cp);
                self.last_mate_white = None;
            }
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct EvalLog {
    #[serde(skip_serializing_if = "Option::is_none")]
    score_cp: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score_mate: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seldepth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pv: Option<Vec<String>>,
}

#[derive(Serialize)]
struct InfoLogEntry<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    game_id: u32,
    ply: u32,
    side_to_move: char,
    engine: &'a str,
    line: &'a str,
}

struct InfoLogger {
    writer: BufWriter<File>,
}

impl InfoLogger {
    fn new(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create info-log directory {}", parent.display())
                })?;
            }
        }
        let file = File::create(path)
            .with_context(|| format!("failed to create info log {}", path.display()))?;
        Ok(Self {
            writer: BufWriter::new(file),
        })
    }

    fn log(&mut self, entry: InfoLogEntry<'_>) -> Result<()> {
        serde_json::to_writer(&mut self.writer, &entry)?;
        self.writer.write_all(b"\n")?;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }
}

#[derive(Default, Clone)]
struct InfoSnapshot {
    score_cp: Option<i32>,
    score_mate: Option<i32>,
    depth: Option<u32>,
    seldepth: Option<u32>,
    nodes: Option<u64>,
    time_ms: Option<u64>,
    nps: Option<u64>,
    pv: Vec<String>,
}

impl InfoSnapshot {
    /// info 行を解析し、multipv=1 の情報を保持する。
    fn update_from_line(&mut self, line: &str) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first().copied() != Some("info") {
            return;
        }
        let mut multipv = 1u32;
        let mut idx = 1;
        while idx + 1 < tokens.len() {
            if tokens[idx] == "multipv" {
                multipv = tokens[idx + 1].parse::<u32>().unwrap_or(1);
                break;
            }
            idx += 1;
        }
        if multipv != 1 {
            return;
        }
        let mut i = 1;
        while i < tokens.len() {
            match tokens[i] {
                "depth" => {
                    if i + 1 < tokens.len() {
                        self.depth = tokens[i + 1].parse::<u32>().ok();
                        i += 1;
                    }
                }
                "seldepth" => {
                    if i + 1 < tokens.len() {
                        self.seldepth = tokens[i + 1].parse::<u32>().ok();
                        i += 1;
                    }
                }
                "nodes" => {
                    if i + 1 < tokens.len() {
                        self.nodes = tokens[i + 1].parse::<u64>().ok();
                        i += 1;
                    }
                }
                "time" => {
                    if i + 1 < tokens.len() {
                        self.time_ms = tokens[i + 1].parse::<u64>().ok();
                        i += 1;
                    }
                }
                "nps" => {
                    if i + 1 < tokens.len() {
                        self.nps = tokens[i + 1].parse::<u64>().ok();
                        i += 1;
                    }
                }
                "score" => {
                    if i + 2 < tokens.len() {
                        match tokens[i + 1] {
                            "cp" => {
                                self.score_cp = tokens[i + 2].parse::<i32>().ok();
                                self.score_mate = None;
                                i += 2;
                            }
                            "mate" => {
                                self.score_mate = tokens[i + 2].parse::<i32>().ok();
                                self.score_cp = None;
                                i += 2;
                            }
                            _ => {}
                        }
                    }
                }
                "pv" => {
                    let mut pv = Vec::new();
                    let mut j = i + 1;
                    while j < tokens.len() {
                        pv.push(tokens[j].to_string());
                        j += 1;
                    }
                    if !pv.is_empty() {
                        self.pv = pv;
                    }
                    break;
                }
                _ => {}
            }
            i += 1;
        }
    }

    fn into_eval_log(self) -> Option<EvalLog> {
        if self.score_cp.is_none()
            && self.score_mate.is_none()
            && self.depth.is_none()
            && self.seldepth.is_none()
            && self.nodes.is_none()
            && self.time_ms.is_none()
            && self.nps.is_none()
            && self.pv.is_empty()
        {
            return None;
        }
        Some(EvalLog {
            score_cp: self.score_cp,
            score_mate: self.score_mate,
            depth: self.depth,
            seldepth: self.seldepth,
            nodes: self.nodes,
            time_ms: self.time_ms,
            nps: self.nps,
            pv: if self.pv.is_empty() {
                None
            } else {
                Some(self.pv)
            },
        })
    }
}

#[derive(Clone, Copy)]
struct TimeArgs {
    btime: u64,
    wtime: u64,
    byoyomi: u64,
    binc: u64,
    winc: u64,
}

/// USI 互換の時間管理を最低限行うヘルパー。
#[derive(Clone, Copy)]
struct TimeControl {
    black_time: u64,
    white_time: u64,
    black_inc: u64,
    white_inc: u64,
    byoyomi: u64,
}

impl TimeControl {
    fn new(cli: &Cli) -> Self {
        Self {
            black_time: cli.btime,
            white_time: cli.wtime,
            black_inc: cli.binc,
            white_inc: cli.winc,
            byoyomi: cli.byoyomi,
        }
    }

    fn time_args(&self) -> TimeArgs {
        TimeArgs {
            btime: self.black_time,
            wtime: self.white_time,
            byoyomi: self.byoyomi,
            binc: self.black_inc,
            winc: self.white_inc,
        }
    }

    /// 残り時間を分割して1手あたりの思考上限を決める。
    fn think_limit_ms(&self, side: Color) -> u64 {
        let remaining = self.remaining(side);
        let inc = self.increment_for(side);
        if self.byoyomi > 0 {
            let available = remaining.saturating_add(self.byoyomi);
            let per_move_budget = remaining / TIME_ALLOCATION_MOVES;
            let candidate = self.byoyomi.saturating_add(per_move_budget);
            let lower = self.byoyomi.max(MIN_THINK_MS.min(available));
            return candidate.clamp(lower, available);
        }
        let per_move_budget = remaining / TIME_ALLOCATION_MOVES;
        let candidate = per_move_budget.saturating_add(inc);
        let lower = MIN_THINK_MS.min(remaining);
        candidate.clamp(lower, remaining)
    }

    fn remaining(&self, side: Color) -> u64 {
        if side == Color::Black {
            self.black_time
        } else {
            self.white_time
        }
    }

    fn increment_for(&self, side: Color) -> u64 {
        if side == Color::Black {
            self.black_inc
        } else {
            self.white_inc
        }
    }

    fn update_after_move(&mut self, side: Color, elapsed_ms: u64) {
        if side == Color::Black {
            self.black_time = self.updated_time(self.black_time, self.black_inc, elapsed_ms);
        } else {
            self.white_time = self.updated_time(self.white_time, self.white_inc, elapsed_ms);
        }
    }

    fn updated_time(&self, current: u64, inc: u64, elapsed_ms: u64) -> u64 {
        let mut next = current;
        if self.byoyomi > 0 {
            let over = elapsed_ms.saturating_sub(self.byoyomi);
            next = next.saturating_sub(over);
        } else {
            next = next.saturating_sub(elapsed_ms);
        }
        next = next.saturating_add(inc);
        next
    }
}

/// エンジンプロセス起動時の設定。
struct EngineConfig {
    path: PathBuf,
    args: Vec<String>,
    threads: usize,
    hash_mb: u32,
    network_delay: Option<i64>,
    network_delay2: Option<i64>,
    minimum_thinking_time: Option<i64>,
    slowmover: Option<i32>,
    ponder: bool,
    /// 追加のUSIオプション (Name=Value 形式)
    usi_options: Vec<String>,
}

/// 1本のエンジンに対する入出力をカプセル化する。
struct EngineProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    rx: Receiver<String>,
    opt_names: HashSet<String>,
    label: &'static str,
}

impl EngineProcess {
    fn spawn(cfg: &EngineConfig, label: &'static str) -> Result<Self> {
        let mut cmd = Command::new(&cfg.path);
        if !cfg.args.is_empty() {
            cmd.args(&cfg.args);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn engine at {}", cfg.path.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut proc = Self {
            child,
            stdin: BufWriter::new(stdin),
            rx,
            opt_names: HashSet::new(),
            label,
        };
        proc.initialize(cfg)?;
        Ok(proc)
    }

    fn initialize(&mut self, cfg: &EngineConfig) -> Result<()> {
        self.write_line("usi")?;
        loop {
            let line = self.recv_line(ENGINE_READY_TIMEOUT)?;
            if let Some(rest) = line.strip_prefix("option ") {
                if let Some(name) = parse_option_name(rest) {
                    self.opt_names.insert(name);
                }
            } else if line == "usiok" {
                break;
            }
        }
        self.set_option_if_available("Threads", &cfg.threads.to_string())?;
        let hash = cfg.hash_mb.to_string();
        self.set_option_if_available("USI_Hash", &hash)?;
        self.set_option_if_available("Hash", &hash)?;
        if let Some(v) = cfg.network_delay {
            self.set_option_if_available("NetworkDelay", &v.to_string())?;
        }
        if let Some(v) = cfg.network_delay2 {
            self.set_option_if_available("NetworkDelay2", &v.to_string())?;
        }
        if let Some(v) = cfg.minimum_thinking_time {
            self.set_option_if_available("MinimumThinkingTime", &v.to_string())?;
        }
        if let Some(v) = cfg.slowmover {
            self.set_option_if_available("SlowMover", &v.to_string())?;
        }
        if self.opt_names.contains("USI_Ponder") || self.opt_names.contains("Ponder") {
            let name = if self.opt_names.contains("USI_Ponder") {
                "USI_Ponder"
            } else {
                "Ponder"
            };
            self.set_option_if_available(name, if cfg.ponder { "true" } else { "false" })?;
        }
        // 追加のUSIオプションを設定
        for opt in &cfg.usi_options {
            if let Some((name, value)) = opt.split_once('=') {
                self.set_option_if_available(name.trim(), value.trim())?;
            } else {
                // "=" がない場合はオプション名のみとみなし、値なしで送る
                self.write_line(&format!("setoption name {}", opt.trim()))?;
            }
        }
        self.sync_ready()?;
        self.write_line("usinewgame")?;
        Ok(())
    }

    fn new_game(&mut self) -> Result<()> {
        self.write_line("usinewgame")
    }

    fn search(
        &mut self,
        req: &SearchRequest<'_>,
        info_logger: &mut Option<InfoLogger>,
    ) -> Result<SearchOutcome> {
        self.write_line(&format!("position sfen {}", req.sfen))?;
        let time_args = &req.time_args;
        self.write_line(&format!(
            "go btime {} wtime {} byoyomi {} binc {} winc {}",
            time_args.btime, time_args.wtime, time_args.byoyomi, time_args.binc, time_args.winc
        ))?;

        let start = Instant::now();
        let soft_limit =
            Duration::from_millis(req.think_limit_ms.saturating_add(req.timeout_margin_ms));
        let hard_limit = soft_limit + Duration::from_millis(req.timeout_margin_ms);
        let mut stop_sent = false;
        let mut snapshot = InfoSnapshot::default();

        loop {
            let elapsed = start.elapsed();
            let deadline = if stop_sent { hard_limit } else { soft_limit };
            if elapsed >= deadline {
                if !stop_sent {
                    self.write_line("stop")?;
                    stop_sent = true;
                    continue;
                }
                return Ok(SearchOutcome {
                    bestmove: None,
                    elapsed_ms: duration_to_millis(elapsed),
                    timed_out: true,
                    eval: snapshot.into_eval_log(),
                });
            }

            let remaining = deadline.saturating_sub(elapsed);
            match self.rx.recv_timeout(remaining) {
                Ok(line) => {
                    if line.starts_with("info") {
                        snapshot.update_from_line(&line);
                        if let Some(logger) = info_logger.as_mut() {
                            logger.log(InfoLogEntry {
                                kind: "info",
                                game_id: req.game_id,
                                ply: req.ply,
                                side_to_move: side_label(req.side),
                                engine: req.engine_label,
                                line: &line,
                            })?;
                        }
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix("bestmove ") {
                        let mut parts = rest.split_whitespace();
                        let mv = parts.next().unwrap_or_default().to_string();
                        let elapsed_ms = duration_to_millis(start.elapsed());
                        let timed_out =
                            elapsed_ms > req.think_limit_ms.saturating_add(req.timeout_margin_ms);
                        return Ok(SearchOutcome {
                            bestmove: Some(mv),
                            elapsed_ms,
                            timed_out,
                            eval: snapshot.into_eval_log(),
                        });
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !stop_sent {
                        self.write_line("stop")?;
                        stop_sent = true;
                    } else {
                        let elapsed_ms = duration_to_millis(start.elapsed());
                        return Ok(SearchOutcome {
                            bestmove: None,
                            elapsed_ms,
                            timed_out: true,
                            eval: snapshot.into_eval_log(),
                        });
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("{}: engine exited unexpectedly", self.label);
                }
            }
        }
    }

    fn sync_ready(&mut self) -> Result<()> {
        self.write_line("isready")?;
        loop {
            let line = self.recv_line(ENGINE_READY_TIMEOUT)?;
            if line == "readyok" {
                break;
            }
        }
        Ok(())
    }

    fn recv_line(&self, timeout: Duration) -> Result<String> {
        self.rx
            .recv_timeout(timeout)
            .map_err(|_| anyhow!("{}: engine read timeout", self.label))
    }

    fn set_option_if_available(&mut self, name: &str, value: &str) -> Result<()> {
        if self.opt_names.is_empty() || self.opt_names.contains(name) {
            self.write_line(&format!("setoption name {} value {}", name, value))?;
        }
        Ok(())
    }

    fn write_line(&mut self, msg: &str) -> Result<()> {
        self.stdin.write_all(msg.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }
}

impl Drop for EngineProcess {
    fn drop(&mut self) {
        let _ = self.write_line("quit");
        let deadline = Instant::now() + ENGINE_QUIT_TIMEOUT;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(ENGINE_QUIT_POLL_INTERVAL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct SearchRequest<'a> {
    sfen: &'a str,
    time_args: TimeArgs,
    think_limit_ms: u64,
    timeout_margin_ms: u64,
    game_id: u32,
    ply: u32,
    side: Color,
    engine_label: &'static str,
}

struct SearchOutcome {
    bestmove: Option<String>,
    elapsed_ms: u64,
    timed_out: bool,
    eval: Option<EvalLog>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GameOutcome {
    InProgress,
    BlackWin,
    WhiteWin,
    Draw,
}

impl GameOutcome {
    fn label(self) -> &'static str {
        match self {
            GameOutcome::InProgress => "in_progress",
            GameOutcome::BlackWin => "black_win",
            GameOutcome::WhiteWin => "white_win",
            GameOutcome::Draw => "draw",
        }
    }
}

fn main() -> Result<()> {
    let mut cli = Cli::parse();

    // 時間制限のバリデーション: すべて0の場合は無限思考モードになりタイムアウト問題が発生するため警告
    if cli.btime == 0 && cli.wtime == 0 && cli.byoyomi == 0 && cli.binc == 0 && cli.winc == 0 {
        eprintln!(
            "Warning: No time control specified. Using default byoyomi=1000ms to prevent infinite thinking."
        );
        cli.byoyomi = 1000;
    }

    let (start_defs, start_commands) =
        load_start_positions(cli.startpos_file.as_deref(), cli.sfen.as_deref())?;
    let timestamp = Local::now();
    let output_path = resolve_output_path(cli.out.as_deref(), &timestamp);
    let info_path = output_path.with_extension("info.jsonl");

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
    }
    let mut writer = BufWriter::new(
        File::create(&output_path)
            .with_context(|| format!("failed to open {}", output_path.display()))?,
    );
    let mut info_logger = if cli.log_info {
        Some(InfoLogger::new(&info_path)?)
    } else {
        None
    };
    let mut eval_writer = if cli.emit_eval_file {
        let eval_path = default_eval_path(&output_path);
        if let Some(parent) = eval_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        Some(BufWriter::new(
            File::create(&eval_path)
                .with_context(|| format!("failed to create {}", eval_path.display()))?,
        ))
    } else {
        None
    };
    let mut metrics_writer = if cli.emit_metrics {
        let metrics_path = default_metrics_path(&output_path);
        if let Some(parent) = metrics_path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
        }
        Some(BufWriter::new(
            File::create(&metrics_path)
                .with_context(|| format!("failed to create {}", metrics_path.display()))?,
        ))
    } else {
        None
    };

    let engine_paths = resolve_engine_paths(&cli);
    let threads_black = cli.threads_black.unwrap_or(cli.threads);
    let threads_white = cli.threads_white.unwrap_or(cli.threads);

    if engine_paths.black.path == engine_paths.white.path
        && engine_paths.black.source == engine_paths.white.source
    {
        let engine_path_display = engine_paths.black.path.display();
        let engine_path_source = engine_paths.black.source;
        println!("using engine binary: {engine_path_display} ({engine_path_source})");
    } else {
        println!(
            "using engine binaries: black={} ({}), white={} ({})",
            engine_paths.black.path.display(),
            engine_paths.black.source,
            engine_paths.white.path.display(),
            engine_paths.white.source
        );
    }
    if threads_black == threads_white {
        println!("threads: {threads_black}");
    } else {
        println!("threads: black={threads_black}, white={threads_white}");
    }
    let common_args = cli.engine_args.clone().unwrap_or_default();
    let black_args = cli.engine_args_black.clone().unwrap_or_else(|| common_args.clone());
    let white_args = cli.engine_args_white.clone().unwrap_or(common_args.clone());

    let common_usi_opts = cli.usi_options.clone().unwrap_or_default();
    let black_usi_opts = cli.usi_options_black.clone().unwrap_or_else(|| common_usi_opts.clone());
    let white_usi_opts = cli.usi_options_white.clone().unwrap_or_else(|| common_usi_opts.clone());

    let mut black = EngineProcess::spawn(
        &EngineConfig {
            path: engine_paths.black.path.clone(),
            args: black_args.clone(),
            threads: threads_black,
            hash_mb: cli.hash_mb,
            network_delay: cli.network_delay,
            network_delay2: cli.network_delay2,
            minimum_thinking_time: cli.minimum_thinking_time,
            slowmover: cli.slowmover,
            ponder: cli.ponder,
            usi_options: black_usi_opts.clone(),
        },
        "black",
    )?;
    let mut white = EngineProcess::spawn(
        &EngineConfig {
            path: engine_paths.white.path.clone(),
            args: white_args.clone(),
            threads: threads_white,
            hash_mb: cli.hash_mb,
            network_delay: cli.network_delay,
            network_delay2: cli.network_delay2,
            minimum_thinking_time: cli.minimum_thinking_time,
            slowmover: cli.slowmover,
            ponder: cli.ponder,
            usi_options: white_usi_opts.clone(),
        },
        "white",
    )?;

    let meta = MetaLog {
        kind: "meta".to_string(),
        timestamp: timestamp.to_rfc3339(),
        settings: MetaSettings {
            games: cli.games,
            max_moves: cli.max_moves,
            btime: cli.btime,
            wtime: cli.wtime,
            binc: cli.binc,
            winc: cli.winc,
            byoyomi: cli.byoyomi,
            timeout_margin_ms: cli.timeout_margin_ms,
            threads: cli.threads,
            threads_black,
            threads_white,
            hash_mb: cli.hash_mb,
            network_delay: cli.network_delay,
            network_delay2: cli.network_delay2,
            minimum_thinking_time: cli.minimum_thinking_time,
            slowmover: cli.slowmover,
            ponder: cli.ponder,
            flush_each_move: cli.flush_each_move,
            emit_eval_file: cli.emit_eval_file,
            emit_metrics: cli.emit_metrics,
            startpos_file: cli.startpos_file.as_ref().map(|p| p.display().to_string()),
            sfen: cli.sfen.clone(),
        },
        engine_cmd: EngineCommandMeta {
            path_black: engine_paths.black.path.display().to_string(),
            path_white: engine_paths.white.path.display().to_string(),
            source_black: engine_paths.black.source.to_string(),
            source_white: engine_paths.white.source.to_string(),
            args_black: black_args.clone(),
            args_white: white_args.clone(),
            usi_options_black: black_usi_opts.clone(),
            usi_options_white: white_usi_opts.clone(),
        },
        start_positions: start_commands.clone(),
        output: output_path.display().to_string(),
        info_log: cli.log_info.then(|| info_path.display().to_string()),
    };
    serde_json::to_writer(&mut writer, &meta)?;
    writer.write_all(b"\n")?;

    // 勝敗カウンター
    let mut black_wins = 0u32;
    let mut white_wins = 0u32;
    let mut draws = 0u32;

    for game_idx in 0..cli.games {
        black.new_game()?;
        white.new_game()?;
        let parsed = &start_defs[(game_idx as usize) % start_defs.len()];
        let mut pos = build_position(parsed)?;
        let mut tc = TimeControl::new(&cli);
        let mut outcome = GameOutcome::InProgress;
        let mut outcome_reason = "max_moves";
        let mut plies_played = 0u32;
        let mut move_list: Vec<String> = Vec::new();
        let mut eval_list: Vec<String> = Vec::new();
        let mut metrics = MetricsCollector::default();

        for ply_idx in 0..cli.max_moves {
            plies_played = ply_idx + 1;
            let side = pos.side_to_move();
            let engine = if side == Color::Black {
                &mut black
            } else {
                &mut white
            };
            let engine_label = if side == Color::Black {
                "black"
            } else {
                "white"
            };
            let sfen_before = pos.to_sfen();
            let think_limit_ms = tc.think_limit_ms(side);
            let req = SearchRequest {
                sfen: &sfen_before,
                time_args: tc.time_args(),
                think_limit_ms,
                timeout_margin_ms: cli.timeout_margin_ms,
                game_id: game_idx + 1,
                ply: plies_played,
                side,
                engine_label,
            };
            let search = engine.search(&req, &mut info_logger)?;

            let timed_out = search.timed_out;
            let mut move_usi = search.bestmove.clone().unwrap_or_else(|| "none".to_string());
            let mut raw_move_usi = None;
            let mut terminal = false;
            let elapsed_ms = search.elapsed_ms;
            let eval_log = search.eval.clone();

            if timed_out {
                outcome = if side == Color::Black {
                    GameOutcome::WhiteWin
                } else {
                    GameOutcome::BlackWin
                };
                outcome_reason = "timeout";
                terminal = true;
                if search.bestmove.is_none() {
                    move_usi = "timeout".to_string();
                }
            } else if let Some(ref mv_str) = search.bestmove {
                raw_move_usi = Some(mv_str.clone());
                match mv_str.as_str() {
                    "resign" => {
                        move_usi = mv_str.clone();
                        outcome = if side == Color::Black {
                            GameOutcome::WhiteWin
                        } else {
                            GameOutcome::BlackWin
                        };
                        outcome_reason = "resign";
                        terminal = true;
                    }
                    "win" => {
                        move_usi = mv_str.clone();
                        outcome = if side == Color::Black {
                            GameOutcome::BlackWin
                        } else {
                            GameOutcome::WhiteWin
                        };
                        outcome_reason = "win";
                        terminal = true;
                    }
                    _ => match Move::from_usi(mv_str) {
                        Some(mv) if pos.is_legal(mv) => {
                            let gives_check = pos.gives_check(mv);
                            pos.do_move(mv, gives_check);
                            tc.update_after_move(side, search.elapsed_ms);
                            move_usi = mv_str.clone();
                            raw_move_usi = None;
                        }
                        _ => {
                            outcome = if side == Color::Black {
                                GameOutcome::WhiteWin
                            } else {
                                GameOutcome::BlackWin
                            };
                            outcome_reason = "illegal_move";
                            terminal = true;
                            move_usi = "illegal".to_string();
                        }
                    },
                }
            } else {
                outcome = if side == Color::Black {
                    GameOutcome::WhiteWin
                } else {
                    GameOutcome::BlackWin
                };
                outcome_reason = "no_bestmove";
                terminal = true;
            }

            if cli.emit_eval_file {
                eval_list.push(eval_label(eval_log.as_ref()));
                move_list.push(move_usi.clone());
            }

            if cli.emit_metrics {
                metrics.update(side, eval_log.as_ref(), plies_played);
            }

            let move_log = MoveLog {
                kind: "move",
                game_id: game_idx + 1,
                ply: plies_played,
                side_to_move: side_label(side),
                sfen_before,
                move_usi,
                raw_move_usi,
                engine: engine_label,
                elapsed_ms,
                think_limit_ms,
                timed_out,
                eval: eval_log,
            };
            serde_json::to_writer(&mut writer, &move_log)?;
            writer.write_all(b"\n")?;
            if cli.flush_each_move {
                writer.flush()?;
            }

            if terminal || outcome != GameOutcome::InProgress {
                break;
            }
        }

        if outcome == GameOutcome::InProgress {
            outcome = GameOutcome::Draw;
            outcome_reason = "max_moves";
        }
        let result = ResultLog {
            kind: "result",
            game_id: game_idx + 1,
            outcome: outcome.label(),
            reason: outcome_reason,
            plies: plies_played,
        };
        serde_json::to_writer(&mut writer, &result)?;
        writer.write_all(b"\n")?;
        if cli.emit_eval_file {
            if let Some(w) = eval_writer.as_mut() {
                let start_cmd = &start_commands[(game_idx as usize) % start_commands.len()];
                let moves_text = if move_list.is_empty() {
                    String::new()
                } else {
                    format!(" moves {}", move_list.join(" "))
                };
                writeln!(w, "game {}: {}{}", game_idx + 1, start_cmd, moves_text)?;
                if !eval_list.is_empty() {
                    writeln!(w, "eval {}", eval_list.join(" "))?;
                } else {
                    writeln!(w, "eval")?;
                }
                writeln!(w)?;
            }
        }
        if cli.emit_metrics {
            if let Some(w) = metrics_writer.as_mut() {
                let metrics_log = MetricsLog {
                    kind: "metrics",
                    game_id: game_idx + 1,
                    plies: plies_played,
                    nodes_black: metrics.nodes_black,
                    nodes_white: metrics.nodes_white,
                    nodes_first60: metrics.nodes_first60,
                    last_cp_black: metrics.last_cp_black,
                    last_cp_white: metrics.last_cp_white,
                    last_mate_black: metrics.last_mate_black,
                    last_mate_white: metrics.last_mate_white,
                    outcome: outcome.label().to_string(),
                    reason: outcome_reason.to_string(),
                };
                serde_json::to_writer(&mut *w, &metrics_log)?;
                w.write_all(b"\n")?;
            }
        }
        writer.flush()?;

        // 勝敗カウント更新
        match outcome {
            GameOutcome::BlackWin => black_wins += 1,
            GameOutcome::WhiteWin => white_wins += 1,
            GameOutcome::Draw => draws += 1,
            GameOutcome::InProgress => {}
        }

        // 進捗表示
        println!(
            "game {}/{}: {} ({}) - black {} / white {} / draw {}",
            game_idx + 1,
            cli.games,
            outcome.label(),
            outcome_reason,
            black_wins,
            white_wins,
            draws
        );
    }

    // 最終サマリー
    println!();
    println!("=== Result Summary ===");
    println!(
        "Total: {} games | Black wins: {} | White wins: {} | Draws: {}",
        cli.games, black_wins, white_wins, draws
    );
    if cli.games > 0 {
        let black_rate = (black_wins as f64 / cli.games as f64) * 100.0;
        let white_rate = (white_wins as f64 / cli.games as f64) * 100.0;
        let draw_rate = (draws as f64 / cli.games as f64) * 100.0;
        println!(
            "Win rate: Black {:.1}% | White {:.1}% | Draw {:.1}%",
            black_rate, white_rate, draw_rate
        );
    }
    println!();
    println!("--- Engine Settings ---");
    println!("Black: {}", format_engine_settings(&engine_paths.black, &black_usi_opts));
    println!("White: {}", format_engine_settings(&engine_paths.white, &white_usi_opts));
    println!("=======================");
    println!();

    // サマリファイル出力
    let summary_path = default_summary_path(&output_path);
    {
        let black_rate = if cli.games > 0 {
            (black_wins as f64 / cli.games as f64) * 100.0
        } else {
            0.0
        };
        let white_rate = if cli.games > 0 {
            (white_wins as f64 / cli.games as f64) * 100.0
        } else {
            0.0
        };
        let draw_rate = if cli.games > 0 {
            (draws as f64 / cli.games as f64) * 100.0
        } else {
            0.0
        };

        let summary = SummaryLog {
            kind: "summary",
            timestamp: timestamp.to_rfc3339(),
            total_games: cli.games,
            black_wins,
            white_wins,
            draws,
            black_win_rate: black_rate,
            white_win_rate: white_rate,
            draw_rate,
            engine_black: EngineSummary {
                path: engine_paths.black.path.display().to_string(),
                name: engine_paths
                    .black
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("engine-usi")
                    .to_string(),
                usi_options: black_usi_opts.clone(),
                threads: threads_black,
            },
            engine_white: EngineSummary {
                path: engine_paths.white.path.display().to_string(),
                name: engine_paths
                    .white
                    .path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("engine-usi")
                    .to_string(),
                usi_options: white_usi_opts.clone(),
                threads: threads_white,
            },
            time_control: TimeControlSummary {
                btime: cli.btime,
                wtime: cli.wtime,
                binc: cli.binc,
                winc: cli.winc,
                byoyomi: cli.byoyomi,
            },
        };

        let mut summary_writer = BufWriter::new(
            File::create(&summary_path)
                .with_context(|| format!("failed to create {}", summary_path.display()))?,
        );
        serde_json::to_writer(&mut summary_writer, &summary)?;
        summary_writer.write_all(b"\n")?;
        summary_writer.flush()?;
    }

    if let Some(logger) = info_logger.as_mut() {
        logger.flush()?;
    }
    if let Some(w) = eval_writer.as_mut() {
        w.flush()?;
    }
    if let Some(w) = metrics_writer.as_mut() {
        w.flush()?;
    }
    writer.flush()?;
    println!("selfplay log written to {}", output_path.display());
    println!("summary written to {}", summary_path.display());
    if cli.log_info {
        println!("info log written to {}", info_path.display());
    }
    let kif_path = default_kif_path(&output_path);
    match convert_jsonl_to_kif(&output_path, &kif_path) {
        Ok(paths) if paths.is_empty() => eprintln!("failed to create KIF: no games found"),
        Ok(paths) if paths.len() == 1 => println!("kif written to {}", paths[0].display()),
        Ok(paths) => {
            println!("kif written (per game):");
            for p in paths {
                println!("  {}", p.display());
            }
        }
        Err(err) => eprintln!("failed to create KIF: {}", err),
    }
    Ok(())
}

fn resolve_output_path(out: Option<&Path>, timestamp: &chrono::DateTime<Local>) -> PathBuf {
    if let Some(path) = out {
        return path.to_path_buf();
    }
    let dir = PathBuf::from("runs/selfplay");
    let name = format!("{}-selfplay.jsonl", timestamp.format("%Y%m%d-%H%M%S"));
    dir.join(name)
}

fn default_kif_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.kif"))
}

fn default_eval_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.eval.txt"))
}

fn default_metrics_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.metrics.jsonl"))
}

fn default_summary_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.summary.jsonl"))
}

fn resolve_engine_paths(cli: &Cli) -> ResolvedEnginePaths {
    let shared = resolve_engine_path(cli);
    let black = cli
        .engine_path_black
        .as_ref()
        .map(|path| ResolvedEnginePath {
            path: path.clone(),
            source: "cli:black",
        })
        .unwrap_or_else(|| shared.clone());
    let white = cli
        .engine_path_white
        .as_ref()
        .map(|path| ResolvedEnginePath {
            path: path.clone(),
            source: "cli:white",
        })
        .unwrap_or_else(|| shared.clone());
    ResolvedEnginePaths { black, white }
}

/// エンジンバイナリを探す。明示指定 > 環境変数 > 同ディレクトリの release > debug > フォールバックの優先順位。
fn resolve_engine_path(cli: &Cli) -> ResolvedEnginePath {
    if let Some(path) = &cli.engine_path {
        return ResolvedEnginePath {
            path: path.clone(),
            source: "cli",
        };
    }
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_engine-usi") {
        return ResolvedEnginePath {
            path: PathBuf::from(p),
            source: "cargo-env",
        };
    }
    if let Ok(exec) = std::env::current_exe() {
        if let Some(dir) = exec.parent() {
            if let Some(found) = find_engine_in_dir(dir) {
                return found;
            }
        }
    }
    ResolvedEnginePath {
        path: PathBuf::from("engine-usi"),
        source: "fallback",
    }
}

fn find_engine_in_dir(dir: &Path) -> Option<ResolvedEnginePath> {
    #[cfg(windows)]
    let release_names = ["engine-usi.exe"];
    #[cfg(not(windows))]
    let release_names = ["engine-usi"];
    #[cfg(windows)]
    let debug_names = ["engine-usi-debug.exe"];
    #[cfg(not(windows))]
    let debug_names = ["engine-usi-debug"];

    for name in release_names {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(ResolvedEnginePath {
                path: candidate,
                source: "auto:release",
            });
        }
    }
    for name in debug_names {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(ResolvedEnginePath {
                path: candidate,
                source: "auto:debug",
            });
        }
    }
    None
}

fn load_start_positions(
    file: Option<&Path>,
    sfen: Option<&str>,
) -> Result<(Vec<ParsedPosition>, Vec<String>)> {
    match (file, sfen) {
        (Some(_), Some(_)) => {
            bail!("--startpos-file and --sfen cannot be used together");
        }
        (Some(path), None) => {
            let file =
                File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
            let reader = BufReader::new(file);
            let mut positions = Vec::new();
            let mut commands = Vec::new();
            for (idx, line) in reader.lines().enumerate() {
                let line = line?;
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let parsed = parse_position_line(trimmed).with_context(|| {
                    format!("invalid position syntax on line {}: {}", idx + 1, trimmed)
                })?;
                build_position(&parsed)?;
                let cmd = describe_position(&parsed);
                positions.push(parsed);
                commands.push(cmd);
            }
            if positions.is_empty() {
                bail!("no usable positions found in {}", path.display());
            }
            Ok((positions, commands))
        }
        (None, Some(sfen_arg)) => {
            let parsed = parse_position_line(sfen_arg).or_else(|_| parse_sfen_only(sfen_arg))?;
            build_position(&parsed)?;
            let cmd = describe_position(&parsed);
            Ok((vec![parsed], vec![cmd]))
        }
        (None, None) => {
            let parsed = ParsedPosition {
                startpos: true,
                sfen: None,
                moves: Vec::new(),
            };
            Ok((vec![parsed], vec!["position startpos".to_string()]))
        }
    }
}

/// USI position 行を分解した結果。
struct ParsedPosition {
    startpos: bool,
    sfen: Option<String>,
    moves: Vec<String>,
}

/// `position ...` 形式の行をパースする。
fn parse_position_line(line: &str) -> Result<ParsedPosition> {
    let mut tokens = line.split_whitespace().peekable();
    if tokens.peek().is_some_and(|tok| *tok == "position") {
        tokens.next();
    }
    match tokens.next() {
        Some("startpos") => {
            let moves = parse_moves(tokens)?;
            Ok(ParsedPosition {
                startpos: true,
                sfen: None,
                moves,
            })
        }
        Some("sfen") => {
            let mut sfen_tokens = Vec::new();
            while let Some(token) = tokens.peek() {
                if *token == "moves" {
                    break;
                }
                sfen_tokens.push(tokens.next().unwrap().to_string());
            }
            if sfen_tokens.is_empty() {
                bail!("missing SFEN payload");
            }
            let moves = parse_moves(tokens)?;
            Ok(ParsedPosition {
                startpos: false,
                sfen: Some(sfen_tokens.join(" ")),
                moves,
            })
        }
        other => bail!("expected 'startpos' or 'sfen' after 'position', got {:?}", other),
    }
}

/// sfen 文字列だけが渡されたときの簡易パーサ。
fn parse_sfen_only(line: &str) -> Result<ParsedPosition> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        bail!("empty SFEN");
    }
    Ok(ParsedPosition {
        startpos: false,
        sfen: Some(trimmed.to_string()),
        moves: Vec::new(),
    })
}

/// moves トークン以降を USI 形式の指し手列として回収する。
fn parse_moves<'a, I>(iter: I) -> Result<Vec<String>>
where
    I: Iterator<Item = &'a str>,
{
    let mut iter = iter.peekable();
    match iter.peek() {
        Some(&"moves") => {
            iter.next();
            Ok(iter.map(|mv| mv.to_string()).collect())
        }
        Some(other) => bail!("expected 'moves' before move list, got '{other}'"),
        None => Ok(Vec::new()),
    }
}

fn build_position(parsed: &ParsedPosition) -> Result<Position> {
    let mut pos = Position::new();
    if parsed.startpos {
        pos.set_sfen(SFEN_HIRATE)?;
    } else if let Some(sfen) = &parsed.sfen {
        pos.set_sfen(sfen)?;
    } else {
        bail!("missing sfen payload");
    }
    for mv_str in &parsed.moves {
        let mv = Move::from_usi(mv_str)
            .ok_or_else(|| anyhow!("invalid move in start position: {}", mv_str))?;
        if !pos.is_legal(mv) {
            bail!("illegal move '{}' in start position", mv_str);
        }
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
    }
    Ok(pos)
}

fn describe_position(parsed: &ParsedPosition) -> String {
    let mut buf = OsString::from("position ");
    if parsed.startpos {
        buf.push("startpos");
    } else if let Some(sfen) = &parsed.sfen {
        buf.push("sfen ");
        buf.push(sfen);
    }
    if !parsed.moves.is_empty() {
        buf.push(" moves ");
        buf.push(parsed.moves.join(" "));
    }
    buf.to_string_lossy().to_string()
}

fn parse_option_name(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace().peekable();
    while let Some(tok) = tokens.next() {
        if tok == "name" {
            let mut parts = Vec::new();
            while let Some(next) = tokens.peek() {
                if *next == "type" {
                    break;
                }
                parts.push(tokens.next().unwrap().to_string());
            }
            if !parts.is_empty() {
                return Some(parts.join(" "));
            }
        }
    }
    None
}

fn side_label(color: Color) -> char {
    if color == Color::Black {
        'b'
    } else {
        'w'
    }
}

fn duration_to_millis(d: Duration) -> u64 {
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result as AnyResult;
    use clap::Parser;
    use std::path::PathBuf;

    #[test]
    fn time_control_allocates_fractional_budget() -> AnyResult<()> {
        let mut cli = Cli::parse_from(["engine_selfplay"]);
        cli.btime = 60_000;
        cli.wtime = 60_000;
        cli.byoyomi = 1_000;
        let tc = TimeControl::new(&cli);
        assert_eq!(tc.think_limit_ms(Color::Black), 2_500);
        assert_eq!(tc.updated_time(60_000, 0, 1_500), 59_500);

        cli.byoyomi = 0;
        cli.binc = 1_000;
        let tc_inc = TimeControl::new(&cli);
        assert_eq!(tc_inc.think_limit_ms(Color::Black), 2_500);
        assert_eq!(tc_inc.updated_time(5_000, 1_000, 4_000), 2_000);
        Ok(())
    }

    #[test]
    fn resolve_engine_paths_uses_per_side_when_provided() {
        let cli = Cli::parse_from([
            "engine_selfplay",
            "--engine-path-black",
            "/path/to/black",
            "--engine-path-white",
            "/path/to/white",
        ]);
        let paths = resolve_engine_paths(&cli);
        assert_eq!(paths.black.path, PathBuf::from("/path/to/black"));
        assert_eq!(paths.white.path, PathBuf::from("/path/to/white"));
        assert_eq!(paths.black.source, "cli:black");
        assert_eq!(paths.white.source, "cli:white");
    }

    #[test]
    fn resolve_engine_paths_uses_shared_when_per_side_missing() {
        let cli = Cli::parse_from([
            "engine_selfplay",
            "--engine-path",
            "/shared/path/engine-usi",
        ]);
        let paths = resolve_engine_paths(&cli);
        assert_eq!(paths.black.path, PathBuf::from("/shared/path/engine-usi"));
        assert_eq!(paths.white.path, PathBuf::from("/shared/path/engine-usi"));
        assert_eq!(paths.black.source, "cli");
        assert_eq!(paths.white.source, "cli");
    }

    #[test]
    fn info_snapshot_parses_primary_pv() {
        let mut snap = InfoSnapshot::default();
        snap.update_from_line(
            "info depth 10 seldepth 12 nodes 12345 time 67 nps 890 score cp 34 pv 7g7f 3c3d",
        );
        assert_eq!(snap.depth, Some(10));
        assert_eq!(snap.seldepth, Some(12));
        assert_eq!(snap.nodes, Some(12_345));
        assert_eq!(snap.time_ms, Some(67));
        assert_eq!(snap.nps, Some(890));
        assert_eq!(snap.score_cp, Some(34));
        assert_eq!(snap.score_mate, None);
        assert_eq!(snap.pv, vec!["7g7f".to_string(), "3c3d".to_string()]);

        // multipv != 1 は無視される
        snap.update_from_line("info multipv 2 depth 20 score cp 100 pv 2g2f");
        assert_eq!(snap.depth, Some(10));
    }

    #[test]
    fn parse_position_line_covers_startpos_and_sfen() -> AnyResult<()> {
        let parsed = parse_position_line("position startpos moves 7g7f 3c3d")?;
        assert!(parsed.startpos);
        assert_eq!(parsed.moves, vec!["7g7f", "3c3d"]);

        let sfen_line = "position sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1 moves 7g7f";
        let parsed_sfen = parse_position_line(sfen_line)?;
        assert!(!parsed_sfen.startpos);
        assert_eq!(parsed_sfen.moves, vec!["7g7f"]);
        assert!(parsed_sfen.sfen.as_deref().is_some_and(|s| s.starts_with("lnsgkgsnl")));

        let parsed_sfen_only =
            parse_sfen_only("lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1")?;
        assert!(parsed_sfen_only.sfen.is_some());
        assert!(parsed_sfen_only.moves.is_empty());
        Ok(())
    }

    #[test]
    fn parse_position_line_rejects_missing_moves_keyword() {
        assert!(parse_position_line("position startpos 7g7f").is_err());
    }
}

#[derive(Default)]
struct GameLog {
    moves: Vec<MoveEntry>,
    result: Option<ResultEntry>,
}

#[derive(Deserialize, Clone)]
struct MoveEntry {
    game_id: u32,
    ply: u32,
    sfen_before: String,
    move_usi: String,
    #[serde(default)]
    elapsed_ms: Option<u64>,
    #[serde(default)]
    eval: Option<EvalLog>,
}

#[derive(Deserialize)]
struct ResultEntry {
    game_id: u32,
    outcome: String,
    reason: String,
    plies: u32,
}

fn convert_jsonl_to_kif(input: &Path, output: &Path) -> Result<Vec<PathBuf>> {
    let file =
        File::open(input).with_context(|| format!("failed to open input {}", input.display()))?;
    let reader = BufReader::new(file);

    let mut meta: Option<MetaLog> = None;
    let mut games: BTreeMap<u32, GameLog> = BTreeMap::new();

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)
            .with_context(|| format!("failed to parse JSON line: {}", trimmed))?;
        match value.get("type").and_then(|v| v.as_str()) {
            Some("meta") => {
                meta = Some(serde_json::from_value(value)?);
            }
            Some("move") => {
                let entry: MoveEntry = serde_json::from_value(value)?;
                games.entry(entry.game_id).or_default().moves.push(entry);
            }
            Some("result") => {
                let entry: ResultEntry = serde_json::from_value(value)?;
                let gid = entry.game_id;
                games.entry(gid).or_default().result = Some(entry);
            }
            _ => {}
        }
    }

    if games.is_empty() {
        bail!("no games found in {}", input.display());
    }

    let parent = output.parent().unwrap_or_else(|| Path::new("."));
    let stem = output.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let ext = output.extension().and_then(|s| s.to_str()).unwrap_or("kif");

    let multi = games.len() > 1;
    let mut written = Vec::new();
    for (game_id, game) in games {
        let path = if multi {
            parent.join(format!("{stem}_g{game_id:02}.{ext}"))
        } else {
            output.to_path_buf()
        };
        let mut writer = BufWriter::new(
            File::create(&path).with_context(|| format!("failed to create {}", path.display()))?,
        );
        export_game_to_kif(&mut writer, meta.as_ref(), game_id, &game)?;
        writer.flush()?;
        written.push(path);
    }
    Ok(written)
}

fn export_game_to_kif<W: Write>(
    writer: &mut W,
    meta: Option<&MetaLog>,
    game_id: u32,
    game: &GameLog,
) -> Result<()> {
    let (mut pos, start_sfen) = start_position_for_game(meta, game_id, &game.moves)
        .ok_or_else(|| anyhow!("could not determine start position for game {}", game_id))?;

    let timestamp = meta.map(|m| m.timestamp.clone()).unwrap_or_else(|| "-".to_string());
    let (black_name, white_name) = engine_names_for(meta);
    let (btime, wtime) = meta.map(|m| (m.settings.btime, m.settings.wtime)).unwrap_or((0, 0));
    writeln!(writer, "開始日時：{}", timestamp)?;
    writeln!(writer, "手合割：平手")?;
    writeln!(writer, "先手：{}", black_name)?;
    writeln!(writer, "後手：{}", white_name)?;
    writeln!(writer, "持ち時間：先手{}ms / 後手{}ms", btime, wtime)?;
    writeln!(writer, "開始局面：{}", start_sfen)?;
    writeln!(writer, "手数----指手---------消費時間--")?;

    let mut moves = game.moves.clone();
    moves.sort_by_key(|m| m.ply);
    let mut total_black = 0u64;
    let mut total_white = 0u64;

    for entry in moves {
        if entry.move_usi == "resign" || entry.move_usi == "win" || entry.move_usi == "timeout" {
            break;
        }
        let side = pos.side_to_move();
        let mv = Move::from_usi(&entry.move_usi)
            .ok_or_else(|| anyhow!("invalid move in log: {}", entry.move_usi))?;
        if !pos.is_legal(mv) {
            bail!("illegal move '{}' in log for game {}", entry.move_usi, game_id);
        }
        let elapsed_ms = entry.elapsed_ms.unwrap_or(0);
        let total_time = if side == Color::Black {
            total_black + elapsed_ms
        } else {
            total_white + elapsed_ms
        };
        let line = format_move_kif(entry.ply, &pos, mv, elapsed_ms, total_time);
        writeln!(writer, "{}", line)?;
        let gives_check = pos.gives_check(mv);
        pos.do_move(mv, gives_check);
        if side == Color::Black {
            total_black = total_time;
        } else {
            total_white = total_time;
        }
        write_eval_comments(writer, entry.eval.as_ref())?;
    }

    let final_plies = game
        .result
        .as_ref()
        .map(|r| r.plies)
        .or_else(|| game.moves.last().map(|m| m.ply))
        .unwrap_or(0);
    if let Some(res) = game.result.as_ref() {
        if res.reason != "max_moves" {
            writeln!(writer, "**終了理由={}", res.reason)?;
        }
    }
    let summary = match game.result.as_ref().map(|r| r.outcome.as_str()).unwrap_or("draw") {
        "black_win" => format!("まで{}手で先手の勝ち", final_plies),
        "white_win" => format!("まで{}手で後手の勝ち", final_plies),
        _ => format!("まで{}手で引き分け", final_plies),
    };
    writeln!(writer, "\n{}", summary)?;
    Ok(())
}

fn start_position_for_game(
    meta: Option<&MetaLog>,
    game_id: u32,
    moves: &[MoveEntry],
) -> Option<(Position, String)> {
    if let Some(meta) = meta {
        if !meta.start_positions.is_empty() {
            let idx = ((game_id - 1) as usize) % meta.start_positions.len();
            if let Ok((pos, sfen)) = start_position_from_command(&meta.start_positions[idx]) {
                return Some((pos, sfen));
            }
        }
    }
    moves.first().and_then(|m| {
        let mut pos = Position::new();
        pos.set_sfen(&m.sfen_before).ok()?;
        let sfen = pos.to_sfen();
        Some((pos, sfen))
    })
}

fn start_position_from_command(cmd: &str) -> Result<(Position, String)> {
    let parsed = parse_position_line(cmd)?;
    let pos = build_position(&parsed)?;
    let sfen = pos.to_sfen();
    Ok((pos, sfen))
}

fn engine_names_for(meta: Option<&MetaLog>) -> (String, String) {
    let default = ("black".to_string(), "white".to_string());
    let Some(meta) = meta else { return default };
    let black_name = Path::new(&meta.engine_cmd.path_black)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&meta.engine_cmd.path_black);
    let white_name = Path::new(&meta.engine_cmd.path_white)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&meta.engine_cmd.path_white);

    let black_opts = &meta.engine_cmd.usi_options_black;
    let white_opts = &meta.engine_cmd.usi_options_white;

    let black_display = if black_opts.is_empty() {
        black_name.to_string()
    } else {
        format!("{} [{}]", black_name, black_opts.join(", "))
    };
    let white_display = if white_opts.is_empty() {
        white_name.to_string()
    } else {
        format!("{} [{}]", white_name, white_opts.join(", "))
    };

    (black_display, white_display)
}

fn format_move_kif(ply: u32, pos: &Position, mv: Move, elapsed_ms: u64, total_ms: u64) -> String {
    let prefix = if pos.side_to_move() == Color::Black {
        "▲"
    } else {
        "△"
    };
    let dest = square_label_kanji(mv.to());
    let (label, from_suffix) = if mv.is_drop() {
        (format!("{}打", piece_label(mv.drop_piece_type(), false)), String::new())
    } else {
        let from = mv.from();
        let piece = pos.piece_on(from);
        let promoted = piece.piece_type().is_promoted() || mv.is_promote();
        let suffix = format!("({}{})", square_file_digit(from), square_rank_digit(from));
        (piece_label(piece.piece_type(), promoted).to_string(), suffix)
    };
    let per_move = format_mm_ss(elapsed_ms);
    let total = format_hh_mm_ss(total_ms);
    format!(
        "{:>4} {}{}{}{}   ({:>5}/{})",
        ply, prefix, dest, label, from_suffix, per_move, total
    )
}

fn square_label_kanji(sq: Square) -> String {
    format!("{}{}", file_kanji(sq), rank_kanji(sq))
}

fn file_kanji(sq: Square) -> &'static str {
    const FILES: [&str; 10] = ["", "１", "２", "３", "４", "５", "６", "７", "８", "９"];
    let idx = sq.file().to_usi_char().to_digit(10).unwrap_or(1) as usize;
    FILES[idx]
}

fn rank_kanji(sq: Square) -> &'static str {
    const RANKS: [&str; 9] = ["一", "二", "三", "四", "五", "六", "七", "八", "九"];
    let rank = sq.rank().to_usi_char() as u8;
    let idx = (rank - b'a') as usize;
    RANKS.get(idx).copied().unwrap_or("一")
}

fn square_file_digit(sq: Square) -> char {
    sq.file().to_usi_char()
}

fn square_rank_digit(sq: Square) -> char {
    let rank = sq.rank().to_usi_char();
    let idx = (rank as u8 - b'a') + 1;
    char::from_digit(idx as u32, 10).unwrap_or('1')
}

fn piece_label(piece_type: PieceType, promoted: bool) -> &'static str {
    match (piece_type, promoted) {
        (PieceType::Pawn, false) => "歩",
        (PieceType::Pawn, true) => "と",
        (PieceType::Lance, false) => "香",
        (PieceType::Lance, true) => "成香",
        (PieceType::Knight, false) => "桂",
        (PieceType::Knight, true) => "成桂",
        (PieceType::Silver, false) => "銀",
        (PieceType::Silver, true) => "成銀",
        (PieceType::Gold, _) => "金",
        (PieceType::Bishop, false) => "角",
        (PieceType::Bishop, true) => "馬",
        (PieceType::Rook, false) => "飛",
        (PieceType::Rook, true) => "龍",
        (PieceType::King, _) => "玉",
        (PieceType::ProPawn, _) => "と",
        (PieceType::ProLance, _) => "成香",
        (PieceType::ProKnight, _) => "成桂",
        (PieceType::ProSilver, _) => "成銀",
        (PieceType::Horse, _) => "馬",
        (PieceType::Dragon, _) => "龍",
    }
}

fn write_eval_comments<W: Write>(writer: &mut W, eval: Option<&EvalLog>) -> Result<()> {
    let Some(eval) = eval else {
        return Ok(());
    };
    writeln!(writer, "*info")?;
    if let Some(mate) = eval.score_mate {
        writeln!(writer, "**詰み={}", mate)?;
    } else if let Some(cp) = eval.score_cp {
        writeln!(writer, "**評価値={:+}", cp)?;
    }
    if let Some(depth) = eval.depth {
        writeln!(writer, "**深さ={}", depth)?;
    }
    if let Some(seldepth) = eval.seldepth {
        writeln!(writer, "**選択深さ={}", seldepth)?;
    }
    if let Some(nodes) = eval.nodes {
        writeln!(writer, "**ノード数={}", nodes)?;
    }
    if let Some(time_ms) = eval.time_ms {
        writeln!(writer, "**探索時間={}ms", time_ms)?;
    }
    if let Some(nps) = eval.nps {
        writeln!(writer, "**NPS={}", nps)?;
    }
    if let Some(pv) = eval.pv.as_ref() {
        if !pv.is_empty() {
            writeln!(writer, "**読み筋={}", pv.join(" "))?;
        }
    }
    Ok(())
}

fn eval_label(eval: Option<&EvalLog>) -> String {
    let Some(eval) = eval else {
        return "?".to_string();
    };
    if let Some(mate) = eval.score_mate {
        return format!("mate{mate}");
    }
    if let Some(cp) = eval.score_cp {
        return format!("{cp:+}");
    }
    "?".to_string()
}

fn format_mm_ss(ms: u64) -> String {
    let secs = ms / 1000;
    let m = secs / 60;
    let s = secs % 60;
    format!("{:>2}:{:02}", m, s)
}

/// エンジン設定を人間可読な形式でフォーマットする
fn format_engine_settings(engine: &ResolvedEnginePath, usi_options: &[String]) -> String {
    let engine_name = engine.path.file_name().and_then(|s| s.to_str()).unwrap_or("engine-usi");

    if usi_options.is_empty() {
        format!("{engine_name} (default)")
    } else {
        format!("{engine_name} [{}]", usi_options.join(", "))
    }
}

fn format_hh_mm_ss(ms: u64) -> String {
    let secs = ms / 1000;
    let h = secs / 3600;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{:02}:{:02}:{:02}", h, m, s)
}
