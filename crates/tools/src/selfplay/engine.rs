use std::cell::Cell;
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

use super::types::{InfoCallback, InfoSnapshot, SearchOutcome, SearchRequest, duration_to_millis};

/// queue に行が残っていても、期限到達後は読み続けない。
pub(crate) fn receive_before_deadline(
    receiver: &Receiver<String>,
    deadline: Instant,
) -> std::result::Result<String, RecvTimeoutError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(RecvTimeoutError::Timeout);
    }
    receiver.recv_timeout(remaining)
}

pub const ENGINE_READY_TIMEOUT: Duration = Duration::from_secs(120);
pub const ENGINE_QUIT_TIMEOUT: Duration = Duration::from_millis(300);
pub const ENGINE_QUIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const STDERR_BUFFER_LINES: usize = 2;

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
    recent_stderr: Arc<Mutex<VecDeque<String>>>,
    opt_names: HashSet<String>,
    read_timeout_hint_printed: Cell<bool>,
    pub label: String,
}

impl EngineProcess {
    /// プロセスを起動し、USI 初期化を完了する。
    pub fn spawn(cfg: &EngineConfig, label: String) -> Result<Self> {
        let mut proc = Self::spawn_uninitialized(cfg, label)?;
        proc.initialize(cfg)?;
        Ok(proc)
    }

    /// プロセスと reader thread のみを起動する。利用前に `initialize` を呼ぶこと。
    pub fn spawn_uninitialized(cfg: &EngineConfig, label: String) -> Result<Self> {
        let mut cmd = Command::new(&cfg.path);
        if !cfg.args.is_empty() {
            cmd.args(&cfg.args);
        }
        // 子プロセスを独立したプロセスグループで起動し、
        // 親プロセスへの SIGINT が子に伝播しないようにする。
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setpgid は async-signal-safe。fork 直後に呼ばれる。
            unsafe {
                cmd.pre_exec(|| {
                    libc::setpgid(0, 0);
                    Ok(())
                });
            }
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context_display(|| format!("failed to spawn engine at {}", cfg.path.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
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
        let recent_stderr = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_BUFFER_LINES)));
        let stderr_buffer = recent_stderr.clone();
        std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if let Ok(mut buffer) = stderr_buffer.lock() {
                            if buffer.len() == STDERR_BUFFER_LINES {
                                buffer.pop_front();
                            }
                            buffer.push_back(line);
                        } else {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            rx,
            recent_stderr,
            opt_names: HashSet::new(),
            read_timeout_hint_printed: Cell::new(false),
            label,
        })
    }

    /// 起動済みプロセスの USI handshake と初期オプション設定を行う。
    pub fn initialize(&mut self, cfg: &EngineConfig) -> Result<()> {
        self.write_line("usi")?;
        let deadline = Instant::now() + ENGINE_READY_TIMEOUT;
        loop {
            let line = self.recv_line_until(deadline)?;
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
        if req.limit_only_timeout_ms == Some(0) {
            bail!("limit_only_timeout_ms must be positive");
        }
        // パス権がある場合は passrights を付加
        let mut position_cmd = if let Some((b, w)) = req.pass_rights {
            format!("position sfen {} passrights {} {}", req.sfen, b, w)
        } else {
            format!("position sfen {}", req.sfen)
        };
        if !req.moves.is_empty() {
            position_cmd.push_str(" moves ");
            position_cmd.push_str(req.moves);
        }
        self.write_line(&position_cmd)?;
        let time_args = &req.time_args;
        // depth/nodes 制限の有無
        let has_limit = req.go_depth.is_some() || req.go_nodes.is_some();
        let has_time = time_args.byoyomi > 0
            || time_args.btime > 0
            || time_args.wtime > 0
            || time_args.binc > 0
            || time_args.winc > 0;

        // 時間制御コマンドを構築
        // byoyomi のみの場合は "go byoyomi N" だけを送る（btime 0 等を付けると
        // 一部エンジンが btime=0 を「持ち時間なし」と解釈して即指しする）
        let time_cmd = if time_args.byoyomi > 0
            && time_args.btime == 0
            && time_args.wtime == 0
            && time_args.binc == 0
            && time_args.winc == 0
        {
            // 純粋な秒読みモード
            format!("go byoyomi {}", time_args.byoyomi)
        } else if time_args.byoyomi > 0 {
            // 持ち時間 + 秒読み
            format!(
                "go btime {} wtime {} byoyomi {}",
                time_args.btime, time_args.wtime, time_args.byoyomi
            )
        } else if time_args.btime > 0 || time_args.binc > 0 {
            // フィッシャー（btime/binc のみ、byoyomi なし）
            format!(
                "go btime {} wtime {} binc {} winc {}",
                time_args.btime, time_args.wtime, time_args.binc, time_args.winc
            )
        } else {
            String::from("go")
        };

        if has_limit && has_time {
            // 組み合わせモード: 時間制御 + depth/nodes 目標
            let mut cmd = time_cmd;
            if let Some(depth) = req.go_depth {
                cmd.push_str(&format!(" depth {depth}"));
            }
            if let Some(nodes) = req.go_nodes {
                cmd.push_str(&format!(" nodes {nodes}"));
            }
            self.write_line(&cmd)?;
        } else if has_limit {
            // depth/nodes のみモード。期限は下の watchdog 設定で決める。
            let mut cmd = String::from("go");
            if let Some(depth) = req.go_depth {
                cmd.push_str(&format!(" depth {depth}"));
            }
            if let Some(nodes) = req.go_nodes {
                cmd.push_str(&format!(" nodes {nodes}"));
            }
            self.write_line(&cmd)?;
        } else {
            self.write_line(&time_cmd)?;
        }

        let start = Instant::now();
        // watchdog 未指定の depth/nodes のみモードは従来の24時間を維持する。
        // Duration::MAX は recv_timeout に渡すと timespec オーバーフローが起きるため、
        // 十分大きな有限値（24 時間）を使う。
        const NO_TIMEOUT: Duration = Duration::from_secs(86400);
        let (soft_limit, hard_limit) = if has_limit && !has_time {
            match req.limit_only_timeout_ms {
                Some(ms) => (
                    Duration::from_millis(ms),
                    Duration::from_millis(ms.saturating_add(req.timeout_margin_ms)),
                ),
                None => (NO_TIMEOUT, NO_TIMEOUT),
            }
        } else {
            let s = Duration::from_millis(req.think_limit_ms.saturating_add(req.timeout_margin_ms));
            let h = s + Duration::from_millis(req.timeout_margin_ms);
            (s, h)
        };
        let watchdog_enabled = has_limit && !has_time && req.limit_only_timeout_ms.is_some();
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
                    watchdog_fired: watchdog_enabled,
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
                        // stop 後の回収結果も、受信時点で期限を超えた結果も勝敗に使わない。
                        let watchdog_fired =
                            watchdog_enabled && (stop_sent || start.elapsed() >= soft_limit);
                        let timed_out = if has_limit && !has_time {
                            watchdog_fired
                        } else {
                            elapsed_ms > req.think_limit_ms.saturating_add(req.timeout_margin_ms)
                        };
                        return Ok(SearchOutcome {
                            bestmove: Some(mv),
                            elapsed_ms,
                            timed_out,
                            watchdog_fired,
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
                            watchdog_fired: watchdog_enabled,
                            eval: snapshot.into_eval_log(),
                        });
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("{}", self.engine_exited_message());
                }
            }
        }
    }

    /// 任意の `go` 引数で 1 局面を探索する。
    ///
    /// `position_tail` は `position sfen ...` の `sfen` 以降にそのまま渡す。
    /// 例: `lnsg... b - 1 moves 7g7f`。
    /// 受信期限は info 行で延長せず、期限後は stop を送り最大 10 秒応答を回収する。
    /// 回収できても `timed_out=true` は探索成功ではないため、呼出側で判定する。
    pub fn search_raw_go(
        &mut self,
        position_tail: &str,
        go_args: &str,
        timeout: Duration,
        info_callback: Option<&mut dyn FnMut(&str)>,
    ) -> Result<SearchOutcome> {
        self.search_raw_go_with_stop_grace(
            position_tail,
            go_args,
            timeout,
            Duration::from_secs(10),
            info_callback,
        )
    }

    fn search_raw_go_with_stop_grace(
        &mut self,
        position_tail: &str,
        go_args: &str,
        timeout: Duration,
        stop_grace: Duration,
        mut info_callback: Option<&mut dyn FnMut(&str)>,
    ) -> Result<SearchOutcome> {
        self.write_line(&format!("position sfen {position_tail}"))?;
        let go_cmd = if go_args.trim().is_empty() {
            "go".to_string()
        } else {
            format!("go {}", go_args.trim())
        };
        self.write_line(&go_cmd)?;

        let start = Instant::now();
        let deadline =
            start.checked_add(timeout).ok_or_else(|| anyhow!("search deadline overflow"))?;
        let mut snapshot = InfoSnapshot::default();
        loop {
            match receive_before_deadline(&self.rx, deadline) {
                Ok(line) => {
                    if line.starts_with("info") {
                        snapshot.update_from_line(&line);
                        if let Some(cb) = info_callback.as_mut() {
                            cb(&line);
                        }
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix("bestmove ") {
                        let mut parts = rest.split_whitespace();
                        let mv = parts.next().unwrap_or_default().to_string();
                        return Ok(SearchOutcome {
                            bestmove: Some(mv),
                            elapsed_ms: duration_to_millis(start.elapsed()),
                            timed_out: false,
                            watchdog_fired: false,
                            eval: snapshot.into_eval_log(),
                        });
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.write_line("stop")?;
                    let stop_deadline = Instant::now()
                        .checked_add(stop_grace)
                        .ok_or_else(|| anyhow!("stop deadline overflow"))?;
                    loop {
                        match receive_before_deadline(&self.rx, stop_deadline) {
                            Ok(line) if line.starts_with("info") => {
                                snapshot.update_from_line(&line);
                            }
                            Ok(line) => {
                                if let Some(rest) = line.strip_prefix("bestmove ") {
                                    let mut parts = rest.split_whitespace();
                                    let mv = parts.next().unwrap_or_default().to_string();
                                    return Ok(SearchOutcome {
                                        bestmove: Some(mv),
                                        elapsed_ms: duration_to_millis(start.elapsed()),
                                        timed_out: true,
                                        watchdog_fired: false,
                                        eval: snapshot.into_eval_log(),
                                    });
                                }
                            }
                            Err(RecvTimeoutError::Timeout) => {
                                return Ok(SearchOutcome {
                                    bestmove: None,
                                    elapsed_ms: duration_to_millis(start.elapsed()),
                                    timed_out: true,
                                    watchdog_fired: false,
                                    eval: snapshot.into_eval_log(),
                                });
                            }
                            Err(RecvTimeoutError::Disconnected) => {
                                bail!("{}", self.engine_exited_message());
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("{}", self.engine_exited_message());
                }
            }
        }
    }

    pub fn sync_ready(&mut self) -> Result<()> {
        self.write_line("isready")?;
        let deadline = Instant::now() + ENGINE_READY_TIMEOUT;
        loop {
            let line = self.recv_line_until(deadline)?;
            if line == "readyok" {
                break;
            }
        }
        Ok(())
    }

    fn recv_line_until(&self, deadline: Instant) -> Result<String> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("{}", self.engine_read_timeout_message(ENGINE_READY_TIMEOUT));
        }
        self.recv_line(remaining)
    }

    pub fn recv_line(&self, timeout: Duration) -> Result<String> {
        match self.rx.recv_timeout(timeout) {
            Ok(line) => Ok(line),
            Err(RecvTimeoutError::Timeout) => {
                let message = self.engine_read_timeout_message(timeout);
                if !self.read_timeout_hint_printed.replace(true) {
                    eprintln!("{message}");
                }
                Err(anyhow!(message))
            }
            Err(RecvTimeoutError::Disconnected) => {
                bail!("{}", self.engine_exited_message())
            }
        }
    }

    fn engine_read_timeout_message(&self, timeout: Duration) -> String {
        let mut message = format!(
            "{}: engine read timeout after {} ms\n  typical causes: missing EvalFile / slow NNUE load / engine panic during isready",
            self.label,
            timeout.as_millis()
        );
        self.append_recent_stderr(&mut message);
        message
    }

    fn engine_exited_message(&self) -> String {
        let mut message = format!(
            "{}: engine exited unexpectedly\n  typical causes: missing EvalFile / engine panic during usi/isready/search",
            self.label
        );
        self.append_recent_stderr(&mut message);
        message
    }

    fn recent_stderr_lines(&self) -> Vec<String> {
        let Ok(buffer) = self.recent_stderr.lock() else {
            return Vec::new();
        };
        buffer.iter().cloned().collect()
    }

    fn append_recent_stderr(&self, message: &mut String) {
        let stderr_lines = self.recent_stderr_lines();
        if !stderr_lines.is_empty() {
            message.push_str("\n  recent engine stderr:");
            for line in stderr_lines {
                message.push_str("\n    ");
                message.push_str(&line);
            }
        }
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

#[cfg(all(test, unix))]
mod deadline_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn expired_deadline_does_not_consume_queued_info() {
        let (tx, rx) = mpsc::channel();
        tx.send("info depth 1".into()).unwrap();
        assert!(matches!(
            receive_before_deadline(&rx, Instant::now()),
            Err(RecvTimeoutError::Timeout)
        ));
        assert_eq!(rx.try_recv().unwrap(), "info depth 1");
    }

    #[test]
    fn raw_go_bounds_both_phases_with_chatty_engine_and_handles_exit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mock.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
flood=
trap 'if [ -n "$flood" ]; then kill "$flood" 2>/dev/null; wait "$flood" 2>/dev/null; fi' 0
while IFS= read -r line; do
  printf '%s
' "$line" >> "$1"
  case "$line" in
    usi) printf 'usiok
' ;;
    isready) printf 'readyok
' ;;
    go*)
      case "$2" in
        normal) printf 'bestmove resign
' ;;
        exit) exit 0 ;;
        *) (while :; do printf 'info depth 1 nodes 1
'; sleep 0.005; done) & flood=$! ;;
      esac ;;
    stop) if [ "$2" = "stop" ]; then printf 'bestmove resign
'; fi ;;
    quit) break ;;
  esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        for mode in ["normal", "stop", "flood", "exit"] {
            let log = dir.path().join(mode);
            let cfg = EngineConfig {
                path: path.clone(),
                args: vec![log.display().to_string(), mode.into()],
                threads: 1,
                hash_mb: 1,
                network_delay: None,
                network_delay2: None,
                minimum_thinking_time: None,
                slowmover: None,
                ponder: false,
                usi_options: vec![],
            };
            let mut engine = EngineProcess::spawn(&cfg, mode.into()).unwrap();
            let start = Instant::now();
            let result = engine.search_raw_go_with_stop_grace(
                "fixture b - 1",
                "depth 1",
                Duration::from_millis(30),
                Duration::from_millis(40),
                None,
            );
            assert!(start.elapsed() < Duration::from_secs(2), "{mode}");
            if mode == "exit" {
                assert!(result.is_err());
            } else {
                let result = result.unwrap();
                assert_eq!(result.timed_out, mode != "normal");
                assert_eq!(result.bestmove.is_some(), mode != "flood");
            }
            drop(engine);
            let commands = std::fs::read_to_string(log).unwrap();
            assert_eq!(
                commands.lines().filter(|line| *line == "stop").count(),
                usize::from(matches!(mode, "stop" | "flood"))
            );
        }
    }
}
