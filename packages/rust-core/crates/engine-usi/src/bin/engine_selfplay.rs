use std::collections::HashSet;
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
use engine_core::types::{Color, Move};
use serde::Serialize;

/// engine-usi 同士の自己対局ハーネス。時間管理と info ログ収集を最小限に実装する。
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

    /// Threads USI option
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Hash/USI_Hash size (MiB)
    #[arg(long, default_value_t = 1024)]
    hash_mb: u32,

    /// Path to engine-usi binary (shared by both sides)
    #[arg(long)]
    engine_path: Option<PathBuf>,

    /// Common extra arguments passed to engine processes
    #[arg(long, num_args = 1..)]
    engine_args: Option<Vec<String>>,

    /// Extra arguments for Black (overrides engine_args when set)
    #[arg(long, num_args = 1..)]
    engine_args_black: Option<Vec<String>>,

    /// Extra arguments for White (overrides engine_args when set)
    #[arg(long, num_args = 1..)]
    engine_args_white: Option<Vec<String>>,

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
}

#[derive(Serialize)]
struct MetaLog {
    #[serde(rename = "type")]
    kind: &'static str,
    timestamp: String,
    settings: MetaSettings,
    engine_cmd: EngineCommandMeta,
    start_positions: Vec<String>,
    output: String,
    info_log: Option<String>,
}

#[derive(Serialize)]
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
    hash_mb: u32,
    network_delay: Option<i64>,
    network_delay2: Option<i64>,
    minimum_thinking_time: Option<i64>,
    slowmover: Option<i32>,
    ponder: bool,
    startpos_file: Option<String>,
    sfen: Option<String>,
}

#[derive(Serialize)]
struct EngineCommandMeta {
    path: String,
    args_black: Vec<String>,
    args_white: Vec<String>,
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
    engine: &'static str,
    elapsed_ms: u64,
    think_limit_ms: u64,
    timed_out: bool,
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

#[derive(Clone, Copy)]
struct TimeArgs {
    btime: u64,
    wtime: u64,
    byoyomi: u64,
    binc: u64,
    winc: u64,
}

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

    fn think_limit_ms(&self, side: Color) -> u64 {
        if self.byoyomi > 0 {
            self.byoyomi
        } else {
            self.remaining(side).saturating_add(self.increment_for(side))
        }
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
}

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
            let line = self.recv_line(Duration::from_secs(30))?;
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
                });
            }

            let remaining = deadline.saturating_sub(elapsed);
            match self.rx.recv_timeout(remaining) {
                Ok(line) => {
                    if line.starts_with("info") {
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
            let line = self.recv_line(Duration::from_secs(30))?;
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
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
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
    let cli = Cli::parse();

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

    let engine_path = resolve_engine_path(&cli);
    let common_args = cli.engine_args.clone().unwrap_or_default();
    let black_args = cli.engine_args_black.clone().unwrap_or_else(|| common_args.clone());
    let white_args = cli.engine_args_white.clone().unwrap_or(common_args.clone());

    let mut black = EngineProcess::spawn(
        &EngineConfig {
            path: engine_path.clone(),
            args: black_args.clone(),
            threads: cli.threads,
            hash_mb: cli.hash_mb,
            network_delay: cli.network_delay,
            network_delay2: cli.network_delay2,
            minimum_thinking_time: cli.minimum_thinking_time,
            slowmover: cli.slowmover,
            ponder: cli.ponder,
        },
        "black",
    )?;
    let mut white = EngineProcess::spawn(
        &EngineConfig {
            path: engine_path.clone(),
            args: white_args.clone(),
            threads: cli.threads,
            hash_mb: cli.hash_mb,
            network_delay: cli.network_delay,
            network_delay2: cli.network_delay2,
            minimum_thinking_time: cli.minimum_thinking_time,
            slowmover: cli.slowmover,
            ponder: cli.ponder,
        },
        "white",
    )?;

    let meta = MetaLog {
        kind: "meta",
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
            hash_mb: cli.hash_mb,
            network_delay: cli.network_delay,
            network_delay2: cli.network_delay2,
            minimum_thinking_time: cli.minimum_thinking_time,
            slowmover: cli.slowmover,
            ponder: cli.ponder,
            startpos_file: cli.startpos_file.as_ref().map(|p| p.display().to_string()),
            sfen: cli.sfen.clone(),
        },
        engine_cmd: EngineCommandMeta {
            path: engine_path.display().to_string(),
            args_black: black_args.clone(),
            args_white: white_args.clone(),
        },
        start_positions: start_commands.clone(),
        output: output_path.display().to_string(),
        info_log: cli.log_info.then(|| info_path.display().to_string()),
    };
    serde_json::to_writer(&mut writer, &meta)?;
    writer.write_all(b"\n")?;

    for game_idx in 0..cli.games {
        black.new_game()?;
        white.new_game()?;
        let parsed = &start_defs[(game_idx as usize) % start_defs.len()];
        let mut pos = build_position(parsed)?;
        let mut tc = TimeControl::new(&cli);
        let mut outcome = GameOutcome::InProgress;
        let mut outcome_reason = "max_moves";
        let mut plies_played = 0u32;

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
            let mut terminal = false;
            let elapsed_ms = search.elapsed_ms;

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
                match mv_str.as_str() {
                    "resign" => {
                        outcome = if side == Color::Black {
                            GameOutcome::WhiteWin
                        } else {
                            GameOutcome::BlackWin
                        };
                        outcome_reason = "resign";
                        terminal = true;
                    }
                    "win" => {
                        outcome = if side == Color::Black {
                            GameOutcome::BlackWin
                        } else {
                            GameOutcome::WhiteWin
                        };
                        outcome_reason = "win";
                        terminal = true;
                    }
                    _ => {
                        let mv = Move::from_usi(mv_str).ok_or_else(|| {
                            anyhow!("{}: invalid move '{}'", engine_label, mv_str)
                        })?;
                        if !pos.is_legal(mv) {
                            outcome = if side == Color::Black {
                                GameOutcome::WhiteWin
                            } else {
                                GameOutcome::BlackWin
                            };
                            outcome_reason = "illegal_move";
                            terminal = true;
                        } else {
                            let gives_check = pos.gives_check(mv);
                            pos.do_move(mv, gives_check);
                            tc.update_after_move(side, search.elapsed_ms);
                        }
                    }
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

            let move_log = MoveLog {
                kind: "move",
                game_id: game_idx + 1,
                ply: plies_played,
                side_to_move: side_label(side),
                sfen_before,
                move_usi,
                engine: engine_label,
                elapsed_ms,
                think_limit_ms,
                timed_out,
            };
            serde_json::to_writer(&mut writer, &move_log)?;
            writer.write_all(b"\n")?;
            writer.flush()?;

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
        writer.flush()?;
    }

    if let Some(logger) = info_logger.as_mut() {
        logger.flush()?;
    }
    writer.flush()?;
    println!("selfplay log written to {}", output_path.display());
    if cli.log_info {
        println!("info log written to {}", info_path.display());
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

fn resolve_engine_path(cli: &Cli) -> PathBuf {
    if let Some(path) = &cli.engine_path {
        return path.clone();
    }
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_engine-usi") {
        return PathBuf::from(p);
    }
    if let Ok(exec) = std::env::current_exe() {
        if let Some(dir) = exec.parent() {
            #[cfg(windows)]
            let candidate = dir.join("engine-usi.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("engine-usi");
            if candidate.exists() {
                return candidate;
            }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_file() {
                        continue;
                    }
                    if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                        #[cfg(windows)]
                        let matches = name.starts_with("engine-usi") && name.ends_with(".exe");
                        #[cfg(not(windows))]
                        let matches = name.starts_with("engine-usi");
                        if matches {
                            return path;
                        }
                    }
                }
            }
        }
    }
    PathBuf::from("engine-usi")
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

struct ParsedPosition {
    startpos: bool,
    sfen: Option<String>,
    moves: Vec<String>,
}

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
        other => {
            bail!("expected 'startpos' or 'sfen' after 'position', got {:?}", other)
        }
    }
}

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
