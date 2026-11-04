//! `--remote-worker` / `--worker-stdio`: 対局を別の機械の worker に任せる。
//!
//! coordinator は worker を `sh -c <command>` で起動し (ssh 越しを想定)、stdin/stdout の JSON 行で
//! pair を渡して結果を受け取る。対局は worker の機械の中で完結させ、通信の往復を時計に乗せない。
//! θ・journal・snapshot は coordinator だけが持ち、worker は渡された設定値で 2 局指して返すだけ。
//!
//! やりとり: worker → `hello` (対局条件の fingerprint) → coordinator → `init` (param 名・θ₀) →
//! worker → `ready` (`--nps-scaling` ならその機械の bench) → `task` と `done` / `failed` の繰り返し。
//! 両方向に一定間隔で `heartbeat` を送り、`--remote-timeout-sec` の間相手から何も届かなければ切れたとみなす
//! (coordinator はその worker を外し、worker は対局を止めて終わる)。coordinator が stdin を閉じたときも
//! worker は対局を止めて終わる。

use std::collections::{HashMap, VecDeque};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::*;

const PROTOCOL_VERSION: u32 = 1;
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const DEFAULT_REMOTE_TIMEOUT_SEC: u64 = 300;
/// heartbeat 1 回の遅れで切らないよう、timeout は heartbeat の間隔の 2 倍以上にする。
const MIN_REMOTE_TIMEOUT_SEC: u64 = 2 * HEARTBEAT_INTERVAL.as_secs();
/// 終了時に stdin を閉じてから worker の終了を待つ時間。過ぎたら process group ごと止める。
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WireParam {
    name: String,
    is_int: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct WireClock {
    black_time: u64,
    white_time: u64,
    black_inc: u64,
    white_inc: u64,
    byoyomi: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WireTask {
    pair_id: u32,
    attempt: u32,
    start_pos_index: usize,
    #[serde(with = "exact_f64")]
    plus: Vec<f64>,
    #[serde(with = "exact_f64")]
    minus: Vec<f64>,
    tc: WireClock,
    game_time_limit_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToWorker {
    Init {
        params: Vec<WireParam>,
        application_mask: Vec<bool>,
        #[serde(with = "exact_f64")]
        theta0: Vec<f64>,
        nps_bench: bool,
        timeout_sec: u64,
    },
    Task(WireTask),
    Heartbeat,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct WireBench {
    slot_nps: Vec<f64>,
    nps: f64,
    depth: u32,
    warmup_depth: u32,
    positions: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FromWorker {
    Hello {
        protocol: u32,
        host: String,
        slots: usize,
        fingerprint: Box<RunFingerprint>,
        spsa_sha256: String,
    },
    Ready {
        bench: Option<WireBench>,
    },
    Done {
        pair_id: u32,
        attempt: u32,
        games: Box<[GameRecord; 2]>,
        details: Box<[GameDetail; 2]>,
        retries: u32,
    },
    Failed {
        pair_id: u32,
        attempt: u32,
        reason: String,
    },
    Heartbeat,
    /// worker が続けられない (engine が起動しない等)。coordinator はその worker を切る。
    Fatal {
        message: String,
    },
}

fn to_line<T: Serialize>(message: &T) -> Result<Vec<u8>> {
    let mut line = serde_json::to_vec(message).context("failed to encode message")?;
    line.push(b'\n');
    Ok(line)
}

// =============================================================================
// worker (`--worker-stdio`)
// =============================================================================

/// stdout (応答専用) へ書く thread への送り口。coordinator が読まなくなって書き込みが詰まっても、
/// 送る側 (結果の受け渡し・切断の監視) は止まらない。行は 1 本の thread が順に書くので混ざらない。
struct Output {
    tx: Mutex<Option<crossbeam_channel::Sender<Vec<u8>>>>,
    done: crossbeam_channel::Receiver<()>,
}

impl Output {
    fn spawn(mut writer: impl Write + Send + 'static) -> Self {
        let (tx, rx) = unbounded::<Vec<u8>>();
        let (done_tx, done) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            for line in rx {
                if writer.write_all(&line).and_then(|()| writer.flush()).is_err() {
                    break;
                }
            }
            let _ = done_tx.send(());
        });
        Self {
            tx: Mutex::new(Some(tx)),
            done,
        }
    }

    fn send(&self, message: &FromWorker) -> Result<()> {
        let line = to_line(message)?;
        let tx = self.tx.lock().map_err(|_| anyhow::anyhow!("stdout lock poisoned"))?;
        match tx.as_ref() {
            Some(tx) => tx.send(line).map_err(|_| anyhow::anyhow!("stdout is closed")),
            None => bail!("stdout is closed"),
        }
    }

    /// 書き込みに失敗して thread が終わった (coordinator 側が出力を閉じた)。worker はこれを切断として止まる。
    /// 止まらないと、`sh -c` の leader が coordinator への pipe を持ったまま残り、coordinator に終わりが届かない。
    fn closed(&self) -> &crossbeam_channel::Receiver<()> {
        &self.done
    }

    fn is_closed(&self) -> bool {
        !self.done.is_empty()
    }

    /// 送り口を閉じ、溜まった行を書き終えるのを `wait` まで待つ (詰まっていれば待たずに終わる)。
    fn finish(&self, wait: Duration) {
        if let Ok(mut tx) = self.tx.lock() {
            tx.take();
        }
        let _ = self.done.recv_timeout(wait);
    }
}

/// 終了時に、最後の行 (`fatal` など) を書き出すのを待つ上限。
const OUTPUT_FINISH_WAIT: Duration = Duration::from_secs(5);

/// coordinator 側の `--remote-worker` 系の flag の検証 (`--dispatch async` の検証から呼ぶ)。
pub(super) fn validate_cli(cli: &Cli) -> Result<()> {
    if cli.remote_workers.is_empty() {
        for (flag, given) in [
            ("--remote-timeout-sec", cli.remote_timeout_sec.is_some()),
            ("--allow-worker-engine-sha256", !cli.allow_worker_engine_sha256.is_empty()),
        ] {
            if given {
                bail!("{flag} は --remote-worker と一緒に指定してください");
            }
        }
        return Ok(());
    }
    if !cfg!(unix) {
        bail!(
            "--remote-worker は Unix の coordinator でだけ使えます (worker を sh -c で起動するため)"
        );
    }
    if cli.remote_timeout_sec.is_some_and(|sec| sec < MIN_REMOTE_TIMEOUT_SEC) {
        bail!("--remote-timeout-sec must be >= {MIN_REMOTE_TIMEOUT_SEC}");
    }
    Ok(())
}

/// `--worker-stdio` の入口。続けられないエラーは `fatal` で coordinator に伝えてから終わる。
pub(crate) fn run_worker(cli: &Cli) -> Result<()> {
    if !cli.remote_workers.is_empty() {
        bail!("--worker-stdio と --remote-worker は同時に指定できません");
    }
    if cli.concurrency == 0 {
        bail!("--concurrency must be >= 1");
    }
    if cli.threads == 0 {
        bail!("--threads must be >= 1");
    }
    if cli.nps_scaling
        && tools::selfplay::nps_bench::sets_threads_option(cli.usi_options.iter().flatten())
    {
        bail!("--nps-scaling では Threads を --usi-option ではなく --threads で指定してください");
    }
    let out = Arc::new(Output::spawn(std::io::stdout()));
    let result = serve(cli, &out);
    if let Err(error) = &result {
        let _ = out.send(&FromWorker::Fatal {
            message: format!("{error:#}"),
        });
    }
    out.finish(OUTPUT_FINISH_WAIT);
    result
}

/// stdin の行を読む thread。読み終わり (coordinator が閉じた) は channel の切断で伝わる。
/// 終了時に join しない (stdin の読み込みは中断できない)。
fn spawn_stdin_reader() -> crossbeam_channel::Receiver<Result<ToWorker>> {
    let (tx, rx) = unbounded();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let message = line.context("failed to read stdin").and_then(|line| {
                serde_json::from_str(&line).context("invalid message from coordinator")
            });
            let stop = message.is_err();
            if tx.send(message).is_err() || stop {
                break;
            }
        }
    });
    rx
}

fn serve(cli: &Cli, out: &Arc<Output>) -> Result<()> {
    let engine_path = resolve_engine_path(cli)?;
    let translator = match &cli.engine_param_mapping {
        Some(path) => EngineNameTranslator::from_mapping_file(path)?,
        None => EngineNameTranslator::empty(),
    };
    let start_positions = load_run_start_positions(cli)?;
    let base_cfg = build_engine_config(cli, engine_path);
    let fingerprint = RunFingerprint::new(cli, &base_cfg)?;
    let (game_cfg, base_tc) = build_game_setup(cli);
    let faults = FaultPlan::from_env()?;
    let spsa_sha256 = std::env::current_exe()
        .context("failed to locate the spsa binary")
        .and_then(|exe| sha256_hex_of_file(&exe))?;
    out.send(&FromWorker::Hello {
        protocol: PROTOCOL_VERSION,
        host: read_hostname(),
        slots: cli.concurrency,
        fingerprint: Box::new(fingerprint),
        spsa_sha256,
    })?;
    {
        let out = Arc::clone(out);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(HEARTBEAT_INTERVAL);
                if out.send(&FromWorker::Heartbeat).is_err() {
                    break;
                }
            }
        });
    }
    let inbox = spawn_stdin_reader();
    // init が届くまでは coordinator の `--remote-timeout-sec` を知らないので、既定値で途絶を見る
    // (coordinator は init より前から heartbeat を送る)。
    let handshake_timeout = Duration::from_secs(DEFAULT_REMOTE_TIMEOUT_SEC);
    let mut last_seen = Instant::now();
    let (wire_params, application_mask, theta0, nps_bench, timeout_sec) = loop {
        match inbox.recv_timeout(POLL_INTERVAL) {
            Ok(Ok(ToWorker::Init {
                params,
                application_mask,
                theta0,
                nps_bench,
                timeout_sec,
            })) => break (params, application_mask, theta0, nps_bench, timeout_sec),
            Ok(Ok(ToWorker::Heartbeat)) => last_seen = Instant::now(),
            Ok(Ok(ToWorker::Task(_))) => bail!("init より先に task が届きました"),
            Ok(Err(error)) => return Err(error),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if out.is_closed() {
                    bail!("coordinator への出力が閉じました");
                }
                if last_seen.elapsed() > handshake_timeout {
                    bail!(
                        "init を待つ間 coordinator から {} 秒何も届きません",
                        handshake_timeout.as_secs()
                    );
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    };
    let timeout = Duration::from_secs(timeout_sec);
    if application_mask.len() != wire_params.len() || theta0.len() != wire_params.len() {
        bail!("init の param 数が揃っていません");
    }
    // worker が使うのは engine へ送る名前と整数の丸めだけ。範囲や更新の係数は coordinator が持つ。
    let params: Vec<SpsaParam> = wire_params
        .into_iter()
        .map(|p| SpsaParam {
            type_name: if p.is_int { "int" } else { "float" }.into(),
            name: p.name,
            is_int: p.is_int,
            value: 0.0,
            min: 0.0,
            max: 0.0,
            c_end: 0.0,
            r_end: 0.0,
            comment: String::new(),
            not_used: false,
        })
        .collect();
    let cpu_affinities = plan_cpu_affinities(cli, cli.concurrency)?;
    let ctx = WorkerContext {
        base_cfg: &base_cfg,
        params: &params,
        start_positions: &start_positions,
        game_cfg: &game_cfg,
        tc: base_tc,
        translator: &translator,
        application_mask: &application_mask,
        cpu_affinities: cpu_affinities.as_deref(),
    };
    let bench = if nps_bench {
        eprintln!("nps bench: {} slot(s) x {} thread(s) ...", cli.concurrency, cli.threads);
        let cancel = AtomicBool::new(false);
        let finished = AtomicBool::new(false);
        let (result, watched) = std::thread::scope(|scope| {
            let watcher = scope
                .spawn(|| watch_coordinator_during_bench(&inbox, out, &finished, &cancel, timeout));
            let result = nps_bench::measure(ctx, cli.concurrency, &theta0, cli.threads, &cancel);
            finished.store(true, Ordering::Release);
            (result, watcher.join())
        });
        watched.map_err(|_| anyhow::anyhow!("bench watcher panic"))??;
        let result = result?;
        eprintln!("{}", result.summary());
        Some(WireBench {
            slot_nps: result.slot_nps,
            nps: result.nps,
            depth: result.depth,
            warmup_depth: result.warmup_depth,
            positions: result.positions,
        })
    } else {
        None
    };
    out.send(&FromWorker::Ready { bench })?;

    std::thread::scope(|scope| -> Result<()> {
        let pool = AsyncPool::new(scope, cli.concurrency, ctx, cli.engine_retries, &faults)?;
        // bench の間に溜まった heartbeat は select で順に読むので、ここから数え始める。
        let mut last_seen = Instant::now();
        loop {
            crossbeam_channel::select! {
                recv(inbox) -> message => match message {
                    Ok(Ok(ToWorker::Heartbeat)) => last_seen = Instant::now(),
                    Ok(Ok(ToWorker::Task(task))) => {
                        last_seen = Instant::now();
                        if task.start_pos_index >= start_positions.len() {
                            bail!(
                                "pair {}: start_pos_index {} が開始局面の数 {} を超えています",
                                task.pair_id,
                                task.start_pos_index,
                                start_positions.len()
                            );
                        }
                        pool.send(PairTask {
                            pair_id: task.pair_id,
                            attempt: task.attempt,
                            start_pos_index: task.start_pos_index,
                            plus: task.plus,
                            minus: task.minus,
                            tc: TimeControl::new(
                                task.tc.black_time,
                                task.tc.white_time,
                                task.tc.black_inc,
                                task.tc.white_inc,
                                task.tc.byoyomi,
                            ),
                            game_time_limit_ms: task.game_time_limit_ms,
                        })?;
                    }
                    Ok(Ok(ToWorker::Init { .. })) => bail!("init が 2 度届きました"),
                    Ok(Err(error)) => return Err(error),
                    // coordinator が stdin を閉じた。指している対局は pool の破棄で止める。
                    Err(_) => return Ok(()),
                },
                recv(pool.results) -> message => {
                    let message = message.context("worker pool closed")??;
                    out.send(&to_wire(message))?;
                }
                recv(out.closed()) -> _ => bail!("coordinator への出力が閉じました"),
                // 経路が半開きのまま切れると stdin の終わりが届かないので、coordinator の heartbeat の途絶で止める。
                default(POLL_INTERVAL) => {
                    if last_seen.elapsed() > timeout {
                        bail!("coordinator から {} 秒何も届きません", timeout.as_secs());
                    }
                }
            }
        }
    })
}

/// bench の間も coordinator の切断 (stdin の終わり・heartbeat の途絶) を見て、切れたら bench を止める。
/// bench が終わると (`finished`) 何もせずに戻る。
fn watch_coordinator_during_bench(
    inbox: &crossbeam_channel::Receiver<Result<ToWorker>>,
    out: &Output,
    finished: &AtomicBool,
    cancel: &AtomicBool,
    timeout: Duration,
) -> Result<()> {
    let mut last_seen = Instant::now();
    let stop = |error: anyhow::Error| {
        cancel.store(true, Ordering::Release);
        Err(error)
    };
    while !finished.load(Ordering::Acquire) {
        match inbox.recv_timeout(POLL_INTERVAL) {
            Ok(Ok(ToWorker::Heartbeat)) => last_seen = Instant::now(),
            Ok(Ok(_)) => return stop(anyhow::anyhow!("ready より先に init / task が届きました")),
            Ok(Err(error)) => return stop(error),
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                if out.is_closed() {
                    return stop(anyhow::anyhow!("coordinator への出力が閉じました"));
                }
                if last_seen.elapsed() > timeout {
                    return stop(anyhow::anyhow!(
                        "coordinator から {} 秒何も届きません",
                        timeout.as_secs()
                    ));
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                return stop(anyhow::anyhow!("coordinator が入力を閉じました"));
            }
        }
    }
    Ok(())
}

fn to_wire(message: WorkerMessage) -> FromWorker {
    match message {
        WorkerMessage::Done {
            pair_id,
            attempt,
            games,
            details,
            retries,
        } => FromWorker::Done {
            pair_id,
            attempt,
            games: Box::new(games),
            details,
            retries,
        },
        WorkerMessage::Failed {
            pair_id,
            attempt,
            reason,
        } => FromWorker::Failed {
            pair_id,
            attempt,
            reason,
        },
    }
}

// =============================================================================
// coordinator (`--remote-worker`)
// =============================================================================

/// 実行環境のパス (EvalFile 等) は機械ごとに違うので、ファイルを指す USI option は中身の sha256 で比べる。
/// engine は `--allow-worker-engine-sha256` で別 build を許すので別に比べる。
fn portable_fingerprint(fingerprint: &RunFingerprint) -> RunFingerprint {
    let files: HashMap<&str, &str> = fingerprint
        .usi_option_files
        .iter()
        .map(|(name, sha)| (name.as_str(), sha.as_str()))
        .collect();
    let mut portable = fingerprint.clone();
    portable.engine_sha256 = String::new();
    portable.usi_options = fingerprint
        .usi_options
        .iter()
        .map(|option| match option.split_once('=') {
            Some((name, _)) if files.contains_key(name.trim()) => {
                format!("{}=<file sha256 {}>", name.trim(), files[name.trim()])
            }
            _ => option.clone(),
        })
        .collect();
    portable
}

/// 2 つの fingerprint で値の違う項目名。
fn fingerprint_diff(expected: &RunFingerprint, actual: &RunFingerprint) -> Result<Vec<String>> {
    let expected = serde_json::to_value(expected)?;
    let actual = serde_json::to_value(actual)?;
    let (Some(expected), Some(actual)) = (expected.as_object(), actual.as_object()) else {
        bail!("fingerprint must be an object");
    };
    let mut keys: Vec<&String> = expected.keys().chain(actual.keys()).collect();
    keys.sort();
    keys.dedup();
    Ok(keys
        .into_iter()
        .filter(|key| expected.get(*key) != actual.get(*key))
        .map(|key| {
            format!(
                "{key}: coordinator={} worker={}",
                expected.get(key).unwrap_or(&serde_json::Value::Null),
                actual.get(key).unwrap_or(&serde_json::Value::Null)
            )
        })
        .collect())
}

pub(super) struct FleetSetup<'a> {
    pub(super) cli: &'a Cli,
    pub(super) fingerprint: &'a RunFingerprint,
    pub(super) params: &'a [SpsaParam],
    pub(super) application_mask: &'a [bool],
    pub(super) theta0: Vec<f64>,
}

/// 対局を始められた worker。coordinator が journal に記録する。
pub(super) struct Joined {
    pub(super) owner: usize,
    pub(super) name: String,
    pub(super) host: String,
    pub(super) slots: usize,
    pub(super) engine_sha256: String,
    pub(super) spsa_sha256: String,
    pub(super) bench: Option<nps_bench::BenchResult>,
}

enum Event {
    Message(Box<FromWorker>),
    Closed(String),
}

/// reader / writer の thread から届く event。`at` は受け取った時刻で、処理の遅れで切断と誤判定しないために使う。
struct Received {
    owner: usize,
    at: Instant,
    event: Event,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WorkerState {
    /// `hello` 待ち。
    Starting,
    /// `init` を送って `ready` 待ち。
    Initializing,
    Ready,
    Lost,
}

struct RemoteWorker {
    name: String,
    /// 止めて回収するまで持つ。回収後の pid は再利用され得るので、process group の kill はその前にだけ送る。
    child: Option<Child>,
    /// 書き込み thread への送り口。落とすと thread が終わり、worker の stdin が閉じる。
    outbox: Option<crossbeam_channel::Sender<Vec<u8>>>,
    state: WorkerState,
    host: String,
    slots: usize,
    clock: Option<PairClock>,
    in_flight: usize,
    last_seen: Instant,
}

pub(super) struct RemoteFleet {
    workers: Vec<RemoteWorker>,
    events: crossbeam_channel::Receiver<Received>,
    timeout: Duration,
    /// 実行中の (pair, attempt) を渡した worker。
    assigned: HashMap<(u32, u32), usize>,
    /// 切れた worker の pair の失敗。coordinator へ順に返す。
    pending: VecDeque<Result<WorkerMessage>>,
    joined: Vec<Joined>,
}

fn spawn_reader(
    owner: usize,
    name: String,
    stdout: ChildStdout,
    events: crossbeam_channel::Sender<Received>,
) {
    let send = move |event| {
        events
            .send(Received {
                owner,
                at: Instant::now(),
                event,
            })
            .is_ok()
    };
    std::thread::spawn(move || {
        let reason = (|| {
            for line in BufReader::new(stdout).lines() {
                let line = match line {
                    Ok(line) => line,
                    Err(error) => return format!("failed to read worker output: {error}"),
                };
                // ssh 先の shell の初期化 (motd・rc ファイルの echo) が stdout に書くことがある。
                // spsa の行は必ず JSON の object なので、それ以外は読み飛ばす。
                if !line.trim_start().starts_with('{') {
                    if !line.trim().is_empty() {
                        eprintln!(
                            "remote worker {name}: ignoring non-protocol output: {line:.200}"
                        );
                    }
                    continue;
                }
                let event = match serde_json::from_str::<FromWorker>(&line) {
                    Ok(message) => Event::Message(Box::new(message)),
                    Err(error) => {
                        return format!("invalid message from worker: {error}: {line:.200}");
                    }
                };
                if !send(event) {
                    return String::new();
                }
            }
            "worker closed its output".into()
        })();
        send(Event::Closed(reason));
    });
}

/// worker の stdin へ書く thread。worker が読まなくなっても coordinator の thread は止まらない
/// (詰まった書き込みは、timeout でその worker の process group を止めたときに失敗して終わる)。
/// 送るものが無い間は heartbeat を送る。
fn spawn_writer(
    owner: usize,
    mut stdin: ChildStdin,
    events: crossbeam_channel::Sender<Received>,
) -> Result<crossbeam_channel::Sender<Vec<u8>>> {
    let heartbeat = to_line(&ToWorker::Heartbeat)?;
    let (tx, rx) = unbounded::<Vec<u8>>();
    std::thread::spawn(move || {
        loop {
            let line = match rx.recv_timeout(HEARTBEAT_INTERVAL) {
                Ok(line) => line,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => heartbeat.clone(),
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            };
            if let Err(error) = stdin.write_all(&line).and_then(|()| stdin.flush()) {
                let _ = events.send(Received {
                    owner,
                    at: Instant::now(),
                    event: Event::Closed(format!("failed to send to worker: {error}")),
                });
                break;
            }
        }
    });
    Ok(tx)
}

/// leader を回収せずに (pid を手放さずに) 終わったかを見る。
fn leader_exited(child: &Child) -> bool {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{}/stat", child.id())) {
            Ok(stat) => stat
                .rsplit_once(')')
                .and_then(|(_, rest)| rest.split_whitespace().next())
                .is_some_and(|state| state == "Z"),
            Err(_) => true,
        }
    }
    // 回収せずに終了を知る手段が std に無いので、Linux 以外では終了時に猶予を使い切ってから止める。
    #[cfg(not(target_os = "linux"))]
    {
        let _ = child;
        false
    }
}

/// `sh -c` とその子 (ssh 等) を process group ごと止めて leader を回収する。leader が先に終わっていても
/// 子が group に残り得るので、group への kill は常に送る (回収前なので pid は再利用されていない)。
fn kill_group(mut child: Child) {
    // libc::kill は unsafe なので、group への送信は kill コマンドに任せる。
    #[cfg(unix)]
    if let Err(error) = Command::new("kill")
        .args(["-KILL", "--", &format!("-{}", child.id())])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        eprintln!(
            "warning: kill を起動できず worker の process group を止められません (sh の子が残り得ます): {error}"
        );
    }
    let _ = child.kill();
    let _ = child.wait();
}

impl RemoteFleet {
    /// worker を全て起動し、`ready` か切断まで待つ。1 台も始められなければ Err。
    pub(super) fn connect(setup: FleetSetup<'_>) -> Result<Self> {
        let cli = setup.cli;
        let timeout_sec = cli.remote_timeout_sec.unwrap_or(DEFAULT_REMOTE_TIMEOUT_SEC);
        let log_dir = cli.run_dir().join("remote");
        std::fs::create_dir_all(&log_dir)
            .with_context(|| format!("failed to create {}", log_dir.display()))?;
        let (tx, events) = unbounded();
        let mut fleet = Self {
            workers: Vec::new(),
            events,
            timeout: Duration::from_secs(timeout_sec),
            assigned: HashMap::new(),
            pending: VecDeque::new(),
            joined: Vec::new(),
        };
        for (owner, command) in cli.remote_workers.iter().enumerate() {
            let name = format!("w{}", owner + 1);
            let log_path = log_dir.join(format!("{name}.log"));
            let log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .with_context(|| format!("failed to open {}", log_path.display()))?;
            let mut shell = Command::new("sh");
            shell
                .arg("-c")
                .arg(command)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(log);
            // 切るときに sh の子 (ssh 等) まで止められるよう、worker ごとに process group を分ける。
            #[cfg(unix)]
            shell.process_group(0);
            let mut child = shell
                .spawn()
                .with_context(|| format!("failed to start remote worker {name}: {command}"))?;
            let stdout = child.stdout.take().context("worker stdout is not piped")?;
            let stdin = child.stdin.take().context("worker stdin is not piped")?;
            spawn_reader(owner, name.clone(), stdout, tx.clone());
            let outbox = spawn_writer(owner, stdin, tx.clone())?;
            eprintln!("remote worker {name}: {command} (stderr: {})", log_path.display());
            fleet.workers.push(RemoteWorker {
                name,
                child: Some(child),
                outbox: Some(outbox),
                state: WorkerState::Starting,
                host: String::new(),
                slots: 0,
                clock: None,
                in_flight: 0,
                last_seen: Instant::now(),
            });
        }
        drop(tx);

        let expected = portable_fingerprint(setup.fingerprint);
        let init = to_line(&ToWorker::Init {
            params: setup
                .params
                .iter()
                .map(|p| WireParam {
                    name: p.name.clone(),
                    is_int: p.is_int,
                })
                .collect(),
            application_mask: setup.application_mask.to_vec(),
            theta0: setup.theta0,
            nps_bench: cli.nps_scaling,
            timeout_sec,
        })?;
        let mut engine_shas = vec![setup.fingerprint.engine_sha256.clone()];
        engine_shas.extend(cli.allow_worker_engine_sha256.iter().map(|sha| sha.to_lowercase()));
        let reference = reference_nps(cli);
        let min_ratio =
            cli.min_nps_ratio.unwrap_or(tools::selfplay::nps_bench::DEFAULT_MIN_NPS_RATIO);

        while fleet
            .workers
            .iter()
            .any(|w| matches!(w.state, WorkerState::Starting | WorkerState::Initializing))
        {
            let Received { owner, at, event } = match fleet.events.recv_timeout(POLL_INTERVAL) {
                Ok(received) => received,
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    fleet.expire();
                    continue;
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    for owner in 0..fleet.workers.len() {
                        fleet.lose(owner, "worker output closed");
                    }
                    break;
                }
            };
            let worker = &mut fleet.workers[owner];
            if worker.state == WorkerState::Lost {
                continue;
            }
            worker.last_seen = at;
            let message = match event {
                Event::Message(message) => *message,
                Event::Closed(reason) => {
                    fleet.lose(owner, &reason);
                    continue;
                }
            };
            match (worker.state, message) {
                (_, FromWorker::Heartbeat) => {}
                (
                    WorkerState::Starting,
                    FromWorker::Hello {
                        protocol,
                        host,
                        slots,
                        fingerprint,
                        spsa_sha256,
                    },
                ) => {
                    let checked = (|| -> Result<()> {
                        if protocol != PROTOCOL_VERSION {
                            bail!("protocol {protocol} (coordinator {PROTOCOL_VERSION})");
                        }
                        if !engine_shas.contains(&fingerprint.engine_sha256) {
                            bail!(
                                "engine sha256 {} は coordinator の engine とも \
                                 --allow-worker-engine-sha256 とも一致しません",
                                fingerprint.engine_sha256
                            );
                        }
                        let diff =
                            fingerprint_diff(&expected, &portable_fingerprint(&fingerprint))?;
                        if !diff.is_empty() {
                            bail!("対局条件が coordinator と違います: {}", diff.join("; "));
                        }
                        if slots == 0 {
                            bail!("worker の slot 数が 0 です");
                        }
                        Ok(())
                    })();
                    match checked {
                        Ok(()) => {
                            worker.host = host;
                            worker.slots = slots;
                            worker.state = WorkerState::Initializing;
                            fleet.joined.push(Joined {
                                owner,
                                name: worker.name.clone(),
                                host: worker.host.clone(),
                                slots,
                                engine_sha256: fingerprint.engine_sha256,
                                spsa_sha256,
                                bench: None,
                            });
                            fleet.write(owner, init.clone());
                        }
                        Err(error) => fleet.lose(owner, &format!("{error:#}")),
                    }
                }
                (WorkerState::Initializing, FromWorker::Ready { bench }) => {
                    let bench = bench.map(|b| nps_bench::BenchResult {
                        slot_nps: b.slot_nps,
                        nps: b.nps,
                        depth: b.depth,
                        warmup_depth: b.warmup_depth,
                        positions: b.positions,
                    });
                    let checked = match (&bench, cli.nps_scaling) {
                        (Some(bench), true) => bench.ensure_above_floor(reference, min_ratio),
                        (None, true) => Err(anyhow::anyhow!("--nps-scaling の bench が届きません")),
                        _ => Ok(()),
                    };
                    match checked {
                        Ok(()) => {
                            if let Some(bench) = &bench {
                                eprintln!(
                                    "worker {} ({}): {}",
                                    worker.name,
                                    worker.host,
                                    bench.summary()
                                );
                            }
                            worker.state = WorkerState::Ready;
                            if let Some(joined) =
                                fleet.joined.iter_mut().find(|joined| joined.owner == owner)
                            {
                                joined.bench = bench;
                            }
                        }
                        Err(error) => fleet.lose(owner, &format!("{error:#}")),
                    }
                }
                (_, FromWorker::Fatal { message }) => fleet.lose(owner, &message),
                (state, _) => fleet.lose(owner, &format!("unexpected message in state {state:?}")),
            }
            fleet.expire();
        }
        fleet
            .joined
            .retain(|joined| fleet.workers[joined.owner].state == WorkerState::Ready);
        // hello の到着順ではなく `--remote-worker` の順に記録する (較正の index を起動順に揃える)。
        fleet.joined.sort_by_key(|joined| joined.owner);
        if fleet.joined.is_empty() {
            bail!(
                "対局を始められる remote worker がありません (各 worker の stderr は {})",
                log_dir.display()
            );
        }
        Ok(fleet)
    }

    pub(super) fn total_slots(&self) -> usize {
        self.workers
            .iter()
            .filter(|w| w.state == WorkerState::Ready)
            .map(|w| w.slots)
            .sum()
    }

    pub(super) fn take_joined(&mut self) -> Vec<Joined> {
        std::mem::take(&mut self.joined)
    }

    pub(super) fn set_clock(&mut self, owner: usize, clock: PairClock) {
        self.workers[owner].clock = Some(clock);
    }

    /// 1 行を書き込み thread に渡す。thread が終わっていればその worker を切る。
    fn write(&mut self, owner: usize, line: Vec<u8>) {
        let sent = self.workers[owner]
            .outbox
            .as_ref()
            .is_some_and(|outbox| outbox.send(line).is_ok());
        if !sent {
            self.lose(owner, "worker input is closed");
        }
    }

    /// worker を切り、その worker の実行中の pair を失敗として返す。
    fn lose(&mut self, owner: usize, reason: &str) {
        let worker = &mut self.workers[owner];
        if worker.state == WorkerState::Lost {
            return;
        }
        eprintln!("remote worker {} ({}) lost: {reason}", worker.name, worker.host);
        worker.state = WorkerState::Lost;
        worker.in_flight = 0;
        worker.outbox = None;
        if let Some(child) = worker.child.take() {
            kill_group(child);
        }
        let reason = format!("remote worker {} lost: {reason}", worker.name);
        let mut lost: Vec<(u32, u32)> = self
            .assigned
            .iter()
            .filter(|(_, assigned)| **assigned == owner)
            .map(|(key, _)| *key)
            .collect();
        lost.sort_unstable();
        for (pair_id, attempt) in lost {
            self.assigned.remove(&(pair_id, attempt));
            self.pending.push_back(Ok(WorkerMessage::Failed {
                pair_id,
                attempt,
                reason: reason.clone(),
            }));
        }
    }

    fn expire(&mut self) {
        for owner in 0..self.workers.len() {
            let worker = &self.workers[owner];
            if worker.state != WorkerState::Lost && worker.last_seen.elapsed() > self.timeout {
                let reason = format!("no message for {} s", self.timeout.as_secs());
                self.lose(owner, &reason);
            }
        }
    }

    /// 対局中の event を処理し、coordinator へ渡す結果があれば返す。
    fn handle(&mut self, received: Received) -> Option<WorkerMessage> {
        let Received { owner, at, event } = received;
        let worker = &mut self.workers[owner];
        if worker.state == WorkerState::Lost {
            return None;
        }
        worker.last_seen = at;
        let message = match event {
            Event::Message(message) => *message,
            Event::Closed(reason) => {
                self.lose(owner, &reason);
                return None;
            }
        };
        let (pair_id, attempt, message) = match message {
            FromWorker::Heartbeat => return None,
            FromWorker::Done {
                pair_id,
                attempt,
                games,
                details,
                retries,
            } => {
                let message = WorkerMessage::Done {
                    pair_id,
                    attempt,
                    games: *games,
                    details,
                    retries,
                };
                (pair_id, attempt, message)
            }
            FromWorker::Failed {
                pair_id,
                attempt,
                reason,
            } => {
                let reason = format!("{} ({}): {reason}", worker.name, worker.host);
                (
                    pair_id,
                    attempt,
                    WorkerMessage::Failed {
                        pair_id,
                        attempt,
                        reason,
                    },
                )
            }
            FromWorker::Fatal { message } => {
                self.lose(owner, &message);
                return None;
            }
            FromWorker::Hello { .. } | FromWorker::Ready { .. } => {
                self.lose(owner, "unexpected handshake message while playing");
                return None;
            }
        };
        if self.assigned.get(&(pair_id, attempt)) == Some(&owner) {
            self.assigned.remove(&(pair_id, attempt));
            self.workers[owner].in_flight -= 1;
        }
        Some(message)
    }
}

impl Executor for RemoteFleet {
    fn free_slot(&self) -> Option<Slot> {
        self.workers
            .iter()
            .enumerate()
            .filter(|(_, w)| w.state == WorkerState::Ready && w.in_flight < w.slots)
            .max_by_key(|(owner, w)| (w.slots - w.in_flight, std::cmp::Reverse(*owner)))
            .map(|(owner, w)| Slot {
                owner,
                clock: w.clock,
            })
    }

    fn send(&mut self, owner: usize, task: PairTask) -> Result<()> {
        let line = to_line(&ToWorker::Task(WireTask {
            pair_id: task.pair_id,
            attempt: task.attempt,
            start_pos_index: task.start_pos_index,
            plus: task.plus,
            minus: task.minus,
            tc: WireClock {
                black_time: task.tc.black_time,
                white_time: task.tc.white_time,
                black_inc: task.tc.black_inc,
                white_inc: task.tc.white_inc,
                byoyomi: task.tc.byoyomi,
            },
            game_time_limit_ms: task.game_time_limit_ms,
        }))?;
        self.assigned.insert((task.pair_id, task.attempt), owner);
        self.workers[owner].in_flight += 1;
        self.write(owner, line);
        Ok(())
    }

    fn recv(&mut self) -> Result<Result<WorkerMessage>> {
        loop {
            if let Some(message) = self.try_recv() {
                return Ok(message);
            }
            if let Some(reason) = self.idle_reason()
                && self.assigned.is_empty()
            {
                bail!(reason);
            }
            match self.events.recv_timeout(POLL_INTERVAL) {
                Ok(received) => {
                    if let Some(message) = self.handle(received) {
                        return Ok(Ok(message));
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                    for owner in 0..self.workers.len() {
                        self.lose(owner, "worker output closed");
                    }
                }
            }
        }
    }

    fn try_recv(&mut self) -> Option<Result<WorkerMessage>> {
        // 届いている event を先に処理してから期限を見る (処理の遅れを切断と取り違えない)。
        let mut drained = false;
        loop {
            if let Some(message) = self.pending.pop_front() {
                return Some(message);
            }
            match self.events.try_recv() {
                Ok(received) => {
                    if let Some(message) = self.handle(received) {
                        return Some(Ok(message));
                    }
                }
                Err(_) if drained => return None,
                Err(_) => {
                    drained = true;
                    self.expire();
                }
            }
        }
    }

    fn idle_reason(&self) -> Option<String> {
        self.workers
            .iter()
            .all(|w| w.state == WorkerState::Lost)
            .then(|| "all remote workers are lost".into())
    }
}

impl Drop for RemoteFleet {
    fn drop(&mut self) {
        for worker in &mut self.workers {
            worker.outbox = None;
        }
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        for worker in &mut self.workers {
            if let Some(child) = worker.child.take() {
                while Instant::now() < deadline && !leader_exited(&child) {
                    std::thread::sleep(Duration::from_millis(50));
                }
                kill_group(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprint() -> RunFingerprint {
        RunFingerprint {
            engine_sha256: "e".into(),
            engine_args: Vec::new(),
            usi_options: vec!["EvalFile=/a/nn.bin".into(), "Hash=16".into()],
            usi_option_files: vec![("EvalFile".into(), "abc".into())],
            engine_param_mapping_sha256: None,
            startpos_sha256: Some("s".into()),
            sfen: None,
            random_startpos: true,
            nodes: None,
            byoyomi: 0,
            btime: Some(10_000),
            binc: 100,
            threads: 1,
            hash_mb: 16,
            max_moves: 320,
            nodes_timeout_ms: 1,
            timeout_margin_ms: 1,
            adjudicate_agreement: None,
            nps_scaling: None,
        }
    }

    #[test]
    fn option_file_paths_and_engine_build_do_not_affect_the_comparison() {
        let coordinator = fingerprint();
        let mut worker = fingerprint();
        worker.usi_options[0] = "EvalFile = /b/nn.bin".into();
        worker.engine_sha256 = "other".into();
        let diff =
            fingerprint_diff(&portable_fingerprint(&coordinator), &portable_fingerprint(&worker))
                .unwrap();
        assert!(diff.is_empty(), "{diff:?}");

        worker.usi_option_files[0].1 = "def".into();
        worker.threads = 2;
        let diff =
            fingerprint_diff(&portable_fingerprint(&coordinator), &portable_fingerprint(&worker))
                .unwrap();
        let keys: Vec<&str> = diff.iter().map(|d| d.split(':').next().unwrap()).collect();
        assert_eq!(keys, ["threads", "usi_option_files", "usi_options"]);
    }

    /// 書き込みが詰まった stdout を模す (書き込みが永久に戻らない)。
    struct StuckWriter;

    impl Write for StuckWriter {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            std::thread::park();
            std::thread::sleep(Duration::from_secs(3600));
            Ok(0)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn stuck_stdout_does_not_block_the_sender() {
        let out = Output::spawn(StuckWriter);
        let start = Instant::now();
        for _ in 0..1000 {
            out.send(&FromWorker::Heartbeat).unwrap();
        }
        out.finish(Duration::from_millis(100));
        assert!(start.elapsed() < Duration::from_secs(5), "{:?}", start.elapsed());
        assert!(out.send(&FromWorker::Heartbeat).is_err());
    }

    #[test]
    fn task_values_round_trip_bit_for_bit() {
        let task = ToWorker::Task(WireTask {
            pair_id: 3,
            attempt: 2,
            start_pos_index: 7,
            plus: vec![0.1 + 0.2, -1.0e-300, 12345.678901234567],
            minus: vec![f64::MIN_POSITIVE, 2.0f64.sqrt(), -0.0],
            tc: WireClock {
                black_time: 1,
                white_time: 2,
                black_inc: 3,
                white_inc: 4,
                byoyomi: 5,
            },
            game_time_limit_ms: Some(9),
        });
        let line = to_line(&task).unwrap();
        let back: ToWorker = serde_json::from_slice(&line).unwrap();
        let (ToWorker::Task(a), ToWorker::Task(b)) = (&task, &back) else {
            panic!("not a task");
        };
        let bits = |v: &[f64]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&a.plus), bits(&b.plus));
        assert_eq!(bits(&a.minus), bits(&b.minus));
        assert_eq!(task, back);
    }
}
