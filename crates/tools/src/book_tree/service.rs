use super::ServeArgs;
use super::journal::{Journal, LockedFile, Pv, Reading, Settings, digest};
use super::model::{Definition, Tree, remove_candidates};
use crate::common::io::write_atomic;
use crate::selfplay::{EngineConfig, EngineProcess};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
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
                    let process = EngineProcess::spawn(&cfg, format!("tree-{id}"))?;
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

#[derive(Serialize, Deserialize)]
struct Accepted {
    out: PathBuf,
    path: PathBuf,
    sha256: String,
    paths_sha256: String,
    fingerprint: String,
}

struct LoadedTree {
    tree: Tree,
    accepted: Accepted,
    lock: LockedFile,
}

pub(super) fn serve(args: ServeArgs) -> Result<()> {
    let journal_path = args.journal.clone().unwrap_or_else(|| args.state.join("journal.jsonl"));
    super::paths::reject_collisions(&journal_path, Some(&args.state), &[])?;
    std::fs::create_dir_all(args.state.join("inbox"))?;
    std::fs::create_dir_all(args.state.join("restart"))?;
    let state_lock = LockedFile::open(&args.state.join("lock"))?;
    let result = serve_locked(&args);
    if let Err(error) = &result {
        monitor(&args.state.join("STATUS"), "FAIL\n");
        alert(&args.state, &format!("FAIL {error:#}"));
    }
    drop(state_lock);
    result
}

pub(super) fn monitor(path: &std::path::Path, contents: &str) {
    if let Err(error) = write_atomic(path, contents) {
        eprintln!("監視ファイル書き込み失敗 {}: {error:#}", path.display());
    }
}

fn monitor_changed(path: &std::path::Path, contents: &str) {
    if std::fs::read_to_string(path).is_ok_and(|current| current == contents) {
        return;
    }
    monitor(path, contents);
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

fn serve_locked(args: &ServeArgs) -> Result<()> {
    if args.stall_secs == 0 {
        bail!("stall-secs は正数が必要です");
    }
    let settings = Settings::new(&args.reading)?;
    println!("fingerprint={}", settings.fingerprint);
    let journal_path = args.journal.clone().unwrap_or_else(|| args.state.join("journal.jsonl"));
    let manifest = args.state.join("accepted.json");
    let mut accepted: Vec<Accepted> = if manifest.exists() {
        serde_json::from_str(&std::fs::read_to_string(&manifest)?)?
    } else {
        Vec::new()
    };
    let mut trees = Vec::new();
    let mut output_locks = Vec::new();
    let mut definitions = Vec::new();
    for entry in &accepted {
        if digest(&entry.path)? != entry.sha256 {
            bail!("受付済みの木の定義は変更できません: {}", entry.path.display());
        }
        let definition = Definition::load(&entry.path)?;
        if digest(&definition.paths)? != entry.paths_sha256
            || settings.fingerprint != entry.fingerprint
        {
            bail!("受付済みの paths または読み条件が違います");
        }
        definitions.push(definition);
    }
    super::paths::reject_collisions(
        &journal_path,
        Some(&args.state),
        &definitions.iter().collect::<Vec<_>>(),
    )?;
    let mut journal = Journal::open(&journal_path)?;
    let mut cache = journal.records_for(&settings.fingerprint);
    for definition in definitions {
        output_locks.push(LockedFile::open(&definition.out.join("tree.lock"))?);
        let tree = Tree::load(definition, &settings)?;
        trees.push(tree);
    }
    let workers_file = args.state.join("workers");
    if !workers_file.exists() {
        write_atomic(&workers_file, &args.workers.to_string())?;
    }
    let (events_tx, events_rx) = mpsc::channel();
    let mut workers: BTreeMap<usize, Worker> = BTreeMap::new();
    let mut retries: BTreeMap<String, usize> = BTreeMap::new();
    let mut rejected = BTreeMap::new();
    let mut workers_error = None;
    let mut failures = BTreeMap::new();
    let mut alerts = Alerts {
        state: &args.state,
        pending: VecDeque::new(),
    };
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
            let mut entries = std::fs::read_dir(args.state.join("inbox"))?
                .collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|e| e.file_name());
            if !stopping {
                for entry in entries {
                    let original = entry.path();
                    if original.extension().is_none_or(|e| e != "toml") {
                        continue;
                    }
                    let stamp = file_stamp(&original);
                    if rejected.get(&original) == Some(&stamp) {
                        continue;
                    }
                    let loaded = (|| -> Result<Option<LoadedTree>> {
                        let path = std::fs::canonicalize(&original)?;
                        if accepted.iter().any(|a| a.path == path) {
                            return Ok(None);
                        }
                        let definition = Definition::load(&path)?;
                        let mut definitions: Vec<_> = trees.iter().map(|t| &t.def).collect();
                        definitions.push(&definition);
                        super::paths::reject_collisions(
                            &journal_path,
                            Some(&args.state),
                            &definitions,
                        )?;
                        let lock = LockedFile::open(&definition.out.join("tree.lock"))?;
                        let sha = digest(&path)?;
                        let paths_sha = digest(&definition.paths)?;
                        let tree = Tree::load(definition, &settings)?;
                        Ok(Some(LoadedTree {
                            accepted: Accepted {
                                out: tree.def.out.clone(),
                                path,
                                sha256: sha,
                                paths_sha256: paths_sha,
                                fingerprint: settings.fingerprint.clone(),
                            },
                            tree,
                            lock,
                        }))
                    })();
                    match loaded {
                        Ok(Some(LoadedTree {
                            tree,
                            accepted: entry,
                            lock,
                        })) => {
                            output_locks.push(lock);
                            rejected.remove(&original);
                            accepted.push(entry);
                            write_atomic(&manifest, &serde_json::to_string_pretty(&accepted)?)?;
                            monitor(&tree.def.out.join("STATUS"), "QUEUED\n");
                            trees.push(tree);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            alerts.push(&format!("木の受付失敗 {}: {e:#}", original.display()));
                            rejected.insert(original, stamp);
                        }
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
                                journal.append(reading.clone(), &settings, "engine".into())?;
                                cache.insert(key, reading);
                            }
                            Err(error) => {
                                if explicit {
                                    alerts.push(&format!(
                                        "制御ファイルによる RESTART worker={id} key={key}"
                                    ));
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
            let mut queued: BTreeMap<String, (usize, usize, u32)> = BTreeMap::new();
            for (order, tree) in trees.iter_mut().enumerate() {
                if tree.finished || tree.failed {
                    continue;
                }
                let step = tree.advance(&cache);
                match step {
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
                                if !running.contains(&key) {
                                    queued.entry(key.clone()).or_insert((
                                        order,
                                        depth,
                                        tree.ply[&key],
                                    ));
                                }
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
            queue.sort_by(|(ka, (oa, da, _)), (kb, (ob, db, _))| (oa, da, ka).cmp(&(ob, db, kb)));
            let mut queue_len = queue.len();
            if !stopping {
                let mut jobs = queue.into_iter();
                for worker in workers.values_mut().filter(|w| w.busy.is_none()) {
                    if let Some((key, (_, _, ply))) = jobs.next() {
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
            monitor_changed(&args.state.join("STATUS"), &format!("{status}\n"));
            let progress = json!({"status":status,"fingerprint":settings.fingerprint,"queued":queue_len,"workers":workers.iter().map(|(id,w)|json!({"id":id,"key":w.busy.as_ref().map(|(k,_)|k),"elapsed_secs":w.busy.as_ref().map(|(_,t)|t.elapsed().as_secs_f64()),"nodes":w.nodes,"depth":w.depth})).collect::<Vec<_>>(),"trees":trees.iter().enumerate().map(|(id,t)|json!({"id":id,"out":t.def.out,"round":t.round,"read":t.nodes.len(),"requested":t.level.len(),"queued":t.level.keys().filter(|k|!cache.contains_key(*k)&&!workers.values().any(|w|w.busy.as_ref().is_some_and(|(key,_)|key==*k))).count(),"running":t.level.keys().filter(|k|workers.values().any(|w|w.busy.as_ref().is_some_and(|(key,_)|key==*k))).count(),"finished":t.finished,"failed":t.failed})).collect::<Vec<_>>()});
            monitor(&args.state.join("progress.json"), &serde_json::to_string_pretty(&progress)?);
            for tree in &trees {
                let tree_status = if tree.failed {
                    "FAIL"
                } else if tree.finished {
                    if tree.ready { "DONE" } else { "NOT_READY" }
                } else if desired == 0
                    && !tree.level.keys().any(|key| {
                        workers.values().any(|w| w.busy.as_ref().is_some_and(|(k, _)| k == key))
                    })
                {
                    "STOPPED"
                } else if tree.nodes.is_empty()
                    && !tree.level.keys().any(|key| {
                        workers.values().any(|w| w.busy.as_ref().is_some_and(|(k, _)| k == key))
                    })
                {
                    "QUEUED"
                } else {
                    "RUNNING"
                };
                if !(tree.finished || tree.failed) || tree.def.out.is_dir() {
                    monitor_changed(&tree.def.out.join("STATUS"), &format!("{tree_status}\n"));
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
        for tree in &trees {
            if !tree.finished && !tree.failed {
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
fn file_stamp(path: &std::path::Path) -> String {
    format!(
        "{:?}:{:?}",
        std::fs::symlink_metadata(path).map(|m| (m.modified().ok(), m.len())),
        std::fs::read(path)
    )
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
