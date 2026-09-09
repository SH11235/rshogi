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
use crate::selfplay::engine::receive_before_deadline;
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
        eval_hash_mb: u32,
        use_eval_hash: bool,
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

        engine.send(&format!("setoption name EvalHash value {eval_hash_mb}"))?;
        engine.send(&format!("setoption name UseEvalHash value {use_eval_hash}"))?;

        // 評価オプション設定
        if let Some(nnue_path) = &eval_config.nnue_file {
            engine.send(&format!("setoption name EvalFile value {}", nnue_path.display()))?;
        }

        // 追加の USI オプション
        for opt in &eval_config.usi_options {
            if let Some((name, value)) = opt.split_once('=') {
                engine.send(&format!("setoption name {name} value {value}"))?;
            }
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

        // 制限タイプに応じた適切なタイムアウトを設定
        let timeout = match limit_type {
            LimitType::Movetime => {
                // movetime の 2 倍 + 5 秒のマージン
                Duration::from_millis(limit.saturating_mul(2).saturating_add(5000))
            }
            LimitType::Depth | LimitType::Nodes => {
                // depth/nodes は時間予測が難しいため保守的に 5 分
                Duration::from_secs(300)
            }
        };

        self.receive_bench_result(sfen, verbose, timeout, Duration::from_secs(10))
    }

    fn receive_bench_result(
        &mut self,
        sfen: &str,
        verbose: bool,
        timeout: Duration,
        stop_grace: Duration,
    ) -> Result<BenchResult> {
        let start = Instant::now();
        let mut deadline = start.checked_add(timeout).context("benchmark deadline overflow")?;
        let mut stopped = false;
        let mut last_info = InfoSnapshot::default();
        loop {
            let line = match receive_before_deadline(&self.rx, deadline) {
                Ok(line) => line,
                Err(mpsc::RecvTimeoutError::Timeout) if !stopped => {
                    self.send("stop")?;
                    stopped = true;
                    deadline =
                        Instant::now().checked_add(stop_grace).context("stop deadline overflow")?;
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    anyhow::bail!("Benchmark timed out after stop grace")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("Engine disconnected during benchmark")
                }
            };

            if line.starts_with("info") {
                last_info.update_from_line(&line);
                if verbose {
                    println!("    {line}");
                }
            } else if line.starts_with("bestmove") {
                if stopped {
                    anyhow::bail!("Benchmark timed out; engine returned bestmove after stop");
                }
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
                    is_warmup: None,
                    search_run_index: None,
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
                "depth" if i + 1 < tokens.len() => {
                    if let Ok(val) = tokens[i + 1].parse() {
                        self.depth = val;
                    } else {
                        eprintln!("Warning: Failed to parse depth: {}", tokens[i + 1]);
                    }
                }
                "nodes" if i + 1 < tokens.len() => {
                    if let Ok(val) = tokens[i + 1].parse() {
                        self.nodes = val;
                    } else {
                        eprintln!("Warning: Failed to parse nodes: {}", tokens[i + 1]);
                    }
                }
                "nps" if i + 1 < tokens.len() => {
                    if let Ok(val) = tokens[i + 1].parse() {
                        self.nps = val;
                    } else {
                        eprintln!("Warning: Failed to parse nps: {}", tokens[i + 1]);
                    }
                }
                "hashfull" if i + 1 < tokens.len() => {
                    if let Ok(val) = tokens[i + 1].parse() {
                        self.hashfull = val;
                    } else {
                        eprintln!("Warning: Failed to parse hashfull: {}", tokens[i + 1]);
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
            config.eval_hash_mb,
            config.use_eval_hash,
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

#[cfg(all(test, unix))]
mod contract_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn benchmark_bounds_chatty_search_and_stop_and_sends_eval_hash_options() {
        let dir = tempfile::tempdir().unwrap();
        for (mode, use_hash) in [
            ("normal", true),
            ("stop", false),
            ("flood", true),
            ("exit", false),
        ] {
            let path = dir.path().join(format!("{mode}.sh"));
            let log = dir.path().join(format!("{mode}.log"));
            // テスト用 mock の固定パスだけを埋め込む。
            let script = format!(
                r#"#!/bin/sh
flood=
trap 'if [ -n "$flood" ]; then kill "$flood" 2>/dev/null; wait "$flood" 2>/dev/null; fi' 0
while IFS= read -r line; do
  printf '%s
' "$line" >> '{}'
  case "$line" in
    usi) printf 'usiok
' ;;
    isready) printf 'readyok
' ;;
    go*)
      case '{}' in
        normal) printf 'info nodes 5 depth 1
bestmove resign
' ;;
        exit) exit 0 ;;
        *) (while :; do printf 'info depth 1 nodes 1
'; sleep 0.005; done) & flood=$! ;;
      esac ;;
    stop) if [ '{}' = "stop" ]; then printf 'bestmove resign
'; fi ;;
    quit) break ;;
  esac
done
"#,
                log.display(),
                mode,
                mode
            );
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            let eval = EvalConfig {
                nnue_file: None,
                usi_options: vec!["EvalHash=32".into(), "UseEvalHash=false".into()],
            };
            let mut engine = UsiEngine::spawn(&path, 1, 1, &eval, 16, use_hash, false).unwrap();
            engine.send("go depth 1").unwrap();
            let start = Instant::now();
            let result = engine.receive_bench_result(
                "fixture",
                false,
                Duration::from_millis(30),
                Duration::from_millis(40),
            );
            assert!(start.elapsed() < Duration::from_secs(2), "{mode}");
            assert_eq!(result.is_ok(), mode == "normal", "{mode}");
            drop(engine);
            let commands = std::fs::read_to_string(log).unwrap();
            let lines = commands.lines().collect::<Vec<_>>();
            let dedicated = lines
                .iter()
                .position(|line| *line == "setoption name EvalHash value 16")
                .unwrap();
            let override_pos = lines
                .iter()
                .position(|line| *line == "setoption name EvalHash value 32")
                .unwrap();
            assert!(dedicated < override_pos);
            let first_flag = format!("setoption name UseEvalHash value {use_hash}");
            assert!(lines.contains(&first_flag.as_str()));
            assert_eq!(
                lines.iter().filter(|line| **line == "stop").count(),
                usize::from(matches!(mode, "stop" | "flood"))
            );
        }
    }
}
