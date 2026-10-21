use super::ServeArgs;
use super::journal::{Journal, LockedFile, Pv, Reading, Readings, Settings};
use super::model::{Tree, remove_candidates};
use super::targets::{
    CHECKPOINT, Checkpoint, Defaults, OUT_ROOT_LOCK, QUEUE, QUEUE_LOCK, RUN_DIR, STATUS_FILE,
    StatusFile, Target, TargetStatus,
};
use crate::common::io::{write_atomic, write_atomic_durable};
use crate::selfplay::{EngineConfig, EngineProcess};
use anyhow::{Context, Result, bail};
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

struct Job {
    key: String,
    ply: u32,
}
enum Event {
    Done(usize, String, bool, Result<Reading>),
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
}

#[derive(Default)]
struct Restart {
    key: Option<String>,
    requested: bool,
}
impl Restart {
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

/// エンジンを起動できない (開けるファイル数の上限など、局面によらない環境の失敗)。
/// 局面の失敗に数えず、次の周回で起動し直す。
#[derive(Debug)]
struct SpawnFailed(anyhow::Error);
impl std::fmt::Display for SpawnFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "engine を起動できません: {:#}", self.0)
    }
}
impl std::error::Error for SpawnFailed {}

fn spawn_worker(
    id: usize,
    args: &ServeArgs,
    settings: &Settings,
    events: mpsc::Sender<Event>,
) -> Worker {
    let (tx, rx) = mpsc::channel::<Option<Job>>();
    let settings = settings.clone();
    let path = args.reading.engine.clone();
    let stall = Duration::from_secs(args.stall_secs);
    let restart = Arc::new(Mutex::new(Restart::default()));
    let restart_worker = restart.clone();
    let handle = thread::spawn(move || {
        let cfg = EngineConfig {
            path,
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
            let result = (|| -> Result<Reading> {
                if engine.is_none() {
                    let process =
                        EngineProcess::spawn(&cfg, format!("tree-{id}")).map_err(SpawnFailed)?;
                    for name in settings.options.keys() {
                        if !process.advertises_option(name) {
                            bail!("engine が option を広告していません: {name}");
                        }
                    }
                    engine = Some(process);
                }
                let engine = engine.as_mut().context("engine がありません")?;
                engine.new_game()?;
                engine.write_line(&format!("position sfen {} {}", job.key, job.ply))?;
                engine.write_line(&format!("go nodes {}", settings.nodes))?;
                let started = Instant::now();
                let mut progressed = started;
                let mut high_nodes = 0;
                let mut high_depth = 0;
                let mut alerted = false;
                let mut collector = Collector::default();
                loop {
                    if restart_worker.lock().unwrap_or_else(|error| error.into_inner()).requested {
                        bail!("制御ファイルによる engine 再起動");
                    }
                    if !alerted && progressed.elapsed() >= stall {
                        let _ = events.send(Event::Alert(format!(
                            "STALL worker={id} key={} nodes={high_nodes} depth={high_depth}",
                            job.key
                        )));
                        alerted = true;
                    }
                    let Some(line) = engine.poll_line(Duration::from_millis(100))? else {
                        continue;
                    };
                    if let Some(rest) = line.strip_prefix("bestmove ") {
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
                        return Ok(reading);
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
                    collector.info(&line).map_err(InvalidReading)?;
                }
            })();
            if result.is_err() {
                engine = None;
            }
            let explicit = {
                let mut control = restart_worker.lock().unwrap_or_else(|error| error.into_inner());
                control.finish()
            };
            if events.send(Event::Done(id, job.key, explicit, result)).is_err() {
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
    }
}

/// 受け付けた対象 1 本の実行時の状態。チェックポイントの `targets` と同じ順に並ぶ。
struct Live {
    tree: Tree,
    prepared: bool,
    superseded: bool,
    /// 終わった木の (読んだ局面数, 要る局面数)。終わった木は中身を縮めて保持する。
    ended: Option<(usize, usize)>,
}
impl Live {
    fn new(tree: Tree) -> Self {
        Self {
            tree,
            prepared: false,
            superseded: false,
            ended: None,
        }
    }
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
    match lock.try_lock_shared() {
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
    let defaults = Defaults {
        opp_n: args.opp_n,
        stop: args.stop,
        rounds: args.rounds,
        draw: args.draw,
    };
    defaults.check()?;
    let settings = Settings::new(&args.reading)?;
    println!("fingerprint={}", settings.fingerprint);
    let journal_path = args.journal.clone().unwrap_or_else(|| args.state.join("journal.jsonl"));
    let (out_root, protected) =
        super::paths::reject_out_root(&journal_path, &args.state, &args.out_root)?;
    std::fs::create_dir_all(&out_root)?;
    // 出力の根の下は、この serve だけが書く (別の serve と、配下の dir への replay を拒む)。
    let _out_lock = LockedFile::open(&out_root.join(OUT_ROOT_LOCK))
        .context("別の serve か replay がこの出力の根を使っています")?;
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
                    Event::Done(id, key, explicit, result) => {
                        if let Some(worker) = workers.get_mut(&id) {
                            worker.busy = None;
                        }
                        match result {
                            Ok(reading) => {
                                once.clear("spawn");
                                journal.append(reading, &settings, "engine".into())?;
                            }
                            Err(error) => {
                                if explicit {
                                    alerts.push(&format!(
                                        "制御ファイルによる RESTART worker={id} key={key}"
                                    ));
                                } else if error.is::<SpawnFailed>() {
                                    once.report_first(&mut alerts, "spawn", format!("{error:#}"));
                                } else if error.is::<InvalidReading>() {
                                    let message = format!("局面の検査失敗: {key}: {error:#}");
                                    alerts.push(&message);
                                    failures.insert(key, message);
                                } else {
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
                            }
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
            // 局面ごとに、それを求める木の (優先度, 受付順, 段) の最小を取る。
            let mut queued: BTreeMap<String, (u32, u64, usize, u32)> = BTreeMap::new();
            for (target, live) in checkpoint.targets.iter().zip(lives.iter_mut()) {
                let tree = &mut live.tree;
                if !live.prepared || target.hold || tree.finished || tree.failed {
                    continue;
                }
                match tree.advance(&journal) {
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
                            for (key, depth) in missing {
                                if running.contains(&key) {
                                    continue;
                                }
                                let rank = (target.priority, target.order, depth, tree.ply[&key]);
                                queued
                                    .entry(key)
                                    .and_modify(|r| {
                                        if (rank.0, rank.1, rank.2) < (r.0, r.1, r.2) {
                                            *r = rank;
                                        }
                                    })
                                    .or_insert(rank);
                            }
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
            let mut queue: Vec<_> = queued.into_iter().collect();
            queue.sort_by(|(ka, (pa, oa, da, _)), (kb, (pb, ob, db, _))| {
                (pa, oa, da, ka).cmp(&(pb, ob, db, kb))
            });
            let mut queue_len = queue.len();
            if !stopping {
                let mut jobs = queue.into_iter();
                for worker in workers.values_mut().filter(|w| w.busy.is_none()) {
                    if let Some((key, (_, _, _, ply))) = jobs.next() {
                        *worker.restart.lock().unwrap_or_else(|error| error.into_inner()) =
                            Restart {
                                key: Some(key.clone()),
                                requested: false,
                            };
                        worker.busy = Some((key.clone(), Instant::now()));
                        worker.nodes = 0;
                        worker.depth = 0;
                        worker.tx.send(Some(Job { key, ply }))?;
                        queue_len -= 1;
                    }
                }
            }
            let active = workers.values().filter(|w| w.busy.is_some()).count();
            let status = if desired == 0 && active == 0 {
                "STOPPED"
            } else if active == 0 && queue_len == 0 {
                "IDLE"
            } else {
                "RUNNING"
            };
            monitor_changed(&args.state.join("STATUS"), &format!("{status} {run_id}\n"));
            let busy = |key: &String| {
                workers.values().any(|w| w.busy.as_ref().is_some_and(|(k, _)| k == key))
            };
            // 終わった木は列にも読みにも関わらないので、局面を数え直さない。
            let counts =
                |live: &Live| live.ended.unwrap_or((live.tree.nodes.len(), live.tree.level.len()));
            let active_tree =
                |live: &Live| live.ended.is_none() && !live.tree.finished && !live.tree.failed;
            if progress_written
                .as_ref()
                .is_none_or(|(last, at)| last != status || at.elapsed() >= Duration::from_secs(1))
            {
                let trees: Vec<_> = checkpoint.targets.iter().zip(&lives).map(|(target, live)| {
                    let t = &live.tree;
                    let (read, requested) = counts(live);
                    let (queued, running) = if active_tree(live) {
                        (
                            t.level.keys().filter(|k| journal.get(k).is_none() && !busy(k)).count(),
                            t.level.keys().filter(|k| busy(k)).count(),
                        )
                    } else {
                        (0, 0)
                    };
                    json!({"id":target.id,"own":target.own,"generation":target.generation,"priority":target.priority,"hold":target.hold,"out":t.def.out,"round":t.round,"read":read,"requested":requested,"queued":queued,"running":running,"finished":t.finished,"failed":t.failed})
                }).collect();
                let progress = json!({"status":status,"run_id":run_id,"fingerprint":settings.fingerprint,"queued":queue_len,"workers":workers.iter().map(|(id,w)|json!({"id":id,"key":w.busy.as_ref().map(|(k,_)|k),"elapsed_secs":w.busy.as_ref().map(|(_,t)|t.elapsed().as_secs_f64()),"nodes":w.nodes,"depth":w.depth})).collect::<Vec<_>>(),"trees":trees});
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
                let reading = active_tree(live) && tree.level.keys().any(busy);
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
            alerts.flush();
            if stopping && active == 0 {
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
                remove_candidates(&tree.def.out)?;
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
    fn completed_job_clears_restart_before_done_is_received() {
        let control = Arc::new(Mutex::new(Restart {
            key: Some("first".into()),
            requested: true,
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
            };
        }
        assert!(rx.recv().unwrap());
        assert!(!control.lock().unwrap().requested);
    }
}
