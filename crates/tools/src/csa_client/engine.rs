//! USIエンジン管理（ponder対応）

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

const READY_TIMEOUT: Duration = Duration::from_secs(120);

/// USIエンジンプロセス
pub struct UsiEngine {
    child: Child,
    writer: BufWriter<ChildStdin>,
    rx: Receiver<String>,
    pub engine_name: String,
    quit_sent: bool,
}

/// bestmove の解析結果
#[derive(Clone, Debug)]
pub struct BestMoveResult {
    pub bestmove: String,
    pub ponder_move: Option<String>,
}

/// info 行から抽出した探索情報
#[derive(Clone, Debug, Default)]
pub struct SearchInfo {
    pub depth: Option<u32>,
    pub score_cp: Option<i32>,
    pub score_mate: Option<i32>,
    pub pv: Vec<String>,
}

impl UsiEngine {
    /// USIエンジンを起動し、初期化する
    pub fn spawn(
        path: &Path,
        options: &HashMap<String, toml::Value>,
        ponder: bool,
        timeout: Duration,
    ) -> Result<Self> {
        let mut cmd = Command::new(path);
        // 子プロセスを独立したプロセスグループで起動
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: setpgid は async-signal-safe
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
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow!("エンジン起動失敗 {}: {e}", path.display()))?;

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

        let mut engine = Self {
            child,
            writer: BufWriter::new(stdin),
            rx,
            engine_name: String::new(),
            quit_sent: false,
        };
        engine.initialize(options, ponder, timeout)?;
        Ok(engine)
    }

    fn initialize(
        &mut self,
        options: &HashMap<String, toml::Value>,
        ponder: bool,
        timeout: Duration,
    ) -> Result<()> {
        self.send("usi")?;
        // usiok を待つ
        loop {
            let line = self.recv(timeout)?;
            if let Some(rest) = line.strip_prefix("id name ") {
                self.engine_name = rest.to_string();
            } else if line == "usiok" {
                break;
            }
        }

        // USI オプション設定
        for (key, value) in options {
            let val_str = match value {
                toml::Value::Integer(n) => n.to_string(),
                toml::Value::Boolean(b) => b.to_string(),
                toml::Value::String(s) => s.clone(),
                toml::Value::Float(f) => f.to_string(),
                _ => continue,
            };
            self.send(&format!("setoption name {key} value {val_str}"))?;
        }

        // Ponder 設定
        if ponder {
            self.send("setoption name USI_Ponder value true")?;
        }

        // isready → readyok
        self.send("isready")?;
        loop {
            let line = self.recv(timeout)?;
            if line == "readyok" {
                break;
            }
        }
        log::info!(
            "[USI] エンジン準備完了: {}",
            if self.engine_name.is_empty() {
                "(unknown)"
            } else {
                &self.engine_name
            }
        );
        Ok(())
    }

    /// 新しい対局を開始
    pub fn new_game(&mut self) -> Result<()> {
        self.send("usinewgame")?;
        self.send("isready")?;
        loop {
            let line = self.recv(READY_TIMEOUT)?;
            if line == "readyok" {
                break;
            }
        }
        Ok(())
    }

    /// 探索を開始し、bestmove を待つ。
    /// `shutdown` がセットされたら stop を送信し、bestmove を "resign" として返す。
    pub fn go(
        &mut self,
        position_cmd: &str,
        go_cmd: &str,
        shutdown: &AtomicBool,
    ) -> Result<(BestMoveResult, SearchInfo)> {
        self.send(position_cmd)?;
        self.send(go_cmd)?;
        self.wait_bestmove(shutdown)
    }

    /// ponder 探索を開始（bestmove を待たない）
    pub fn go_ponder(&mut self, position_cmd: &str, go_cmd: &str) -> Result<()> {
        self.send(position_cmd)?;
        self.send(go_cmd)?;
        Ok(())
    }

    /// ponderhit を送信し、bestmove を待つ。
    /// `shutdown` がセットされたら stop を送信し、bestmove を "resign" として返す。
    pub fn ponderhit(&mut self, shutdown: &AtomicBool) -> Result<(BestMoveResult, SearchInfo)> {
        self.send("ponderhit")?;
        self.wait_bestmove(shutdown)
    }

    /// stop を送信し、bestmove を待つ（ponder 中断用）
    pub fn stop_and_wait(&mut self) -> Result<()> {
        self.send("stop")?;
        // bestmove を読み捨てる
        loop {
            let line = self.recv(Duration::from_secs(10))?;
            if line.starts_with("bestmove") {
                break;
            }
        }
        Ok(())
    }

    /// gameover を送信
    pub fn gameover(&mut self, result: &str) -> Result<()> {
        self.send(&format!("gameover {result}"))
    }

    /// quit を送信してプロセスを終了（タイムアウト付き）
    pub fn quit(&mut self) {
        if !self.quit_sent {
            let _ = self.send("quit");
            // 3秒待ってまだ終了しなければ kill
            for _ in 0..30 {
                if let Ok(Some(_)) = self.child.try_wait() {
                    self.quit_sent = true;
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            log::warn!("[USI] quit タイムアウト、kill します");
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.quit_sent = true;
        }
    }

    fn wait_bestmove(&mut self, shutdown: &AtomicBool) -> Result<(BestMoveResult, SearchInfo)> {
        use std::time::Instant;

        const OVERALL_TIMEOUT: Duration = Duration::from_secs(3600);
        const POST_STOP_TIMEOUT: Duration = Duration::from_secs(10);

        let mut info = SearchInfo::default();
        let mut stop_sent = false;
        let start = Instant::now();
        let mut stop_sent_at: Option<Instant> = None;

        loop {
            // 全体タイムアウト: 通常時1時間、stop送信後10秒
            let elapsed = start.elapsed();
            if let Some(st) = stop_sent_at {
                if st.elapsed() >= POST_STOP_TIMEOUT {
                    bail!(
                        "stop 送信後 {}秒以内に bestmove が返りませんでした",
                        POST_STOP_TIMEOUT.as_secs()
                    );
                }
            } else if elapsed >= OVERALL_TIMEOUT {
                log::warn!("[USI] 全体タイムアウト ({}秒)、stop 送信", OVERALL_TIMEOUT.as_secs());
                self.send("stop")?;
                stop_sent = true;
                stop_sent_at = Some(Instant::now());
            }

            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    log::trace!("[USI] < {line}");
                    if line.starts_with("info") {
                        update_search_info(&mut info, &line);
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix("bestmove ") {
                        let mut parts = rest.split_whitespace();
                        let bestmove = parts.next().unwrap_or("resign").to_string();
                        // shutdown で stop を送った場合は bestmove を resign に差し替え
                        let bestmove = if stop_sent {
                            "resign".to_string()
                        } else {
                            bestmove
                        };
                        let ponder_move = if !stop_sent && parts.next() == Some("ponder") {
                            parts.next().map(|s| s.to_string())
                        } else {
                            None
                        };
                        return Ok((
                            BestMoveResult {
                                bestmove,
                                ponder_move,
                            },
                            info,
                        ));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // shutdown が要求されたら stop を送信
                    if !stop_sent && shutdown.load(Ordering::SeqCst) {
                        log::info!("[USI] shutdown 要求により stop 送信");
                        self.send("stop")?;
                        stop_sent = true;
                        stop_sent_at = Some(Instant::now());
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("エンジンプロセスが終了しました");
                }
            }
        }
    }

    pub fn send(&mut self, cmd: &str) -> Result<()> {
        log::debug!("[USI] > {cmd}");
        self.writer.write_all(cmd.as_bytes())?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }

    fn recv(&self, timeout: Duration) -> Result<String> {
        match self.rx.recv_timeout(timeout) {
            Ok(line) => {
                log::trace!("[USI] < {line}");
                Ok(line)
            }
            Err(RecvTimeoutError::Timeout) => {
                bail!("エンジン応答タイムアウト");
            }
            Err(RecvTimeoutError::Disconnected) => {
                bail!("エンジンプロセスが終了しました");
            }
        }
    }

    /// バッファに溜まっている行を非ブロッキングで全て読み捨てる
    pub fn drain(&self) {
        while self.rx.try_recv().is_ok() {}
    }
}

impl Drop for UsiEngine {
    fn drop(&mut self) {
        if !self.quit_sent {
            let _ = self.send("quit");
            // 少し待ってからプロセスを kill
            std::thread::sleep(Duration::from_millis(100));
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn update_search_info(info: &mut SearchInfo, line: &str) {
    let mut tokens = line.split_whitespace().peekable();
    let mut in_pv = false;
    while let Some(token) = tokens.next() {
        if in_pv {
            info.pv.push(token.to_string());
            continue;
        }
        match token {
            "depth" => {
                if let Some(v) = tokens.peek().and_then(|s| s.parse().ok()) {
                    info.depth = Some(v);
                    tokens.next();
                }
            }
            "score" => {
                if let Some(&kind) = tokens.peek() {
                    tokens.next();
                    if kind == "cp" {
                        if let Some(v) = tokens.peek().and_then(|s| s.parse().ok()) {
                            info.score_cp = Some(v);
                            info.score_mate = None;
                            tokens.next();
                        }
                    } else if kind == "mate"
                        && let Some(v) = tokens.peek().and_then(|s| s.parse().ok())
                    {
                        info.score_mate = Some(v);
                        info.score_cp = None;
                        tokens.next();
                    }
                }
            }
            "pv" => {
                info.pv.clear();
                in_pv = true;
            }
            _ => {}
        }
    }
}
