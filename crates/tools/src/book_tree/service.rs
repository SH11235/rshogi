use super::ServeArgs;
use super::journal::{Journal, LockedFile, Pv, Reading, Settings, digest};
use super::model::{Definition, Tree, remove_candidates};
use crate::common::io::write_atomic;
use crate::selfplay::{EngineConfig, EngineProcess};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
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
    Done(usize, String, Result<Reading>),
    Info(usize, u64, u32),
    Alert(String),
}
struct Worker {
    tx: mpsc::Sender<Option<Job>>,
    handle: JoinHandle<()>,
    restart: Arc<AtomicBool>,
    busy: Option<(String, Instant)>,
    nodes: u64,
    depth: u32,
}

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
    let restart = Arc::new(AtomicBool::new(false));
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
                    if restart_worker.swap(false, Ordering::SeqCst) {
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
                        reading.validate(&settings)?;
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
                    collector.info(&line)?;
                }
            })();
            if result.is_err() {
                engine = None;
            }
            if events.send(Event::Done(id, job.key, result)).is_err() {
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

pub(super) fn serve(args: ServeArgs) -> Result<()> {
    std::fs::create_dir_all(args.state.join("inbox"))?;
    std::fs::create_dir_all(args.state.join("restart"))?;
    let state_lock = LockedFile::open(&args.state.join("lock"))?;
    let manifest = args.state.join("accepted.json");
    if manifest.exists() {
        let accepted: Vec<Accepted> = serde_json::from_str(&std::fs::read_to_string(manifest)?)?;
        for entry in accepted {
            remove_candidates(&entry.out)?;
        }
    }
    let result = serve_locked(&args);
    if let Err(error) = &result {
        write_atomic(&args.state.join("STATUS"), "FAIL\n")?;
        alert(&args.state, &format!("FAIL {error:#}"))?;
    }
    drop(state_lock);
    result
}

fn alert(state: &std::path::Path, message: &str) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("ALERT"))?;
    writeln!(file, "{} {message}", chrono::Utc::now().to_rfc3339())?;
    Ok(())
}

fn serve_locked(args: &ServeArgs) -> Result<()> {
    if args.stall_secs == 0 {
        bail!("stall-secs は正数が必要です");
    }
    let settings = Settings::new(&args.reading)?;
    println!("fingerprint={}", settings.fingerprint);
    let journal_path = args.journal.clone().unwrap_or_else(|| args.state.join("journal.jsonl"));
    let mut journal = Journal::open(&journal_path)?;
    let mut cache = journal.records_for(&settings.fingerprint);
    let manifest = args.state.join("accepted.json");
    let mut accepted: Vec<Accepted> = if manifest.exists() {
        serde_json::from_str(&std::fs::read_to_string(&manifest)?)?
    } else {
        Vec::new()
    };
    let mut trees = Vec::new();
    let mut outputs = BTreeSet::new();
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
        let tree = Tree::load(definition, &settings)?;
        if !outputs.insert(std::fs::canonicalize(&tree.def.out)?) {
            bail!("出力先が重複しています");
        }
        trees.push(tree);
    }
    let workers_file = args.state.join("workers");
    if !workers_file.exists() {
        write_atomic(&workers_file, &args.workers.to_string())?;
    }
    let (events_tx, events_rx) = mpsc::channel();
    let mut workers: BTreeMap<usize, Worker> = BTreeMap::new();
    let mut retries: BTreeMap<String, usize> = BTreeMap::new();
    let mut rejected = BTreeSet::new();
    let result = (|| -> Result<()> {
        loop {
            let stopping = args.state.join("stop").exists();
            let desired: usize = match std::fs::read_to_string(&workers_file)
                .context("workers を読めません")
                .and_then(|s| Ok(s.trim().parse()?))
            {
                Ok(n) => n,
                Err(e) => {
                    alert(&args.state, &format!("workers が不正です: {e}"))?;
                    workers.len()
                }
            };
            let desired = if stopping { 0 } else { desired };
            let mut entries = std::fs::read_dir(args.state.join("inbox"))?
                .collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|e| e.file_name());
            if !stopping {
                for entry in entries {
                    let path = std::fs::canonicalize(entry.path())?;
                    if path.extension().is_none_or(|e| e != "toml")
                        || accepted.iter().any(|a| a.path == path)
                        || rejected.contains(&path)
                    {
                        continue;
                    }
                    let loaded = (|| -> Result<Tree> {
                        let definition = Definition::load(&path)?;
                        std::fs::create_dir_all(&definition.out)?;
                        let out = std::fs::canonicalize(&definition.out)?;
                        if outputs.contains(&out) {
                            bail!("木の出力先が重複しています");
                        }
                        let tree = Tree::load(definition, &settings)?;
                        outputs.insert(out);
                        Ok(tree)
                    })();
                    match loaded {
                        Ok(tree) => {
                            accepted.push(Accepted {
                                out: tree.def.out.clone(),
                                sha256: digest(&path)?,
                                paths_sha256: digest(&tree.def.paths)?,
                                fingerprint: settings.fingerprint.clone(),
                                path,
                            });
                            write_atomic(&manifest, &serde_json::to_string_pretty(&accepted)?)?;
                            write_atomic(&tree.def.out.join("STATUS"), "QUEUED\n")?;
                            trees.push(tree);
                        }
                        Err(e) => {
                            alert(&args.state, &format!("木の受付失敗 {}: {e:#}", path.display()))?;
                            rejected.insert(path);
                        }
                    }
                }
            }
            while let Ok(event) = events_rx.try_recv() {
                match event {
                    Event::Alert(message) => alert(&args.state, &message)?,
                    Event::Info(id, nodes, depth) => {
                        if let Some(worker) = workers.get_mut(&id) {
                            worker.nodes = nodes;
                            worker.depth = depth;
                        }
                    }
                    Event::Done(id, key, result) => {
                        if let Some(worker) = workers.get_mut(&id) {
                            worker.busy = None;
                        }
                        match result {
                            Ok(reading) => {
                                journal.append(reading.clone(), &settings, "engine".into())?;
                                cache.insert(key, reading);
                            }
                            Err(error) => {
                                let attempts = retries.entry(key.clone()).or_default();
                                *attempts += 1;
                                alert(
                                    &args.state,
                                    &format!(
                                        "RESTART worker={id} attempt={attempts} key={key}: {error:#}"
                                    ),
                                )?;
                                if *attempts > args.max_restarts {
                                    bail!("engine の再起動上限: {key}");
                                }
                            }
                        }
                    }
                }
            }
            for (id, worker) in &workers {
                let restart = args.state.join("restart").join(id.to_string());
                if restart.exists() {
                    if worker.busy.is_some() {
                        worker.restart.store(true, Ordering::SeqCst);
                    }
                    std::fs::remove_file(restart)?;
                }
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
                        if tree.finished {
                            if let Err(error) = tree.write_outputs() {
                                tree.failed = true;
                                remove_candidates(&tree.def.out)?;
                                write_atomic(&tree.def.out.join("STATUS"), "FAIL\n")?;
                                alert(&args.state, &format!("出力失敗: {error:#}"))?;
                            }
                        } else {
                            write_atomic(&tree.def.out.join("STATUS"), "RUNNING\n")?;
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
                        write_atomic(&tree.def.out.join("STATUS"), "FAIL\n")?;
                        alert(&args.state, &format!("木の失敗 {}: {e:#}", tree.def.out.display()))?;
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
                        worker.busy = Some((key.clone(), Instant::now()));
                        worker.nodes = 0;
                        worker.depth = 0;
                        worker.tx.send(Some(Job { key, ply }))?;
                        queue_len -= 1;
                    }
                }
            }
            let active = workers.values().filter(|w| w.busy.is_some()).count();
            let status = if stopping && active == 0 {
                "STOPPED"
            } else if active == 0 && queue_len == 0 {
                "IDLE"
            } else {
                "RUNNING"
            };
            write_atomic(&args.state.join("STATUS"), &format!("{status}\n"))?;
            let progress = json!({"status":status,"fingerprint":settings.fingerprint,"queued":queue_len,"workers":workers.iter().map(|(id,w)|json!({"id":id,"key":w.busy.as_ref().map(|(k,_)|k),"elapsed_secs":w.busy.as_ref().map(|(_,t)|t.elapsed().as_secs_f64()),"nodes":w.nodes,"depth":w.depth})).collect::<Vec<_>>(),"trees":trees.iter().enumerate().map(|(id,t)|json!({"id":id,"out":t.def.out,"round":t.round,"read":t.nodes.len(),"requested":t.level.len(),"queued":t.level.keys().filter(|k|!cache.contains_key(*k)&&!workers.values().any(|w|w.busy.as_ref().is_some_and(|(key,_)|key==*k))).count(),"running":t.level.keys().filter(|k|workers.values().any(|w|w.busy.as_ref().is_some_and(|(key,_)|key==*k))).count(),"finished":t.finished,"failed":t.failed})).collect::<Vec<_>>()});
            write_atomic(
                &args.state.join("progress.json"),
                &serde_json::to_string_pretty(&progress)?,
            )?;
            if stopping && active == 0 {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    })();
    if result.is_err() {
        for tree in &trees {
            if !tree.finished {
                remove_candidates(&tree.def.out)?;
                write_atomic(&tree.def.out.join("STATUS"), "FAIL\n")?;
            }
        }
    }
    for worker in workers.into_values() {
        let _ = worker.tx.send(None);
        if result.is_err() {
            worker.restart.store(true, Ordering::SeqCst);
        }
        let _ = worker.handle.join();
    }
    result
}
