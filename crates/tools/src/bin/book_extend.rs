//! YANEURAOU-DB2016 テキスト `.db` の候補集合へエンジン bestmove を追加するツール。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
use rshogi_core::position::Position;
use rshogi_core::types::Move;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tools::common::io::write_atomic;
use tools::progress::MultiFileProgress;
use tools::selfplay::{EngineConfig, EngineProcess};

const BOOK_HEADER: &str = "#YANEURAOU-DB2016 1.00";
const MATE_CAP: i32 = 30_000;

#[derive(Parser, Debug)]
#[command(about = "YANEURAOU-DB2016 テキスト定跡 .db の候補集合へエンジン bestmove を追加する")]
struct Cli {
    #[arg(long)]
    book: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long)]
    engine: PathBuf,
    #[arg(long = "engine-option", num_args = 1)]
    engine_options: Vec<String>,
    #[arg(long, default_value = "nodes 100000")]
    go: String,
    #[arg(long, default_value_t = 1)]
    parallel: usize,
    #[arg(long)]
    journal: PathBuf,
    #[arg(long, default_value_t = false)]
    resume: bool,
    #[arg(long)]
    parent_journal: Option<PathBuf>,
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct BookDb {
    entries: BTreeMap<String, PositionEntry>,
}

#[derive(Debug, Clone)]
struct PositionEntry {
    sfen: String,
    moves: Vec<BookMove>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BookMove {
    move_usi: Option<String>,
    ponder_usi: Option<String>,
    value: i32,
    depth: i32,
    count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum JournalKind {
    Child,
    Parent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalRecord {
    kind: JournalKind,
    sfen: String,
    go: String,
    #[serde(default)]
    engine_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    depth: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bestmove: Option<String>,
}

#[derive(Debug, Clone)]
struct EvalRecord {
    value: i32,
    depth: i32,
}

#[derive(Debug, Clone)]
struct ParentRecord {
    bestmove: Option<String>,
}

#[derive(Debug, Clone)]
struct SearchTask {
    kind: JournalKind,
    key: String,
    position_tail: String,
}

#[derive(Debug, Clone)]
struct SearchResult {
    record: JournalRecord,
}

#[derive(Debug)]
enum WorkerMessage {
    Task(Result<SearchResult>),
    Fatal(anyhow::Error),
}

#[derive(Debug, Default, Clone)]
struct LoadedJournal {
    child: HashMap<String, EvalRecord>,
    parent: HashMap<String, ParentRecord>,
}

#[derive(Debug, Default, Clone)]
struct ParentJournal {
    parent: HashMap<String, ParentRecord>,
}

#[derive(Debug, Clone)]
struct AdditionCandidate {
    child_key: String,
    move_usi: String,
    old_best: Option<i32>,
}

#[derive(Debug, Default, Clone)]
struct ExtensionPlan {
    candidates: BTreeMap<String, AdditionCandidate>,
    skipped_illegal: Vec<SkippedBestmove>,
}

#[derive(Debug, Clone)]
struct SkippedBestmove {
    sfen: String,
    bestmove: String,
    reason: String,
}

#[derive(Debug, Default, Clone)]
struct ReportStats {
    parent_total: u64,
    best_in_book_before: u64,
    best_in_book_after: u64,
    parent_journal_reused: u64,
    parent_searched: u64,
    added_total: u64,
    skipped_illegal: u64,
    added_values: Vec<i32>,
    improvements: Vec<Improvement>,
}

#[derive(Debug, Clone)]
struct Improvement {
    sfen: String,
    move_usi: String,
    old_best: i32,
    added_value: i32,
    diff: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentSource {
    OwnJournal,
    ParentJournal,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    validate_cli(&cli)?;

    rshogi_book::Book::from_path(&cli.book, true)
        .with_context(|| format!("定跡を rshogi-book で読めません: {}", cli.book.display()))?;
    let book = read_book_db(&cli.book)?;
    let engine_fingerprint = engine_fingerprint(&cli.engine, &cli.engine_options)?;
    let mut journal = if cli.resume {
        load_journal(&cli.journal, &cli.go, &engine_fingerprint)?
    } else {
        LoadedJournal::default()
    };
    let parent_journal = load_parent_journal_opt(cli.parent_journal.as_deref())?;

    let parent_tasks = build_parent_tasks(&book, &journal.parent, &parent_journal);
    let parent_searched = parent_tasks.len() as u64;
    if !parent_tasks.is_empty() {
        run_search_tasks(&cli, parent_tasks, &engine_fingerprint, &mut journal)?;
    }

    let plan = build_extension_plan(&book, &journal.parent, &parent_journal)?;
    for skipped in &plan.skipped_illegal {
        eprintln!(
            "警告: 非合法 bestmove をスキップします: sfen={} bestmove={} error={}",
            skipped.sfen, skipped.bestmove, skipped.reason
        );
    }

    let child_tasks = build_child_tasks(&book, &plan, &journal.child)?;
    if !child_tasks.is_empty() {
        run_search_tasks(&cli, child_tasks, &engine_fingerprint, &mut journal)?;
    }

    write_extended_book(&book, &plan, &journal.child, &cli.out)?;
    if let Some(path) = &cli.report {
        let stats = collect_report_stats(
            &book,
            &plan,
            &journal.child,
            &journal.parent,
            &parent_journal,
            parent_searched,
        );
        write_report(&stats, path)?;
    }
    Ok(())
}

fn validate_cli(cli: &Cli) -> Result<()> {
    if cli.parallel == 0 {
        bail!("--parallel は 1 以上を指定してください");
    }
    // --journal は実行中に追記され、--out/--report は完了時に書き出される。
    // どれか同士が衝突しても入出力を破壊するため、全ペアで正準パス一致を拒否する。
    let mut paths: Vec<(&str, &Path)> = vec![
        ("--book", cli.book.as_path()),
        ("--out", cli.out.as_path()),
        ("--journal", cli.journal.as_path()),
    ];
    if let Some(report) = &cli.report {
        paths.push(("--report", report.as_path()));
    }
    for (i, (a_name, a)) in paths.iter().enumerate() {
        for (b_name, b) in &paths[i + 1..] {
            reject_same_canonical_path(a, b, a_name, b_name)?;
        }
    }
    Ok(())
}

fn reject_same_canonical_path(a: &Path, b: &Path, a_name: &str, b_name: &str) -> Result<()> {
    let a_canon = canonicalize_output_collision_path(a)
        .with_context(|| format!("{a_name} の正準化に失敗しました: {}", a.display()))?;
    let b_canon = canonicalize_output_collision_path(b)
        .with_context(|| format!("{b_name} の正準化に失敗しました: {}", b.display()))?;
    if a_canon == b_canon {
        bail!("{a_name} と {b_name} が同じファイルを指しています: {}", a_canon.display());
    }
    Ok(())
}

fn canonicalize_output_collision_path(path: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let parent =
                path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
            let parent = std::fs::canonicalize(parent).with_context(|| {
                format!("親ディレクトリを正準化できません: {}", parent.display())
            })?;
            let file_name = path
                .file_name()
                .ok_or_else(|| anyhow!("ファイル名がありません: {}", path.display()))?;
            Ok(parent.join(file_name))
        }
        Err(err) => Err(err).with_context(|| format!("正準化できません: {}", path.display())),
    }
}

fn read_book_db(path: &Path) -> Result<BookDb> {
    let file = File::open(path).with_context(|| format!("定跡を開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut entries = BTreeMap::<String, PositionEntry>::new();
    let mut current_key: Option<String> = None;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.trim().is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("sfen ") {
            let sfen = rest.trim().to_string();
            let key = sfen.clone();
            entries.entry(key.clone()).or_insert(PositionEntry {
                sfen,
                moves: Vec::new(),
            });
            current_key = Some(key);
            continue;
        }
        let Some(key) = current_key.as_ref() else {
            continue;
        };
        if let Some(book_move) = parse_move_line(line) {
            entries
                .get_mut(key)
                .ok_or_else(|| anyhow!("内部エラー: current_key の entry がありません"))?
                .moves
                .push(book_move);
        }
    }

    Ok(BookDb { entries })
}

fn parse_move_line(line: &str) -> Option<BookMove> {
    let mut tokens = line.split_whitespace();
    let move_usi = tokens.next().map(parse_move_field)?;
    let ponder_usi = tokens.next().map(parse_move_field).unwrap_or(None);
    let value = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let depth = tokens.next().map_or(0, |t| t.parse::<i32>().unwrap_or(0));
    let count = tokens.next().map_or(1, |t| t.parse::<u64>().unwrap_or(0));
    Some(BookMove {
        move_usi,
        ponder_usi,
        value,
        depth,
        count,
    })
}

fn parse_move_field(token: &str) -> Option<String> {
    match token {
        "none" | "None" | "resign" => None,
        other => Some(other.to_string()),
    }
}

fn strip_ply(sfen: &str) -> &str {
    match sfen.rsplit_once(' ') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => head,
        _ => sfen,
    }
}

fn engine_fingerprint(engine_path: &Path, engine_options: &[String]) -> Result<String> {
    let engine_name =
        engine_path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default();
    let engine_bytes = std::fs::read(engine_path)
        .with_context(|| format!("engine binary を読めません: {}", engine_path.display()))?;
    let engine_sha256 = Sha256::digest(&engine_bytes);
    let mut normalized_options: Vec<&str> = engine_options.iter().map(String::as_str).collect();
    normalized_options
        .sort_by(|a, b| engine_option_key(a).cmp(engine_option_key(b)).then_with(|| a.cmp(b)));
    Ok(format!(
        "{engine_name}\tsha256={engine_sha256:x}\t{}",
        normalized_options.join("\n")
    ))
}

fn engine_option_key(option: &str) -> &str {
    option.split_once('=').map_or(option, |(key, _)| key)
}

fn child_position_after_move(parent_sfen: &str, move_usi: &str) -> Result<Position> {
    let mut pos = Position::new();
    pos.set_sfen(parent_sfen)
        .map_err(|e| anyhow!("親局面 SFEN が不正です: {parent_sfen}: {e}"))?;
    let decoded =
        Move::from_usi(move_usi).ok_or_else(|| anyhow!("USI 指し手が不正です: {move_usi}"))?;
    let mv = pos
        .to_move(decoded)
        .ok_or_else(|| anyhow!("局面に適用できない指し手です: {move_usi}: {parent_sfen}"))?;
    if mv == Move::NONE || !pos.pseudo_legal(mv) || !pos.is_legal(mv) {
        bail!("非合法手です: {move_usi}: {parent_sfen}");
    }
    let gives_check = pos.gives_check(mv);
    pos.do_move(mv, gives_check);
    Ok(pos)
}

fn child_key_after_move(parent_sfen: &str, move_usi: &str) -> Result<String> {
    let pos = child_position_after_move(parent_sfen, move_usi)?;
    Ok(strip_ply(&pos.to_sfen()).to_string())
}

fn load_journal(path: &Path, go_args: &str, engine_fingerprint: &str) -> Result<LoadedJournal> {
    if !path.exists() {
        return Ok(LoadedJournal::default());
    }
    let file =
        File::open(path).with_context(|| format!("journal を開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut loaded = LoadedJournal::default();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: JournalRecord = serde_json::from_str(&line).with_context(|| {
            format!("journal JSON が不正です: {}:{}", path.display(), line_no + 1)
        })?;
        if !journal_record_matches(&rec, go_args, engine_fingerprint) {
            continue;
        }
        match rec.kind {
            JournalKind::Child => {
                if let (Some(value), Some(depth)) = (rec.value, rec.depth) {
                    loaded.child.insert(rec.sfen, EvalRecord { value, depth });
                }
            }
            JournalKind::Parent => {
                loaded.parent.insert(
                    rec.sfen,
                    ParentRecord {
                        bestmove: rec.bestmove,
                    },
                );
            }
        }
    }
    Ok(loaded)
}

fn load_parent_journal_opt(path: Option<&Path>) -> Result<ParentJournal> {
    match path {
        Some(path) => load_parent_journal(path),
        None => Ok(ParentJournal::default()),
    }
}

fn load_parent_journal(path: &Path) -> Result<ParentJournal> {
    let file = File::open(path)
        .with_context(|| format!("parent-journal を開けません: {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut loaded = ParentJournal::default();
    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let rec: JournalRecord = serde_json::from_str(&line).with_context(|| {
            format!("parent-journal JSON が不正です: {}:{}", path.display(), line_no + 1)
        })?;
        if rec.kind == JournalKind::Parent {
            loaded.parent.insert(
                strip_ply(&rec.sfen).to_string(),
                ParentRecord {
                    bestmove: rec.bestmove,
                },
            );
        }
    }
    Ok(loaded)
}

fn journal_record_matches(rec: &JournalRecord, go_args: &str, engine_fingerprint: &str) -> bool {
    rec.go == go_args && rec.engine_fingerprint == engine_fingerprint
}

fn build_parent_tasks(
    book: &BookDb,
    own_parent: &HashMap<String, ParentRecord>,
    parent_journal: &ParentJournal,
) -> Vec<SearchTask> {
    let mut tasks = Vec::new();
    let mut seen = HashSet::new();
    for entry in book.entries.values() {
        let parent_key = strip_ply(&entry.sfen);
        if parent_source(parent_key, own_parent, parent_journal).is_some()
            || !seen.insert(parent_key.to_string())
        {
            continue;
        }
        tasks.push(SearchTask {
            kind: JournalKind::Parent,
            key: parent_key.to_string(),
            position_tail: entry.sfen.clone(),
        });
    }
    tasks
}

fn parent_source(
    parent_key: &str,
    own_parent: &HashMap<String, ParentRecord>,
    parent_journal: &ParentJournal,
) -> Option<ParentSource> {
    if own_parent.contains_key(parent_key) {
        Some(ParentSource::OwnJournal)
    } else if parent_journal.parent.contains_key(parent_key) {
        Some(ParentSource::ParentJournal)
    } else {
        None
    }
}

fn parent_record<'a>(
    parent_key: &str,
    own_parent: &'a HashMap<String, ParentRecord>,
    parent_journal: &'a ParentJournal,
) -> Option<&'a ParentRecord> {
    own_parent.get(parent_key).or_else(|| parent_journal.parent.get(parent_key))
}

fn build_extension_plan(
    book: &BookDb,
    own_parent: &HashMap<String, ParentRecord>,
    parent_journal: &ParentJournal,
) -> Result<ExtensionPlan> {
    let mut plan = ExtensionPlan::default();
    for entry in book.entries.values() {
        let parent_key = strip_ply(&entry.sfen).to_string();
        let Some(parent) = parent_record(&parent_key, own_parent, parent_journal) else {
            continue;
        };
        let Some(bestmove) = parent.bestmove.as_deref().filter(|m| *m != "none" && *m != "resign")
        else {
            continue;
        };
        if entry.moves.iter().any(|m| m.move_usi.as_deref() == Some(bestmove)) {
            continue;
        }
        let child_key = match child_key_after_move(&entry.sfen, bestmove) {
            Ok(key) => key,
            Err(err) => {
                plan.skipped_illegal.push(SkippedBestmove {
                    sfen: entry.sfen.clone(),
                    bestmove: bestmove.to_string(),
                    reason: format!("{err:#}"),
                });
                continue;
            }
        };
        plan.candidates.insert(
            entry.sfen.clone(),
            AdditionCandidate {
                child_key,
                move_usi: bestmove.to_string(),
                old_best: entry.moves.iter().map(|m| m.value).max(),
            },
        );
    }
    Ok(plan)
}

fn build_child_tasks(
    book: &BookDb,
    plan: &ExtensionPlan,
    child_records: &HashMap<String, EvalRecord>,
) -> Result<Vec<SearchTask>> {
    let mut tasks = Vec::new();
    let mut seen = HashSet::new();
    for (sfen, candidate) in &plan.candidates {
        if child_records.contains_key(&candidate.child_key) || !seen.insert(&candidate.child_key) {
            continue;
        }
        let entry = book
            .entries
            .get(sfen)
            .ok_or_else(|| anyhow!("内部エラー: entry がありません: {sfen}"))?;
        tasks.push(SearchTask {
            kind: JournalKind::Child,
            key: candidate.child_key.clone(),
            position_tail: format!("{} moves {}", entry.sfen, candidate.move_usi),
        });
    }
    Ok(tasks)
}

fn run_search_tasks(
    cli: &Cli,
    tasks: Vec<SearchTask>,
    engine_fingerprint: &str,
    journal: &mut LoadedJournal,
) -> Result<()> {
    let (task_tx, task_rx) = crossbeam_channel::unbounded::<SearchTask>();
    let (result_tx, result_rx) = crossbeam_channel::unbounded::<WorkerMessage>();
    let task_count = tasks.len();
    for task in tasks {
        task_tx.send(task)?;
    }
    drop(task_tx);

    let engine_options = Arc::new(cli.engine_options.clone());
    let go_args = Arc::new(cli.go.clone());
    let engine_path = Arc::new(cli.engine.clone());
    let engine_fingerprint = Arc::new(engine_fingerprint.to_string());
    let worker_count = cli.parallel;

    let mut handles = Vec::with_capacity(worker_count);
    for worker_id in 0..worker_count {
        let task_rx = task_rx.clone();
        let result_tx = result_tx.clone();
        let engine_options = engine_options.clone();
        let go_args = go_args.clone();
        let engine_path = engine_path.clone();
        let engine_fingerprint = engine_fingerprint.clone();
        handles.push(std::thread::spawn(move || {
            worker_loop(
                worker_id,
                &engine_path,
                &engine_options,
                &go_args,
                &engine_fingerprint,
                task_rx,
                result_tx,
            );
        }));
    }
    drop(result_tx);

    let progress = MultiFileProgress::new(task_count as u64, 1, "book_extend");
    let file_progress = progress.start_file("book", 1, task_count as u64);
    let fp = &file_progress;
    let outcome = (move || -> Result<()> {
        let journal_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&cli.journal)
            .with_context(|| {
                format!("journal を追記オープンできません: {}", cli.journal.display())
            })?;
        let writer = Mutex::new(BufWriter::new(journal_file));

        let mut first_error: Option<anyhow::Error> = None;
        let mut processed_count = 0usize;
        for message in result_rx {
            match message {
                WorkerMessage::Task(Ok(search_result)) => {
                    processed_count += 1;
                    fp.inc(1);
                    append_journal_record(&writer, &search_result.record)?;
                    match search_result.record.kind {
                        JournalKind::Child => {
                            if let (Some(value), Some(depth)) =
                                (search_result.record.value, search_result.record.depth)
                            {
                                journal
                                    .child
                                    .insert(search_result.record.sfen, EvalRecord { value, depth });
                            }
                        }
                        JournalKind::Parent => {
                            journal.parent.insert(
                                search_result.record.sfen,
                                ParentRecord {
                                    bestmove: search_result.record.bestmove,
                                },
                            );
                        }
                    }
                }
                WorkerMessage::Task(Err(err)) => {
                    processed_count += 1;
                    fp.inc(1);
                    if first_error.is_none() {
                        first_error = Some(err);
                    }
                }
                WorkerMessage::Fatal(err) if first_error.is_none() => first_error = Some(err),
                WorkerMessage::Fatal(_) => {}
            }
        }

        for handle in handles {
            handle.join().map_err(|_| anyhow!("worker thread が panic しました"))?;
        }
        if processed_count < task_count {
            return Err(search_tasks_incomplete_error(
                processed_count,
                task_count,
                &cli.journal,
                first_error,
            ));
        }
        if let Some(err) = first_error {
            return Err(err.context(format!(
                "探索中にエラーが発生しました。journal には途中結果が追記済みのため --resume で再開できます: {}",
                cli.journal.display()
            )));
        }
        Ok(())
    })();
    match &outcome {
        Ok(()) => file_progress.finish_with_message("完了"),
        Err(_) => file_progress.abandon_with_message("中断"),
    }
    progress.finish();
    outcome
}

fn worker_loop(
    worker_id: usize,
    engine_path: &Path,
    engine_options: &[String],
    go_args: &str,
    engine_fingerprint: &str,
    task_rx: crossbeam_channel::Receiver<SearchTask>,
    result_tx: crossbeam_channel::Sender<WorkerMessage>,
) {
    let cfg = EngineConfig {
        path: engine_path.to_path_buf(),
        args: Vec::new(),
        threads: 1,
        hash_mb: 256,
        network_delay: None,
        network_delay2: None,
        minimum_thinking_time: None,
        slowmover: None,
        ponder: false,
        usi_options: engine_options.to_vec(),
    };
    let mut engine = match EngineProcess::spawn(&cfg, format!("book_extend-{worker_id}")) {
        Ok(engine) => engine,
        Err(err) => {
            let _ = result_tx.send(WorkerMessage::Fatal(
                err.context(format!("worker {worker_id}: engine 起動に失敗しました")),
            ));
            return;
        }
    };
    let timeout = Duration::from_secs(24 * 60 * 60);
    for task in task_rx {
        let result = search_one(&mut engine, &task, go_args, engine_fingerprint, timeout)
            .with_context(|| {
                format!("worker {worker_id}: {:?} 探索に失敗しました: {}", task.kind, task.key)
            });
        // 失敗後は応答の対応関係を保証できないため、この engine を再利用しない。
        let failed = result.is_err();
        if result_tx.send(WorkerMessage::Task(result)).is_err() || failed {
            break;
        }
    }
}

fn search_tasks_incomplete_error(
    processed_count: usize,
    task_count: usize,
    journal_path: &Path,
    cause: Option<anyhow::Error>,
) -> anyhow::Error {
    let message = format!(
        "探索タスクが未完了です: {processed_count}/{task_count} 件完了。journal には途中結果が追記済みのため --resume で再開できます: {}",
        journal_path.display()
    );
    match cause {
        Some(err) => err.context(message),
        None => anyhow!(message),
    }
}

fn search_one(
    engine: &mut EngineProcess,
    task: &SearchTask,
    go_args: &str,
    engine_fingerprint: &str,
    timeout: Duration,
) -> Result<SearchResult> {
    let outcome = engine.search_raw_go(&task.position_tail, go_args, timeout, None)?;
    if outcome.timed_out {
        bail!("探索がタイムアウトしました: {}", task.key);
    }
    let eval = outcome
        .eval
        .ok_or_else(|| anyhow!("info score が得られませんでした: {}", task.key))?;
    let depth = eval.depth.map(|d| d as i32).unwrap_or(0);
    let record = match task.kind {
        JournalKind::Child => {
            let child_score = score_to_cp(eval.score_cp, eval.score_mate)
                .ok_or_else(|| anyhow!("score cp/mate が得られませんでした: {}", task.key))?;
            JournalRecord {
                kind: JournalKind::Child,
                sfen: task.key.clone(),
                go: go_args.to_string(),
                engine_fingerprint: engine_fingerprint.to_string(),
                value: Some(value_from_child_score(child_score)),
                depth: Some(depth),
                bestmove: None,
            }
        }
        JournalKind::Parent => JournalRecord {
            kind: JournalKind::Parent,
            sfen: task.key.clone(),
            go: go_args.to_string(),
            engine_fingerprint: engine_fingerprint.to_string(),
            value: None,
            depth: Some(depth),
            bestmove: outcome.bestmove,
        },
    };
    Ok(SearchResult { record })
}

fn append_journal_record(writer: &Mutex<BufWriter<File>>, rec: &JournalRecord) -> Result<()> {
    let mut guard = writer.lock().map_err(|_| anyhow!("journal writer mutex poisoned"))?;
    serde_json::to_writer(&mut *guard, rec)?;
    guard.write_all(b"\n")?;
    guard.flush()?;
    Ok(())
}

fn score_to_cp(score_cp: Option<i32>, score_mate: Option<i32>) -> Option<i32> {
    if let Some(cp) = score_cp {
        return Some(cp.clamp(-MATE_CAP, MATE_CAP));
    }
    score_mate.map(mate_to_cp)
}

fn mate_to_cp(mate: i32) -> i32 {
    let ply = mate.saturating_abs().min(MATE_CAP);
    if mate >= 0 {
        (MATE_CAP - ply).clamp(-MATE_CAP, MATE_CAP)
    } else {
        (-MATE_CAP + ply).clamp(-MATE_CAP, MATE_CAP)
    }
}

fn value_from_child_score(child_score: i32) -> i32 {
    -child_score
}

fn write_extended_book(
    book: &BookDb,
    plan: &ExtensionPlan,
    child_records: &HashMap<String, EvalRecord>,
    out: &Path,
) -> Result<()> {
    let mut content = String::new();
    content.push_str(BOOK_HEADER);
    content.push('\n');
    for entry in book.entries.values() {
        writeln!(content, "sfen {}", entry.sfen)?;
        let mut moves = entry.moves.clone();
        if let Some(candidate) = plan.candidates.get(&entry.sfen) {
            let record = child_records.get(&candidate.child_key).ok_or_else(|| {
                anyhow!(
                    "追加手の子局面探索結果が journal にありません: sfen={} move={} child={}",
                    entry.sfen,
                    candidate.move_usi,
                    candidate.child_key
                )
            })?;
            moves.push(BookMove {
                move_usi: Some(candidate.move_usi.clone()),
                ponder_usi: None,
                value: record.value,
                depth: record.depth,
                count: 0,
            });
        }
        moves.sort_by(|a, b| {
            b.count.cmp(&a.count).then_with(|| move_sort_key(a).cmp(move_sort_key(b)))
        });
        for book_move in &moves {
            writeln!(
                content,
                "{} {} {} {} {}",
                book_move.move_usi.as_deref().unwrap_or("none"),
                book_move.ponder_usi.as_deref().unwrap_or("none"),
                book_move.value,
                book_move.depth,
                book_move.count
            )?;
        }
    }
    write_atomic(out, &content).with_context(|| format!("出力を書けません: {}", out.display()))
}

fn move_sort_key(book_move: &BookMove) -> &str {
    book_move.move_usi.as_deref().unwrap_or("none")
}

fn collect_report_stats(
    book: &BookDb,
    plan: &ExtensionPlan,
    child_records: &HashMap<String, EvalRecord>,
    own_parent: &HashMap<String, ParentRecord>,
    parent_journal: &ParentJournal,
    parent_searched: u64,
) -> ReportStats {
    let mut stats = ReportStats {
        parent_searched,
        skipped_illegal: plan.skipped_illegal.len() as u64,
        ..ReportStats::default()
    };
    for entry in book.entries.values() {
        let parent_key = strip_ply(&entry.sfen);
        let Some(parent) = parent_record(parent_key, own_parent, parent_journal) else {
            continue;
        };
        let Some(bestmove) = parent.bestmove.as_deref().filter(|m| *m != "none" && *m != "resign")
        else {
            continue;
        };
        stats.parent_total += 1;
        if parent_source(parent_key, own_parent, parent_journal)
            == Some(ParentSource::ParentJournal)
        {
            stats.parent_journal_reused += 1;
        }
        let before = entry.moves.iter().any(|m| m.move_usi.as_deref() == Some(bestmove));
        if before {
            stats.best_in_book_before += 1;
            stats.best_in_book_after += 1;
            continue;
        }
        if let Some(candidate) = plan.candidates.get(&entry.sfen)
            && let Some(record) = child_records.get(&candidate.child_key)
        {
            stats.best_in_book_after += 1;
            stats.added_total += 1;
            stats.added_values.push(record.value);
            if let Some(old_best) = candidate.old_best {
                let diff = record.value - old_best;
                if diff > 0 {
                    stats.improvements.push(Improvement {
                        sfen: entry.sfen.clone(),
                        move_usi: candidate.move_usi.clone(),
                        old_best,
                        added_value: record.value,
                        diff,
                    });
                }
            }
        }
    }
    stats.improvements.sort_by(|a, b| {
        b.diff
            .cmp(&a.diff)
            .then_with(|| a.sfen.cmp(&b.sfen))
            .then_with(|| a.move_usi.cmp(&b.move_usi))
    });
    stats
}

fn write_report(stats: &ReportStats, path: &Path) -> Result<()> {
    let mut content = String::new();
    writeln!(content, "# book_extend report")?;
    writeln!(content)?;
    writeln!(content, "## coverage")?;
    writeln!(content)?;
    writeln!(content, "| metric | value |")?;
    writeln!(content, "|---|---:|")?;
    writeln!(content, "| parent_total | {} |", stats.parent_total)?;
    writeln!(
        content,
        "| bestmove_in_book_before | {} ({:.6}) |",
        stats.best_in_book_before,
        ratio(stats.best_in_book_before, stats.parent_total)
    )?;
    writeln!(
        content,
        "| bestmove_in_book_after | {} ({:.6}) |",
        stats.best_in_book_after,
        ratio(stats.best_in_book_after, stats.parent_total)
    )?;
    writeln!(content, "| added_total | {} |", stats.added_total)?;
    writeln!(content, "| skipped_illegal_bestmove | {} |", stats.skipped_illegal)?;
    writeln!(content, "| parent_journal_reused | {} |", stats.parent_journal_reused)?;
    writeln!(content, "| parent_searched | {} |", stats.parent_searched)?;
    writeln!(content)?;
    writeln!(content, "## added value distribution")?;
    writeln!(content)?;
    write_value_distribution(&mut content, &stats.added_values)?;
    writeln!(content)?;
    writeln!(content, "## top improvements")?;
    writeln!(content)?;
    writeln!(content, "| rank | diff_cp | added_value | old_best | move | sfen |")?;
    writeln!(content, "|---:|---:|---:|---:|---|---|")?;
    for (idx, item) in stats.improvements.iter().take(20).enumerate() {
        writeln!(
            content,
            "| {} | {} | {} | {} | {} | `{}` |",
            idx + 1,
            item.diff,
            item.added_value,
            item.old_best,
            item.move_usi,
            item.sfen
        )?;
    }
    write_atomic(path, &content).with_context(|| format!("report を書けません: {}", path.display()))
}

fn write_value_distribution(writer: &mut impl FmtWrite, values: &[i32]) -> Result<()> {
    writeln!(writer, "| metric | value |")?;
    writeln!(writer, "|---|---:|")?;
    writeln!(writer, "| count | {} |", values.len())?;
    if values.is_empty() {
        return Ok(());
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    writeln!(writer, "| min | {} |", sorted[0])?;
    writeln!(writer, "| p10 | {} |", percentile(&sorted, 10))?;
    writeln!(writer, "| p25 | {} |", percentile(&sorted, 25))?;
    writeln!(writer, "| p50 | {} |", percentile(&sorted, 50))?;
    writeln!(writer, "| p75 | {} |", percentile(&sorted, 75))?;
    writeln!(writer, "| p90 | {} |", percentile(&sorted, 90))?;
    writeln!(writer, "| max | {} |", sorted[sorted.len() - 1])?;
    Ok(())
}

fn percentile(sorted: &[i32], pct: usize) -> i32 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) * pct) / 100;
    sorted[idx]
}

fn ratio(num: u64, den: u64) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn worker_retires_after_search_error_before_next_task() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mock.sh");
        std::fs::write(
            &path,
            r#"#!/bin/sh
while IFS= read -r line; do
 case "$line" in
 usi) echo usiok ;;
 isready) echo readyok ;;
 go*) echo 'bestmove resign' ;;
 quit) break ;;
 esac
done
"#,
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        for key in ["first", "second"] {
            tx.send(SearchTask {
                kind: JournalKind::Child,
                key: key.into(),
                position_tail: START.into(),
            })
            .unwrap();
        }
        drop(tx);
        let (result_tx, result_rx) = crossbeam_channel::unbounded();
        worker_loop(0, &path, &[], "depth 1", "fixture", rx, result_tx);
        // score がない最初の応答で失敗し、次のタスクは探索しない。
        assert!(matches!(result_rx.try_recv(), Ok(WorkerMessage::Task(Err(_)))));
        assert!(matches!(
            result_rx.try_recv(),
            Err(crossbeam_channel::TryRecvError::Disconnected)
        ));
    }

    use super::*;

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    fn sample_book(moves: Vec<BookMove>) -> BookDb {
        let mut entries = BTreeMap::new();
        entries.insert(
            START.to_string(),
            PositionEntry {
                sfen: START.to_string(),
                moves,
            },
        );
        BookDb { entries }
    }

    fn book_move(move_usi: &str, value: i32, depth: i32, count: u64) -> BookMove {
        BookMove {
            move_usi: Some(move_usi.to_string()),
            ponder_usi: None,
            value,
            depth,
            count,
        }
    }

    fn parent_journal_with(bestmove: &str) -> ParentJournal {
        let mut parent = HashMap::new();
        parent.insert(
            strip_ply(START).to_string(),
            ParentRecord {
                bestmove: Some(bestmove.to_string()),
            },
        );
        ParentJournal { parent }
    }

    fn child_records_for(start_move: &str, value: i32, depth: i32) -> HashMap<String, EvalRecord> {
        let mut records = HashMap::new();
        records
            .insert(child_key_after_move(START, start_move).unwrap(), EvalRecord { value, depth });
        records
    }

    #[test]
    fn db_output_from_same_journal_is_deterministic() {
        let book = sample_book(vec![book_move("2g2f", 10, 3, 8), book_move("5i6h", -5, 2, 3)]);
        let plan =
            build_extension_plan(&book, &HashMap::new(), &parent_journal_with("7g7f")).unwrap();
        let child_records = child_records_for("7g7f", 123, 9);
        let dir = tempfile::tempdir().unwrap();
        let out1 = dir.path().join("a.db");
        let out2 = dir.path().join("b.db");

        write_extended_book(&book, &plan, &child_records, &out1).unwrap();
        write_extended_book(&book, &plan, &child_records, &out2).unwrap();

        let a = std::fs::read_to_string(out1).unwrap();
        let b = std::fs::read_to_string(out2).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn bestmove_absent_is_added_with_zero_count() {
        let book = sample_book(vec![book_move("2g2f", 10, 3, 8)]);
        let plan =
            build_extension_plan(&book, &HashMap::new(), &parent_journal_with("7g7f")).unwrap();
        let child_records = child_records_for("7g7f", 321, 11);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.db");

        write_extended_book(&book, &plan, &child_records, &out).unwrap();

        let text = std::fs::read_to_string(out).unwrap();
        assert!(text.contains("7g7f none 321 11 0\n"));
    }

    #[test]
    fn bestmove_already_present_leaves_node_unchanged() {
        let original = vec![book_move("7g7f", 10, 3, 8), book_move("2g2f", -1, 2, 2)];
        let book = sample_book(original.clone());
        let plan =
            build_extension_plan(&book, &HashMap::new(), &parent_journal_with("7g7f")).unwrap();

        assert!(plan.candidates.is_empty());
        assert_eq!(book.entries[START].moves, original);
    }

    #[test]
    fn existing_move_values_are_preserved_when_adding() {
        let book = sample_book(vec![BookMove {
            move_usi: Some("2g2f".to_string()),
            ponder_usi: Some("8c8d".to_string()),
            value: -77,
            depth: 6,
            count: 42,
        }]);
        let plan =
            build_extension_plan(&book, &HashMap::new(), &parent_journal_with("7g7f")).unwrap();
        let child_records = child_records_for("7g7f", 50, 10);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.db");

        write_extended_book(&book, &plan, &child_records, &out).unwrap();

        let text = std::fs::read_to_string(out).unwrap();
        assert!(text.contains("2g2f 8c8d -77 6 42\n"));
        assert!(text.contains("7g7f none 50 10 0\n"));
    }

    #[test]
    fn parent_journal_reuse_suppresses_parent_search_task() {
        let book = sample_book(vec![book_move("2g2f", 10, 3, 8)]);
        let tasks = build_parent_tasks(&book, &HashMap::new(), &parent_journal_with("7g7f"));
        assert!(tasks.is_empty());
    }

    #[test]
    fn illegal_parent_bestmove_is_skipped() {
        let book = sample_book(vec![book_move("2g2f", 10, 3, 8)]);
        let plan =
            build_extension_plan(&book, &HashMap::new(), &parent_journal_with("9a9b")).unwrap();

        assert!(plan.candidates.is_empty());
        assert_eq!(plan.skipped_illegal.len(), 1);
    }

    #[test]
    fn output_roundtrips_with_rshogi_book() {
        let book = sample_book(vec![book_move("2g2f", 10, 3, 8)]);
        let plan =
            build_extension_plan(&book, &HashMap::new(), &parent_journal_with("7g7f")).unwrap();
        let child_records = child_records_for("7g7f", 123, 9);
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.db");

        write_extended_book(&book, &plan, &child_records, &out).unwrap();

        rshogi_book::Book::from_path(&out, true).unwrap();
    }

    #[test]
    fn value_uses_parent_perspective_by_negating_child_score() {
        assert_eq!(value_from_child_score(123), -123);
        assert_eq!(value_from_child_score(-456), 456);
    }

    #[test]
    fn mate_score_converts_to_capped_cp() {
        assert_eq!(mate_to_cp(1), 29_999);
        assert_eq!(mate_to_cp(-12), -29_988);
        assert_eq!(score_to_cp(Some(40_001), None), Some(30_000));
        assert_eq!(score_to_cp(Some(-40_001), None), Some(-30_000));
    }

    #[test]
    fn engine_fingerprint_changes_for_same_name_different_binary_content() {
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let engine_a = dir_a.path().join("engine");
        let engine_b = dir_b.path().join("engine");
        std::fs::write(&engine_a, b"binary-a").unwrap();
        std::fs::write(&engine_b, b"binary-b").unwrap();

        let fp_a = engine_fingerprint(&engine_a, &["Hash=256".to_string()]).unwrap();
        let fp_b = engine_fingerprint(&engine_b, &["Hash=256".to_string()]).unwrap();

        assert_ne!(fp_a, fp_b);
        assert!(fp_a.contains("sha256="));
        assert!(fp_b.contains("sha256="));
    }

    #[test]
    fn validate_cli_rejects_book_out_and_book_report_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("book.db");
        let engine = dir.path().join("engine");
        std::fs::write(&book, BOOK_HEADER).unwrap();
        std::fs::write(&engine, b"engine").unwrap();

        let cli = Cli {
            book: book.clone(),
            out: book.clone(),
            engine: engine.clone(),
            engine_options: Vec::new(),
            go: "nodes 1".to_string(),
            parallel: 1,
            journal: dir.path().join("journal.jsonl"),
            resume: false,
            parent_journal: None,
            report: None,
        };
        assert!(validate_cli(&cli).is_err());

        let cli = Cli {
            out: dir.path().join("out.db"),
            report: Some(book),
            ..cli
        };
        assert!(validate_cli(&cli).is_err());
    }

    #[test]
    fn validate_cli_rejects_output_path_collisions() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("book.db");
        let engine = dir.path().join("engine");
        std::fs::write(&book, BOOK_HEADER).unwrap();
        std::fs::write(&engine, b"engine").unwrap();

        let make_cli = |out: PathBuf, journal: PathBuf, report: Option<PathBuf>| Cli {
            book: book.clone(),
            out,
            engine: engine.clone(),
            engine_options: Vec::new(),
            go: "nodes 1".to_string(),
            parallel: 1,
            journal,
            resume: false,
            parent_journal: None,
            report,
        };
        let out = dir.path().join("out.db");
        let journal = dir.path().join("journal.jsonl");
        let report = dir.path().join("report.md");

        // 衝突なしは通る。
        let cli = make_cli(out.clone(), journal.clone(), Some(report.clone()));
        assert!(validate_cli(&cli).is_ok());

        // --out == --report: 拡張 .db を report が上書きしてしまうため拒否する。
        let cli = make_cli(out.clone(), journal.clone(), Some(out.clone()));
        assert!(validate_cli(&cli).is_err());

        // --journal == --out: 再開用 journal を出力 book が破壊するため拒否する。
        let cli = make_cli(out.clone(), out.clone(), None);
        assert!(validate_cli(&cli).is_err());

        // --journal == --book: 入力 book へ journal が追記されるため拒否する。
        let cli = make_cli(out.clone(), book.clone(), None);
        assert!(validate_cli(&cli).is_err());

        // --journal == --report: 実行中の journal を report が上書きするため拒否する。
        let cli = make_cli(out, journal.clone(), Some(journal));
        assert!(validate_cli(&cli).is_err());
    }
}
