use chrono::Local;
use serde_json::json;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use engine_core::engine::controller::EngineType;
use engine_core::shogi::{Color, Move, Position};
use engine_core::shogihome_basic::{
    BasicEngine as BasicOpponent, RepetitionTable, ShogihomeBasicStyle,
};
use engine_core::usi::{create_position, move_to_usi, parse_usi_move, position_to_sfen};
use serde::Serialize;
use tools::kif_export::convert_jsonl_to_kif;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Selfplay harness: main engine (Black) vs ShogiHome basic engine (White)"
)]
struct Cli {
    /// Number of games to run
    #[arg(long, default_value_t = 10)]
    games: u32,

    /// Maximum plies per game before declaring a draw
    #[arg(long, default_value_t = 512)]
    max_moves: u32,

    /// Fixed thinking time per Black move in milliseconds
    #[arg(long, default_value_t = 1000)]
    think_ms: u64,

    /// Threads for the main engine
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Depth for the ShogiHome basic engine search
    #[arg(long, default_value_t = 2)]
    basic_depth: u8,

    /// Enable random noise in the ShogiHome basic engine evaluation
    #[arg(long, default_value_t = false)]
    basic_noise: bool,

    /// Optional RNG seed for the ShogiHome basic engine
    #[arg(long)]
    basic_seed: Option<u64>,

    /// Style preset for the ShogiHome basic engine (static-rook, ranging-rook, random)
    #[arg(long, default_value = "static-rook", value_parser = parse_basic_style)]
    basic_style: ShogihomeBasicStyle,

    /// Engine type for the main engine (enhanced, enhanced-nnue, nnue, material)
    #[arg(long, default_value = "enhanced", value_parser = parse_engine_type)]
    engine_type: EngineType,

    /// Path to the engine-usi binary (defaults to sibling of this executable or PATH)
    #[arg(long)]
    engine_path: Option<PathBuf>,

    /// Extra arguments to pass to the engine process
    #[arg(long, num_args = 1..)]
    engine_args: Option<Vec<String>>,

    /// Hash size (MiB) passed to engine-usi via USI options
    #[arg(long, default_value_t = 1024)]
    hash_mb: u32,

    /// Optional file that lists starting positions (USI position commands per line)
    #[arg(long)]
    startpos_file: Option<PathBuf>,

    /// Output path template (optional)
    #[arg(long)]
    out: Option<PathBuf>,
}

#[derive(Serialize)]
struct MoveLog {
    game_id: u32,
    ply: u32,
    side_to_move: char,
    sfen_before: String,
    move_usi: String,
    engine: &'static str,
    main_eval: Option<MainEvalLog>,
    basic_eval: Option<BasicEvalLog>,
    result: Option<String>,
}

#[derive(Serialize)]
struct MainEvalLog {
    score_cp: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score_mate: Option<i32>,
    depth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seldepth: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nps: Option<u64>,
    pv: Option<Vec<String>>,
}

#[derive(Serialize)]
struct BasicEvalLog {
    score: i32,
    style: &'static str,
}

enum MainAction {
    Move(Move),
    Resign,
    Win,
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

#[derive(Serialize)]
struct InfoLogEntry<'a> {
    kind: &'static str,
    game_id: u32,
    ply: u32,
    side_to_move: char,
    engine: &'static str,
    line: &'a str,
}

struct InfoLogger {
    writer: BufWriter<File>,
    path: PathBuf,
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
            path: path.to_path_buf(),
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

    fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Clone, Default)]
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
    fn update_from_line(&mut self, line: &str) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.first().copied() != Some("info") {
            return;
        }
        let mut multipv = 1u32;
        let mut idx = 1;
        while idx < tokens.len() {
            if tokens[idx] == "multipv" && idx + 1 < tokens.len() {
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

    fn into_eval_log(self) -> Option<MainEvalLog> {
        if self.score_cp.is_none()
            && self.score_mate.is_none()
            && self.depth.is_none()
            && self.nodes.is_none()
            && self.pv.is_empty()
        {
            return None;
        }
        Some(MainEvalLog {
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

struct UsiEngineConfig {
    bin: PathBuf,
    args: Vec<String>,
    engine_type: EngineType,
    threads: usize,
    hash_mb: u32,
}

struct SearchOutcome {
    bestmove: String,
    eval: InfoSnapshot,
}

struct InfoContext {
    game_id: u32,
    ply: u32,
    side: Color,
}

struct UsiEngineProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    rx: Receiver<String>,
    opt_names: HashSet<String>,
}

impl UsiEngineProcess {
    fn spawn(cfg: &UsiEngineConfig) -> Result<Self> {
        let mut cmd = Command::new(&cfg.bin);
        if !cfg.args.is_empty() {
            cmd.args(&cfg.args);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn engine at {}", cfg.bin.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let (tx, rx) = mpsc::channel::<String>();
        thread::spawn(move || {
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
        };
        proc.initialize(cfg)?;
        Ok(proc)
    }

    fn initialize(&mut self, cfg: &UsiEngineConfig) -> Result<()> {
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
        self.set_option_if_available("EngineType", engine_type_option_value(cfg.engine_type))?;
        self.sync_ready()?;
        self.write_line("usinewgame")?;
        Ok(())
    }

    fn new_game(&mut self) -> Result<()> {
        self.write_line("usinewgame")?;
        Ok(())
    }

    fn search(
        &mut self,
        sfen: &str,
        think_ms: u64,
        ctx: &InfoContext,
        info_logger: &mut InfoLogger,
    ) -> Result<SearchOutcome> {
        self.write_line(&format!("position sfen {}", sfen))?;
        self.write_line(&format!("go byoyomi {}", think_ms))?;
        let mut snapshot = InfoSnapshot::default();
        let mut stop_sent = false;
        let slack = think_ms.saturating_div(2).saturating_add(5000).max(6000);
        let deadline = Instant::now() + Duration::from_millis(think_ms.saturating_add(slack));
        loop {
            let now = Instant::now();
            if now >= deadline {
                if !stop_sent {
                    self.write_line("stop")?;
                    stop_sent = true;
                    continue;
                }
                return Err(anyhow!(
                    "USI engine timed out without sending bestmove (think_ms={think_ms})"
                ));
            }
            let remain = deadline.saturating_duration_since(now);
            match self.rx.recv_timeout(remain) {
                Ok(line) => {
                    if line.starts_with("info") {
                        info_logger.log(InfoLogEntry {
                            kind: "info",
                            game_id: ctx.game_id,
                            ply: ctx.ply,
                            side_to_move: side_label(ctx.side),
                            engine: "main",
                            line: &line,
                        })?;
                        snapshot.update_from_line(&line);
                    } else if let Some(rest) = line.strip_prefix("bestmove ") {
                        let mv = rest.split_whitespace().next().unwrap_or("resign");
                        return Ok(SearchOutcome {
                            bestmove: mv.to_string(),
                            eval: snapshot,
                        });
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    if !stop_sent {
                        self.write_line("stop")?;
                        stop_sent = true;
                    } else {
                        return Err(anyhow!(
                            "USI engine failed to answer even after stop (think_ms={think_ms})"
                        ));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("USI engine exited unexpectedly"));
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
        self.rx.recv_timeout(timeout).map_err(|_| anyhow!("engine read timeout"))
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

impl Drop for UsiEngineProcess {
    fn drop(&mut self) {
        let _ = self.write_line("quit");
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_option_name(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace().peekable();
    while let Some(tok) = tokens.next() {
        if tok == "name" {
            let mut parts = Vec::new();
            while let Some(&next) = tokens.peek() {
                if next == "type" {
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

fn main() -> Result<()> {
    let cli = Cli::parse();

    let engine_bin = resolve_engine_path(&cli);
    let engine_args = cli.engine_args.clone().unwrap_or_default();
    let mut usi_engine = UsiEngineProcess::spawn(&UsiEngineConfig {
        bin: engine_bin.clone(),
        args: engine_args,
        engine_type: cli.engine_type,
        threads: cli.threads,
        hash_mb: cli.hash_mb,
    })?;

    let mut basic = BasicOpponent::new(cli.basic_style);
    basic.enable_noise(cli.basic_noise);
    if let Some(seed) = cli.basic_seed {
        basic.set_seed(seed);
    }

    let starts = load_start_positions(cli.startpos_file.as_deref())?;
    let base_out = cli.out.clone().unwrap_or_else(|| default_output_base(&cli));
    let timestamp = Local::now();
    let timestamp_prefix = timestamp.format("%Y%m%d-%H%M%S").to_string();
    let timestamp_iso = timestamp.to_rfc3339();
    let final_out = resolve_output_path(&base_out, &timestamp_prefix);
    println!("selfplay_basic: writing log to {}", final_out.display());
    let meta_start_pos = starts.first().cloned().unwrap_or_else(Position::startpos);
    let info_log_path = default_info_log_path(&final_out);
    let mut info_logger = InfoLogger::new(&info_log_path)?;
    let mut writer = prepare_writer(&final_out)?;
    let meta_params = MetadataParams {
        timestamp_iso: &timestamp_iso,
        base_path: &base_out,
        final_path: &final_out,
        start_position: &meta_start_pos,
        info_log_path: info_logger.path(),
        engine_bin: &engine_bin,
    };
    write_metadata(&mut writer, &cli, &meta_params)?;

    for game_idx in 0..cli.games {
        usi_engine.new_game()?;
        let mut pos = starts[(game_idx as usize) % starts.len()].clone();
        let mut outcome = GameOutcome::InProgress;

        for ply_idx in 0..cli.max_moves {
            let side = pos.side_to_move;
            let sfen_before = position_to_sfen(&pos);

            if side == Color::Black {
                let ctx = InfoContext {
                    game_id: game_idx + 1,
                    ply: ply_idx + 1,
                    side,
                };
                let (action, eval) =
                    search_main_move(&mut usi_engine, &pos, cli.think_ms, ctx, &mut info_logger)?;
                let mut move_record = match action {
                    MainAction::Move(mv) => {
                        let move_str = move_to_usi(&mv);
                        pos.do_move(mv);
                        MoveLog::main(game_idx + 1, ply_idx + 1, side, sfen_before, move_str, eval)
                    }
                    MainAction::Resign => {
                        outcome = GameOutcome::WhiteWin;
                        MoveLog::main(
                            game_idx + 1,
                            ply_idx + 1,
                            side,
                            sfen_before,
                            "resign".to_string(),
                            eval,
                        )
                    }
                    MainAction::Win => {
                        outcome = GameOutcome::BlackWin;
                        MoveLog::main(
                            game_idx + 1,
                            ply_idx + 1,
                            side,
                            sfen_before,
                            "win".to_string(),
                            eval,
                        )
                    }
                };
                if outcome != GameOutcome::InProgress || ply_idx + 1 == cli.max_moves {
                    if outcome == GameOutcome::InProgress {
                        outcome = GameOutcome::Draw;
                    }
                    move_record.result = Some(outcome.label().to_string());
                }
                serde_json::to_writer(&mut writer, &move_record)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
                if outcome != GameOutcome::InProgress {
                    break;
                }
            } else {
                let rep = RepetitionTable::from_position(&pos);
                let basic_result = basic
                    .search(&pos, cli.basic_depth, Some(&rep))
                    .map_err(|e| anyhow!("basic engine search failed: {e}"))?;
                let mut move_record = if let Some(mv) = basic_result.best_move {
                    let move_str = move_to_usi(&mv);
                    let log = MoveLog::basic(
                        game_idx + 1,
                        ply_idx + 1,
                        side,
                        sfen_before,
                        move_str.clone(),
                        basic_result.score,
                        cli.basic_style,
                    );
                    pos.do_move(mv);
                    log
                } else {
                    outcome = GameOutcome::BlackWin;
                    MoveLog::basic(
                        game_idx + 1,
                        ply_idx + 1,
                        side,
                        sfen_before,
                        "resign".to_string(),
                        basic_result.score,
                        cli.basic_style,
                    )
                };
                if outcome != GameOutcome::InProgress || ply_idx + 1 == cli.max_moves {
                    if outcome == GameOutcome::InProgress {
                        outcome = GameOutcome::Draw;
                    }
                    move_record.result = Some(outcome.label().to_string());
                }
                serde_json::to_writer(&mut writer, &move_record)?;
                writer.write_all(b"\n")?;
                writer.flush()?;
                if outcome != GameOutcome::InProgress {
                    break;
                }
            }
        }
    }

    writer.flush()?;
    info_logger.flush()?;

    // generate KIF automatically
    let kif_path = default_kif_path(&final_out);
    match convert_jsonl_to_kif(&final_out, &kif_path) {
        Ok(paths) if paths.is_empty() => {
            eprintln!("failed to create KIF: no games found");
        }
        Ok(paths) if paths.len() == 1 => {
            println!("kif written to {}", paths[0].display());
        }
        Ok(paths) => {
            println!("kif written (per game):");
            for p in paths {
                println!("  {}", p.display());
            }
        }
        Err(err) => {
            eprintln!("failed to create KIF: {}", err);
        }
    }

    Ok(())
}

fn parse_basic_style(value: &str) -> Result<ShogihomeBasicStyle, String> {
    match value.to_ascii_lowercase().as_str() {
        "static-rook" => Ok(ShogihomeBasicStyle::StaticRookV1),
        "ranging-rook" => Ok(ShogihomeBasicStyle::RangingRookV1),
        "random" => Ok(ShogihomeBasicStyle::Random),
        other => Err(format!(
            "invalid basic style '{other}'. expected static-rook, ranging-rook, or random"
        )),
    }
}

fn parse_engine_type(value: &str) -> Result<EngineType, String> {
    match value.to_ascii_lowercase().as_str() {
        "enhanced-nnue" => Ok(EngineType::EnhancedNnue),
        "enhanced" => Ok(EngineType::Enhanced),
        "nnue" => Ok(EngineType::Nnue),
        "material" => Ok(EngineType::Material),
        other => Err(format!(
            "invalid engine type '{other}'. expected enhanced-nnue, enhanced, nnue, or material"
        )),
    }
}

fn prepare_writer(path: &Path) -> Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create output directory {}", parent.display())
            })?;
        }
    }
    let file = File::create(path).with_context(|| format!("failed to open {}", path.display()))?;
    Ok(BufWriter::new(file))
}

fn load_start_positions(path: Option<&Path>) -> Result<Vec<Position>> {
    if let Some(path) = path {
        let file = File::open(path)
            .with_context(|| format!("failed to open start position file {}", path.display()))?;
        let reader = BufReader::new(file);
        let mut positions = Vec::new();
        for (idx, line) in reader.lines().enumerate() {
            let line = line?;
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (startpos, sfen, moves) = parse_position_line(trimmed).with_context(|| {
                format!("invalid position syntax on line {}: {}", idx + 1, trimmed)
            })?;
            let pos = create_position(startpos, sfen.as_deref(), &moves).with_context(|| {
                format!("failed to create position from line {}: {}", idx + 1, trimmed)
            })?;
            positions.push(pos);
        }
        if positions.is_empty() {
            anyhow::bail!("no usable positions found in {}", path.display());
        }
        Ok(positions)
    } else {
        Ok(vec![Position::startpos()])
    }
}

fn parse_position_line(line: &str) -> Result<(bool, Option<String>, Vec<String>)> {
    let mut tokens = line.split_whitespace().peekable();
    if tokens.peek().is_some_and(|tok| *tok == "position") {
        tokens.next();
    }
    match tokens.next() {
        Some("startpos") => {
            let moves = parse_moves(tokens)?;
            Ok((true, None, moves))
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
                return Err(anyhow!("missing SFEN payload"));
            }
            let moves = parse_moves(tokens)?;
            Ok((false, Some(sfen_tokens.join(" ")), moves))
        }
        other => Err(anyhow!("expected 'startpos' or 'sfen' after 'position', got {:?}", other)),
    }
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
        Some(other) => Err(anyhow!("expected 'moves' keyword before move list, got '{other}'")),
        None => Ok(Vec::new()),
    }
}

fn search_main_move(
    engine: &mut UsiEngineProcess,
    pos: &Position,
    think_ms: u64,
    ctx: InfoContext,
    info_logger: &mut InfoLogger,
) -> Result<(MainAction, Option<MainEvalLog>)> {
    let sfen = position_to_sfen(pos);
    let outcome = engine.search(&sfen, think_ms, &ctx, info_logger)?;
    let eval = outcome.eval.into_eval_log();
    let action = match outcome.bestmove.as_str() {
        "resign" => MainAction::Resign,
        "win" => MainAction::Win,
        mv_str => {
            let mv = parse_usi_move(mv_str)
                .with_context(|| format!("engine returned invalid move '{mv_str}'"))?;
            if !pos.is_legal_move(mv) {
                anyhow::bail!("engine returned illegal move '{mv_str}'");
            }
            MainAction::Move(mv)
        }
    };
    Ok((action, eval))
}

impl MoveLog {
    fn main(
        game_id: u32,
        ply: u32,
        side: Color,
        sfen_before: String,
        move_usi: String,
        eval: Option<MainEvalLog>,
    ) -> Self {
        Self {
            game_id,
            ply,
            side_to_move: side_label(side),
            sfen_before,
            move_usi,
            engine: "main",
            main_eval: eval,
            basic_eval: None,
            result: None,
        }
    }

    fn basic(
        game_id: u32,
        ply: u32,
        side: Color,
        sfen_before: String,
        move_usi: String,
        score: i32,
        style: ShogihomeBasicStyle,
    ) -> Self {
        Self {
            game_id,
            ply,
            side_to_move: side_label(side),
            sfen_before,
            move_usi,
            engine: "basic",
            main_eval: None,
            basic_eval: Some(BasicEvalLog {
                score,
                style: style_label(style),
            }),
            result: None,
        }
    }
}

fn side_label(color: Color) -> char {
    if color == Color::Black {
        'b'
    } else {
        'w'
    }
}

fn style_label(style: ShogihomeBasicStyle) -> &'static str {
    match style {
        ShogihomeBasicStyle::StaticRookV1 => "static-rook",
        ShogihomeBasicStyle::RangingRookV1 => "ranging-rook",
        ShogihomeBasicStyle::Random => "random",
    }
}

fn engine_type_label(engine_type: EngineType) -> &'static str {
    match engine_type {
        EngineType::Material => "material",
        EngineType::Nnue => "nnue",
        EngineType::Enhanced => "enhanced",
        EngineType::EnhancedNnue => "enhanced-nnue",
    }
}

fn engine_type_option_value(engine_type: EngineType) -> &'static str {
    match engine_type {
        EngineType::Material => "Material",
        EngineType::Nnue => "Nnue",
        EngineType::Enhanced => "Enhanced",
        EngineType::EnhancedNnue => "EnhancedNnue",
    }
}

fn default_output_base(cli: &Cli) -> PathBuf {
    let dir = PathBuf::from("runs/selfplay-basic");
    let file = format!(
        "selfplay_{engine}_{threads}t_{style}_d{depth}_{think}ms.jsonl",
        engine = engine_type_label(cli.engine_type),
        threads = cli.threads,
        style = style_label(cli.basic_style),
        depth = cli.basic_depth,
        think = cli.think_ms,
    );
    dir.join(file)
}

fn default_kif_path(jsonl: &Path) -> PathBuf {
    let parent = jsonl.parent().unwrap_or_else(|| Path::new("."));
    let stem = jsonl.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    parent.join(format!("{stem}.kif"))
}

fn default_info_log_path(jsonl: &Path) -> PathBuf {
    jsonl.with_extension("info.jsonl")
}

fn resolve_output_path(out: &Path, timestamp: &str) -> PathBuf {
    let default_name = OsString::from("selfplay.jsonl");
    let (mut dir, base) = match std::fs::metadata(out) {
        Ok(meta) if meta.is_dir() => (out.to_path_buf(), default_name.clone()),
        _ => match out.file_name() {
            Some(name) => {
                let parent =
                    out.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
                (
                    if parent.as_os_str().is_empty() {
                        PathBuf::from(".")
                    } else {
                        parent
                    },
                    name.to_os_string(),
                )
            }
            None => (out.to_path_buf(), default_name.clone()),
        },
    };
    let file_name = if base.is_empty() { default_name } else { base };
    let mut new_name = OsString::from(format!("{timestamp}-"));
    new_name.push(&file_name);
    dir.push(new_name);
    dir
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

struct MetadataParams<'a> {
    timestamp_iso: &'a str,
    base_path: &'a Path,
    final_path: &'a Path,
    start_position: &'a Position,
    info_log_path: &'a Path,
    engine_bin: &'a Path,
}

fn write_metadata(
    writer: &mut BufWriter<File>,
    cli: &Cli,
    params: &MetadataParams<'_>,
) -> Result<()> {
    let command = std::env::args().collect::<Vec<_>>().join(" ");
    let start_sfen = position_to_sfen(params.start_position);
    let meta = json!({
        "type": "meta",
        "timestamp": params.timestamp_iso,
        "output": params.final_path.display().to_string(),
        "output_template": params.base_path.display().to_string(),
        "info_log": params.info_log_path.display().to_string(),
        "engine_binary": params.engine_bin.display().to_string(),
        "command": command,
        "settings": {
            "games": cli.games,
            "max_moves": cli.max_moves,
            "think_ms": cli.think_ms,
            "threads": cli.threads,
            "basic_depth": cli.basic_depth,
            "basic_noise": cli.basic_noise,
            "basic_seed": cli.basic_seed,
            "basic_style": style_label(cli.basic_style),
            "engine_type": engine_type_label(cli.engine_type),
            "startpos_file": cli.startpos_file.as_ref().map(|p| p.display().to_string()),
            "output_base": params.base_path.display().to_string(),
        },
        "start_sfen": start_sfen,
        "engine_names": {
            "black": format!("main ({})", engine_type_label(cli.engine_type)),
            "white": format!("basic ({})", style_label(cli.basic_style)),
        },
        "think_ms": {
            "black": cli.think_ms,
            "white": 0
        },
    });
    serde_json::to_writer(&mut *writer, &meta)?;
    writer.write_all(b"\n")?;
    Ok(())
}
