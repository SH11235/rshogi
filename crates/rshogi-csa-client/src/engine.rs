//! USIエンジン管理（ponder対応）

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};

use crate::event::Event;
use crate::protocol::parse_game_result;

const READY_TIMEOUT: Duration = Duration::from_secs(120);

/// USIエンジンプロセス
pub struct UsiEngine {
    child: Child,
    writer: BufWriter<ChildStdin>,
    rx: Receiver<String>,
    pub engine_name: String,
    opt_names: HashSet<String>,
    quit_sent: bool,
}

/// bestmove の解析結果
#[derive(Clone, Debug)]
pub struct BestMoveResult {
    pub bestmove: String,
    pub ponder_move: Option<String>,
}

/// 探索の終了理由
pub enum SearchOutcome {
    /// エンジンが bestmove を返した
    BestMove(BestMoveResult, SearchInfo),
    /// サーバーから終局通知が来たため探索を中断した
    ServerInterrupt(Vec<String>),
}

/// USI `info` 行を観測する都度呼び出される callback。`(累積 SearchInfo, 生の info 行)`
/// を受け取り、`SessionEventSink` への `SearchInfo` snapshot 発火に使われる。
pub type InfoCallback<'a> = dyn FnMut(&SearchInfo, &str) + 'a;

/// info 行から抽出した探索情報
///
/// `depth` / `score_cp` / `score_mate` / `pv` は CSA Floodgate 拡張コメント生成に使われる。
/// `seldepth` / `nodes` / `time_ms` / `nps` は JSONL 出力モード（analyze_selfplay 互換）の
/// `move.eval` フィールドへの転写用。`info` 行から最後に観測した値を保持する。
#[derive(Clone, Debug, Default)]
pub struct SearchInfo {
    pub depth: Option<u32>,
    pub seldepth: Option<u32>,
    pub score_cp: Option<i32>,
    pub score_mate: Option<i32>,
    pub nodes: Option<u64>,
    pub time_ms: Option<u64>,
    pub nps: Option<u64>,
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
            opt_names: HashSet::new(),
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
        // usiok を待つ。option 行からオプション名を収集。
        loop {
            let line = self.recv(timeout)?;
            if let Some(rest) = line.strip_prefix("id name ") {
                self.engine_name = rest.to_string();
            } else if let Some(rest) = line.strip_prefix("option ") {
                if let Some(name) = parse_option_name(rest) {
                    self.opt_names.insert(name);
                }
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

        // Ponder 設定（エンジンが対応するオプション名を使う）
        if ponder {
            if self.opt_names.contains("USI_Ponder") {
                self.send("setoption name USI_Ponder value true")?;
            } else if self.opt_names.contains("Ponder") {
                self.send("setoption name Ponder value true")?;
            }
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
    /// サーバーから終局通知が来た場合は探索を中断して `ServerInterrupt` を返す。
    pub fn go(
        &mut self,
        position_cmd: &str,
        go_cmd: &str,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
    ) -> Result<SearchOutcome> {
        self.send(position_cmd)?;
        self.send(go_cmd)?;
        self.wait_bestmove(shutdown, server_rx, None)
    }

    /// `go` と同じだが、`info` 行を観測する都度 `info_callback` を呼んで累積
    /// `SearchInfo` と生 line を渡す。`SessionEventSink` への `SearchInfo`
    /// 発火 (累積 snapshot + throttle) の hook 用。
    pub fn go_with_info(
        &mut self,
        position_cmd: &str,
        go_cmd: &str,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
        info_callback: &mut InfoCallback<'_>,
    ) -> Result<SearchOutcome> {
        self.send(position_cmd)?;
        self.send(go_cmd)?;
        self.wait_bestmove(shutdown, server_rx, Some(info_callback))
    }

    /// ponder 探索を開始（bestmove を待たない）
    pub fn go_ponder(&mut self, position_cmd: &str, go_cmd: &str) -> Result<()> {
        self.send(position_cmd)?;
        self.send(go_cmd)?;
        Ok(())
    }

    /// ponderhit を送信し、bestmove を待つ。
    /// サーバーから終局通知が来た場合は探索を中断して `ServerInterrupt` を返す。
    pub fn ponderhit(
        &mut self,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
    ) -> Result<SearchOutcome> {
        self.send("ponderhit")?;
        self.wait_bestmove(shutdown, server_rx, None)
    }

    /// `ponderhit` と同じだが、`info` 行を観測する都度 `info_callback` を呼ぶ。
    pub fn ponderhit_with_info(
        &mut self,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
        info_callback: &mut InfoCallback<'_>,
    ) -> Result<SearchOutcome> {
        self.send("ponderhit")?;
        self.wait_bestmove(shutdown, server_rx, Some(info_callback))
    }

    /// stop を送信し、bestmove を待つ（ponder 中断用）。
    /// ponder 中でない場合は空振りで安全に終了する。
    pub fn stop_and_wait(&mut self) -> Result<()> {
        // stop 前にチャネルにある bestmove を消費（レース対策）
        while let Ok(line) = self.rx.try_recv() {
            if line.starts_with("bestmove") {
                return Ok(());
            }
        }
        self.send("stop")?;
        // bestmove を読み捨てる（5秒タイムアウト。ponder 中でなければ即返る）
        while let Ok(line) = self.rx.recv_timeout(Duration::from_secs(5)) {
            if line.starts_with("bestmove") {
                break;
            }
        }
        Ok(())
    }

    /// stop を送信（未送信なら）し、bestmove を読み捨てる。
    /// wait_bestmove 内のサーバー割り込み用。
    fn stop_and_drain_bestmove(&mut self, already_stopped: bool) {
        if !already_stopped {
            let _ = self.send("stop");
        }
        while let Ok(line) = self.rx.recv_timeout(Duration::from_secs(5)) {
            if line.starts_with("bestmove") {
                break;
            }
        }
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

    fn wait_bestmove(
        &mut self,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
        mut info_callback: Option<&mut InfoCallback<'_>>,
    ) -> Result<SearchOutcome> {
        use std::time::Instant;

        const OVERALL_TIMEOUT: Duration = Duration::from_secs(3600);
        const POST_STOP_TIMEOUT: Duration = Duration::from_secs(10);

        let mut info = SearchInfo::default();
        let mut stop_sent = false;
        let start = Instant::now();
        let mut stop_sent_at: Option<Instant> = None;
        // サーバーから受信した行をバッファ（終局検出時に呼び出し元へ返す）
        let mut server_lines: Vec<String> = Vec::new();

        loop {
            // 全体タイムアウト
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

            // サーバーイベントをチェック（ノンブロッキング）
            while let Ok(event) = server_rx.try_recv() {
                match event {
                    Event::ServerLine(ref line) => {
                        server_lines.push(line.clone());
                        if line.starts_with('#') && parse_game_result(line).is_some() {
                            log::info!("[USI] サーバー終局検出、探索中断: {line}");
                            self.stop_and_drain_bestmove(stop_sent);
                            return Ok(SearchOutcome::ServerInterrupt(server_lines));
                        }
                    }
                    Event::ServerDisconnected => {
                        log::warn!("[USI] サーバー切断検出、探索中断");
                        self.stop_and_drain_bestmove(stop_sent);
                        server_lines.push("#DISCONNECTED".to_string());
                        return Ok(SearchOutcome::ServerInterrupt(server_lines));
                    }
                }
            }

            // エンジンからの応答
            match self.rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    log::trace!("[USI] < {line}");
                    if line.starts_with("info") {
                        update_search_info(&mut info, &line);
                        if let Some(cb) = info_callback.as_deref_mut() {
                            cb(&info, &line);
                        }
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix("bestmove ") {
                        let mut parts = rest.split_whitespace();
                        let bestmove = parts.next().unwrap_or("resign").to_string();
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
                        return Ok(SearchOutcome::BestMove(
                            BestMoveResult {
                                bestmove,
                                ponder_move,
                            },
                            info,
                        ));
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
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

    pub(crate) fn send(&mut self, cmd: &str) -> Result<()> {
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

/// `option name <NAME> type ...` からオプション名を抽出
fn parse_option_name(rest: &str) -> Option<String> {
    // "name <NAME> type ..." の形式
    let rest = rest.strip_prefix("name ")?.trim_start();
    // "type" の手前までがオプション名
    if let Some(pos) = rest.find(" type ") {
        Some(rest[..pos].trim().to_string())
    } else {
        Some(rest.trim().to_string())
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
            "seldepth" => {
                if let Some(v) = tokens.peek().and_then(|s| s.parse().ok()) {
                    info.seldepth = Some(v);
                    tokens.next();
                }
            }
            "nodes" => {
                if let Some(v) = tokens.peek().and_then(|s| s.parse().ok()) {
                    info.nodes = Some(v);
                    tokens.next();
                }
            }
            "time" => {
                if let Some(v) = tokens.peek().and_then(|s| s.parse().ok()) {
                    info.time_ms = Some(v);
                    tokens.next();
                }
            }
            "nps" => {
                if let Some(v) = tokens.peek().and_then(|s| s.parse().ok()) {
                    info.nps = Some(v);
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
