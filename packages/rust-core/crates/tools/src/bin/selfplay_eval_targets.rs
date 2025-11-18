use anyhow::{bail, Context, Result};
use clap::Parser;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Deserialize;
use serde::Serialize;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_PROFILES: &[ProfileDef] = &[
    ProfileDef {
        name: "base",
        search_params: &[("RootBeamForceFullCount", "0")],
        root_options: &[],
        env: &[],
    },
    // 短TC（例: 1000ms）を想定したプロファイル。
    // - RootSeeGate を有効化し、静かな手のうち XSEE が大きく悪いものをルートで間引く。
    // - Quiet SEE Guard / capture futility は少し強め寄りの設定を想定（環境変数で制御）。
    ProfileDef {
        name: "short",
        search_params: &[("RootBeamForceFullCount", "0")],
        root_options: &[("RootSeeGate", "true"), ("RootSeeGate.XSEE", "150")],
        env: &[
            ("SHOGI_QUIET_SEE_GUARD", "1"),
            ("SHOGI_CAPTURE_FUT_SCALE", "120"),
        ],
    },
    ProfileDef {
        name: "rootfull",
        search_params: &[("RootBeamForceFullCount", "4")],
        root_options: &[],
        env: &[],
    },
    ProfileDef {
        name: "gates",
        search_params: &[("RootBeamForceFullCount", "0")],
        root_options: &[("RootSeeGate.XSEE", "0")],
        env: &[("SHOGI_QUIET_SEE_GUARD", "0")],
    },
];

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Re-run engine-usi on targets.json (run_eval_targets.py equivalent)"
)]
struct Cli {
    /// Path to targets.json produced by selfplay_blunder_report
    targets: PathBuf,

    /// Output directory for logs/summary (defaults to parent of targets.json)
    #[arg(long)]
    out: Option<PathBuf>,

    /// Path to engine-usi binary
    #[arg(long)]
    engine_path: Option<PathBuf>,

    /// Threads option passed to engine
    #[arg(long, default_value_t = 1)]
    threads: usize,

    /// Byoyomi time (ms) per replay
    #[arg(long, default_value_t = 2000)]
    byoyomi: u64,

    /// Minimum think time (ms) via SearchParams.MinThinkMs
    #[arg(long, default_value_t = 0)]
    min_think: u64,

    /// Warmup milliseconds (Warmup.Ms setoption)
    #[arg(long, default_value_t = 0)]
    warmup_ms: u64,
}

#[derive(Deserialize)]
struct TargetsFile {
    targets: Vec<TargetSpec>,
}

#[derive(Deserialize)]
struct TargetSpec {
    tag: String,
    pre_position: String,
    #[serde(default)]
    origin_log: Option<String>,
    #[serde(default)]
    origin_ply: Option<u32>,
    #[serde(default)]
    back_plies: Option<u32>,
}

#[derive(Clone)]
struct ProfileDef {
    name: &'static str,
    search_params: &'static [(&'static str, &'static str)],
    root_options: &'static [(&'static str, &'static str)],
    env: &'static [(&'static str, &'static str)],
}

#[derive(Serialize)]
struct EvalResult {
    tag: String,
    profile: String,
    eval_cp: Option<i32>,
    depth: Option<u32>,
    bestmove: Option<String>,
    log_path: String,
    origin_log: Option<String>,
    origin_ply: Option<u32>,
    back_plies: Option<u32>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let engine_bin = resolve_engine_path(&cli.engine_path);
    let targets = load_targets(&cli.targets)?;
    let out_dir = cli
        .out
        .clone()
        .unwrap_or_else(|| cli.targets.parent().unwrap_or_else(|| Path::new(".")).to_path_buf());
    std::fs::create_dir_all(&out_dir)?;
    let mut all_results = Vec::new();

    for target in targets {
        for profile in DEFAULT_PROFILES {
            let result = run_profile(&cli, &engine_bin, &out_dir, &target, profile)
                .with_context(|| format!("failed to evaluate {} ({})", target.tag, profile.name))?;
            println!(
                "{} {}: cp={:?} depth={:?}",
                target.tag, profile.name, result.eval_cp, result.depth
            );
            all_results.push(result);
        }
    }

    let summary_path = out_dir.join("summary.json");
    let mut writer = BufWriter::new(File::create(&summary_path)?);
    serde_json::to_writer_pretty(&mut writer, &all_results)?;
    writer.flush()?;
    println!("summary written to {}", summary_path.display());
    Ok(())
}

fn load_targets(path: &Path) -> Result<Vec<TargetSpec>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let targets_file: TargetsFile = serde_json::from_reader(reader)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if targets_file.targets.is_empty() {
        bail!("no targets found in {}", path.display());
    }
    Ok(targets_file.targets)
}

fn run_profile(
    cli: &Cli,
    engine_bin: &Path,
    out_dir: &Path,
    target: &TargetSpec,
    profile: &ProfileDef,
) -> Result<EvalResult> {
    let mut cmd = Command::new(engine_bin);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for (key, value) in profile.env {
        cmd.env(key, value);
    }
    let mut child = cmd.spawn().with_context(|| format!("spawn {}", engine_bin.display()))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("missing stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow::anyhow!("missing stderr"))?;
    let (tx, rx) = mpsc::channel();
    spawn_reader(stdout, tx.clone());
    spawn_reader(stderr, tx.clone());
    drop(tx);
    let mut stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("missing stdin"))?;
    let mut log_lines: Vec<String> = Vec::new();

    send_cmd(&mut stdin, "usi")?;
    wait_for_patterns(&rx, &["usiok"], Duration::from_secs(5), &mut log_lines)
        .context("waiting usiok")?;
    apply_base_options(&mut stdin, cli)?;
    apply_profile_options(&mut stdin, profile)?;
    send_cmd(&mut stdin, "isready")?;
    wait_for_patterns(&rx, &["readyok"], Duration::from_secs(5), &mut log_lines)
        .context("waiting readyok")?;
    send_cmd(&mut stdin, &format!("position {}", target.pre_position))?;
    send_cmd(&mut stdin, &format!("go byoyomi {}", cli.byoyomi))?;
    wait_for_patterns(
        &rx,
        &["bestmove "],
        Duration::from_millis(cli.byoyomi + 6000),
        &mut log_lines,
    )
    .context("waiting bestmove")?;
    send_cmd(&mut stdin, "quit")?;
    drain_channel(&rx, Duration::from_millis(200), &mut log_lines);
    let _ = child.wait();

    let log_path = out_dir.join(format!("{}__{}.log", target.tag, profile.name));
    write_log(&log_path, &log_lines)?;
    let (eval_cp, depth) = parse_last_eval(&log_lines);
    let bestmove = extract_bestmove(&log_lines);

    Ok(EvalResult {
        tag: target.tag.clone(),
        profile: profile.name.to_string(),
        eval_cp,
        depth,
        bestmove,
        log_path: log_path.display().to_string(),
        origin_log: target.origin_log.clone(),
        origin_ply: target.origin_ply,
        back_plies: target.back_plies,
    })
}

fn send_cmd(stdin: &mut ChildStdin, cmd: &str) -> Result<()> {
    stdin.write_all(cmd.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

fn apply_base_options(stdin: &mut ChildStdin, cli: &Cli) -> Result<()> {
    send_cmd(stdin, "setoption name USI_Ponder value false")?;
    send_cmd(stdin, &format!("setoption name Warmup.Ms value {}", cli.warmup_ms))?;
    send_cmd(stdin, "setoption name ForceTerminateOnHardDeadline value true")?;
    send_cmd(stdin, &format!("setoption name Threads value {}", cli.threads))?;
    send_cmd(stdin, "setoption name USI_Hash value 1024")?;
    send_cmd(stdin, "setoption name MultiPV value 3")?;
    send_cmd(
        stdin,
        &format!("setoption name SearchParams.MinThinkMs value {}", cli.min_think),
    )?;
    Ok(())
}

fn apply_profile_options(stdin: &mut ChildStdin, profile: &ProfileDef) -> Result<()> {
    for (k, v) in profile.search_params {
        send_cmd(stdin, &format!("setoption name SearchParams.{k} value {v}"))?;
    }
    for (k, v) in profile.root_options {
        send_cmd(stdin, &format!("setoption name {k} value {v}"))?;
    }
    Ok(())
}

fn wait_for_patterns(
    rx: &Receiver<String>,
    patterns: &[&str],
    timeout: Duration,
    sink: &mut Vec<String>,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(remaining) {
            Ok(line) => {
                sink.push(line.clone());
                if patterns.iter().any(|p| line.contains(p)) {
                    return Ok(());
                }
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    bail!("timeout waiting for patterns {:?}", patterns);
}

fn drain_channel(rx: &Receiver<String>, timeout: Duration, sink: &mut Vec<String>) {
    while let Ok(line) = rx.recv_timeout(timeout) {
        sink.push(line);
    }
}

fn spawn_reader<T: 'static + std::io::Read + Send>(reader: T, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines() {
            if let Ok(l) = line {
                let _ = tx.send(l);
            } else {
                break;
            }
        }
    });
}

fn write_log(path: &Path, lines: &[String]) -> Result<()> {
    let mut writer = BufWriter::new(File::create(path)?);
    for line in lines {
        writer.write_all(line.as_bytes())?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

static INFO_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"info\s+depth\s+(\d+).*?score\s+cp\s+([+-]?\d+)").unwrap());

fn parse_last_eval(lines: &[String]) -> (Option<i32>, Option<u32>) {
    let mut best_depth = None;
    let mut best_cp = None;
    for line in lines {
        if let Some(caps) = INFO_RE.captures(line) {
            if let (Ok(depth), Ok(cp)) = (caps[1].parse::<u32>(), caps[2].parse::<i32>()) {
                if best_depth.is_none_or(|d| depth >= d) {
                    best_depth = Some(depth);
                    best_cp = Some(cp);
                }
            }
        }
    }
    (best_cp, best_depth)
}

fn extract_bestmove(lines: &[String]) -> Option<String> {
    for line in lines.iter().rev() {
        if let Some(rest) = line.strip_prefix("bestmove ") {
            return rest.split_whitespace().next().map(|s| s.to_string());
        }
    }
    None
}

fn resolve_engine_path(choice: &Option<PathBuf>) -> PathBuf {
    if let Some(p) = choice {
        return p.clone();
    }
    if let Ok(p) = std::env::var("CARGO_BIN_EXE_engine-usi") {
        return PathBuf::from(p);
    }
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            #[cfg(windows)]
            let candidate = dir.join("engine-usi.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("engine-usi");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("engine-usi")
}
