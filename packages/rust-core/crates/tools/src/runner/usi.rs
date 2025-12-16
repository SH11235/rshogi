//! USIプロトコル経由でのベンチマーク実行

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::config::{BenchmarkConfig, EvalConfig, LimitType};
use crate::positions::load_positions;
use crate::report::{BenchResult, BenchmarkReport, EvalInfo, ThreadResult};
use crate::system::collect_system_info;

/// USIエンジンクライアント
struct UsiEngine {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    rx: Receiver<String>,
    /// stdout 読み込みスレッドのハンドル
    reader_handle: Option<thread::JoinHandle<()>>,
}

impl Drop for UsiEngine {
    fn drop(&mut self) {
        // ベストエフォートで quit コマンドを送信
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();

        // プロセスが終了するまで少し待つ
        thread::sleep(Duration::from_millis(100));

        // まだ終了していなければ強制終了（これにより stdout が閉じられる）
        let _ = self.child.kill();

        // リーダースレッドの終了を待つ（stdout が閉じられれば終了する）
        if let Some(handle) = self.reader_handle.take() {
            let _ = handle.join();
        }
    }
}

impl UsiEngine {
    /// エンジンプロセスを起動してUSI初期化
    ///
    /// # 評価オプションについて
    /// `eval_config` で指定された評価設定は、以下のUSIオプションとして送信されます：
    /// - `MaterialLevel`: Material評価レベル（1, 2, 3, 4, 7, 8, 9）
    /// - `EvalFile`: NNUEファイルパス（指定時のみ）
    ///
    /// 注意: これらのオプション名はエンジン依存です。対象エンジンが異なる
    /// オプション名を使用している場合、設定は無視される可能性があります。
    fn spawn(
        engine_path: &Path,
        tt_mb: u32,
        threads: usize,
        eval_config: &EvalConfig,
        verbose: bool,
    ) -> Result<Self> {
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

        // 非同期読み込みスレッド（名前付きで管理しやすく）
        let (tx, rx) = mpsc::channel();
        let reader_handle = thread::Builder::new()
            .name("usi-reader".to_string())
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            })
            .context("Failed to spawn reader thread")?;

        let mut engine = UsiEngine {
            child,
            stdin,
            rx,
            reader_handle: Some(reader_handle),
        };

        // USI初期化
        engine.send("usi")?;
        engine.wait_for("usiok", Duration::from_secs(10))?;

        // オプション設定
        engine.send(&format!("setoption name USI_Hash value {tt_mb}"))?;
        engine.send(&format!("setoption name Threads value {threads}"))?;

        // 評価オプション設定
        engine
            .send(&format!("setoption name MaterialLevel value {}", eval_config.material_level))?;
        if let Some(nnue_path) = &eval_config.nnue_file {
            engine.send(&format!("setoption name EvalFile value {}", nnue_path.display()))?;
        }

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

        // 制限タイプに応じた適切なタイムアウトを設定
        let timeout = match limit_type {
            LimitType::Movetime => {
                // movetime の 2 倍 + 5 秒のマージン
                Duration::from_millis(limit * 2 + 5000)
            }
            LimitType::Depth | LimitType::Nodes => {
                // depth/nodes は時間予測が難しいため保守的に 5 分
                Duration::from_secs(300)
            }
        };

        loop {
            let line = self.rx.recv_timeout(timeout).with_context(|| {
                format!(
                    "Timeout after {:?} waiting for engine response (limit_type={:?}, limit={})",
                    timeout, limit_type, limit
                )
            })?;

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

        let mut engine = UsiEngine::spawn(
            engine_path,
            config.tt_mb,
            *threads,
            &config.eval_config,
            config.verbose,
        )?;
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
        eval_info: Some(EvalInfo::from(&config.eval_config)),
        results: all_results,
    })
}
