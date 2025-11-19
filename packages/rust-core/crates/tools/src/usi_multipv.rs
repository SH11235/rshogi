use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Score {
    Cp(i32),
    Mate(i32),
}

#[derive(Clone, Debug, Serialize)]
pub struct MultipvLine {
    pub rank: u8,
    pub score: Score,
    pub depth: u32,
    pub seldepth: Option<u32>,
    pub nodes: Option<u64>,
    pub pv: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum PositionSpec {
    /// Bare SFEN string without leading `sfen`
    BareSfen(String),
    /// Full USI position command (e.g. `position sfen ... moves ...`)
    FullCommand(String),
}

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub engine_path: String,
    pub engine_type: Option<String>,
    pub threads: u32,
    pub hash_mb: u32,
    pub profile: Option<String>,
    pub extra_env: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct SearchConfig {
    pub time_ms: u64,
    pub multipv: u8,
    pub position: PositionSpec,
    pub tag: Option<String>,
    pub raw_log_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AnalysisOutput {
    pub sfen: String,
    pub engine_type: Option<String>,
    pub time_ms: u64,
    pub actual_ms: u128,
    pub depth: u32,
    pub nodes: u64,
    pub threads: u32,
    pub hash_mb: u32,
    pub profile: Option<String>,
    pub tag: Option<String>,
    pub multipv: Vec<MultipvLine>,
}

struct UsiChild {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    raw_log: Option<Box<dyn Write + Send>>,
}

impl Drop for UsiChild {
    fn drop(&mut self) {
        let _ = self.write_line("quit");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl UsiChild {
    fn spawn(config: &EngineConfig, raw_log_path: &Option<String>) -> Result<Self> {
        let mut cmd = Command::new(&config.engine_path);
        for (k, v) in &config.extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("failed to start engine: {}", config.engine_path))?;

        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stdout = BufReader::new(stdout);

        let raw_log: Option<Box<dyn Write + Send>> = if let Some(path) = raw_log_path {
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(Path::new(path))
                .with_context(|| format!("failed to open raw log file: {path}"))?;
            Some(Box::new(file))
        } else {
            None
        };

        Ok(Self {
            child,
            stdin,
            stdout,
            raw_log,
        })
    }

    fn write_line(&mut self, line: &str) -> Result<()> {
        self.stdin
            .write_all(line.as_bytes())
            .context("failed to write to engine stdin")?;
        self.stdin.write_all(b"\n").context("failed to write newline to engine stdin")?;
        self.stdin.flush().ok();
        Ok(())
    }

    fn read_line_with_timeout(&mut self, timeout: Duration) -> Result<Option<String>> {
        let start = Instant::now();
        loop {
            if self.stdout.fill_buf()?.is_empty() && start.elapsed() >= timeout {
                return Ok(None);
            }
            let mut buf = String::new();
            let read = self.stdout.read_line(&mut buf)?;
            if read == 0 {
                return Ok(None);
            }
            if let Some(writer) = self.raw_log.as_mut() {
                let _ = writer.write_all(buf.as_bytes());
                let _ = writer.flush();
            }
            if buf.is_empty() {
                continue;
            }
            return Ok(Some(buf.trim_end_matches(&['\r', '\n'][..]).to_string()));
        }
    }
}

#[derive(Default)]
struct InfoAggregate {
    max_depth: u32,
    max_nodes: u64,
    lines: HashMap<u8, MultipvLine>,
}

#[derive(Debug)]
struct InfoLine {
    depth: Option<u32>,
    seldepth: Option<u32>,
    nodes: Option<u64>,
    multipv: Option<u8>,
    score: Option<Score>,
    pv: Vec<String>,
}

fn parse_info_line(line: &str) -> Option<InfoLine> {
    if !line.starts_with("info ") {
        return None;
    }
    let mut depth = None;
    let mut seldepth = None;
    let mut nodes = None;
    let mut multipv = None;
    let mut score = None;
    let mut pv: Vec<String> = Vec::new();
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut i = 1;
    while i < tokens.len() {
        match tokens[i] {
            "depth" => {
                if i + 1 < tokens.len() {
                    depth = tokens[i + 1].parse::<u32>().ok();
                    i += 2;
                } else {
                    break;
                }
            }
            "seldepth" => {
                if i + 1 < tokens.len() {
                    seldepth = tokens[i + 1].parse::<u32>().ok();
                    i += 2;
                } else {
                    break;
                }
            }
            "nodes" => {
                if i + 1 < tokens.len() {
                    nodes = tokens[i + 1].parse::<u64>().ok();
                    i += 2;
                } else {
                    break;
                }
            }
            "multipv" => {
                if i + 1 < tokens.len() {
                    multipv = tokens[i + 1].parse::<u8>().ok();
                    i += 2;
                } else {
                    break;
                }
            }
            "score" => {
                if i + 2 < tokens.len() {
                    match tokens[i + 1] {
                        "cp" => {
                            if let Ok(v) = tokens[i + 2].parse::<i32>() {
                                score = Some(Score::Cp(v));
                            }
                        }
                        "mate" => {
                            if let Ok(v) = tokens[i + 2].parse::<i32>() {
                                score = Some(Score::Mate(v));
                            }
                        }
                        _ => {}
                    }
                    i += 3;
                } else {
                    break;
                }
            }
            "pv" => {
                pv.extend(tokens[i + 1..].iter().map(|s| s.to_string()));
                break;
            }
            _ => {
                i += 1;
            }
        }
    }

    Some(InfoLine {
        depth,
        seldepth,
        nodes,
        multipv,
        score,
        pv,
    })
}

fn format_position_command(position: &PositionSpec) -> String {
    match position {
        PositionSpec::BareSfen(sfen) => format!("position sfen {}", sfen),
        PositionSpec::FullCommand(cmd) => cmd.clone(),
    }
}

fn apply_base_and_profile_options(
    engine: &mut UsiChild,
    engine_cfg: &EngineConfig,
    search_cfg: &SearchConfig,
) -> Result<()> {
    if let Some(ref eng) = engine_cfg.engine_type {
        engine.write_line(&format!("setoption name UsiEngineType value {eng}"))?;
    }
    engine.write_line(&format!("setoption name Threads value {}", engine_cfg.threads))?;
    engine.write_line(&format!("setoption name USI_Hash value {}", engine_cfg.hash_mb))?;

    // selfplay_eval_targets の DEFAULT_PROFILES と同等のプリセット
    if let Some(profile) = engine_cfg.profile.as_deref() {
        match profile {
            // base: RootBeamForceFullCount = 0
            "base" => {
                engine.write_line("setoption name SearchParams.RootBeamForceFullCount value 0")?;
            }
            // short:
            // - RootBeamForceFullCount = 0
            // - RootSeeGate = true
            // - RootSeeGate.XSEE = 150
            "short" => {
                engine.write_line("setoption name SearchParams.RootBeamForceFullCount value 0")?;
                engine.write_line("setoption name RootSeeGate value true")?;
                engine.write_line("setoption name RootSeeGate.XSEE value 150")?;
            }
            // rootfull: RootBeamForceFullCount = 4
            "rootfull" => {
                engine.write_line("setoption name SearchParams.RootBeamForceFullCount value 4")?;
            }
            // gates:
            // - RootBeamForceFullCount = 0
            // - RootSeeGate.XSEE = 0
            "gates" => {
                engine.write_line("setoption name SearchParams.RootBeamForceFullCount value 0")?;
                engine.write_line("setoption name RootSeeGate.XSEE value 0")?;
            }
            // custom / その他: プロファイル由来の setoption は送らない
            _ => {}
        }
    }

    engine.write_line(&format!("setoption name MultiPV value {}", search_cfg.multipv))?;

    Ok(())
}

pub fn run_multipv_analysis(
    engine_cfg: &EngineConfig,
    search_cfg: &SearchConfig,
) -> Result<AnalysisOutput> {
    let mut engine = UsiChild::spawn(engine_cfg, &search_cfg.raw_log_path)?;

    engine.write_line("usi")?;
    let start = Instant::now();
    loop {
        if let Some(line) = engine.read_line_with_timeout(Duration::from_millis(1000))? {
            if line.contains("usiok") {
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(5) {
            return Err(anyhow!("timeout waiting for usiok"));
        }
    }

    apply_base_and_profile_options(&mut engine, engine_cfg, search_cfg)?;

    engine.write_line("isready")?;
    let ready_start = Instant::now();
    loop {
        if let Some(line) = engine.read_line_with_timeout(Duration::from_millis(1000))? {
            if line.contains("readyok") {
                break;
            }
        }
        if ready_start.elapsed() > Duration::from_secs(10) {
            return Err(anyhow!("timeout waiting for readyok"));
        }
    }

    let pos_cmd = format_position_command(&search_cfg.position);
    engine.write_line(&pos_cmd)?;

    let go_cmd = format!("go byoyomi {}", search_cfg.time_ms);
    let search_start = Instant::now();
    engine.write_line(&go_cmd)?;

    let mut aggregate = InfoAggregate::default();
    let mut bestmove_seen = false;
    while !bestmove_seen {
        if let Some(line) = engine.read_line_with_timeout(Duration::from_millis(1000))? {
            if let Some(info) = parse_info_line(&line) {
                if let Some(d) = info.depth {
                    aggregate.max_depth = aggregate.max_depth.max(d);
                }
                if let Some(n) = info.nodes {
                    aggregate.max_nodes = aggregate.max_nodes.max(n);
                }
                if let (Some(rank), Some(score), Some(depth), false) =
                    (info.multipv, info.score, info.depth, info.pv.is_empty())
                {
                    let entry = MultipvLine {
                        rank,
                        score,
                        depth,
                        seldepth: info.seldepth,
                        nodes: info.nodes,
                        pv: info.pv,
                    };
                    aggregate.lines.insert(rank, entry);
                }
            }
            if line.starts_with("bestmove ") {
                bestmove_seen = true;
            }
        } else {
            break;
        }
    }
    let actual_ms = search_start.elapsed().as_millis();

    let mut lines: Vec<MultipvLine> = aggregate.lines.into_values().collect();
    lines.sort_by_key(|l| l.rank);

    let sfen = match &search_cfg.position {
        PositionSpec::BareSfen(s) => s.clone(),
        PositionSpec::FullCommand(cmd) => cmd.clone(),
    };

    Ok(AnalysisOutput {
        sfen,
        engine_type: engine_cfg.engine_type.clone(),
        time_ms: search_cfg.time_ms,
        actual_ms,
        depth: aggregate.max_depth,
        nodes: aggregate.max_nodes,
        threads: engine_cfg.threads,
        hash_mb: engine_cfg.hash_mb,
        profile: engine_cfg.profile.clone(),
        tag: search_cfg.tag.clone(),
        multipv: lines,
    })
}
