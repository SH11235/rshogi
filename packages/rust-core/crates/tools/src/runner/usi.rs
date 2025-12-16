//! USIプロトコル経由でのベンチマーク実行

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::config::{BenchmarkConfig, LimitType};
use crate::positions::load_positions;
use crate::report::{BenchResult, BenchmarkReport, ThreadResult};
use crate::system::collect_system_info;

/// USIエンジンクライアント
struct UsiEngine {
    _child: Child,
    stdin: BufWriter<ChildStdin>,
    rx: Receiver<String>,
}

impl UsiEngine {
    /// エンジンプロセスを起動してUSI初期化
    fn spawn(engine_path: &Path, tt_mb: u32, threads: usize, verbose: bool) -> Result<Self> {
        // verbose モードでは stderr を表示（デバッグ用）
        let stderr_config = if verbose {
            Stdio::inherit()
        } else {
            Stdio::null()
        };

        let mut child = Command::new(engine_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr_config)
            .spawn()
            .with_context(|| format!("Failed to spawn engine: {}", engine_path.display()))?;

        let stdin = BufWriter::new(child.stdin.take().context("Failed to get engine stdin")?);

        let stdout = child.stdout.take().context("Failed to get engine stdout")?;

        // 非同期読み込みスレッド
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        let mut engine = UsiEngine {
            _child: child,
            stdin,
            rx,
        };

        // USI初期化
        engine.send("usi")?;
        engine.wait_for("usiok", Duration::from_secs(10))?;

        // オプション設定
        engine.send(&format!("setoption name USI_Hash value {tt_mb}"))?;
        engine.send(&format!("setoption name Threads value {threads}"))?;
        engine.send("isready")?;
        engine.wait_for("readyok", Duration::from_secs(30))?;

        Ok(engine)
    }

    /// コマンド送信
    fn send(&mut self, cmd: &str) -> Result<()> {
        writeln!(self.stdin, "{cmd}").context("Failed to write to engine")?;
        self.stdin.flush().context("Failed to flush engine stdin")?;
        Ok(())
    }

    /// 特定の応答を待つ（タイムアウト付き）
    fn wait_for(&mut self, expected: &str, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(line) if line.starts_with(expected) => return Ok(()),
                Ok(_) => continue, // 別の応答が来た
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("Engine disconnected while waiting for '{expected}'")
                }
            }
        }
        anyhow::bail!("Timeout waiting for '{expected}'")
    }

    /// 1局面のベンチマークを実行
    fn bench_position(
        &mut self,
        sfen: &str,
        limit_type: LimitType,
        limit: u64,
        verbose: bool,
    ) -> Result<BenchResult> {
        self.send(&format!("position sfen {sfen}"))?;
        self.send(&format!("go {} {limit}", limit_type.to_usi_cmd()))?;

        let mut last_info = InfoSnapshot::default();
        let start = Instant::now();

        loop {
            let line = self
                .rx
                .recv_timeout(Duration::from_secs(600))
                .context("Timeout waiting for engine response")?;

            if line.starts_with("info") {
                last_info.update_from_line(&line);
                if verbose {
                    println!("    {line}");
                }
            } else if line.starts_with("bestmove") {
                let bestmove =
                    line.split_whitespace().nth(1).map(|s| s.to_string()).unwrap_or_else(|| {
                        eprintln!("Warning: Invalid bestmove format: {line}");
                        "none".to_string()
                    });

                return Ok(BenchResult {
                    sfen: sfen.to_string(),
                    depth: last_info.depth,
                    nodes: last_info.nodes,
                    time_ms: start.elapsed().as_millis() as u64,
                    nps: last_info.nps,
                    hashfull: last_info.hashfull,
                    bestmove,
                });
            }
        }
    }
}

/// info行のスナップショット
#[derive(Debug, Clone, Default)]
struct InfoSnapshot {
    depth: i32,
    nodes: u64,
    nps: u64,
    hashfull: u32,
}

impl InfoSnapshot {
    /// info行をパースして更新
    fn update_from_line(&mut self, line: &str) {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let mut i = 0;

        while i < tokens.len() {
            match tokens[i] {
                "depth" => {
                    if i + 1 < tokens.len() {
                        if let Ok(val) = tokens[i + 1].parse() {
                            self.depth = val;
                        } else {
                            eprintln!("Warning: Failed to parse depth: {}", tokens[i + 1]);
                        }
                    }
                }
                "nodes" => {
                    if i + 1 < tokens.len() {
                        if let Ok(val) = tokens[i + 1].parse() {
                            self.nodes = val;
                        } else {
                            eprintln!("Warning: Failed to parse nodes: {}", tokens[i + 1]);
                        }
                    }
                }
                "nps" => {
                    if i + 1 < tokens.len() {
                        if let Ok(val) = tokens[i + 1].parse() {
                            self.nps = val;
                        } else {
                            eprintln!("Warning: Failed to parse nps: {}", tokens[i + 1]);
                        }
                    }
                }
                "hashfull" => {
                    if i + 1 < tokens.len() {
                        if let Ok(val) = tokens[i + 1].parse() {
                            self.hashfull = val;
                        } else {
                            eprintln!("Warning: Failed to parse hashfull: {}", tokens[i + 1]);
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// USI経由でベンチマークを実行
pub fn run_usi_benchmark(config: &BenchmarkConfig, engine_path: &Path) -> Result<BenchmarkReport> {
    let positions = load_positions(config)?;
    let mut all_results = Vec::new();

    for threads in &config.threads {
        println!("=== Threads: {} ===", threads);

        let mut engine = UsiEngine::spawn(engine_path, config.tt_mb, *threads, config.verbose)?;
        let mut thread_results = Vec::new();

        for iteration in 0..config.iterations {
            if config.iterations > 1 {
                println!("Iteration {}/{}", iteration + 1, config.iterations);
            }

            for (name, sfen) in &positions {
                if config.verbose {
                    println!("  Position: {name}");
                }

                let bench_result =
                    engine.bench_position(sfen, config.limit_type, config.limit, config.verbose)?;

                if config.verbose {
                    println!(
                        "    depth={} nodes={} time={}ms nps={}",
                        bench_result.depth,
                        bench_result.nodes,
                        bench_result.time_ms,
                        bench_result.nps
                    );
                }

                thread_results.push(bench_result);
            }
        }

        all_results.push(ThreadResult {
            threads: *threads,
            results: thread_results,
        });
    }

    // エンジン名をパスから取得
    let engine_name = engine_path.file_name().and_then(|n| n.to_str()).map(|s| s.to_string());

    Ok(BenchmarkReport {
        system_info: collect_system_info(),
        engine_name,
        engine_path: Some(engine_path.display().to_string()),
        results: all_results,
    })
}
