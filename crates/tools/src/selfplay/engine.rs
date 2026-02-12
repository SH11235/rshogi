use std::collections::HashSet;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use super::types::{duration_to_millis, InfoCallback, InfoSnapshot, SearchOutcome, SearchRequest};

pub const ENGINE_READY_TIMEOUT: Duration = Duration::from_secs(30);
pub const ENGINE_QUIT_TIMEOUT: Duration = Duration::from_millis(300);
pub const ENGINE_QUIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// エンジンプロセス起動時の設定。
pub struct EngineConfig {
    pub path: PathBuf,
    pub args: Vec<String>,
    pub threads: usize,
    pub hash_mb: u32,
    pub network_delay: Option<i64>,
    pub network_delay2: Option<i64>,
    pub minimum_thinking_time: Option<i64>,
    pub slowmover: Option<i32>,
    pub ponder: bool,
    /// 追加のUSIオプション (Name=Value 形式)
    pub usi_options: Vec<String>,
}

/// 1本のエンジンに対する入出力をカプセル化する。
pub struct EngineProcess {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    rx: Receiver<String>,
    opt_names: HashSet<String>,
    pub label: String,
}

impl EngineProcess {
    pub fn spawn(cfg: &EngineConfig, label: String) -> Result<Self> {
        let mut cmd = Command::new(&cfg.path);
        if !cfg.args.is_empty() {
            cmd.args(&cfg.args);
        }
        let mut child =
            cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().with_context_display(
                || format!("failed to spawn engine at {}", cfg.path.display()),
            )?;
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

    pub fn new_game(&mut self) -> Result<()> {
        self.write_line("usinewgame")?;
        self.sync_ready()
    }

    /// 探索を実行する。
    ///
    /// `info_callback`: info行を受け取るコールバック。
    ///   - 引数: (info行, SearchRequest)
    ///   - `None` の場合はinfo行をスキップする。
    pub fn search(
        &mut self,
        req: &SearchRequest<'_>,
        info_callback: Option<&mut InfoCallback<'_>>,
    ) -> Result<SearchOutcome> {
        // パス権がある場合は passrights を付加
        let position_cmd = if let Some((b, w)) = req.pass_rights {
            format!("position sfen {} passrights {} {}", req.sfen, b, w)
        } else {
            format!("position sfen {}", req.sfen)
        };
        self.write_line(&position_cmd)?;
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
        let mut info_callback = info_callback;

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
                        if let Some(ref mut cb) = info_callback {
                            cb(&line, req);
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

    pub fn sync_ready(&mut self) -> Result<()> {
        self.write_line("isready")?;
        loop {
            let line = self.recv_line(ENGINE_READY_TIMEOUT)?;
            if line == "readyok" {
                break;
            }
        }
        Ok(())
    }

    pub fn recv_line(&self, timeout: Duration) -> Result<String> {
        self.rx
            .recv_timeout(timeout)
            .map_err(|_| anyhow!("{}: engine read timeout", self.label))
    }

    pub fn set_option_if_available(&mut self, name: &str, value: &str) -> Result<()> {
        if self.opt_names.is_empty() || self.opt_names.contains(name) {
            self.write_line(&format!("setoption name {} value {}", name, value))?;
        }
        Ok(())
    }

    pub fn write_line(&mut self, msg: &str) -> Result<()> {
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

pub fn parse_option_name(line: &str) -> Option<String> {
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

/// `anyhow::Context` の `with_context` と同等だが、クロージャが `String` を返す簡易ヘルパー。
trait ContextDisplay<T> {
    fn with_context_display<F: FnOnce() -> String>(self, f: F) -> Result<T>;
}

impl<T> ContextDisplay<T> for std::result::Result<T, std::io::Error> {
    fn with_context_display<F: FnOnce() -> String>(self, f: F) -> Result<T> {
        self.map_err(|e| anyhow!("{}: {}", f(), e))
    }
}

/// エンジンバイナリを指定ディレクトリから探す。
pub fn find_engine_in_dir(dir: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let release_names = ["rshogi-usi.exe"];
    #[cfg(not(windows))]
    let release_names = ["rshogi-usi"];
    #[cfg(windows)]
    let debug_names = ["rshogi-usi-debug.exe"];
    #[cfg(not(windows))]
    let debug_names = ["rshogi-usi-debug"];

    for name in release_names {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for name in debug_names {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
