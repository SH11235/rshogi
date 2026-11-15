use std::cell::Cell;
use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};

use super::types::{
    InfoCallback, InfoSnapshot, SearchOutcome, SearchRequest, TimeArgs, duration_to_millis,
};

/// 先後の時計から USI の時間制御コマンドを構築する。
fn build_time_command(time_args: &TimeArgs) -> String {
    // byoyomi のみの場合は "go byoyomi N" だけを送る（btime 0 等を付けると
    // 一部エンジンが btime=0 を「持ち時間なし」と解釈して即指しする）
    if time_args.byoyomi > 0
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
    } else if time_args.btime > 0 || time_args.wtime > 0 || time_args.binc > 0 || time_args.winc > 0
    {
        // フィッシャー（先後の持ち時間・加算時間、byoyomi なし）
        format!(
            "go btime {} wtime {} binc {} winc {}",
            time_args.btime, time_args.wtime, time_args.binc, time_args.winc
        )
    } else {
        String::from("go")
    }
}

/// queue に行が残っていても、期限到達後は読み続けない。
pub(crate) fn receive_before_deadline<T>(
    receiver: &Receiver<T>,
    deadline: Instant,
) -> std::result::Result<T, RecvTimeoutError> {
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

// 巨大な1行も保持・一時バッファともに制限し、超過分は改行まで読み捨てる。
fn read_stderr_line(reader: &mut impl BufRead) -> std::io::Result<Option<String>> {
    const MAX_BYTES: usize = 4096;
    let mut line = Vec::new();
    let mut seen = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(seen.then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        seen = true;
        let end = available.iter().position(|byte| *byte == b'\n');
        let count = end.map_or(available.len(), |index| index + 1);
        let text_len = end.unwrap_or(available.len());
        let keep = text_len.min(MAX_BYTES - line.len());
        line.extend_from_slice(&available[..keep]);
        reader.consume(count);
        if end.is_some() {
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
    }
}

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

fn discard_ponder_eval(snapshot: &mut InfoSnapshot) {
    *snapshot = InfoSnapshot {
        nodes: snapshot.nodes,
        time_ms: snapshot.time_ms,
        nps: snapshot.nps,
        ..InfoSnapshot::default()
    };
}

fn parse_scored_primary_pv(line: &str) -> Option<Option<String>> {
    let mut info = InfoSnapshot::default();
    info.update_from_line(line);
    (info.score_cp.is_some() || info.score_mate.is_some() || info.score_mate_sign.is_some())
        .then(|| info.pv.into_iter().next())
}

fn retain_matching_ponder_eval(
    snapshot: &mut InfoSnapshot,
    scored_pv: Option<&str>,
    bestmove: &str,
) {
    if matches!(bestmove, "resign" | "win")
        || scored_pv != Some(bestmove)
        || snapshot.pv.first().map(String::as_str) != Some(bestmove)
    {
        discard_ponder_eval(snapshot);
    } else if snapshot
        .last_exact_primary
        .as_ref()
        .is_some_and(|primary| primary.pv.first().map(String::as_str) != Some(bestmove))
    {
        snapshot.last_exact_primary = None;
    }
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
    pub(crate) ponder_enabled: bool,
    pondering: Option<String>,
    predicted_move: Option<String>,
    search_pending: bool,
    reusable: bool,
    stop_requested: bool,
    pub(crate) ponder_stats: PonderStats,
    pub(crate) turn_start: Option<Instant>,
}

/// ponder の送信回数と的中回数。
#[derive(Clone, Copy, Default)]
pub struct PonderStats {
    pub sent: u64,
    pub hits: u64,
}

impl serde::Serialize for PonderStats {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("PonderStats", 3)?;
        state.serialize_field("sent", &self.sent)?;
        state.serialize_field("hits", &self.hits)?;
        let rate = (self.sent > 0).then(|| self.hits as f64 / self.sent as f64);
        state.serialize_field("hit_rate", &rate)?;
        state.end()
    }
}

impl EngineProcess {
    fn search_commands(req: &SearchRequest<'_>) -> (String, String) {
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
        let mut go = build_time_command(&req.time_args);
        if let Some(depth) = req.go_depth {
            go.push_str(&format!(" depth {depth}"));
        }
        if let Some(nodes) = req.go_nodes {
            go.push_str(&format!(" nodes {nodes}"));
        }
        (position_cmd, go)
    }

    /// 合法な予想手がある場合だけ、相手手番の探索を開始する。
    pub(crate) fn start_ponder(
        &mut self,
        req: &SearchRequest<'_>,
        position: &rshogi_core::position::Position,
    ) -> Result<()> {
        if !self.ponder_enabled {
            return Ok(());
        }
        let Some(prediction) = self.predicted_move.take() else {
            return Ok(());
        };
        let Some(mv) = rshogi_core::types::Move::from_usi(&prediction) else {
            return Ok(());
        };
        if !super::position::is_legal_game_move(position, mv) {
            return Ok(());
        }
        let expected = mv.to_usi();
        let (mut position_cmd, go) = Self::search_commands(req);
        position_cmd.push_str(if req.moves.is_empty() { " moves " } else { " " });
        position_cmd.push_str(&expected);
        self.write_line(&position_cmd)?;
        self.pondering = Some(expected);
        self.search_pending = true;
        self.stop_requested = false;
        self.write_line(&go.replacen("go", "go ponder", 1))?;
        self.ponder_stats.sent += 1;
        Ok(())
    }

    /// tournament の相手手番思考を有効にする。USI の設定値とは独立に制御する。
    pub fn enable_tournament_ponder(&mut self, enabled: bool) -> Result<()> {
        self.ponder_enabled = enabled;
        if enabled && self.opt_names.is_empty() {
            self.write_line("setoption name USI_Ponder value true")?;
            self.sync_ready()?;
        }
        Ok(())
    }

    /// 未回収の探索がなく、次の対局に利用できるか。
    pub fn is_reusable(&self) -> bool {
        self.reusable
    }

    /// ponder の bestmove を回収する。回収不能ならプロセスを再利用しない。
    pub(crate) fn stop_ponder(&mut self) -> Result<()> {
        let result = self.drain_ponder();
        if result.is_err() {
            self.reusable = false;
        }
        result
    }

    fn drain_ponder(&mut self) -> Result<()> {
        if self.pondering.is_none() && !self.search_pending {
            return Ok(());
        }
        if !self.stop_requested {
            self.write_line("stop")?;
            self.stop_requested = true;
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if self.recv_line_until(deadline)?.starts_with("bestmove ") {
                break;
            }
        }
        self.pondering = None;
        self.search_pending = false;
        self.stop_requested = false;
        Ok(())
    }

    /// プロセスを起動し、USI 初期化を完了する。
    pub fn spawn(cfg: &EngineConfig, label: String) -> Result<Self> {
        let mut proc = Self::spawn_uninitialized(cfg, label)?;
        proc.initialize(cfg)?;
        Ok(proc)
    }

    /// usi/isready を含む初期化全体に期限を設けて起動する。
    pub fn spawn_with_timeout(
        cfg: &EngineConfig,
        label: String,
        timeout: Duration,
    ) -> Result<Self> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow!("invalid startup timeout"))?;
        let mut proc = Self::spawn_uninitialized(cfg, label)?;
        proc.initialize_until(cfg, Some(deadline))?;
        Ok(proc)
    }

    /// プロセスと reader thread のみを起動する。利用前に `initialize` を呼ぶこと。
    pub fn spawn_uninitialized(cfg: &EngineConfig, label: String) -> Result<Self> {
        Self::spawn_uninitialized_with_cpu_affinity(cfg, label, None)
    }

    /// 指定した論理 CPU 集合へ固定してプロセスと reader thread のみを起動する。
    ///
    /// Linux の子プロセスにのみ適用される。エンジンが後から生成する探索スレッドも
    /// affinity mask を継承するため、SPSA の各 worker を一つの NUMA node 内へ
    /// 閉じ込められる。利用前に `initialize` を呼ぶこと。
    pub fn spawn_uninitialized_with_cpu_affinity(
        cfg: &EngineConfig,
        label: String,
        cpu_affinity: Option<&[usize]>,
    ) -> Result<Self> {
        if cpu_affinity.is_some_and(<[usize]>::is_empty) {
            bail!("CPU affinity must contain at least one CPU");
        }
        #[cfg(not(target_os = "linux"))]
        if cpu_affinity.is_some() {
            bail!("CPU affinity is supported only on Linux");
        }

        let mut cmd = Command::new(&cfg.path);
        if !cfg.args.is_empty() {
            cmd.args(&cfg.args);
        }
        // 子プロセスを独立したプロセスグループで起動し、
        // 親プロセスへの SIGINT が子に伝播しないようにする。
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            #[cfg(target_os = "linux")]
            let cpu_affinity = cpu_affinity.map(<[usize]>::to_vec);
            #[cfg(target_os = "linux")]
            if let Some(cpus) = &cpu_affinity
                && let Some(&cpu) = cpus.iter().find(|&&cpu| cpu >= libc::CPU_SETSIZE as usize)
            {
                bail!("CPU {cpu} exceeds CPU_SETSIZE ({})", libc::CPU_SETSIZE);
            }
            // SAFETY: setpgid と sched_setaffinity は async-signal-safe な syscall wrapper。
            // cpu_set_t は fork 前に検証済みの CPU 番号だけで初期化し、closure 内では
            // allocation や共有状態へのアクセスを行わない。
            unsafe {
                cmd.pre_exec(move || {
                    if libc::setpgid(0, 0) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    #[cfg(target_os = "linux")]
                    if let Some(cpus) = &cpu_affinity {
                        let mut set: libc::cpu_set_t = std::mem::zeroed();
                        libc::CPU_ZERO(&mut set);
                        for &cpu in cpus {
                            libc::CPU_SET(cpu, &mut set);
                        }
                        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set)
                            != 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
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
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        });
        let recent_stderr = Arc::new(Mutex::new(VecDeque::with_capacity(STDERR_BUFFER_LINES)));
        let stderr_buffer = recent_stderr.clone();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            while let Ok(Some(line)) = read_stderr_line(&mut reader) {
                if let Ok(mut buffer) = stderr_buffer.lock() {
                    if buffer.len() == STDERR_BUFFER_LINES {
                        buffer.pop_front();
                    }
                    buffer.push_back(line);
                } else {
                    break;
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
            ponder_enabled: false,
            pondering: None,
            predicted_move: None,
            search_pending: false,
            reusable: true,
            stop_requested: false,
            ponder_stats: PonderStats::default(),
            turn_start: None,
        })
    }

    /// 起動済みプロセスの USI handshake と初期オプション設定を行う。
    pub fn initialize(&mut self, cfg: &EngineConfig) -> Result<()> {
        self.initialize_until(cfg, None)
    }

    fn initialize_until(
        &mut self,
        cfg: &EngineConfig,
        total_deadline: Option<Instant>,
    ) -> Result<()> {
        let deadline = total_deadline.unwrap_or_else(|| Instant::now() + ENGINE_READY_TIMEOUT);
        self.write_line("usi")?;
        loop {
            let line = self.recv_line_until(deadline)?;
            if total_deadline.is_some() && line.starts_with("info string Error") {
                bail!("{}: engine initialization: {line}", self.label);
            }
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
        self.write_line("isready")?;
        let deadline = total_deadline.unwrap_or_else(|| Instant::now() + ENGINE_READY_TIMEOUT);
        loop {
            let line = self.recv_line_until(deadline)?;
            if total_deadline.is_some() && line.starts_with("info string Error") {
                bail!("{}: engine initialization: {line}", self.label);
            }
            if line == "readyok" {
                break;
            }
        }
        self.write_line("usinewgame")?;
        Ok(())
    }

    pub fn new_game(&mut self) -> Result<()> {
        if !self.reusable {
            bail!("{}: engine has an undrained search", self.label);
        }
        self.stop_ponder()?;
        self.turn_start = None;
        self.predicted_move = None;
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
        let turn_start = self.turn_start.take();
        self.predicted_move = None;
        let hit = self
            .pondering
            .as_deref()
            .is_some_and(|mv| req.moves.split_whitespace().last() == Some(mv));
        if hit {
            self.write_line("ponderhit")?;
            self.pondering = None;
            self.ponder_stats.hits += 1;
        } else {
            self.stop_ponder()?;
            let (position, go) = Self::search_commands(req);
            self.write_line(&position)?;
            self.search_pending = self.ponder_enabled;
            self.stop_requested = false;
            self.write_line(&go)?;
        }
        let start = if self.ponder_enabled {
            turn_start.unwrap_or_else(Instant::now)
        } else {
            Instant::now()
        };
        let has_limit = req.go_depth.is_some() || req.go_nodes.is_some();
        let time_args = &req.time_args;
        let has_time = time_args.byoyomi > 0
            || time_args.btime > 0
            || time_args.wtime > 0
            || time_args.binc > 0
            || time_args.winc > 0;
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
        let mut scored_primary_pv = None;

        loop {
            let elapsed = start.elapsed();
            let deadline = if stop_sent { hard_limit } else { soft_limit };
            if elapsed >= deadline {
                if !stop_sent {
                    self.write_line("stop")?;
                    self.stop_requested = self.ponder_enabled;
                    stop_sent = true;
                    continue;
                }
                if hit {
                    discard_ponder_eval(&mut snapshot);
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
                        if hit && let Some(pv) = parse_scored_primary_pv(&line) {
                            scored_primary_pv = pv;
                        }
                        snapshot.update_from_line(&line);
                        if !hit && let Some(ref mut cb) = info_callback {
                            cb(&line, req);
                        }
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix("bestmove ") {
                        let mut parts = rest.split_whitespace();
                        let mv = parts.next().unwrap_or_default().to_string();
                        self.search_pending = false;
                        self.predicted_move =
                            if self.ponder_enabled && parts.next() == Some("ponder") {
                                parts.next().map(str::to_owned)
                            } else {
                                None
                            };
                        if hit {
                            retain_matching_ponder_eval(
                                &mut snapshot,
                                scored_primary_pv.as_deref(),
                                &mv,
                            );
                        }
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
                        self.stop_requested = self.ponder_enabled;
                        stop_sent = true;
                    } else {
                        let elapsed_ms = duration_to_millis(start.elapsed());
                        if hit {
                            discard_ponder_eval(&mut snapshot);
                        }
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

    /// 待ち時間内の行を返す。無通信は None、切断はエラーに分ける。
    pub fn poll_line(&self, timeout: Duration) -> Result<Option<String>> {
        match self.rx.recv_timeout(timeout) {
            Ok(line) => Ok(Some(line)),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => bail!("{}", self.engine_exited_message()),
        }
    }

    /// 継続的な info 出力でも延長されない絶対期限で1行受信する。
    pub fn recv_line_until(&self, deadline: Instant) -> Result<String> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| anyhow!("{}", self.engine_read_timeout_message(Duration::ZERO)))?;
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

    /// `set_option_if_available` が実際に送信するかどうか。
    ///
    /// `usi` に対して 1 つもオプションを広告しないエンジンでは広告名の集合が空になるため、
    /// 判別できず全て送信対象として扱う。呼び出し側が「初期化で送られなかった必須オプション」
    /// を補う際は、送信済みの再送を避けるためにこの述語で判定する。
    pub fn is_option_available(&self, name: &str) -> bool {
        self.opt_names.is_empty() || self.opt_names.contains(name)
    }

    /// `usi` への応答で `name` のオプションが広告されたか。
    ///
    /// `is_option_available` と違い、オプションを 1 つも広告しないエンジンでは `false` を返す。
    pub fn advertises_option(&self, name: &str) -> bool {
        self.opt_names.contains(name)
    }

    pub fn set_option_if_available(&mut self, name: &str, value: &str) -> Result<()> {
        if self.is_option_available(name) {
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

#[cfg(test)]
mod stderr_tests {
    use super::*;

    #[test]
    fn stderr_large_line_is_capped_and_next_line_preserved() {
        let mut data = vec![b'x'; 100_000];
        data.extend_from_slice(b"\nnext\n");
        let mut reader = BufReader::with_capacity(17, data.as_slice());
        assert_eq!(read_stderr_line(&mut reader).unwrap().unwrap().len(), 4096);
        assert_eq!(read_stderr_line(&mut reader).unwrap().unwrap(), "next");
        assert_eq!(read_stderr_line(&mut reader).unwrap(), None);
    }
}

#[cfg(all(test, windows))]
mod primary_process_tests {
    use super::*;
    use crate::selfplay::types::PrimaryScore;

    #[test]
    fn primary_record_survives_stop_and_eof_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("primary.ps1");
        std::fs::write(
            &script,
            r#"
param([string]$mode)
while ($null -ne ($line = [Console]::ReadLine())) {
    switch -Wildcard ($line) {
        'usi' { [Console]::WriteLine('usiok') }
        'isready' { [Console]::WriteLine('readyok') }
        'go*' {
            if ($mode -ne 'empty') {
                [Console]::WriteLine('info depth 8 score cp 42 pv 7g7f 3c3d')
                [Console]::WriteLine('info depth 9 nodes 123')
            }
            if ($mode -eq 'normal') { [Console]::WriteLine('bestmove 7g7f') }
            if ($mode -eq 'exit') { exit 0 }
        }
        'stop' {
            [Console]::WriteLine('info depth 10 nodes 999')
            [Console]::WriteLine('bestmove 7g7f')
        }
        'quit' { exit 0 }
    }
}
"#,
        )
        .unwrap();
        for mode in ["normal", "stop", "empty", "exit"] {
            let cfg = EngineConfig {
                path: PathBuf::from("powershell.exe"),
                args: vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-File".into(),
                    script.display().to_string(),
                    mode.into(),
                ],
                threads: 1,
                hash_mb: 1,
                network_delay: None,
                network_delay2: None,
                minimum_thinking_time: None,
                slowmover: None,
                ponder: false,
                usi_options: vec![],
            };
            let mut engine =
                EngineProcess::spawn_with_timeout(&cfg, mode.into(), Duration::from_secs(10))
                    .unwrap();
            let result = engine.search_raw_go_with_stop_grace(
                "fixture b - 1",
                "depth 12",
                Duration::from_millis(500),
                Duration::from_secs(2),
                None,
            );
            if mode == "exit" {
                assert!(result.is_err(), "EOF must not fabricate a completed search");
                continue;
            }
            let result = result.unwrap();
            assert_eq!(result.bestmove.as_deref(), Some("7g7f"));
            assert_eq!(result.timed_out, mode != "normal");
            let eval = result.eval.unwrap();
            assert_eq!(eval.nodes, Some(if mode == "normal" { 123 } else { 999 }));
            if mode == "empty" {
                assert!(eval.last_exact_primary.is_none());
            } else {
                let primary = eval.last_exact_primary.unwrap();
                assert_eq!(primary.depth, 8);
                assert_eq!(primary.score, PrimaryScore::Cp(42));
                assert_eq!(primary.pv, ["7g7f", "3c3d"]);
                assert_eq!(primary.raw_line, "info depth 8 score cp 42 pv 7g7f 3c3d");
            }
        }
    }
}

// 短い期限を検証する mock と Python mock の起動を重ねない。
#[cfg(all(test, unix))]
static ENGINE_MOCK_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(all(test, unix))]
mod deadline_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn expired_deadline_does_not_consume_queued_info() {
        let (tx, rx) = mpsc::channel::<String>();
        tx.send("info depth 1".into()).unwrap();
        assert!(matches!(
            receive_before_deadline(&rx, Instant::now()),
            Err(RecvTimeoutError::Timeout)
        ));
        assert_eq!(rx.try_recv().unwrap(), "info depth 1");
    }

    #[test]
    fn raw_go_bounds_both_phases_with_chatty_engine_and_handles_exit() {
        let guard = ENGINE_MOCK_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
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
      printf 'info depth 8 score cp 42 pv 7g7f 3c3d\ninfo depth 9 nodes 123\n'
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
                let primary = result.eval.unwrap().last_exact_primary.unwrap();
                assert_eq!(primary.depth, 8);
                assert_eq!(primary.score, crate::selfplay::types::PrimaryScore::Cp(42));
                assert_eq!(primary.pv, ["7g7f", "3c3d"]);
                assert_eq!(primary.raw_line, "info depth 8 score cp 42 pv 7g7f 3c3d");
            }
            drop(engine);
            let commands = std::fs::read_to_string(log).unwrap();
            assert_eq!(
                commands.lines().filter(|line| *line == "stop").count(),
                usize::from(matches!(mode, "stop" | "flood"))
            );
        }
        drop(guard);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn child_process_inherits_requested_cpu_affinity() {
        let allowed_list = std::fs::read_to_string("/proc/self/status")
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .unwrap()
            .trim()
            .to_owned();
        let first_cpu = allowed_list
            .split(',')
            .next()
            .unwrap()
            .split('-')
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("affinity-mock.sh");
        let log = dir.path().join("affinity.log");
        std::fs::write(
            &path,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    usi)
      awk '/Cpus_allowed_list/ {print $2}' /proc/self/status > "$1"
      printf 'usiok\n'
      ;;
    isready) printf 'readyok\n' ;;
    quit) break ;;
  esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cfg = EngineConfig {
            path,
            args: vec![log.display().to_string()],
            threads: 1,
            hash_mb: 1,
            network_delay: None,
            network_delay2: None,
            minimum_thinking_time: None,
            slowmover: None,
            ponder: false,
            usi_options: vec![],
        };

        let mut engine = EngineProcess::spawn_uninitialized_with_cpu_affinity(
            &cfg,
            "affinity".into(),
            Some(&[first_cpu]),
        )
        .unwrap();
        engine.initialize(&cfg).unwrap();
        assert_eq!(std::fs::read_to_string(log).unwrap().trim(), first_cpu.to_string());
    }
}

#[cfg(test)]
mod time_command_tests {
    use super::*;
    #[test]
    fn time_command_preserves_clock_shapes() {
        for (btime, wtime, binc, winc, byoyomi, expected) in [
            (0, 6000, 0, 200, 0, "go btime 0 wtime 6000 binc 0 winc 200"),
            (0, 6000, 0, 0, 0, "go btime 0 wtime 6000 binc 0 winc 0"),
            (0, 0, 0, 200, 0, "go btime 0 wtime 0 binc 0 winc 200"),
            (1000, 1000, 200, 200, 0, "go btime 1000 wtime 1000 binc 200 winc 200"),
            (0, 0, 0, 0, 1000, "go byoyomi 1000"),
            (1000, 2000, 0, 0, 500, "go btime 1000 wtime 2000 byoyomi 500"),
            (0, 0, 0, 0, 0, "go"),
        ] {
            let args = TimeArgs {
                btime,
                wtime,
                binc,
                winc,
                byoyomi,
            };
            assert_eq!(build_time_command(&args), expected);
        }
    }
}

#[cfg(all(test, unix))]
mod ponder_info_tests {
    use super::*;

    #[test]
    fn game_error_retains_both_cleanup_failures() {
        let guard = ENGINE_MOCK_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        use crate::selfplay::{
            game::{GameConfig, run_game},
            position::ParsedPosition,
            time_control::TimeControl,
        };
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cleanup.py");
        std::fs::write(
            &script,
            r#"
import sys
for raw in sys.stdin:
    line = raw.strip()
    if line == 'usi': print('usiok', flush=True)
    elif line == 'isready': print('readyok', flush=True)
    elif line in ('stop', 'quit'): break
"#,
        )
        .unwrap();
        let cfg = EngineConfig {
            path: "python3".into(),
            args: vec![script.display().to_string()],
            threads: 1,
            hash_mb: 1,
            network_delay: None,
            network_delay2: None,
            minimum_thinking_time: None,
            slowmover: None,
            ponder: false,
            usi_options: vec![],
        };
        let mut black = EngineProcess::spawn(&cfg, "black".into()).unwrap();
        let mut white = EngineProcess::spawn(&cfg, "white".into()).unwrap();
        black.search_pending = true;
        white.search_pending = true;
        let config = GameConfig {
            resign_rule: None,
            win_rule: None,
            draw_rule: None,
            agreement_rule: None,
            max_moves: 5,
            timeout_margin_ms: 1000,
            limit_only_timeout_ms: Some(0),
            game_time_limit_ms: None,
            cancel: None,
            pass_rights: None,
            go_depth: None,
            go_nodes_black: None,
            go_nodes_white: None,
        };
        let start = ParsedPosition {
            startpos: true,
            sfen: None,
            moves: vec![],
            source_line: None,
        };
        let result = run_game(
            &mut black,
            &mut white,
            &start,
            TimeControl::new(0, 0, 0, 0, 10000),
            &config,
            1,
            &mut |_| {},
            None,
        );
        let error = result.err().unwrap();
        assert_eq!(error.root_cause().to_string(), "limit_only_timeout_ms must be positive");
        let detail = format!("{error:#}");
        assert!(detail.contains("black: engine exited"), "{detail}");
        assert!(detail.contains("white: engine exited"), "{detail}");
        assert!(!black.is_reusable());
        assert!(!white.is_reusable());
        drop(guard);
    }

    #[test]
    fn legacy_ponder_config_preserves_options_and_timeout_cleanup() {
        let guard = ENGINE_MOCK_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        for advertised in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("legacy.py");
            let log = dir.path().join("commands");
            std::fs::write(
                &script,
                format!(
                    r#"
import sys
with open(sys.argv[1], 'w', buffering=1) as log:
    for raw in sys.stdin:
        line = raw.strip()
        log.write(line + '\n')
        if line == 'usi':
            if {advertised}: print('option name USI_Ponder type check default false', flush=True)
            print('usiok', flush=True)
        elif line == 'isready': print('readyok', flush=True)
        elif line == 'quit': break
"#,
                    advertised = if advertised { "True" } else { "False" }
                ),
            )
            .unwrap();
            let cfg = EngineConfig {
                path: "python3".into(),
                args: vec![script.display().to_string(), log.display().to_string()],
                threads: 1,
                hash_mb: 1,
                network_delay: None,
                network_delay2: None,
                minimum_thinking_time: None,
                slowmover: None,
                ponder: true,
                usi_options: vec!["USI_Ponder=false".into()],
            };
            let mut engine = EngineProcess::spawn(&cfg, "legacy".into()).unwrap();
            let req = SearchRequest {
                sfen: "fixture",
                moves: "",
                time_args: TimeArgs {
                    btime: 0,
                    wtime: 0,
                    binc: 0,
                    winc: 0,
                    byoyomi: 0,
                },
                think_limit_ms: 10000,
                timeout_margin_ms: 10,
                limit_only_timeout_ms: Some(10),
                game_id: 1,
                ply: 1,
                side: rshogi_core::types::Color::Black,
                engine_label: "legacy".into(),
                pass_rights: None,
                go_depth: None,
                go_nodes: Some(1),
            };
            assert!(engine.search(&req, None).unwrap().timed_out);
            assert!(!engine.search_pending);
            engine.stop_ponder().unwrap();
            engine.sync_ready().unwrap();
            let commands = std::fs::read_to_string(&log).unwrap();
            let mut expected = vec!["usi"];
            if advertised {
                expected.push("setoption name USI_Ponder value true");
            } else {
                expected.extend([
                    "setoption name Threads value 1",
                    "setoption name USI_Hash value 1",
                    "setoption name Hash value 1",
                ]);
            }
            expected.extend([
                "setoption name USI_Ponder value false",
                "isready",
                "usinewgame",
                "position sfen fixture",
                "go nodes 1",
                "stop",
                "isready",
            ]);
            assert_eq!(commands.lines().collect::<Vec<_>>(), expected);
        }
        drop(guard);
    }

    #[test]
    fn hit_drops_mixed_primary_records() {
        for lines in [
            vec![
                "info depth 10 score cp 42 pv 7g7f",
                "info depth 11 score cp 99 lowerbound pv 3c3d",
            ],
            vec!["info depth 10 score cp 42 pv 7g7f", "info score cp 99"],
            vec!["info depth 10 score cp 42 pv 7g7f", "info pv 3c3d"],
        ] {
            let mut snapshot = InfoSnapshot::default();
            let mut scored_pv = None;
            for line in lines {
                if let Some(pv) = parse_scored_primary_pv(line) {
                    scored_pv = pv;
                }
                snapshot.update_from_line(line);
            }
            snapshot.update_from_line("info depth 11 multipv 2 score cp 20 pv 8c8d");
            snapshot.update_from_line("info nodes 123");
            retain_matching_ponder_eval(&mut snapshot, scored_pv.as_deref(), "7g7f");
            assert!(snapshot.score_cp.is_none());
            assert!(snapshot.last_exact_primary.is_none());
            assert!(snapshot.pv.is_empty());
            assert_eq!(snapshot.nodes, Some(123));
        }
    }

    #[test]
    fn hit_checks_bound_mate_and_missing_bestmove() {
        use super::super::types::{PrimaryScore, ScoreBound};

        let guard = ENGINE_MOCK_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        for (info, bestmove, expected_cp, expected_mate, expected_exact) in [
            (
                "info depth 10 score cp 42 pv 3c3d\ninfo depth 11 score cp 99 lowerbound pv 7g7f",
                Some("7g7f"),
                Some(99),
                None,
                false,
            ),
            (
                "info depth 10 score cp 42 pv 7g7f\ninfo depth 11 score cp 99 lowerbound pv 3c3d",
                Some("7g7f"),
                None,
                None,
                false,
            ),
            ("info depth 10 score mate 3 pv 7g7f", Some("7g7f"), None, Some(3), true),
            ("info depth 10 score mate 3 pv 3c3d", Some("7g7f"), None, None, false),
            ("info depth 10 score mate + pv 7g7f", Some("7g7f"), None, None, true),
            ("info depth 10 score mate + pv 3c3d", Some("7g7f"), None, None, false),
            ("info depth 10 score cp 42 pv 7g7f", None, None, None, false),
            ("info depth 10 score cp 42 pv 7g7f", Some("resign"), None, None, false),
            ("info depth 10 score cp 42 pv 7g7f", Some("win"), None, None, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("engine.py");
            std::fs::write(
                &script,
                format!(
                    r#"
import sys
for raw in sys.stdin:
    line = raw.strip()
    if line == 'usi': print('usiok', flush=True)
    elif line == 'isready': print('readyok', flush=True)
    elif line == 'go ponder':
        print({info:?}, flush=True)
        print('info nodes 123 nps 456 time 78', flush=True)
    elif line == 'ponderhit':
        if {reply:?}: print({reply:?}, flush=True)
    elif line == 'quit': break
"#,
                    reply = bestmove.map(|mv| format!("bestmove {mv}")).unwrap_or_default(),
                ),
            )
            .unwrap();
            let cfg = EngineConfig {
                path: "python3".into(),
                args: vec![script.display().to_string()],
                threads: 1,
                hash_mb: 1,
                network_delay: None,
                network_delay2: None,
                minimum_thinking_time: None,
                slowmover: None,
                ponder: false,
                usi_options: vec![],
            };
            let mut engine = EngineProcess::spawn(&cfg, "fake".into()).unwrap();
            engine.enable_tournament_ponder(true).unwrap();
            engine.pondering = Some("3c3d".into());
            engine.search_pending = true;
            engine.write_line("go ponder").unwrap();
            let req = SearchRequest {
                sfen: "startpos",
                moves: "3c3d",
                time_args: TimeArgs {
                    btime: 0,
                    wtime: 0,
                    binc: 0,
                    winc: 0,
                    byoyomi: 0,
                },
                think_limit_ms: 10000,
                timeout_margin_ms: 20,
                limit_only_timeout_ms: Some(100),
                game_id: 1,
                ply: 3,
                side: rshogi_core::types::Color::Black,
                engine_label: "fake".into(),
                pass_rights: None,
                go_depth: None,
                go_nodes: Some(1),
            };
            let mut lines = Vec::new();
            let mut callback = |line: &str, _: &SearchRequest<'_>| lines.push(line.to_owned());
            let outcome = engine.search(&req, Some(&mut callback)).unwrap();
            assert_eq!(outcome.bestmove.as_deref(), bestmove);
            assert_eq!(outcome.timed_out, bestmove.is_none());
            assert_eq!(outcome.watchdog_fired, bestmove.is_none());
            assert!(lines.is_empty());
            let eval = outcome.eval.unwrap();
            assert_eq!(eval.nodes, Some(123));
            assert_eq!(eval.nps, Some(456));
            assert_eq!(eval.time_ms, Some(78));
            assert_eq!(eval.score_cp, expected_cp);
            assert_eq!(eval.score_mate, expected_mate);
            assert_eq!(eval.last_exact_primary.is_some(), expected_exact);
            let retained = expected_cp.is_some() || expected_exact;
            assert_eq!(eval.pv.as_deref(), retained.then_some(["7g7f".to_owned()].as_slice()));
            assert_eq!(eval.score_bound, expected_cp.map(|_| ScoreBound::Lowerbound));
            assert_eq!(
                eval.score_mate_sign,
                (expected_exact && info.contains("mate +")).then_some(1)
            );
            if expected_mate.is_some() {
                assert_eq!(eval.last_exact_primary.unwrap().score, PrimaryScore::Mate(3));
            }
        }
        drop(guard);
    }

    #[test]
    fn hit_validates_primary_pv_and_preserves_counters() {
        let guard = ENGINE_MOCK_TEST_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        for (pv, post, delayed, early, expected) in [
            ("7g7f", false, false, false, Some(42)),
            ("3c3d", false, false, false, None),
            ("3c3d", true, false, false, Some(99)),
            ("7g7f", true, false, false, Some(99)),
            ("3c3d", false, true, false, None),
            ("", false, false, false, None),
            ("3c3d", false, false, true, None),
            ("7g7f", false, false, true, Some(42)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("engine.py");
            std::fs::write(
                &script,
                format!(
                    r#"
import sys
for raw in sys.stdin:
    line = raw.strip()
    if line == 'usi': print('usiok', flush=True)
    elif line == 'isready': print('readyok', flush=True)
    elif line == 'go ponder':
        if not {delayed}:
            print('info depth 10 score cp 42 pv {pv}', flush=True)
            print('info multipv 2 depth 10 score cp -200 pv 8c8d', flush=True)
            print('info nodes 123 nps 456 time 78', flush=True)
        if {early}:
            print('bestmove 7g7f', flush=True)
    elif line == 'ponderhit':
        if {delayed}:
            print('info depth 10 score cp 42 pv {pv}', flush=True)
            print('info nodes 123 nps 456 time 78', flush=True)
        if {post}:
            print('info depth 2 score cp 99 pv 7g7f', flush=True)
        if not {early}:
            print('bestmove 7g7f', flush=True)
    elif line == 'quit': break
"#,
                    post = if post { "True" } else { "False" },
                    delayed = if delayed { "True" } else { "False" },
                    early = if early { "True" } else { "False" }
                ),
            )
            .unwrap();
            let cfg = EngineConfig {
                path: "python3".into(),
                args: vec![script.display().to_string()],
                threads: 1,
                hash_mb: 1,
                network_delay: None,
                network_delay2: None,
                minimum_thinking_time: None,
                slowmover: None,
                ponder: false,
                usi_options: vec![],
            };
            let mut engine = EngineProcess::spawn(&cfg, "fake".into()).unwrap();
            engine.enable_tournament_ponder(true).unwrap();
            engine.pondering = Some("3c3d".into());
            engine.search_pending = true;
            engine.write_line("go ponder").unwrap();
            let req = SearchRequest {
                sfen: "startpos",
                moves: "3c3d",
                time_args: TimeArgs {
                    btime: 0,
                    wtime: 0,
                    binc: 0,
                    winc: 0,
                    byoyomi: 0,
                },
                think_limit_ms: 10000,
                timeout_margin_ms: 1000,
                limit_only_timeout_ms: None,
                game_id: 1,
                ply: 3,
                side: rshogi_core::types::Color::Black,
                engine_label: "fake".into(),
                pass_rights: None,
                go_depth: None,
                go_nodes: Some(1),
            };
            let mut lines = Vec::new();
            let mut callback = |line: &str, _: &SearchRequest<'_>| lines.push(line.to_owned());
            let result = engine.search(&req, Some(&mut callback)).unwrap();
            assert_eq!(result.eval.as_ref().and_then(|e| e.score_cp), expected);
            assert!(lines.is_empty());
            assert!(!engine.search_pending);
            let eval = result.eval.unwrap();
            assert_eq!(eval.nodes, Some(123));
            assert_eq!(eval.nps, Some(456));
            assert_eq!(eval.time_ms, Some(78));
            if let Some(expected) = expected {
                assert_eq!(eval.pv.unwrap(), ["7g7f"]);
                assert_eq!(
                    eval.last_exact_primary.unwrap().score,
                    super::super::types::PrimaryScore::Cp(expected)
                );
            } else {
                assert!(eval.pv.is_none());
                assert!(eval.last_exact_primary.is_none());
                assert!(eval.score_mate.is_none());
                assert!(eval.score_mate_sign.is_none());
            }
            engine.new_game().unwrap();
        }
        drop(guard);
    }
}
