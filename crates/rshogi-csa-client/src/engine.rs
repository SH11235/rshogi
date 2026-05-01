//! USIエンジン管理（ponder対応）

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::event::Event;
use crate::protocol::parse_game_result;

const READY_TIMEOUT: Duration = Duration::from_secs(120);

/// stderr ring buffer の最大行数 (engine 死亡時の診断 message 用)。
const STDERR_TAIL_MAX_LINES: usize = 64;

/// stderr 1 行あたりの byte 上限。超過分は破棄して次の `\n` で改行扱い。
const STDERR_LINE_MAX_BYTES: usize = 4096;

/// `engine_exited_error()` が stderr reader thread の EOF 観測完了を待つ最大時間。
const STDERR_DRAIN_WAIT: Duration = Duration::from_millis(200);
const STDERR_DRAIN_POLL: Duration = Duration::from_millis(50);

/// `quit()` (graceful path) で stderr reader thread の join を待つ最大時間。
const STDERR_JOIN_WAIT_GRACEFUL: Duration = Duration::from_millis(500);

/// `Drop` (fast path) で stderr reader thread の join を待つ最大時間。
const STDERR_JOIN_WAIT_DROP: Duration = Duration::from_millis(100);

/// USIエンジンプロセス
pub struct UsiEngine {
    child: Child,
    writer: BufWriter<ChildStdin>,
    rx: Receiver<String>,
    pub engine_name: String,
    opt_names: HashSet<String>,
    quit_sent: bool,
    /// engine binary の path。`engine_name` は handshake 後しか入らないため、
    /// 死亡時の error message には path も並記して debugging を支援する。
    /// spawn は対局開始時 1 回しか呼ばれず、`PathBuf` の heap allocation は
    /// ホットパスにないので許容範囲。
    engine_path: PathBuf,
    /// engine の stderr 末尾 64 行 ring buffer。各行 4096 bytes cap。
    /// engine 死亡時の error message に末尾を付けて原因 debugging を支援する。
    /// reader thread は best-effort: poison/UTF-8/IO error は loop 終了で吸収。
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// stderr reader thread が EOF/IO error 観測後 true をセット。
    /// `engine_exited_error()` は snapshot 前に短時間 (200ms) この flag を
    /// best-effort で待つ。実際の tail 可視性は snapshot 時の `Mutex::lock()`
    /// が Acquire 同期点として機能することで保証される (Acquire load は
    /// 早期 break 用 signal で、可視性 chain は Mutex 経由)。
    stderr_done: Arc<AtomicBool>,
    /// stderr reader thread の handle。`quit()` (graceful) と `Drop` (fast)
    /// で bounded join + detach fallback。`engine_exited_error()` は handle を
    /// take しない (副作用なし、cleanup ownership は quit/Drop に集約)。
    stderr_reader_handle: Option<thread::JoinHandle<()>>,
}

/// bestmove の解析結果
#[derive(Clone, Debug)]
pub struct BestMoveResult {
    pub bestmove: String,
    pub ponder_move: Option<String>,
}

/// 探索の終了理由
pub enum SearchOutcome {
    /// エンジンが bestmove を返した。第 2 引数は終了時点の累積 [`SearchInfo`]
    /// (final score, final pv, final depth 等)。
    BestMove(BestMoveResult, SearchInfo),

    /// 探索中に server から終局通知 (or disconnect) を受信して中断した。
    /// 内包する `Vec<String>` は中断契機となった server からの行を含む:
    ///
    /// - `#WIN` / `#LOSE` / `#DRAW` / `#CENSORED` / `#CHUDAN` / `#TIME_UP` などの結果行
    /// - `#ILLEGAL_MOVE` / `#JISHOGI` / `#SENNICHITE` / `#MAX_MOVES` などの理由行
    /// - server 切断時は `#DISCONNECTED` という synthetic line
    ///
    /// session 側は本 Vec の内容から `GameEndEvent.reason` ([`crate::events::GameEndReason`])
    /// を解釈する。実装は中断時の server 受信行を漏れなくこの Vec に含めること。
    ServerInterrupt(Vec<String>),
}

/// USI `info` 行を観測する都度呼び出される callback。
///
/// 引数:
/// - `&SearchInfo`: 観測時点の累積 search info (depth/score/pv 等は最後に観測した値で更新済)
/// - `&str`: USI engine から受信した raw info 行 (例: `"info depth 12 score cp 87 pv 7g7f 8c8d ..."`)
///
/// 呼び出される順序:
/// - bestmove を受信する**前**に、各 info 行ごとに 1 度だけ呼ばれる
/// - 探索中断 (stop / server interrupt) の場合、それまでに受信した info 行ぶんは呼ばれている
///
/// session 側は本 callback を `SearchInfoEmitPolicy` で throttle した上で
/// `SessionEventSink::on_event(SessionProgress::SearchInfo(...))` に変換する。
pub type InfoCallback<'a> = dyn FnMut(&SearchInfo, &str) + 'a;

/// CSA 対局ループから呼び出される USI engine の最小 contract。
///
/// 既存 [`UsiEngine`] (外部 USI プロセス driver) が reference impl。consumer は本 trait を
/// 実装することで in-process engine / mock engine 等を
/// [`run_game_session_with_events`](crate::run_game_session_with_events) に渡せる。
///
/// 同期前提。`Send + Sync` bound は付けない (consumer が必要なら自身の型で課す)。
/// `Result` は [`anyhow::Result`] を採用する (現行 `UsiEngine` API と整合)。
///
/// 利用例:
///
/// ```ignore
/// use rshogi_csa_client::{run_game_session_with_events, UsiEngine, UsiEngineDriver};
///
/// // 1. 具象 `UsiEngine` をそのまま渡す
/// let mut engine = UsiEngine::spawn(&path, &options, ponder, timeout)?;
/// run_game_session_with_events(&config, &mut conn, &mut engine, shutdown, &mut sink)?;
///
/// // 2. dyn dispatch で複数 engine 実装を切り替える
/// let mut engine: Box<dyn UsiEngineDriver> = if use_builtin {
///     Box::new(BuiltinEngine::new(...))
/// } else {
///     Box::new(UsiEngine::spawn(&path, &options, ponder, timeout)?)
/// };
/// run_game_session_with_events(&config, &mut conn, &mut *engine, shutdown, &mut sink)?;
/// ```
pub trait UsiEngineDriver {
    /// USI `usinewgame` 相当を実装する。対局開始前に 1 度呼ばれる。
    fn new_game(&mut self) -> Result<()>;

    /// `position` + `go` を送信し、bestmove または server interrupt まで block する。
    ///
    /// 実装の責任:
    /// 1. `position_cmd` (例: `"position sfen ... moves ..."`) を engine に送信
    /// 2. `go_cmd` (例: `"go btime 60000 wtime 60000 byoyomi 5000"`) を engine に送信
    /// 3. info 行を受信するたびに `info_callback` を呼び、累積 [`SearchInfo`] と raw info 行を渡す
    /// 4. 探索中、`shutdown.load(Ordering::SeqCst)` が true になった場合は `stop` を engine
    ///    に送り、bestmove を待ってから `Ok(SearchOutcome::BestMove(...))` で返す (resign 扱いも可)
    /// 5. 探索中、`server_rx` から [`Event::ServerLine`] を受信し、`#GAME_OVER` などの終局
    ///    行を検出した場合は engine に `stop` を送り、bestmove を drain してから
    ///    `Ok(SearchOutcome::ServerInterrupt(server_lines))` を返す
    /// 6. [`Event::ServerDisconnected`] を受信した場合も同様に終局として扱う
    ///
    /// 実装は探索中、`shutdown` を `try_recv` / timeout poll 等で blocking しない形で観測
    /// すること (例: 200ms 以下の poll interval、または engine 応答ストリームと multiplex)。
    /// blocking read のみで `shutdown` / `server_rx` を放置すると session 全体が固まる。
    ///
    /// `server_rx` から [`Event::ServerLine`] / [`Event::ServerDisconnected`] を受信したら
    /// 探索を中断 (`stop` 送信 + bestmove drain) し、
    /// [`SearchOutcome::ServerInterrupt`] で返す。`server_lines` には中断契機の行を
    /// 漏れなく含めること。
    fn go_with_info(
        &mut self,
        position_cmd: &str,
        go_cmd: &str,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
        info_callback: &mut InfoCallback<'_>,
    ) -> Result<SearchOutcome>;

    /// `position` + `go ponder` を送信し、bestmove を**待たない**で即座に return する。
    /// 後続で [`UsiEngineDriver::ponderhit_with_info`] または
    /// [`UsiEngineDriver::stop_and_wait`] が呼ばれる。
    fn go_ponder(&mut self, position_cmd: &str, go_cmd: &str) -> Result<()>;

    /// USI `ponderhit` を送信し、bestmove または server interrupt まで block する。
    /// `shutdown` / `server_rx` / `info_callback` の semantics は
    /// [`UsiEngineDriver::go_with_info`] と同じ。
    fn ponderhit_with_info(
        &mut self,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
        info_callback: &mut InfoCallback<'_>,
    ) -> Result<SearchOutcome>;

    /// USI `stop` を送信し、bestmove の drain (= 棋譜には載せない) を行う。
    ///
    /// session は ponder 中の cleanup 用にこの method を呼ぶ。実装は ponder 中で
    /// なくとも安全に呼び出されることを保証すること (副作用は USI `stop` 送信のみ
    /// に留める)。
    fn stop_and_wait(&mut self) -> Result<()>;

    /// USI `gameover <result>` を送信する。
    ///
    /// `result` は `"win"` / `"lose"` / `"draw"` のいずれか (USI 仕様準拠)。
    /// session 側は [`crate::protocol::GameResult::Interrupted`] /
    /// [`crate::protocol::GameResult::Censored`] を `"draw"` に丸める (既存
    /// `gameover_str` 関数の挙動)。
    fn gameover(&mut self, result: &str) -> Result<()>;
}

impl UsiEngineDriver for UsiEngine {
    fn new_game(&mut self) -> Result<()> {
        UsiEngine::new_game(self)
    }

    fn go_with_info(
        &mut self,
        position_cmd: &str,
        go_cmd: &str,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
        info_callback: &mut InfoCallback<'_>,
    ) -> Result<SearchOutcome> {
        UsiEngine::go_with_info(self, position_cmd, go_cmd, shutdown, server_rx, info_callback)
    }

    fn go_ponder(&mut self, position_cmd: &str, go_cmd: &str) -> Result<()> {
        UsiEngine::go_ponder(self, position_cmd, go_cmd)
    }

    fn ponderhit_with_info(
        &mut self,
        shutdown: &AtomicBool,
        server_rx: &Receiver<Event>,
        info_callback: &mut InfoCallback<'_>,
    ) -> Result<SearchOutcome> {
        UsiEngine::ponderhit_with_info(self, shutdown, server_rx, info_callback)
    }

    fn stop_and_wait(&mut self) -> Result<()> {
        UsiEngine::stop_and_wait(self)
    }

    fn gameover(&mut self, result: &str) -> Result<()> {
        UsiEngine::gameover(self, result)
    }
}

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
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| anyhow!("エンジン起動失敗 {}: {e}", path.display()))?;

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

        // stderr ring buffer。EOF/IO error までは best-effort で末尾 64 行を
        // 保持し、engine 死亡時に診断 error message へ転写する。
        let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_TAIL_MAX_LINES)));
        let stderr_done = Arc::new(AtomicBool::new(false));
        let stderr_tail_writer = Arc::clone(&stderr_tail);
        let stderr_done_writer = Arc::clone(&stderr_done);
        let stderr_reader_handle = std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buf: Vec<u8> = Vec::with_capacity(STDERR_LINE_MAX_BYTES);
            loop {
                buf.clear();
                match read_line_capped(&mut reader, &mut buf, STDERR_LINE_MAX_BYTES) {
                    Ok(ReadLineOutcome::Eof) => break,
                    Ok(ReadLineOutcome::Line) => {
                        // 空行 (buf.len() == 0 の `Line`) もここに含まれる。EOF と空行を取り違えて
                        // reader thread を早期終了させると以降の stderr が失われ、
                        // pipe が詰まって engine 側が write で block するリスクがある
                        // ため、空行は通常行と同じく ring buffer に push する
                        // (PR #596 review で指摘された bug への対応)。
                        let raw = String::from_utf8_lossy(&buf).into_owned();
                        let line = raw.trim_end_matches('\r').to_owned();
                        let mut tail = match stderr_tail_writer.lock() {
                            Ok(g) => g,
                            Err(p) => p.into_inner(),
                        };
                        if tail.len() >= STDERR_TAIL_MAX_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                    Err(_) => break,
                }
            }
            stderr_done_writer.store(true, Ordering::Release);
        });

        let mut engine = Self {
            child,
            writer: BufWriter::new(stdin),
            rx,
            engine_name: String::new(),
            opt_names: HashSet::new(),
            quit_sent: false,
            engine_path: path.to_path_buf(),
            stderr_tail,
            stderr_done,
            stderr_reader_handle: Some(stderr_reader_handle),
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
        if self.quit_sent {
            return;
        }
        let send_result = self.send("quit");
        if send_result.is_ok() {
            // 3 秒待ってまだ終了しなければ kill
            for _ in 0..30 {
                if let Ok(Some(_)) = self.child.try_wait() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            log::warn!("[USI] quit タイムアウト、kill します");
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // stderr handle bounded join (graceful path のみ実施)
        if let Some(handle) = self.stderr_reader_handle.take()
            && let Err(unfinished) = join_handle_bounded(handle, STDERR_JOIN_WAIT_GRACEFUL)
        {
            drop(unfinished);
        }
        self.quit_sent = true;
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
                    return Err(self.engine_exited_error());
                }
            }
        }
    }

    pub(crate) fn send(&mut self, cmd: &str) -> Result<()> {
        log::debug!("[USI] > {cmd}");
        let result = self
            .writer
            .write_all(cmd.as_bytes())
            .and_then(|_| self.writer.write_all(b"\n"))
            .and_then(|_| self.writer.flush());
        match result {
            Ok(()) => Ok(()),
            Err(io_err) if io_err.kind() == std::io::ErrorKind::BrokenPipe => {
                // BrokenPipe = engine 死亡確定の強い signal (Linux primary scope)。
                // Windows の `ConnectionAborted` / `ConnectionReset` は followup-J で対応。
                Err(self.engine_exited_error())
            }
            Err(io_err) => {
                // BrokenPipe 以外は engine 生存中の transient error。
                // `anyhow::Context::context` で source を保持しつつ伝搬。
                Err(io_err).context("エンジン I/O エラー")
            }
        }
    }

    fn recv(&mut self, timeout: Duration) -> Result<String> {
        match self.rx.recv_timeout(timeout) {
            Ok(line) => {
                log::trace!("[USI] < {line}");
                Ok(line)
            }
            Err(RecvTimeoutError::Timeout) => {
                bail!("エンジン応答タイムアウト");
            }
            Err(RecvTimeoutError::Disconnected) => Err(self.engine_exited_error()),
        }
    }

    /// engine 死亡確定経路 (BrokenPipe / Disconnected) から呼ばれる、
    /// stderr 末尾と exit status を含む診断 error の組み立て。
    ///
    /// 副作用なし: `child` の reaping は `try_wait` の std cache 経由のみ、
    /// `stderr_reader_handle` の take や `quit_sent` のセットは行わない。
    /// 同一 session で複数回呼ばれても idempotent。cleanup ownership は
    /// `quit()` (graceful) と `Drop` (fast) に集約する。
    fn engine_exited_error(&mut self) -> anyhow::Error {
        // (1) stderr reader の EOF 観測完了を best-effort で 200ms 待つ。
        //     実際の tail 可視性は snapshot 時の `Mutex::lock()` で確保される
        //     (Acquire 同期点)。本 wait は reader が最後の行を push し終えるのを
        //     待つ目的のみ。fatal communication error 経路 (recv Disconnected /
        //     send BrokenPipe / wait_bestmove Disconnected) から呼ばれる契約
        //     なので、try_wait の結果に関係なく常に wait する。
        let poll_iters = (STDERR_DRAIN_WAIT.as_millis() / STDERR_DRAIN_POLL.as_millis()).max(1);
        for _ in 0..poll_iters {
            if self.stderr_done.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(STDERR_DRAIN_POLL);
        }
        // (2) try_wait で exit_status 取得 (副作用なし、reaping は std が cache する)
        let exit_status = match self.child.try_wait() {
            Ok(Some(status)) => Some(format!("{status}")),
            _ => None,
        };
        // (3) tail snapshot (poison-resistant)
        let tail: Vec<String> = self
            .stderr_tail
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .cloned()
            .collect();
        // (4) error message 構築
        let path = self.engine_path.display();
        let prefix = match exit_status {
            Some(s) => format!("エンジンプロセスが終了しました (path={path}, exit={s})"),
            None => format!("エンジンプロセスが終了しました (path={path}, status=unknown)"),
        };
        if tail.is_empty() {
            anyhow!("{prefix}; エンジン stderr は空")
        } else {
            anyhow!("{prefix}; エンジン stderr 末尾 {} 行:\n{}", tail.len(), tail.join("\n"))
        }
    }

    /// バッファに溜まっている行を非ブロッキングで全て読み捨てる
    pub fn drain(&self) {
        while self.rx.try_recv().is_ok() {}
    }
}

impl Drop for UsiEngine {
    fn drop(&mut self) {
        // 既存 fast path を維持: send("quit") + sleep 100ms + kill + wait。
        // engine 死亡確定 (BrokenPipe) の場合 send 経由で `engine_exited_error()`
        // が走り 200ms wait を払うが、Drop 全体の遅延は最大 ~400ms に bounded。
        // panic-on-drop / test runner 経路で 3.5 秒+ の遅延を回避する設計。
        if !self.quit_sent {
            let _ = self.send("quit");
            std::thread::sleep(Duration::from_millis(100));
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // stderr handle が残っていれば best-effort で cleanup (bounded join 100ms、
        // 超過なら detach)
        if let Some(handle) = self.stderr_reader_handle.take()
            && let Err(unfinished) = join_handle_bounded(handle, STDERR_JOIN_WAIT_DROP)
        {
            drop(unfinished);
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

/// `read_line_capped` の戻り値。EOF と「空行の delimiter 読み取り」を
/// 区別するために使う。
///
/// ## 背景
///
/// 戻り値を単に byte 数 (`usize`) にしてしまうと、空行 `"\n"` を読んだとき
/// (`buf.len() == 0`) と EOF (`buf.len() == 0`) が区別できず、呼び出し側で
/// 空行を EOF として誤認して reader thread を早期終了させる bug を生む
/// (PR #596 codex review で指摘)。`buf` への push は呼び出し側で観測できる
/// ため、`Line` バリアントは byte 数を持たず純粋に「区切りを 1 行読んだ」
/// signal として機能させる。
enum ReadLineOutcome {
    /// 1 行読み取り完了 (空行を含む)。実 byte 数は呼び出し側が `buf.len()`
    /// で観測する。
    Line,
    /// EOF (これ以上 reader からは読めない)。reader thread を終了させる
    /// signal として使う。
    Eof,
}

/// stderr stream から `\n` 区切りで 1 行読み込む。
///
/// `max_bytes` を超えた分は読み飛ばし (discard) し、次の `\n` で 1 行として
/// return する (4 KB 超の長い 1 行は truncate されて残りは破棄、複数行に分割
/// しない)。LF 区切りのみ。CR 単独 (古い Mac 形式) は同一行扱い。CRLF の `\r`
/// は戻り値の buf に残るため、ring buffer 投入直前に `trim_end_matches('\r')`
/// で除去すること。
///
/// 戻り値は [`ReadLineOutcome`] で EOF と空行を区別する。EOF の場合は
/// `ReadLineOutcome::Eof` を返し、呼び出し側はこれを受けて reader loop を
/// 終了する。1 行読み取り (空行を含む) の場合は `ReadLineOutcome::Line` を
/// 返し、実 byte 数は呼び出し側が `buf.len()` で観測する (delimiter `\n` を
/// 含まず、`max_bytes` 超過分も含まない)。
fn read_line_capped<R: BufRead>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    max_bytes: usize,
) -> std::io::Result<ReadLineOutcome> {
    let mut byte = [0u8; 1];
    let mut got_byte = false;
    loop {
        match reader.read(&mut byte) {
            Ok(0) => {
                // EOF: 何も読まずに EOF なら Eof、何か読んだ後 EOF なら最終行扱い
                if got_byte {
                    return Ok(ReadLineOutcome::Line);
                }
                return Ok(ReadLineOutcome::Eof);
            }
            Ok(_) => {
                got_byte = true;
                if byte[0] == b'\n' {
                    return Ok(ReadLineOutcome::Line);
                }
                if buf.len() < max_bytes {
                    buf.push(byte[0]);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
}

/// `JoinHandle` を最大 `deadline` まで待機して join を試みる。timeout 時は
/// `Err(handle)` を返し caller が `drop()` で detach する。
fn join_handle_bounded(
    handle: thread::JoinHandle<()>,
    deadline: Duration,
) -> std::result::Result<(), thread::JoinHandle<()>> {
    let start = std::time::Instant::now();
    while start.elapsed() < deadline {
        if handle.is_finished() {
            let _ = handle.join();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(handle)
}
