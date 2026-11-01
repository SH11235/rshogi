use super::ServeArgs;
use super::journal::{Journal, LockedFile, Pv, Reading, Readings, Settings};
use super::model::{PrefetchCandidate, Tree, remove_candidates};
use super::targets::{
    CHECKPOINT, Checkpoint, Defaults, OUT_ROOT_LOCK, QUEUE, QUEUE_LOCK, RUN_DIR, STATUS_FILE,
    StatusFile, Target, TargetStatus,
};
use crate::common::io::{write_atomic, write_atomic_durable};
use crate::selfplay::{EngineConfig, EngineProcess};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct Collector {
    last: BTreeMap<usize, Pv>,
    nodes: Option<u64>,
}
impl Collector {
    pub fn info(&mut self, line: &str) -> Result<()> {
        if !line.starts_with("info") || !line.contains(" pv ") || !line.contains(" score ") {
            return Ok(());
        }
        let tokens: Vec<_> = line.split_whitespace().collect();
        if tokens.contains(&"lowerbound") || tokens.contains(&"upperbound") {
            return Ok(());
        }
        let get =
            |name| tokens.iter().position(|t| *t == name).and_then(|i| tokens.get(i + 1)).copied();
        let score = tokens.iter().position(|t| *t == "score").context("score がありません")?;
        let kind = tokens.get(score + 1).context("score 種別がありません")?;
        let n: i32 = tokens.get(score + 2).context("score 値がありません")?.parse()?;
        let cp = if *kind == "cp" {
            n
        } else {
            (30000 - n.checked_abs().context("mate 値が範囲外")?) * if n > 0 { 1 } else { -1 }
        };
        let index: usize = get("multipv").unwrap_or("1").parse()?;
        if get("pv") == Some("win") && index != 1 {
            bail!("宣言勝ちの multipv は 1 が必要です");
        }
        if index == 0 {
            bail!("multipv は正数が必要です");
        }
        self.last.insert(
            index,
            Pv {
                mv: get("pv").context("pv がありません")?.to_owned(),
                cp,
                depth: get("depth").context("depth がありません")?.parse()?,
            },
        );
        if let Some(nodes) = get("nodes") {
            self.nodes = Some(nodes.parse()?);
        }
        Ok(())
    }
    pub fn finish(self) -> (Vec<Pv>, Option<u64>) {
        (self.last.into_values().collect(), self.nodes)
    }
}

pub(super) struct Demand<'a> {
    pub priority: u32,
    pub order: u64,
    pub missing: Vec<(String, usize)>,
    pub ply: &'a BTreeMap<String, u32>,
}

/// 局面ごとに、それを求める木の (優先度, 受付順, 段) の最小を取る。
pub(super) fn reading_queue<'a>(
    demands: impl IntoIterator<Item = Demand<'a>>,
    running: &BTreeSet<String>,
) -> Vec<(String, u32)> {
    let mut queued: BTreeMap<String, (u32, u64, usize, u32)> = BTreeMap::new();
    for demand in demands {
        for (key, depth) in demand.missing {
            if running.contains(&key) {
                continue;
            }
            let rank = (demand.priority, demand.order, depth, demand.ply[&key]);
            queued
                .entry(key)
                .and_modify(|r| {
                    // 同順位の手数は、先に走査した木のものを保つ。
                    if (rank.0, rank.1, rank.2) < (r.0, r.1, r.2) {
                        *r = rank;
                    }
                })
                .or_insert(rank);
        }
    }
    let mut queue: Vec<_> = queued.into_iter().collect();
    queue.sort_by(|(ka, (pa, oa, da, _)), (kb, (pb, ob, db, _))| {
        (pa, oa, da, ka).cmp(&(pb, ob, db, kb))
    });
    queue.into_iter().map(|(key, (_, _, _, ply))| (key, ply)).collect()
}

struct Job {
    key: String,
    ply: u32,
}
enum Event {
    Done(usize, String, bool, Option<StopReason>, Result<Outcome>, Option<Box<Trace>>),
    Info(usize, u64, u32),
    Alert(String),
}
struct Worker {
    tx: mpsc::Sender<Option<Job>>,
    handle: JoinHandle<()>,
    restart: Arc<Mutex<Restart>>,
    busy: Option<(String, Instant)>,
    nodes: u64,
    depth: u32,
    prefetch: bool,
    demand: BTreeSet<Request>,
    priority: u32,
}

/// 1 回の探索で記録する info 行の上限。bestmove を返さず info を出し続ける engine で
/// メモリが尽きないようにする (通常の読みは数十 KB)。
const TRACE_LIMIT_BYTES: usize = 4 << 20;

/// 読み 1 回の探索の途中経過。`--trace-dir` のときだけ集める。
struct Trace {
    started: chrono::DateTime<chrono::Utc>,
    finished: chrono::DateTime<chrono::Utc>,
    lines: Vec<String>,
}

/// 1 回の探索の info 行。上限を超えたらその探索の分を捨て、以後は集めない。
struct TraceBuffer {
    lines: Vec<String>,
    bytes: usize,
    limit: usize,
    overflowed: bool,
}
impl TraceBuffer {
    fn new(limit: usize) -> Self {
        Self {
            lines: Vec::new(),
            bytes: 0,
            limit,
            overflowed: false,
        }
    }
    /// 上限を超えたその行でだけ真を返す。
    fn push(&mut self, line: &str) -> bool {
        if self.overflowed {
            return false;
        }
        self.bytes += line.len();
        if self.bytes > self.limit {
            self.overflowed = true;
            self.lines = Vec::new();
            return true;
        }
        self.lines.push(line.to_owned());
        false
    }
}

/// 起動ごとの探索の記録。1 行 1 読みで追記する。
struct TraceLog {
    dir: PathBuf,
    path: PathBuf,
    file: Option<std::fs::File>,
    /// 書きかけで失敗した行がありうる。次の行を改行から始め、壊れた行に繋げない。
    torn: bool,
}
impl TraceLog {
    fn append(&mut self, record: &serde_json::Value) -> Result<()> {
        use std::io::Write;
        let mut text = if self.torn {
            "\n".to_owned()
        } else {
            String::new()
        };
        text.push_str(&serde_json::to_string(record)?);
        text.push('\n');
        let file = match &mut self.file {
            Some(file) => file,
            None => {
                std::fs::create_dir_all(&self.dir)?;
                self.file
                    .insert(std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?)
            }
        };
        if let Err(error) = file.write_all(text.as_bytes()) {
            self.torn = true;
            self.file = None;
            return Err(error.into());
        }
        self.torn = false;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StopReason {
    Preempt,
    NoDemand,
    Stop,
    Workers,
    Restart,
}

enum Outcome {
    Completed(Reading),
    Stopped(StopReason, f64, f64),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Request {
    id: String,
    own: String,
    generation: u32,
    predicted: bool,
}

#[derive(Default, Serialize, Deserialize)]
struct PrefetchStats {
    used: BTreeSet<String>,
    stopped: BTreeMap<StopReason, usize>,
    discarded: f64,
}
impl PrefetchStats {
    fn load(path: &Path) -> Result<Self> {
        super::paths::reject_symlink(path)?;
        match std::fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error.into()),
        }
    }
    fn save(&self, path: &Path) -> Result<()> {
        write_atomic_durable(path, &serde_json::to_string_pretty(self)?)
    }
    fn mark(&mut self, tree: &Tree, prefetched: &BTreeSet<String>) -> bool {
        let previous = self.used.len();
        self.used
            .extend(prefetched.iter().filter(|key| tree.level.contains_key(*key)).cloned());
        self.used.len() != previous
    }
}

fn median(durations: &VecDeque<f64>) -> Option<f64> {
    if durations.len() < 10 {
        return None;
    }
    let mut sorted: Vec<_> = durations.iter().copied().collect();
    sorted.sort_by(f64::total_cmp);
    let mid = sorted.len() / 2;
    let value = if sorted.len() % 2 == 0 {
        (sorted[mid - 1] + sorted[mid]) / 2.0
    } else {
        sorted[mid]
    };
    (value > 0.0).then_some(value)
}

fn select_victims(
    mut candidates: Vec<(usize, f64, u32)>,
    needed: usize,
    threshold: f64,
) -> Vec<usize> {
    candidates.retain(|(_, progress, _)| *progress < threshold);
    candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.2.cmp(&a.2)).then(a.0.cmp(&b.0)));
    candidates.into_iter().take(needed).map(|(id, _, _)| id).collect()
}

#[derive(Default)]
struct Restart {
    key: Option<String>,
    requested: bool,
    stop: Option<StopReason>,
    sent: Option<Instant>,
    go: Option<Instant>,
}
impl Restart {
    fn progress(&self, expected: f64, nodes: u64, limit: u64) -> Option<f64> {
        Some((self.go?.elapsed().as_secs_f64() / expected).max(nodes as f64 / limit as f64))
    }
    fn stop(&mut self, reason: StopReason) {
        if self.key.is_some() && self.stop.is_none() && !self.requested {
            self.stop = Some(reason);
        }
    }
    fn yielding(&self, normal_demand: bool) -> bool {
        self.stop.is_some() && !normal_demand
    }
    fn promote(&mut self) {
        if self.sent.is_none() {
            self.stop = None;
        }
    }
    fn completed(&mut self, prefetch_enabled: bool) -> Result<Option<StopReason>> {
        if !prefetch_enabled {
            return Ok(None);
        }
        if self.requested {
            bail!("制御ファイルによる engine 再起動");
        }
        self.key = None;
        Ok(self.sent.and(self.stop))
    }
    fn request(&mut self) -> bool {
        if self.key.is_none() {
            return false;
        }
        self.requested = true;
        true
    }
    fn finish(&mut self) -> bool {
        let requested = self.requested;
        *self = Self::default();
        requested
    }
}

#[derive(Debug)]
struct InvalidReading(anyhow::Error);
impl std::fmt::Display for InvalidReading {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "読みの検査失敗: {:#}", self.0)
    }
}
impl std::error::Error for InvalidReading {}

/// 入力照合に成功した後のエンジン起動失敗。入力の読取り失敗は再試行せず停止する。
/// 局面の失敗に数えず、次の周回で起動し直す。
#[derive(Debug)]
struct SpawnFailed(anyhow::Error);
impl std::fmt::Display for SpawnFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "engine を起動できません: {:#}", self.0)
    }
}
impl std::error::Error for SpawnFailed {}

#[derive(Debug)]
struct InputsChanged(anyhow::Error);
impl std::fmt::Display for InputsChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "入力の指紋を確認できません。serve を停止します: {:#}", self.0)
    }
}
impl std::error::Error for InputsChanged {}

#[derive(Debug, PartialEq, Eq)]
enum FailureKind {
    InputsChanged,
    Restart,
    Spawn,
    Prefetch,
    InvalidReading,
    Retry,
}

fn failure_kind(
    error: &anyhow::Error,
    explicit: bool,
    prefetch: bool,
    reason: Option<StopReason>,
) -> FailureKind {
    if error.is::<InputsChanged>() {
        FailureKind::InputsChanged
    } else if explicit {
        FailureKind::Restart
    } else if error.is::<SpawnFailed>() {
        FailureKind::Spawn
    } else if prefetch || reason.is_some() {
        FailureKind::Prefetch
    } else if error.is::<InvalidReading>() {
        FailureKind::InvalidReading
    } else {
        FailureKind::Retry
    }
}

fn verify_inputs(args: &super::ReadingArgs, expected: &Settings) -> Result<()> {
    let actual = Settings::new(args).map_err(InputsChanged)?;
    if !expected.same_condition(&actual) {
        return Err(InputsChanged(anyhow::anyhow!(
            "engine/model/parameter が開始時の条件と一致しません"
        ))
        .into());
    }
    Ok(())
}

fn spawn_worker(
    id: usize,
    args: &ServeArgs,
    settings: &Settings,
    events: mpsc::Sender<Event>,
) -> Worker {
    let (tx, rx) = mpsc::channel::<Option<Job>>();
    let settings = settings.clone();
    let reading_args = args.reading.clone();
    let stall = Duration::from_secs(args.stall_secs);
    let restart = Arc::new(Mutex::new(Restart::default()));
    let restart_worker = restart.clone();
    let tracing = args.trace_dir.is_some();
    let prefetch_enabled = args.prefetch;
    let handle = thread::spawn(move || {
        let cfg = EngineConfig {
            path: reading_args.engine.clone(),
            args: Vec::new(),
            threads: 1,
            hash_mb: settings.hash,
            network_delay: None,
            network_delay2: None,
            minimum_thinking_time: None,
            slowmover: None,
            ponder: false,
            usi_options: settings.options.iter().map(|(k, v)| format!("{k}={v}")).collect(),
        };
        let mut engine = None;
        while let Ok(Some(job)) = rx.recv() {
            let mut trace = None;
            let result = (|| -> Result<Outcome> {
                let starting = engine.is_none();
                if starting {
                    verify_inputs(&reading_args, &settings)?;
                    let process = EngineProcess::spawn(&cfg, format!("tree-{id}"));
                    // 初期化が失敗しても入力変更を局面の再試行に紛れ込ませない。
                    verify_inputs(&reading_args, &settings)?;
                    let process = process.map_err(SpawnFailed)?;
                    for name in settings.options.keys() {
                        if !process.advertises_option(name) {
                            bail!("engine が option を広告していません: {name}");
                        }
                    }
                    engine = Some(process);
                }
                let engine = engine.as_mut().context("engine がありません")?;
                // stop 後の遅れた行は new_game の readyok 同期で読み捨てる。
                let initialized = engine.new_game();
                if starting {
                    verify_inputs(&reading_args, &settings)?;
                }
                initialized?;
                engine.write_line(&format!("position sfen {} {}", job.key, job.ply))?;
                engine.write_line(&format!("go nodes {}", settings.nodes))?;
                let started_at = chrono::Utc::now();
                let mut buffer = tracing.then(|| TraceBuffer::new(TRACE_LIMIT_BYTES));
                let started = Instant::now();
                restart_worker.lock().unwrap_or_else(|e| e.into_inner()).go = Some(started);
                let mut progressed = started;
                let mut high_nodes = 0;
                let mut high_depth = 0;
                let mut alerted = false;
                let mut collector = Collector::default();
                let mut drained_before_stop = 0;
                loop {
                    let line = {
                        let mut control = restart_worker.lock().unwrap_or_else(|e| e.into_inner());
                        if control.requested {
                            bail!("制御ファイルによる engine 再起動");
                        }
                        if control.sent.is_some_and(|at| at.elapsed() >= Duration::from_secs(10)) {
                            bail!("stop の応答期限を超えました");
                        }
                        // 受信済み bestmove と stop の順序を、取り消しと同じロック内で確定する。
                        let pending = engine.poll_line(Duration::ZERO)?;
                        if control.stop.is_some() && control.sent.is_none() {
                            // info が途切れなくても stop の送信と応答期限を始める。
                            drained_before_stop += 1;
                            if !pending.as_ref().is_some_and(|line| line.starts_with("bestmove "))
                                && (pending.is_none() || drained_before_stop >= 256)
                            {
                                control.sent = Some(Instant::now());
                                engine.write_line("stop")?;
                            }
                        } else {
                            drained_before_stop = 0;
                        }
                        pending
                    };
                    if !alerted && progressed.elapsed() >= stall {
                        let _ = events.send(Event::Alert(format!(
                            "STALL worker={id} key={} nodes={high_nodes} depth={high_depth}",
                            job.key
                        )));
                        alerted = true;
                    }
                    let line = match line {
                        Some(line) => line,
                        None => match engine.poll_line({
                            let control = restart_worker.lock().unwrap_or_else(|e| e.into_inner());
                            control.sent.map_or(Duration::from_millis(100), |at| {
                                Duration::from_secs(10)
                                    .saturating_sub(at.elapsed())
                                    .min(Duration::from_millis(100))
                            })
                        })? {
                            Some(line) => line,
                            None => continue,
                        },
                    };
                    if let Some(rest) = line.strip_prefix("bestmove ") {
                        let mut control = restart_worker.lock().unwrap_or_else(|e| e.into_inner());
                        if control.sent.is_some_and(|at| at.elapsed() >= Duration::from_secs(10)) {
                            bail!("stop の応答期限を超えました");
                        }
                        if let Some(reason) = control.completed(prefetch_enabled)? {
                            return Ok(Outcome::Stopped(
                                reason,
                                high_nodes as f64 / settings.nodes as f64,
                                started.elapsed().as_secs_f64(),
                            ));
                        }
                        drop(control);
                        let bestmove = rest
                            .split_whitespace()
                            .next()
                            .context("bestmove がありません")?
                            .to_owned();
                        let (multipv, nodes) = collector.finish();
                        let reading = Reading {
                            tag: "serve".into(),
                            ply: job.ply,
                            key: job.key.clone(),
                            nodes_limit: settings.nodes,
                            nodes,
                            sec: started.elapsed().as_secs_f64(),
                            threads: 1,
                            hash: settings.hash,
                            draw: settings.draw(),
                            multipv,
                            bestmove,
                        };
                        reading.validate(&settings).map_err(InvalidReading)?;
                        if let Some(buffer) = buffer.take()
                            && !buffer.overflowed
                        {
                            trace = Some(Box::new(Trace {
                                started: started_at,
                                finished: chrono::Utc::now(),
                                lines: buffer.lines,
                            }));
                        }
                        return Ok(Outcome::Completed(reading));
                    }
                    if line.starts_with("info ") && !line.starts_with("info string ") {
                        let tokens: Vec<_> = line.split_whitespace().collect();
                        let number = |name| {
                            tokens
                                .iter()
                                .position(|t| *t == name)
                                .and_then(|i| tokens.get(i + 1))
                                .and_then(|s| s.parse::<u64>().ok())
                                .unwrap_or(0)
                        };
                        let nodes = number("nodes");
                        let depth = u32::try_from(number("depth")).unwrap_or(u32::MAX);
                        if nodes > high_nodes || depth > high_depth {
                            high_nodes = high_nodes.max(nodes);
                            high_depth = high_depth.max(depth);
                            progressed = Instant::now();
                            alerted = false;
                            let _ = events.send(Event::Info(id, high_nodes, high_depth));
                        }
                    }
                    // Collector と同じ接頭辞で集め、記録から台帳の読みを再現できるようにする。
                    if line.starts_with("info")
                        && let Some(buffer) = buffer.as_mut()
                        && buffer.push(&line)
                    {
                        let _ = events.send(Event::Alert(format!(
                            "探索の記録が {TRACE_LIMIT_BYTES} バイトを超えたのでこの読みの記録を捨てます worker={id} key={}",
                            job.key
                        )));
                    }
                    if restart_worker.lock().unwrap_or_else(|e| e.into_inner()).sent.is_none() {
                        collector.info(&line).map_err(InvalidReading)?;
                    }
                }
            })();
            if result.is_err() {
                engine = None;
            }
            let (explicit, reason) = {
                let mut control = restart_worker.lock().unwrap_or_else(|error| error.into_inner());
                let reason = if control.requested {
                    Some(StopReason::Restart)
                } else {
                    control.sent.and(control.stop)
                };
                (control.finish(), reason)
            };
            if events.send(Event::Done(id, job.key, explicit, reason, result, trace)).is_err() {
                break;
            }
        }
    });
    Worker {
        tx,
        handle,
        restart,
        busy: None,
        nodes: 0,
        depth: 0,
        prefetch: false,
        demand: BTreeSet::new(),
        priority: 0,
    }
}

/// 受け付けた対象 1 本の実行時の状態。チェックポイントの `targets` と同じ順に並ぶ。
struct Live {
    tree: Tree,
    prepared: bool,
    superseded: bool,
    /// 終わった木の (読んだ局面数, 要る局面数)。終わった木は中身を縮めて保持する。
    ended: Option<(usize, usize)>,
    shadow: Vec<PrefetchCandidate>,
    dirty: bool,
    requests_dirty: bool,
    requests_active: bool,
}
impl Live {
    fn new(tree: Tree) -> Self {
        Self {
            tree,
            prepared: false,
            superseded: false,
            ended: None,
            shadow: Vec::new(),
            dirty: true,
            requests_dirty: true,
            requests_active: false,
        }
    }

    fn refresh_requests(
        &mut self,
        target: &Target,
        journal: &impl Readings,
        failures: &BTreeMap<String, String>,
        requests: &mut BTreeMap<String, BTreeSet<Request>>,
    ) {
        let active = !target.hold && !self.tree.finished && !self.tree.failed && self.prepared;
        if self.requests_dirty || self.requests_active != active {
            requests.retain(|_, demand| {
                demand.retain(|r| r.id != target.id || r.own != target.own);
                !demand.is_empty()
            });
            if active {
                for (key, predicted) in self
                    .tree
                    .waiting()
                    .iter()
                    .map(|(key, _)| (key, false))
                    .chain(self.shadow.iter().map(|c| (&c.key, true)))
                {
                    if journal.get(key).is_none() && !failures.contains_key(key) {
                        requests.entry(key.clone()).or_default().insert(Request {
                            id: target.id.clone(),
                            own: target.own.clone(),
                            generation: target.generation,
                            predicted,
                        });
                    }
                }
            }
            self.requests_dirty = false;
            self.requests_active = active;
        }
    }
}

fn rebuild_shadows(
    lives: &mut [Live],
    targets: &[Target],
    cache: &impl Readings,
    work_limit: usize,
    cursor: &mut usize,
) -> Result<()> {
    let mut budget = work_limit;
    let mut rebuilt = 0;
    let start = *cursor;
    for offset in 0..lives.len() {
        let index = (start + offset) % lives.len();
        let live = &mut lives[index];
        let target = &targets[index];
        if !live.dirty || !live.prepared || target.hold || live.tree.finished || live.tree.failed {
            continue;
        }
        if rebuilt == 4 || budget < work_limit || budget == 0 {
            *cursor = index;
            break;
        }
        let report = live.tree.prefetch_report_borrowed(
            target.definition.borrowed_lines(),
            cache,
            false,
            work_limit,
        )?;
        budget = budget.saturating_sub(report.work);
        if !report.limited {
            live.shadow = report.candidates;
            live.requests_dirty = true;
        }
        live.dirty = report.limited;
        rebuilt += 1;
        *cursor = (index + 1) % lives.len();
    }
    Ok(())
}

fn generation_dir(checkpoint: &Checkpoint, target: &Target, generation: u32) -> PathBuf {
    checkpoint
        .out_root
        .join(&target.id)
        .join(&target.own)
        .join(format!("g{generation}"))
}

/// チェックポイントから今の世代の出力の副作用 (dir・記録・QUEUED) を行う。
/// どれも冪等で、失敗したら次の周回でやり直す。
fn prepare(live: &mut Live, target: &Target) -> Result<()> {
    super::paths::artifacts(&target.out)?;
    std::fs::create_dir_all(&target.out)?;
    // serve.lock より前に始まった replay は tree.lock を持っている。終わるまで用意しない
    // (serve.lock を作った後に始まる replay は serve.lock で止まる)。
    let _replay = LockedFile::open(&target.out.join("tree.lock"))?;
    remove_candidates(&target.out)?;
    write_atomic(&target.out.join("paths.record.tsv"), &target.definition.record())?;
    monitor(&target.out.join("STATUS"), if target.hold { "HELD\n" } else { "QUEUED\n" });
    live.prepared = true;
    Ok(())
}

/// 前の世代の dir を SUPERSEDED にする。消された dir は作り直さない。
/// 今の世代の準備とは別にやり直すので、前の世代の失敗で今の世代が止まらない。
fn supersede(checkpoint: &Checkpoint, target: &Target) -> Result<()> {
    for generation in 1..target.generation {
        let dir = generation_dir(checkpoint, target, generation);
        if !dir.try_exists()? {
            continue;
        }
        super::paths::artifacts(&dir)?;
        let _replay = LockedFile::open(&dir.join("tree.lock"))?;
        if std::fs::read_to_string(dir.join("STATUS")).ok().as_deref() != Some("SUPERSEDED\n") {
            write_atomic(&dir.join("STATUS"), "SUPERSEDED\n")?;
        }
    }
    Ok(())
}

/// キューを見た結果。
enum Poll {
    /// 読んでいない (前回と同じ内容、または enqueue が書き込み中)。
    Skipped,
    /// 処理済みの範囲を確かめ、新しい行は無い。
    Clean,
    /// 新しい行と、処理済みの範囲までの hash。
    Lines(Vec<u8>, Sha256),
    /// 処理済みの範囲が変わった。内容が変わるまで読み直さない。
    Broken,
}

fn poll_queue(state: &Path, checkpoint: &Checkpoint, seen: &mut Option<String>) -> Result<Poll> {
    let path = state.join(QUEUE);
    super::paths::reject_symlink(&path)?;
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    if metadata.as_ref().is_some_and(|m| !m.file_type().is_file()) {
        bail!("キューは通常のファイルにしてください: {}", path.display());
    }
    let stamp = format!("{:?}", metadata.as_ref().map(|m| (m.len(), m.modified().ok())));
    if seen.as_ref() == Some(&stamp) {
        return Ok(Poll::Skipped);
    }
    let lock_path = state.join(QUEUE_LOCK);
    super::paths::reject_symlink(&lock_path)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    match super::file_lock::try_lock_shared(&lock) {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(Poll::Skipped),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    let bytes = match metadata {
        Some(_) => std::fs::read(&path)?,
        None => Vec::new(),
    };
    drop(lock);
    // 読めてから見た内容を覚える (読み取りの失敗は同じ内容のまま次の周回でやり直す)。
    *seen = Some(stamp);
    let processed = usize::try_from(checkpoint.processed_bytes)?;
    let mut hasher = Sha256::new();
    if bytes.len() >= processed {
        hasher.update(&bytes[..processed]);
    }
    if bytes.len() < processed
        || format!("{:x}", hasher.clone().finalize()) != checkpoint.processed_sha256
    {
        return Ok(Poll::Broken);
    }
    let end = bytes[processed..]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(processed, |i| processed + i + 1);
    if end == processed {
        return Ok(Poll::Clean);
    }
    Ok(Poll::Lines(bytes[processed..end].to_vec(), hasher))
}

pub(super) fn serve(args: ServeArgs) -> Result<()> {
    let journal_path = args.journal.clone().unwrap_or_else(|| args.state.join("journal.jsonl"));
    super::paths::reject_collisions(&journal_path, Some(&args.state), &[])?;
    std::fs::create_dir_all(args.state.join("restart"))?;
    // `tree wait` は共有 lock を一瞬取って serve の生存を確かめるので、その間は待つ。
    let mut attempt = 0;
    let state_lock = loop {
        match LockedFile::open(&args.state.join("lock")) {
            Ok(lock) => break lock,
            Err(_) if attempt < 20 => attempt += 1,
            Err(error) => return Err(error),
        }
        thread::sleep(Duration::from_millis(50));
    };
    let run_id =
        format!("{}-{}", chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"), std::process::id());
    let run_lock = hold_run_lock(&args.state, &run_id);
    let result = run_lock
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e:#}"))
        .and_then(|_| serve_locked(&args, &run_id));
    if let Err(error) = &result {
        monitor(&args.state.join("STATUS"), "FAIL\n");
        alert(&args.state, &format!("FAIL {error:#}"));
    }
    drop(run_lock);
    drop(state_lock);
    result
}

/// 起動ごとの lock を取って保持する。wait は状態のファイルの run_id の lock が取られているかで、
/// その状態を書いた serve 自身の生存を確かめる (前の起動の状態を今の serve のものと取り違えない)。
/// 状態のファイルは、この lock を取った後にしか今の run_id で書かない。
fn hold_run_lock(state: &Path, run_id: &str) -> Result<LockedFile> {
    let dir = state.join(RUN_DIR);
    super::paths::reject_symlink(&dir)?;
    std::fs::create_dir_all(&dir)?;
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        // state の lock を持っているので、残っているのは終わった起動の lock だけ。
        if std::fs::symlink_metadata(&path)?.file_type().is_file() {
            std::fs::remove_file(&path)?;
        }
    }
    LockedFile::open(&dir.join(format!("{run_id}.lock")))
}

pub(super) fn monitor(path: &std::path::Path, contents: &str) {
    if let Err(error) = write_atomic(path, contents) {
        eprintln!("監視ファイル書き込み失敗 {}: {error:#}", path.display());
    }
}

/// 内容が変わったときだけ書く。書けたか、既に同じ内容なら真。
fn monitor_changed(path: &std::path::Path, contents: &str) -> bool {
    if std::fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return true;
    }
    match write_atomic(path, contents) {
        Ok(()) => true,
        Err(error) => {
            eprintln!("監視ファイル書き込み失敗 {}: {error:#}", path.display());
            false
        }
    }
}

fn alert(state: &std::path::Path, message: &str) {
    if let Err(error) = append_alert(state, message) {
        eprintln!("ALERT 書き込み失敗: {error:#}; {message}");
    }
}

fn append_alert(state: &std::path::Path, message: &str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("ALERT"))?;
    writeln!(file, "{} {message}", chrono::Utc::now().to_rfc3339())?;
    Ok(())
}

struct Alerts<'a> {
    state: &'a std::path::Path,
    pending: VecDeque<String>,
}
impl Alerts<'_> {
    fn push(&mut self, message: &str) {
        if !self.pending.iter().any(|s| s == message) {
            self.pending.push_back(message.to_owned());
        }
    }
    fn flush(&mut self) {
        while let Some(message) = self.pending.front() {
            if let Err(error) = append_alert(self.state, message) {
                eprintln!("ALERT 書き込み失敗: {error:#}; {message}");
                break;
            }
            self.pending.pop_front();
        }
    }
}

/// 同じ内容の通知を、内容が変わるまで 1 回にする。
#[derive(Default)]
struct Once(BTreeMap<String, String>);
impl Once {
    fn report(&mut self, alerts: &mut Alerts, slot: &str, message: String) {
        if self.0.get(slot) != Some(&message) {
            alerts.push(&message);
            self.0.insert(slot.to_owned(), message);
        }
    }
    /// 文面によらず、解除されるまで 1 回だけ通知する (失敗の文面が揺れる通知に使う)。
    fn report_first(&mut self, alerts: &mut Alerts, slot: &str, message: String) {
        if !self.0.contains_key(slot) {
            alerts.push(&message);
            self.0.insert(slot.to_owned(), message);
        }
    }
    fn clear(&mut self, slot: &str) {
        self.0.remove(slot);
    }
}

fn serve_locked(args: &ServeArgs, run_id: &str) -> Result<()> {
    if args.stall_secs == 0 {
        bail!("stall-secs は正数が必要です");
    }
    if !args.prefetch_preempt_below.is_finite()
        || !(0.0..=1.0).contains(&args.prefetch_preempt_below)
    {
        bail!("prefetch-preempt-below は 0 以上 1 以下が必要です");
    }
    let defaults = Defaults {
        opp_n: args.opp_n,
        stop: args.stop,
        rounds: args.rounds,
        draw: args.draw,
    };
    defaults.check()?;
    let mut trace_log = args.trace_dir.as_ref().map(|dir| TraceLog {
        dir: dir.clone(),
        path: dir.join(format!("{run_id}.jsonl")),
        file: None,
        torn: false,
    });
    let mut settings = Settings::new(&args.reading)?;
    settings.engine_version = super::transfer::engine_source_version(&args.reading.engine);
    super::transfer::warn_unknown_version(&args.reading.engine, &settings.engine_version);
    println!("fingerprint={}", settings.fingerprint);
    let journal_path = args.journal.clone().unwrap_or_else(|| args.state.join("journal.jsonl"));
    let (out_root, protected) =
        super::paths::reject_out_root(&journal_path, &args.state, &args.out_root)?;
    if let Some(dir) = &args.trace_dir {
        super::paths::reject_trace_dir(dir, &journal_path, &args.state, &out_root)?;
    }
    std::fs::create_dir_all(&out_root)?;
    // 出力の根の下は、この serve だけが書く (別の serve と、配下の dir への replay を拒む)。
    let out_lock = out_root.join(OUT_ROOT_LOCK);
    super::paths::serve_lock_exists(&out_lock)?;
    let _out_lock =
        LockedFile::open(&out_lock).context("別の serve か replay がこの出力の根を使っています")?;
    let manifest = args.state.join(CHECKPOINT);
    let mut checkpoint = match Checkpoint::load(&manifest)? {
        Some(checkpoint) => {
            if checkpoint.out_root != out_root {
                bail!(
                    "--out-root は state に固定されています: {} (指定 {})",
                    checkpoint.out_root.display(),
                    out_root.display()
                );
            }
            if checkpoint.fingerprint != settings.fingerprint {
                bail!(
                    "受付済みの対象と読み条件が違います。条件を変えるときは新しい state dir で起動してください"
                );
            }
            checkpoint
        }
        None => Checkpoint::new(out_root, settings.fingerprint.clone()),
    };
    let status_path = args.state.join(STATUS_FILE);
    // 出力に触れる前に、前回の ready を無効にする。
    write_atomic(
        &status_path,
        &serde_json::to_string_pretty(&StatusFile::resuming(run_id, &checkpoint))?,
    )
    .context("状態のファイルを書けません")?;
    let mut journal = Journal::open(&journal_path, &settings.fingerprint)?;
    let stats_path = args.state.join("prefetch_stats.json");
    let mut stats = if args.prefetch {
        PrefetchStats::load(&stats_path)?
    } else {
        PrefetchStats::default()
    };
    let mut stats_dirty = args.prefetch;
    let mut unused_counts = (0, 0);
    let mut unused_prefetch = 0;
    let mut prefetch_failures: BTreeMap<String, usize> = BTreeMap::new();
    let mut shadow_cursor = 0usize;
    let mut requests: BTreeMap<String, BTreeSet<Request>> = BTreeMap::new();
    let mut lives = checkpoint
        .targets
        .iter()
        .map(|t| Ok(Live::new(t.definition.build(&t.own, &t.out)?)))
        .collect::<Result<Vec<_>>>()?;
    let workers_file = args.state.join("workers");
    if !workers_file.exists() {
        write_atomic(&workers_file, &args.workers.to_string())?;
    }
    let (events_tx, events_rx) = mpsc::channel();
    let mut workers: BTreeMap<usize, Worker> = BTreeMap::new();
    let mut retries: BTreeMap<String, usize> = BTreeMap::new();
    let mut workers_error = None;
    let mut failures = BTreeMap::new();
    let mut alerts = Alerts {
        state: &args.state,
        pending: VecDeque::new(),
    };
    let mut once = Once::default();
    let mut queue_seen = None;
    let mut status_written = None;
    let mut queue_error = None;
    let mut progress_written: Option<(String, Instant)> = None;
    let result = (|| -> Result<()> {
        loop {
            let stopping = args.state.join("stop").exists();
            let desired: usize = match std::fs::read_to_string(&workers_file)
                .context("workers を読めません")
                .and_then(|s| Ok(s.trim().parse()?))
            {
                Ok(n) => {
                    workers_error = None;
                    n
                }
                Err(e) => {
                    let stamp = file_stamp(&workers_file);
                    if workers_error.as_ref() != Some(&stamp) {
                        alerts.push(&format!("workers が不正です: {e}"));
                        workers_error = Some(stamp);
                    }
                    workers.len()
                }
            };
            let desired = if stopping { 0 } else { desired };
            if !stopping {
                match poll_queue(&args.state, &checkpoint, &mut queue_seen) {
                    Ok(Poll::Skipped) => {}
                    Ok(Poll::Clean) => {
                        once.clear("queue");
                        queue_error = None;
                    }
                    Ok(Poll::Lines(chunk, prefix)) => {
                        let (next, mut built) =
                            checkpoint.apply(&chunk, &defaults, &prefix, &protected);
                        match serde_json::to_string_pretty(&next)
                            .map_err(anyhow::Error::from)
                            .and_then(|text| write_atomic_durable(&manifest, &text))
                        {
                            Ok(()) => {
                                once.clear("queue");
                                queue_error = None;
                                built.sort_by_key(|(index, _)| *index);
                                for (index, tree) in built {
                                    if let Some(live) = lives.get_mut(index) {
                                        *live = Live::new(tree);
                                    } else {
                                        lives.push(Live::new(tree));
                                    }
                                }
                                for rejected in &next.rejected[checkpoint.rejected.len()..] {
                                    alerts.push(&format!(
                                        "キューの行を拒否 [{}, {}) id={}: {}",
                                        rejected.start,
                                        rejected.end,
                                        rejected.id.as_deref().unwrap_or("-"),
                                        rejected.reason
                                    ));
                                }
                                requests.retain(|_, demand| {
                                    demand.retain(|request| {
                                        next.targets.iter().any(|target| {
                                            target.id == request.id
                                                && target.own == request.own
                                                && target.generation == request.generation
                                        })
                                    });
                                    !demand.is_empty()
                                });
                                checkpoint = next;
                            }
                            Err(error) => {
                                queue_seen = None;
                                let message = format!("チェックポイントを書けません: {error:#}");
                                queue_error = Some(message.clone());
                                once.report(&mut alerts, "queue", message);
                            }
                        }
                    }
                    Ok(Poll::Broken) => {
                        let message = "キューの処理済みの範囲が変わりました。戻るまで受付を止めます (読みは続けます)".to_owned();
                        queue_error = Some(message.clone());
                        once.report(&mut alerts, "queue", message);
                    }
                    Err(error) => {
                        // 読めなかった・通常のファイルでなかった後は、内容が前と同じでも確かめ直す。
                        queue_seen = None;
                        let message = format!("{error:#}");
                        queue_error = Some(message.clone());
                        once.report(&mut alerts, "queue", message);
                    }
                }
            }
            for (index, live) in lives.iter_mut().enumerate() {
                let target = &checkpoint.targets[index];
                if !live.prepared {
                    let slot = format!("prepare-{index}");
                    match prepare(live, target) {
                        Ok(()) => once.clear(&slot),
                        Err(error) => once.report(
                            &mut alerts,
                            &slot,
                            format!("出力先を用意できません {}: {error:#}", target.out.display()),
                        ),
                    }
                }
                if !live.superseded {
                    let slot = format!("supersede-{index}");
                    match supersede(&checkpoint, target) {
                        Ok(()) => {
                            live.superseded = true;
                            once.clear(&slot);
                        }
                        Err(error) => once.report(
                            &mut alerts,
                            &slot,
                            format!("前の世代を SUPERSEDED にできません {}: {error:#}", target.id),
                        ),
                    }
                }
            }
            while let Ok(event) = events_rx.try_recv() {
                match event {
                    Event::Alert(message) => alerts.push(&message),
                    Event::Info(id, nodes, depth) => {
                        if let Some(worker) = workers.get_mut(&id) {
                            worker.nodes = nodes;
                            worker.depth = depth;
                        }
                    }
                    Event::Done(id, key, explicit, reason, result, trace) => {
                        let prefetch = workers.get(&id).is_some_and(|w| w.prefetch);
                        if result.is_err()
                            && prefetch
                            && let Some(reason) = reason
                        {
                            stats_dirty = true;
                            *stats.stopped.entry(reason).or_default() += 1;
                            stats.discarded += workers
                                .get(&id)
                                .map_or(0.0, |w| w.nodes as f64 / settings.nodes as f64);
                        }
                        if let Some(worker) = workers.get_mut(&id) {
                            worker.busy = None;
                            worker.demand.clear();
                        }
                        for live in &mut lives {
                            if live.tree.level.contains_key(&key)
                                || live.shadow.iter().any(|c| c.key == key)
                            {
                                live.dirty = true;
                                live.requests_dirty = true;
                            }
                        }
                        match result {
                            Ok(Outcome::Stopped(reason, amount, sec)) => {
                                stats_dirty = true;
                                *stats.stopped.entry(reason).or_default() += 1;
                                stats.discarded += amount.max(
                                    median(&journal.durations)
                                        .map_or(0.0, |expected| sec / expected),
                                );
                            }
                            Ok(Outcome::Completed(reading)) => {
                                once.clear("spawn");
                                let record = trace.map(|t| {
                                    json!({"key":reading.key,"ply":reading.ply,"fingerprint":settings.fingerprint,"worker":id,"started":t.started.to_rfc3339(),"finished":t.finished.to_rfc3339(),"bestmove":reading.bestmove,"lines":t.lines})
                                });
                                journal.append_marked(
                                    reading,
                                    &settings,
                                    "engine".into(),
                                    prefetch,
                                )?;
                                // 探索の記録は読みの結果に関わらないので、書けなくても読みは止めない。
                                if let (Some(log), Some(record)) = (trace_log.as_mut(), record) {
                                    match log.append(&record) {
                                        Ok(()) => once.clear("trace"),
                                        Err(error) => once.report_first(
                                            &mut alerts,
                                            "trace",
                                            format!(
                                                "探索の記録を書けません {}: {error:#}",
                                                log.path.display()
                                            ),
                                        ),
                                    }
                                }
                            }
                            Err(error) => match failure_kind(&error, explicit, prefetch, reason) {
                                FailureKind::InputsChanged => {
                                    alerts.push(&format!("{error:#}"));
                                    return Err(error);
                                }
                                FailureKind::Restart => {
                                    alerts.push(&format!(
                                        "制御ファイルによる RESTART worker={id} key={key}"
                                    ));
                                }
                                FailureKind::Spawn => {
                                    once.report_first(&mut alerts, "spawn", format!("{error:#}"));
                                }
                                FailureKind::Prefetch => {
                                    *prefetch_failures.entry(key.clone()).or_default() += 1;
                                    once.report_first(
                                        &mut alerts,
                                        &format!("prefetch-{key}"),
                                        format!("先読みの異常 key={key}: {error:#}"),
                                    );
                                }
                                FailureKind::InvalidReading => {
                                    let message = format!("局面の検査失敗: {key}: {error:#}");
                                    alerts.push(&message);
                                    failures.insert(key, message);
                                }
                                FailureKind::Retry => {
                                    let attempts = retries.entry(key.clone()).or_default();
                                    *attempts += 1;
                                    alerts.push(&format!("RESTART worker={id} attempt={attempts} key={key}: {error:#}"));
                                    if *attempts > args.max_restarts {
                                        let message =
                                            format!("局面の失敗 (再起動上限): {key}: {error:#}");
                                        alerts.push(&message);
                                        failures.insert(key, message);
                                    }
                                }
                            },
                        }
                    }
                }
            }
            for entry in std::fs::read_dir(args.state.join("restart"))? {
                let entry = entry?;
                if let Err(error) = super::paths::reject_symlink(&entry.path()) {
                    alerts.push(&format!("restart の指示を無視しました: {error:#}"));
                    let _ = std::fs::remove_file(entry.path());
                    continue;
                }
                let id = entry.file_name().to_string_lossy().parse::<usize>().ok();
                let mut targeted = false;
                if let Some(worker) = id.and_then(|id| workers.get(&id)) {
                    let mut control =
                        worker.restart.lock().unwrap_or_else(|error| error.into_inner());
                    targeted = control.request();
                }
                if !targeted {
                    alerts.push(&format!(
                        "restart 対象の読みがありません: {}",
                        entry.path().display()
                    ));
                }
                std::fs::remove_file(entry.path())?;
            }
            let retired: Vec<_> = workers
                .iter()
                .filter(|(id, w)| **id >= desired && w.busy.is_none())
                .map(|(id, _)| *id)
                .collect();
            for id in retired {
                if let Some(worker) = workers.remove(&id) {
                    let _ = worker.tx.send(None);
                    worker.handle.join().map_err(|_| anyhow::anyhow!("worker が panic"))?;
                }
            }
            for id in 0..desired {
                workers
                    .entry(id)
                    .or_insert_with(|| spawn_worker(id, args, &settings, events_tx.clone()));
            }
            let running: BTreeSet<_> = workers
                .values()
                .filter_map(|w| w.busy.as_ref().map(|(k, _)| k.clone()))
                .collect();
            let mut demands = Vec::new();
            for (target, live) in checkpoint.targets.iter().zip(lives.iter_mut()) {
                let tree = &mut live.tree;
                if !live.prepared || target.hold || tree.finished || tree.failed {
                    live.shadow.clear();
                    live.dirty = true;
                    continue;
                }
                let previous_nodes = tree.nodes.len();
                let advanced = tree.advance(&journal);
                if args.prefetch && tree.nodes.len() != previous_nodes {
                    stats_dirty |= stats.mark(tree, &journal.prefetched);
                    live.requests_dirty = true;
                    live.dirty = true;
                }
                match advanced {
                    Ok(missing) => {
                        if let Some((key, _)) =
                            missing.iter().find(|(key, _)| failures.contains_key(key))
                        {
                            tree.failed = true;
                            remove_candidates(&tree.def.out)?;
                            monitor(&tree.def.out.join("STATUS"), "FAIL\n");
                            alerts.push(&format!(
                                "木の失敗 {}: {}",
                                tree.def.out.display(),
                                failures[key]
                            ));
                            continue;
                        }
                        if tree.finished {
                            match tree.write_outputs() {
                                Ok(ready) => tree.ready = ready,
                                Err(error) => {
                                    tree.failed = true;
                                    remove_candidates(&tree.def.out)?;
                                    monitor(&tree.def.out.join("STATUS"), "FAIL\n");
                                    alerts.push(&format!("出力失敗: {error:#}"));
                                }
                            }
                        } else {
                            demands.push(Demand {
                                priority: target.priority,
                                order: target.order,
                                missing,
                                ply: &tree.ply,
                            });
                        }
                    }
                    Err(e) => {
                        tree.failed = true;
                        remove_candidates(&tree.def.out)?;
                        monitor(&tree.def.out.join("STATUS"), "FAIL\n");
                        alerts.push(&format!("木の失敗 {}: {e:#}", tree.def.out.display()));
                    }
                }
            }
            let queue = reading_queue(demands, &running);
            let mut queue_len = queue.len();
            let cap = args.prefetch_workers.unwrap_or(desired).min(desired);
            if args.prefetch
                && !stopping
                && queue.is_empty()
                && workers.values().any(|w| w.busy.is_none())
            {
                rebuild_shadows(
                    &mut lives,
                    &checkpoint.targets,
                    &journal,
                    args.prefetch_work,
                    &mut shadow_cursor,
                )?;
            }
            let mut predicted: BTreeMap<String, (u32, u64, usize, u32)> = BTreeMap::new();
            if args.prefetch {
                for (target, live) in checkpoint.targets.iter().zip(&mut lives) {
                    live.refresh_requests(target, &journal, &failures, &mut requests);
                    if target.hold || live.tree.finished || live.tree.failed || !live.prepared {
                        continue;
                    }
                    // 再構築待ちでも需要は保ち、古い候補の新規配布だけを止める。
                    for candidate in &live.shadow {
                        if journal.get(&candidate.key).is_some()
                            || failures.contains_key(&candidate.key)
                        {
                            continue;
                        }
                        if live.dirty
                            || prefetch_failures.get(&candidate.key).copied().unwrap_or(0) >= 2
                        {
                            continue;
                        }
                        let rank = (target.priority, target.order, candidate.level, candidate.ply);
                        predicted
                            .entry(candidate.key.clone())
                            .and_modify(|old| {
                                if (rank.0, rank.1, rank.2) < (old.0, old.1, old.2) {
                                    *old = rank;
                                }
                            })
                            .or_insert(rank);
                    }
                }
                for (id, worker) in &mut workers {
                    let Some((key, _)) = &worker.busy else {
                        continue;
                    };
                    worker.demand = requests.get(key).cloned().unwrap_or_default();
                    worker.priority = checkpoint
                        .targets
                        .iter()
                        .filter(|target| {
                            worker
                                .demand
                                .iter()
                                .any(|r| r.id == target.id && r.generation == target.generation)
                        })
                        .map(|target| target.priority)
                        .min()
                        .unwrap_or(u32::MAX);
                    let mut control = worker.restart.lock().unwrap_or_else(|e| e.into_inner());
                    if worker.demand.iter().any(|r| !r.predicted) {
                        control.promote();
                    } else if worker.prefetch {
                        if stopping {
                            control.stop(StopReason::Stop);
                        } else if *id >= desired {
                            control.stop(StopReason::Workers);
                        } else if worker.demand.is_empty() {
                            control.stop(StopReason::NoDemand);
                        }
                    }
                }
                let speculative = |w: &&Worker| {
                    w.prefetch && w.busy.is_some() && !w.demand.iter().any(|r| !r.predicted)
                };
                let mut excess = workers
                    .values()
                    .filter(speculative)
                    .filter(|w| w.restart.lock().unwrap_or_else(|e| e.into_inner()).stop.is_none())
                    .count()
                    .saturating_sub(cap);
                for worker in workers.values().rev().filter(speculative) {
                    if excess == 0 {
                        break;
                    }
                    let mut control = worker.restart.lock().unwrap_or_else(|e| e.into_inner());
                    if control.stop.is_none() {
                        control.stop(StopReason::Workers);
                        excess -= 1;
                    }
                }
                if !stopping && let Some(expected) = median(&journal.durations) {
                    let yielding = workers
                        .iter()
                        .filter(|(id, w)| {
                            **id < desired
                                && w.busy.is_some()
                                && w.restart
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .yielding(w.demand.iter().any(|r| !r.predicted))
                        })
                        .count();
                    let free = workers.values().filter(|w| w.busy.is_none()).count();
                    let needed = queue_len.saturating_sub(yielding + free);
                    let mut victims = Vec::new();
                    for (id, worker) in &workers {
                        if !speculative(&worker) {
                            continue;
                        }
                        let control = worker.restart.lock().unwrap_or_else(|e| e.into_inner());
                        if control.stop.is_some() || control.requested || control.key.is_none() {
                            continue;
                        }
                        let Some(progress) =
                            control.progress(expected, worker.nodes, settings.nodes)
                        else {
                            continue;
                        };
                        if progress < args.prefetch_preempt_below {
                            victims.push((*id, progress, worker.priority));
                        }
                    }
                    for id in select_victims(victims, needed, args.prefetch_preempt_below) {
                        workers[&id]
                            .restart
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .stop(StopReason::Preempt);
                    }
                }
            }
            let mut pref_queue: Vec<_> = predicted
                .into_iter()
                .filter(|(key, _)| {
                    !running.contains(key)
                        && requests.get(key).is_some_and(|rows| rows.iter().all(|r| r.predicted))
                })
                .collect();
            pref_queue.sort_by(|(ka, a), (kb, b)| (a.0, a.1, a.2, ka).cmp(&(b.0, b.1, b.2, kb)));
            let mut pref_queue_len = pref_queue.len();
            let mut pref_active = workers
                .values()
                .filter(|w| {
                    w.prefetch && w.busy.is_some() && !w.demand.iter().any(|r| !r.predicted)
                })
                .count();
            if !stopping {
                let mut jobs = queue.into_iter();
                let mut pref_jobs = pref_queue.into_iter();
                for (id, worker) in
                    workers.iter_mut().filter(|(id, w)| **id < desired && w.busy.is_none())
                {
                    let job = if let Some((key, ply)) = jobs.next() {
                        queue_len -= 1;
                        Some((key, ply, false, 0))
                    } else if args.prefetch && pref_active < cap {
                        pref_jobs.next().map(|(key, (priority, _, _, ply))| {
                            pref_active += 1;
                            pref_queue_len -= 1;
                            (key, ply, true, priority)
                        })
                    } else {
                        None
                    };
                    if let Some((key, ply, prefetch, priority)) = job {
                        *worker.restart.lock().unwrap_or_else(|error| error.into_inner()) =
                            Restart {
                                key: Some(key.clone()),
                                ..Restart::default()
                            };
                        worker.busy = Some((key.clone(), Instant::now()));
                        worker.nodes = 0;
                        worker.depth = 0;
                        worker.prefetch = prefetch;
                        worker.priority = priority;
                        worker.demand = requests.get(&key).cloned().unwrap_or_default();
                        worker
                            .tx
                            .send(Some(Job { key, ply }))
                            .with_context(|| format!("worker {id} が終了しました"))?;
                    }
                }
            }
            let active = workers.values().filter(|w| w.busy.is_some()).count();
            let status = if desired == 0 && active == 0 {
                "STOPPED"
            } else if active == 0 && queue_len == 0 && (pref_queue_len == 0 || cap == 0) {
                "IDLE"
            } else {
                "RUNNING"
            };
            monitor_changed(&args.state.join("STATUS"), &format!("{status} {run_id}\n"));
            // 段 0 の木は数十万局面を持つので、局面ごとに worker を走査しない。
            let busy_keys: BTreeSet<&String> = workers
                .values()
                .filter(|w| !args.prefetch || w.demand.iter().any(|r| !r.predicted))
                .filter_map(|w| w.busy.as_ref().map(|(k, _)| k))
                .collect();
            let busy = |key: &String| busy_keys.contains(key);
            // 終わった木は列にも読みにも関わらないので、局面を数え直さない。
            let counts =
                |live: &Live| live.ended.unwrap_or((live.tree.nodes.len(), live.tree.level.len()));
            let active_tree =
                |live: &Live| live.ended.is_none() && !live.tree.finished && !live.tree.failed;
            let counts_now = (journal.prefetched.len(), stats.used.len());
            if args.prefetch && unused_counts != counts_now {
                unused_prefetch = journal.prefetched.difference(&stats.used).count();
                unused_counts = counts_now;
            }
            if progress_written
                .as_ref()
                .is_none_or(|(last, at)| last != status || at.elapsed() >= Duration::from_secs(1))
            {
                let trees: Vec<_> = checkpoint.targets.iter().zip(&lives).map(|(target, live)| {
                    let t = &live.tree;
                    let (read, requested) = counts(live);
                    let (queued, running) = if active_tree(live) && (!args.prefetch || !target.hold) {
                        (
                            t.level.keys().filter(|k| journal.get(k).is_none() && !busy(k)).count(),
                            busy_keys.iter().filter(|k| t.level.contains_key(**k)).count(),
                        )
                    } else {
                        (0, 0)
                    };
                    json!({"id":target.id,"own":target.own,"generation":target.generation,"priority":target.priority,"hold":target.hold,"out":t.def.out,"round":t.round,"read":read,"requested":requested,"queued":queued,"running":running,"finished":t.finished,"failed":t.failed})
                }).collect();
                let mut progress = json!({"status":status,"run_id":run_id,"fingerprint":settings.fingerprint,"queued":queue_len,"workers":workers.iter().map(|(id,w)|json!({"id":id,"key":w.busy.as_ref().map(|(k,_)|k),"elapsed_secs":w.busy.as_ref().map(|(_,t)|t.elapsed().as_secs_f64()),"nodes":w.nodes,"depth":w.depth})).collect::<Vec<_>>(),"trees":trees});
                if args.prefetch {
                    progress["queued"] = json!({"normal": queue_len, "prefetch": pref_queue_len});
                    progress["prefetch"] = json!({"queued": pref_queue_len, "running": pref_active, "stopped": stats.stopped, "discarded": stats.discarded, "read": journal.prefetched.len(), "used": stats.used.len(), "unused": unused_prefetch});
                }
                monitor(
                    &args.state.join("progress.json"),
                    &serde_json::to_string_pretty(&progress)?,
                );
                progress_written = Some((status.to_owned(), Instant::now()));
            }
            let mut statuses = Vec::new();
            for (target, live) in checkpoint.targets.iter().zip(lives.iter_mut()) {
                let (read, required) = counts(live);
                let tree = &live.tree;
                let reading = active_tree(live)
                    && (!args.prefetch || !target.hold)
                    && busy_keys.iter().any(|k| tree.level.contains_key(*k));
                let tree_status = if tree.failed {
                    "FAIL"
                } else if tree.finished {
                    if tree.ready { "DONE" } else { "NOT_READY" }
                } else if target.hold {
                    "HELD"
                } else if desired == 0 && !reading {
                    "STOPPED"
                } else if read == 0 && !reading {
                    "QUEUED"
                } else {
                    "RUNNING"
                };
                let state = match tree_status {
                    "STOPPED" if read == 0 => "QUEUED",
                    "STOPPED" => "RUNNING",
                    other => other,
                };
                statuses.push(TargetStatus {
                    id: target.id.clone(),
                    own: target.own.clone(),
                    generation: target.generation,
                    priority: target.priority,
                    hold: target.hold,
                    state: state.into(),
                    ready: tree.finished && tree.ready,
                    read,
                    required,
                    out: target.out.clone(),
                });
                if !live.prepared || live.ended.is_some() {
                    continue;
                }
                let ended = tree.finished || tree.failed;
                let written = if ended && !tree.def.out.is_dir() {
                    true
                } else {
                    monitor_changed(&tree.def.out.join("STATUS"), &format!("{tree_status}\n"))
                };
                // 終わった木は STATUS を書けてから中身を縮める。
                if ended && written {
                    live.ended = Some((read, required));
                    live.shadow.clear();
                    live.tree.shrink();
                }
            }
            let text = serde_json::to_string_pretty(&StatusFile::new(
                run_id,
                &checkpoint,
                statuses,
                queue_error.clone(),
            ))?;
            if status_written.as_ref() != Some(&text) {
                match write_atomic(&status_path, &text) {
                    Ok(()) => {
                        once.clear("status");
                        status_written = Some(text);
                    }
                    Err(error) => once.report(
                        &mut alerts,
                        "status",
                        format!("状態のファイルを書けません: {error:#}"),
                    ),
                }
            }
            // 次の周回で世代が置き換わっても、旧世代での利用を復元できるよう先に確定する。
            if args.prefetch && stats_dirty {
                stats.save(&stats_path)?;
                stats_dirty = false;
            }
            alerts.flush();
            if stopping && active == 0 {
                if args.prefetch {
                    stats.save(&stats_path)?;
                }
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })();
    if result.is_err() {
        for live in &lives {
            let tree = &live.tree;
            if live.prepared && !tree.finished && !tree.failed {
                if let Err(error) = remove_candidates(&tree.def.out) {
                    alerts.push(&format!("候補の削除失敗: {error:#}"));
                }
                monitor(&tree.def.out.join("STATUS"), "FAIL\n");
            }
        }
    }
    alerts.flush();
    for worker in workers.into_values() {
        let _ = worker.tx.send(None);
        if result.is_err() {
            worker.restart.lock().unwrap_or_else(|error| error.into_inner()).requested = true;
        }
        let _ = worker.handle.join();
    }
    result
}

// 内容と metadata の両方で変更を捉え、同じ受付エラーの連続通知を避ける。
// 通常のファイル以外 (リンク・FIFO など) は開かない。書き手のいない FIFO を読むと受付が止まる。
fn file_stamp(path: &std::path::Path) -> String {
    let metadata = std::fs::symlink_metadata(path);
    let content = match &metadata {
        Ok(m) if m.file_type().is_file() => format!("{:?}", std::fs::read(path)),
        Ok(m) if m.file_type().is_symlink() => format!("{:?}", std::fs::read_link(path)),
        _ => String::new(),
    };
    format!("{:?}:{content}", metadata.map(|m| (m.modified().ok(), m.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopped_anomalies_and_spawn_failures_do_not_consume_normal_retries() {
        for prefetch in [false, true] {
            for reason in [
                StopReason::Preempt,
                StopReason::NoDemand,
                StopReason::Stop,
                StopReason::Workers,
            ] {
                for message in ["stop の応答期限", "EOF", "stop の送信失敗"] {
                    assert_eq!(
                        failure_kind(&anyhow::anyhow!(message), false, prefetch, Some(reason)),
                        FailureKind::Prefetch
                    );
                }
            }
            assert_eq!(
                failure_kind(&SpawnFailed(anyhow::anyhow!("spawn")).into(), false, prefetch, None),
                FailureKind::Spawn
            );
            assert_eq!(
                failure_kind(
                    &InputsChanged(anyhow::anyhow!("input")).into(),
                    false,
                    prefetch,
                    Some(StopReason::Stop)
                ),
                FailureKind::InputsChanged
            );
            assert_eq!(
                failure_kind(
                    &anyhow::anyhow!("restart"),
                    true,
                    prefetch,
                    Some(StopReason::Restart)
                ),
                FailureKind::Restart
            );
        }
        assert_eq!(failure_kind(&anyhow::anyhow!("EOF"), false, false, None), FailureKind::Retry);
        assert_eq!(
            failure_kind(&InvalidReading(anyhow::anyhow!("reading")).into(), false, false, None),
            FailureKind::InvalidReading
        );
    }

    #[test]
    fn default_bestmove_keeps_restart_key_and_accepts_pending_restart() {
        for requested in [false, true] {
            let mut control = Restart {
                key: Some("key".into()),
                requested,
                ..Restart::default()
            };
            assert_eq!(control.completed(false).unwrap(), None);
            assert_eq!(control.key.as_deref(), Some("key"));
            assert!(control.request());
            assert!(control.finish());
            assert!(control.key.is_none());
        }
    }

    #[test]
    fn bestmove_before_stop_write_completes_and_after_write_is_discarded() {
        let mut control = Restart {
            key: Some("key".into()),
            ..Restart::default()
        };
        control.stop(StopReason::Preempt);
        assert_eq!(control.completed(true).unwrap(), None);
        assert!(!control.request());
        let mut control = Restart {
            key: Some("key".into()),
            sent: Some(Instant::now()),
            stop: Some(StopReason::Preempt),
            ..Restart::default()
        };
        assert_eq!(control.completed(true).unwrap(), Some(StopReason::Preempt));
        let mut control = Restart {
            key: Some("key".into()),
            sent: Some(Instant::now()),
            stop: Some(StopReason::Preempt),
            ..Restart::default()
        };
        assert!(control.request());
        assert!(control.completed(true).is_err());
    }

    #[test]
    fn stop_promotion_and_restart_order_are_atomic() {
        for reason in [
            StopReason::Preempt,
            StopReason::NoDemand,
            StopReason::Stop,
            StopReason::Workers,
        ] {
            let mut control = Restart {
                key: Some("key".into()),
                ..Restart::default()
            };
            control.stop(reason);
            assert_eq!(control.stop, Some(reason));
            control.promote();
            assert_eq!(control.stop, None);
            control.stop(reason);
            control.sent = Some(Instant::now());
            assert!(control.yielding(false));
            control.promote();
            assert_eq!(control.stop, Some(reason));
            assert!(!control.yielding(true));
            assert!(control.request());
            control.stop(StopReason::Preempt);
            assert!(control.finish());
            assert!(control.key.is_none());
            assert!(control.stop.is_none());
        }
    }

    #[test]
    fn preempt_threshold_priority_and_per_iteration_limit() {
        let candidates = vec![(0, 0.25, 9), (1, 0.10, 1), (2, 0.10, 5), (3, 0.2, 99)];
        assert_eq!(select_victims(candidates.clone(), 1, 0.25), [2]);
        assert_eq!(select_victims(candidates.clone(), 9, 0.25), [2, 1, 3]);
        assert!(select_victims(candidates.clone(), 0, 0.25).is_empty());
        assert!(select_victims(candidates, 9, 0.0).is_empty());
    }

    #[test]
    fn progress_uses_go_and_observed_nodes_to_protect_work() {
        let mut control = Restart::default();
        assert!(control.progress(10.0, 0, 100).is_none());
        control.go = Some(Instant::now() - Duration::from_secs(2));
        let progress = control.progress(10.0, 0, 100).unwrap();
        assert!((0.2..0.21).contains(&progress));
        assert_eq!(control.progress(10.0, 50, 100), Some(0.5));
        assert!(
            select_victims(vec![(0, control.progress(10.0, 25, 100).unwrap(), 0)], 1, 0.25)
                .is_empty()
        );
    }

    #[test]
    fn duration_estimate_requires_ten_and_uses_median() {
        assert!(median(&(1..10).map(f64::from).collect()).is_none());
        assert_eq!(median(&(1..11).map(f64::from).collect()), Some(5.5));
        assert_eq!(median(&(1..12).rev().map(f64::from).collect()), Some(6.0));
        assert!(median(&VecDeque::from(vec![0.0; 10])).is_none());
    }

    #[test]
    fn shadow_iteration_defers_dirty_trees_and_rotates_after_work_limits() {
        let dir = tempfile::tempdir().unwrap();
        let root = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let chunk: String = (0..6).map(|id| format!("{}\n", json!({"id":format!("tree{id}"),"root":format!("sfen {root} 1"),"own":"b","depth":1}))).collect();
        let defaults = Defaults {
            opp_n: 1,
            stop: 300,
            rounds: 1,
            draw: 0,
        };
        let (checkpoint, built) = Checkpoint::new(dir.path().join("trees"), "fp".into()).apply(
            chunk.as_bytes(),
            &defaults,
            &Sha256::new(),
            &[],
        );
        assert!(checkpoint.rejected.is_empty());
        let mut lives: Vec<_> = built
            .into_iter()
            .map(|(_, tree)| {
                let mut live = Live::new(tree);
                live.prepared = true;
                live
            })
            .collect();
        let cache = BTreeMap::<String, Reading>::new();
        let mut cursor = 0;
        rebuild_shadows(
            &mut lives,
            &checkpoint.targets,
            &cache,
            super::super::model::PREFETCH_WORK,
            &mut cursor,
        )
        .unwrap();
        assert_eq!(lives.iter().filter(|live| !live.dirty).count(), 1);
        assert_eq!(cursor, 1);
        for _ in 0..4 {
            rebuild_shadows(
                &mut lives,
                &checkpoint.targets,
                &cache,
                super::super::model::PREFETCH_WORK,
                &mut cursor,
            )
            .unwrap();
        }
        rebuild_shadows(
            &mut lives,
            &checkpoint.targets,
            &cache,
            super::super::model::PREFETCH_WORK,
            &mut cursor,
        )
        .unwrap();
        assert!(lives.iter().all(|live| !live.dirty && !live.shadow.is_empty()));
        for live in &mut lives {
            live.dirty = true;
        }
        cursor = 0;
        rebuild_shadows(&mut lives, &checkpoint.targets, &cache, 1, &mut cursor).unwrap();
        assert!(lives.iter().all(|live| live.dirty));
        assert!(lives.iter().all(|live| !live.shadow.is_empty()));
        assert_eq!(cursor, 1);
        let mut requests = BTreeMap::new();
        for (live, target) in lives.iter_mut().zip(&checkpoint.targets) {
            live.refresh_requests(target, &cache, &BTreeMap::new(), &mut requests);
        }
        assert_eq!(requests[root].len(), lives.len());
        assert!(requests[root].iter().all(|request| request.predicted));
        rebuild_shadows(
            &mut lives,
            &checkpoint.targets,
            &cache,
            super::super::model::PREFETCH_WORK,
            &mut cursor,
        )
        .unwrap();
        assert!(!lives[1].dirty);

        lives.truncate(2);
        let work = lives[0]
            .tree
            .prefetch_report_borrowed(
                checkpoint.targets[0].definition.borrowed_lines(),
                &cache,
                false,
                super::super::model::PREFETCH_WORK,
            )
            .unwrap()
            .work;
        assert!(work > 1);
        for live in &mut lives {
            live.dirty = true;
            live.shadow.clear();
        }
        cursor = 0;
        rebuild_shadows(&mut lives, &checkpoint.targets, &cache, 2 * work - 1, &mut cursor)
            .unwrap();
        assert_eq!(cursor, 1);
        assert!(lives[1].shadow.is_empty());
        lives[0].dirty = true;
        rebuild_shadows(&mut lives, &checkpoint.targets, &cache, 2 * work - 1, &mut cursor)
            .unwrap();
        assert!(!lives[1].dirty);
        assert!(!lives[1].shadow.is_empty());
    }

    #[test]
    fn prefetch_stats_survive_shrink_and_shared_generations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefetch_stats.json");
        let key = "4k4/9/9/9/9/9/9/9/4K4 b -".to_owned();
        let def = super::super::model::TreeParams {
            out: dir.path().join("tree"),
            depth: 0,
            opp_n: 1,
            stop: 300,
            own: "b".into(),
            rounds: 0,
            extra: vec![],
            draw: 0,
        };
        let mut tree = Tree::build(def.clone(), &[(key.clone(), 1, vec![])]).unwrap();
        let prefetched = BTreeSet::from([key.clone(), "unused".into()]);
        let mut stats = PrefetchStats::load(&path).unwrap();
        assert!(stats.mark(&tree, &prefetched));
        tree.shrink();
        assert!(!stats.mark(&tree, &prefetched));
        let next = Tree::build(def, &[(key.clone(), 1, vec![])]).unwrap();
        assert!(!stats.mark(&next, &prefetched));
        stats.stopped.insert(StopReason::Preempt, 2);
        stats.discarded = 0.3;
        stats.save(&path).unwrap();
        let restored = PrefetchStats::load(&path).unwrap();
        assert_eq!(restored.used, BTreeSet::from([key]));
        assert_eq!(restored.stopped[&StopReason::Preempt], 2);
        assert_eq!(restored.discarded, 0.3);
    }

    #[test]
    fn fixed_readings_preserve_normal_dispatch_across_shared_trees() {
        let dir = tempfile::tempdir().unwrap();
        let root = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let chunk = format!(
            "{}\n{}\n",
            json!({"id":"low","root":format!("sfen {root} 11"),"own":"b","depth":1,"priority":10}),
            json!({"id":"high","root":format!("sfen {root} 21"),"own":"b","depth":1,"priority":1})
        );
        let defaults = Defaults {
            opp_n: 1,
            stop: 300,
            rounds: 0,
            draw: 0,
        };
        let (checkpoint, built) = Checkpoint::new(dir.path().join("trees"), "fp".into()).apply(
            chunk.as_bytes(),
            &defaults,
            &Sha256::new(),
            &[],
        );
        assert!(checkpoint.rejected.is_empty());
        let mut trees: Vec<_> = built.into_iter().map(|(_, tree)| tree).collect();
        let mut cache = BTreeMap::new();
        let queue = |trees: &mut Vec<Tree>,
                     cache: &BTreeMap<String, Reading>,
                     running: &BTreeSet<String>| {
            let missing: Vec<_> =
                trees.iter_mut().map(|tree| tree.advance(cache).unwrap()).collect();
            reading_queue(
                trees.iter().zip(&checkpoint.targets).zip(missing).map(
                    |((tree, target), missing)| Demand {
                        priority: target.priority,
                        order: target.order,
                        missing,
                        ply: &tree.ply,
                    },
                ),
                running,
            )
        };
        assert_eq!(queue(&mut trees, &cache, &BTreeSet::new()), [(root.into(), 21)]);
        assert!(queue(&mut trees, &cache, &BTreeSet::from([root.into()])).is_empty());
        cache.insert(
            root.into(),
            Reading {
                tag: "fixed".into(),
                key: root.into(),
                ply: 21,
                nodes_limit: 100,
                nodes: Some(100),
                sec: 1.0,
                threads: 1,
                hash: 16,
                draw: BTreeMap::new(),
                bestmove: "5i4i".into(),
                multipv: vec![Pv {
                    mv: "5i4i".into(),
                    cp: 10,
                    depth: 8,
                }],
            },
        );
        let child = super::super::model::child(root, "5i4i").unwrap();
        assert_eq!(queue(&mut trees, &cache, &BTreeSet::new()), [(child.clone(), 22)]);
        let mut reading = cache[root].clone();
        reading.key = child.clone();
        reading.ply = 22;
        reading.bestmove = "resign".into();
        reading.multipv.clear();
        cache.insert(child, reading);
        assert!(queue(&mut trees, &cache, &BTreeSet::new()).is_empty());
        assert!(trees.iter().all(|tree| tree.finished && tree.nodes.len() == 2));
    }

    #[test]
    fn reading_queue_preserves_first_ply_on_equal_rank() {
        let first = BTreeMap::from([("shared".into(), 19)]);
        let second = BTreeMap::from([("shared".into(), 3)]);
        let demands = [&first, &second].map(|ply| Demand {
            priority: 2,
            order: 7,
            missing: vec![("shared".into(), 1)],
            ply,
        });
        assert_eq!(reading_queue(demands, &BTreeSet::new()), [("shared".into(), 19)]);
    }

    #[test]
    fn reading_queue_orders_equal_ranks_by_key_and_excludes_running() {
        let ply = BTreeMap::from([("c".into(), 3), ("a".into(), 9), ("b".into(), 1)]);
        let demand = Demand {
            priority: 2,
            order: 7,
            missing: vec![("c".into(), 1), ("a".into(), 1), ("b".into(), 1)],
            ply: &ply,
        };
        assert_eq!(
            reading_queue([demand], &BTreeSet::from(["b".into()])),
            [("a".into(), 9), ("c".into(), 3)]
        );
    }

    #[test]
    fn reading_queue_merges_by_priority_order_then_depth() {
        let first = BTreeMap::from([("p".into(), 10), ("o".into(), 11), ("d".into(), 12)]);
        let second = BTreeMap::from([("p".into(), 20), ("o".into(), 21), ("d".into(), 22)]);
        let demands = vec![
            Demand {
                priority: 2,
                order: 0,
                missing: vec![("p".into(), 0)],
                ply: &first,
            },
            Demand {
                priority: 1,
                order: 9,
                missing: vec![("p".into(), 9)],
                ply: &second,
            },
            Demand {
                priority: 1,
                order: 8,
                missing: vec![("o".into(), 0)],
                ply: &first,
            },
            Demand {
                priority: 1,
                order: 7,
                missing: vec![("o".into(), 9)],
                ply: &second,
            },
            Demand {
                priority: 1,
                order: 7,
                missing: vec![("d".into(), 8)],
                ply: &first,
            },
            Demand {
                priority: 1,
                order: 7,
                missing: vec![("d".into(), 2)],
                ply: &second,
            },
        ];
        assert_eq!(
            reading_queue(demands, &BTreeSet::new()),
            [("d".into(), 22), ("o".into(), 21), ("p".into(), 20)]
        );
    }

    #[test]
    fn trace_buffer_drops_the_search_once_over_the_limit() {
        let mut buffer = TraceBuffer::new(10);
        assert!(!buffer.push("info 12345"));
        assert_eq!(buffer.lines, ["info 12345"]);
        assert!(buffer.push("i"));
        assert!(buffer.overflowed && buffer.lines.is_empty());
        assert!(!buffer.push("i"));
        assert!(buffer.lines.is_empty());
    }

    #[test]
    fn trace_after_failed_write_starts_on_a_new_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.jsonl");
        std::fs::write(&path, "{\"key\":\"tor").unwrap();
        let mut log = TraceLog {
            dir: dir.path().to_owned(),
            path: path.clone(),
            file: None,
            torn: true,
        };
        log.append(&json!({"key": "a"})).unwrap();
        log.append(&json!({"key": "b"})).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines, ["{\"key\":\"tor", "{\"key\":\"a\"}", "{\"key\":\"b\"}"]);
    }

    #[test]
    fn input_verification_covers_directory_members_and_explicit_spsa() {
        let dir = tempfile::tempdir().unwrap();
        let engine = dir.path().join("engine");
        let model = dir.path().join("model");
        let params = dir.path().join("explicit.params");
        std::fs::write(&engine, "engine").unwrap();
        std::fs::create_dir(&model).unwrap();
        let weights = model.join("nn.bin");
        std::fs::write(&weights, "weights").unwrap();
        std::fs::write(&params, "params").unwrap();
        let args = super::super::ReadingArgs {
            engine,
            engine_options: vec![
                format!("EvalDir={}", model.display()),
                format!("SPSAParamsFile={}", params.display()),
            ],
            nodes: 100,
            multipv: 1,
            hash: 16,
        };
        let settings = Settings::new(&args).unwrap();
        verify_inputs(&args, &settings).unwrap();
        for path in [&weights, &params] {
            let original = std::fs::read(path).unwrap();
            std::fs::write(path, "changed").unwrap();
            assert!(verify_inputs(&args, &settings).unwrap_err().is::<InputsChanged>());
            std::fs::remove_file(path).unwrap();
            assert!(verify_inputs(&args, &settings).unwrap_err().is::<InputsChanged>());
            std::fs::write(path, original).unwrap();
            verify_inputs(&args, &settings).unwrap();
        }
        std::fs::write(model.join("new.bin"), "new").unwrap();
        assert!(verify_inputs(&args, &settings).unwrap_err().is::<InputsChanged>());
        std::fs::remove_file(model.join("new.bin")).unwrap();
        verify_inputs(&args, &settings).unwrap();
        assert_eq!(Settings::new(&args).unwrap().fingerprint, settings.fingerprint);
    }

    #[test]
    fn completed_job_clears_restart_before_done_is_received() {
        let control = Arc::new(Mutex::new(Restart {
            key: Some("first".into()),
            requested: true,
            ..Restart::default()
        }));
        let (tx, rx) = mpsc::channel();
        let worker_control = control.clone();
        let thread = thread::spawn(move || {
            let mut state = worker_control.lock().unwrap();
            let explicit = state.finish();
            drop(state);
            tx.send(explicit).unwrap();
        });
        thread.join().unwrap();
        {
            let mut state = control.lock().unwrap();
            assert!(state.key.is_none());
            assert!(!state.request());
            assert!(!state.requested);
            // Done が未受信でも次の局面に再起動要求を持ち越さない。
            *state = Restart {
                key: Some("second".into()),
                requested: false,
                ..Restart::default()
            };
        }
        assert!(rx.recv().unwrap());
        assert!(!control.lock().unwrap().requested);
    }
}
