//! serve の進みと、後続の段で増える局面を含む残り時間の推定。

use super::{
    journal::Record,
    targets::{CHECKPOINT, Checkpoint, Line, QUEUE, STATUS_FILE, StatusFile, TargetStatus},
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

const CHUNK: usize = 64 * 1024;
const MAX_LINE: usize = 8 * 1024 * 1024;
const SAMPLES: usize = 400;

#[derive(Args, Debug)]
pub(super) struct StatusArgs {
    /// serve の状態 dir (繰り返し可)
    #[arg(long, required = true)]
    state: Vec<PathBuf>,
    /// state と同じ順序の台帳 (指定する場合は同数)
    #[arg(long)]
    journal: Vec<PathBuf>,
    #[arg(long)]
    json: bool,
    /// progress.json がこの時間より古い台を合計から除外する
    #[arg(long, default_value_t = 2.0)]
    stale_hours: f64,
}

#[derive(Deserialize)]
struct Progress {
    workers: Vec<Worker>,
}

#[derive(Deserialize)]
struct Worker {
    key: Option<String>,
}

struct Snapshot {
    status: StatusFile,
    depths: BTreeMap<(String, String, u32), usize>,
    notice: Option<String>,
    progress: Progress,
    stale: bool,
    rate: Option<f64>,
    nps_per_engine: Option<f64>,
}

#[derive(Default, Serialize, Debug)]
struct Row {
    kind: String,
    state: Option<String>,
    status: String,
    reason: Option<String>,
    workers_busy: usize,
    workers_total: usize,
    trees_running: usize,
    trees_waiting: usize,
    trees_done: usize,
    not_ready: usize,
    trees_held: usize,
    positions_read: usize,
    positions_required: usize,
    remaining_visible: usize,
    remaining_mean: f64,
    remaining_p80: f64,
    positions_per_hour: Option<f64>,
    nps_per_engine: Option<f64>,
    nps_total: Option<f64>,
    hours_until_idle_mean: Option<f64>,
    hours_until_idle_p80: Option<f64>,
    depth_histogram: BTreeMap<String, usize>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    serde_json::from_reader(BufReader::new(
        File::open(path).with_context(|| format!("開けません: {}", path.display()))?,
    ))
    .with_context(|| format!("JSON を読めません: {}", path.display()))
}

fn snapshot(
    state: &Path,
    journal: Option<&Path>,
    stale_hours: f64,
    now: SystemTime,
) -> Result<Snapshot> {
    let status: StatusFile = read_json(&state.join(STATUS_FILE))?;
    let (depths, notice) = match Checkpoint::load(&state.join(CHECKPOINT))
        .and_then(|checkpoint| checkpoint.context("accepted.json がありません"))
    {
        Ok(checkpoint) => (
            checkpoint
                .targets
                .into_iter()
                .map(|t| ((t.id, t.own, t.generation), t.definition.depth))
                .collect(),
            None,
        ),
        Err(error) => {
            let reason = format!("accepted.json を使えません: {error:#}");
            let fallback = queue_depths(&state.join(QUEUE));
            let (fallback, reason) = match fallback {
                Ok(depths) => (depths, reason),
                Err(error) => (BTreeMap::new(), format!("{reason}; {error:#}")),
            };
            let depths = status
                .targets
                .iter()
                .filter_map(|t| fallback.get(&t.id).map(|&depth| (tree_key(t), depth)))
                .collect();
            (
                depths,
                Some(format!("{reason}; depth は targets.jsonl の近似値 (取得できない木は不明)")),
            )
        }
    };
    let progress_path = state.join("progress.json");
    let progress: Progress = read_json(&progress_path)?;
    let age = now
        .duration_since(
            std::fs::metadata(&progress_path)
                .and_then(|metadata| metadata.modified())
                .with_context(|| format!("更新時刻を読めません: {}", progress_path.display()))?,
        )
        .unwrap_or_default();
    let rate = journal
        .map(|path| journal_rate(path, progress.workers.len()))
        .transpose()?
        .unwrap_or_default();
    Ok(Snapshot {
        status,
        depths,
        notice,
        progress,
        stale: age.as_secs_f64() > stale_hours * 3600.0,
        rate: rate.positions_per_hour,
        nps_per_engine: rate.nps_per_engine,
    })
}

fn tree_key(tree: &TargetStatus) -> (String, String, u32) {
    (tree.id.clone(), tree.own.clone(), tree.generation)
}

fn queue_depths(path: &Path) -> Result<BTreeMap<String, usize>> {
    let file = File::open(path).with_context(|| format!("開けません: {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut bytes = Vec::new();
    let mut depths = BTreeMap::new();
    loop {
        bytes.clear();
        let size = Read::take(&mut reader, (MAX_LINE + 1) as u64)
            .read_until(b'\n', &mut bytes)
            .with_context(|| format!("読めません: {}", path.display()))?;
        if size > MAX_LINE {
            if !bytes.ends_with(b"\n") {
                reader.skip_until(b'\n')?;
            }
            continue;
        }
        if size == 0 || !bytes.ends_with(b"\n") {
            break;
        }
        if let Ok(line) = serde_json::from_slice::<Line>(&bytes)
            && line.check().is_ok()
        {
            depths.entry(line.id).or_insert(line.depth);
        }
    }
    Ok(depths)
}

#[derive(Default, Debug, PartialEq)]
struct JournalRates {
    positions_per_hour: Option<f64>,
    nps_per_engine: Option<f64>,
}

fn journal_rate(path: &Path, workers: usize) -> Result<JournalRates> {
    let mut file = File::open(path).with_context(|| format!("開けません: {}", path.display()))?;
    tail_rate(&mut file, workers)
}

fn tail_rate<R: Read + Seek>(file: &mut R, workers: usize) -> Result<JournalRates> {
    let mut position = file.seek(SeekFrom::End(0))?;
    let mut suffix = Vec::new();
    let mut count = 0;
    let mut seconds = 0.0;
    let mut nps = Vec::with_capacity(SAMPLES);
    while position > 0 {
        let size = position.min(CHUNK as u64) as usize;
        position -= size as u64;
        file.seek(SeekFrom::Start(position))?;
        let mut buffer = vec![0; size];
        file.read_exact(&mut buffer)?;
        buffer.extend_from_slice(&suffix);
        let mut end = buffer.len();
        for start in (0..buffer.len()).rev().filter(|&i| buffer[i] == b'\n') {
            let line = &buffer[start + 1..end];
            if add_duration(line, &mut count, &mut seconds, &mut nps)? && count == SAMPLES {
                return Ok(rate(workers, count, seconds, nps));
            }
            end = start;
        }
        ensure!(end <= MAX_LINE, "台帳の行が上限 {MAX_LINE} bytes を超えています");
        suffix = buffer[..end].to_vec();
    }
    add_duration(&suffix, &mut count, &mut seconds, &mut nps)?;
    Ok(rate(workers, count, seconds, nps))
}

fn add_duration(
    line: &[u8],
    count: &mut usize,
    seconds: &mut f64,
    nps: &mut Vec<f64>,
) -> Result<bool> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(false);
    }
    ensure!(line.len() <= MAX_LINE, "台帳の行が上限 {MAX_LINE} bytes を超えています");
    let Ok(record) = serde_json::from_slice::<Record>(line) else {
        return Ok(false);
    };
    if record.provenance == "engine" && record.reading.sec.is_finite() && record.reading.sec >= 0.0
    {
        *count += 1;
        *seconds += record.reading.sec;
        if let Some(nodes) = record.reading.nodes
            && record.reading.sec > 0.0
        {
            let sample = nodes as f64 / record.reading.sec;
            if sample.is_finite() {
                nps.push(sample);
            }
        }
        return Ok(true);
    }
    Ok(false)
}

fn rate(workers: usize, count: usize, seconds: f64, mut nps: Vec<f64>) -> JournalRates {
    nps.sort_unstable_by(f64::total_cmp);
    let nps_per_engine = (!nps.is_empty()).then(|| {
        let middle = nps.len() / 2;
        if nps.len().is_multiple_of(2) {
            nps[middle - 1] / 2.0 + nps[middle] / 2.0
        } else {
            nps[middle]
        }
    });
    JournalRates {
        positions_per_hour: (count > 0 && seconds > 0.0)
            .then(|| workers as f64 * 3600.0 * count as f64 / seconds),
        nps_per_engine,
    }
}

#[derive(Default)]
struct Samples {
    reads: Vec<usize>,
    suffix_sums: Vec<f64>,
}

impl Samples {
    fn prepare(&mut self) {
        self.reads.sort_unstable();
        self.suffix_sums = vec![0.0; self.reads.len() + 1];
        for i in (0..self.reads.len()).rev() {
            self.suffix_sums[i] = self.suffix_sums[i + 1] + self.reads[i] as f64;
        }
    }

    fn estimate(&self, required: usize) -> Option<(f64, f64)> {
        let first = self.reads.partition_point(|&read| read < required);
        let count = self.reads.len() - first;
        (count >= 10).then(|| {
            (self.suffix_sums[first] / count as f64, self.reads[first + count * 4 / 5] as f64)
        })
    }
}

type Pool = BTreeMap<(usize, Option<bool>), Samples>;

fn pool(snapshots: &[Result<Snapshot>]) -> Pool {
    let mut pool = Pool::new();
    let mut unique = BTreeMap::new();
    for snapshot in snapshots.iter().filter_map(|s| s.as_ref().ok()) {
        for tree in &snapshot.status.targets {
            if tree.state == "DONE" {
                let depth = snapshot.depths.get(&tree_key(tree)).copied();
                let sample = unique.entry((&tree.id, &tree.own)).or_insert((tree, depth));
                if tree.read > sample.0.read {
                    *sample = (tree, depth);
                }
            }
        }
    }
    for (tree, depth) in unique.into_values() {
        let Some(depth) = depth else { continue };
        for generation in [Some(tree.generation >= 2), None] {
            pool.entry((depth, generation)).or_default().reads.push(tree.read);
        }
    }
    for samples in pool.values_mut() {
        samples.prepare();
    }
    pool
}

fn estimate(tree: &TargetStatus, depth: Option<usize>, pool: &Pool) -> (f64, f64) {
    let expected = depth.and_then(|depth| {
        [Some(tree.generation >= 2), None].into_iter().find_map(|generation| {
            pool.get(&(depth, generation))
                .and_then(|samples| samples.estimate(tree.required))
        })
    });
    let (mean, p80) = expected.unwrap_or((tree.required as f64, tree.required as f64));
    let remaining =
        |expected: f64| (expected.max(tree.required as f64) - tree.read as f64).max(0.0);
    (remaining(mean), remaining(p80))
}

fn row(path: &Path, snapshot: &Result<Snapshot>, pool: &Pool) -> Row {
    let mut row = Row {
        kind: "state".into(),
        state: Some(path.display().to_string()),
        status: "unreadable".into(),
        ..Row::default()
    };
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            row.reason = Some(format!("{error:#}"));
            return row;
        }
    };
    row.status = if snapshot.stale { "stale" } else { "ok" }.into();
    row.reason = snapshot.notice.clone();
    row.workers_total = snapshot.progress.workers.len();
    row.workers_busy = snapshot.progress.workers.iter().filter(|w| w.key.is_some()).count();
    row.positions_per_hour = snapshot.rate;
    row.nps_per_engine = snapshot.nps_per_engine;
    row.nps_total = snapshot.nps_per_engine.map(|nps| nps * row.workers_total as f64);
    for tree in &snapshot.status.targets {
        match tree.state.as_str() {
            "RUNNING" => row.trees_running += 1,
            "QUEUED" => row.trees_waiting += 1,
            "NOT_READY" => row.not_ready += 1,
            "DONE" => row.trees_done += 1,
            "HELD" => row.trees_held += 1,
            _ => (),
        }
        if !matches!(tree.state.as_str(), "RUNNING" | "QUEUED") {
            continue;
        }
        row.positions_read += tree.read;
        row.positions_required += tree.required;
        row.remaining_visible += tree.required.saturating_sub(tree.read);
        let depth = snapshot.depths.get(&tree_key(tree)).copied();
        *row.depth_histogram
            .entry(depth.map_or_else(|| "不明".into(), |d| d.to_string()))
            .or_default() += 1;
        let (mean, p80) = estimate(tree, depth, pool);
        row.remaining_mean += mean;
        row.remaining_p80 += p80;
    }
    set_hours(&mut row);
    row
}

fn set_hours(row: &mut Row) {
    let rate = row.positions_per_hour.filter(|r| *r > 0.0);
    row.hours_until_idle_mean = rate.map(|r| row.remaining_mean / r);
    row.hours_until_idle_p80 = rate.map(|r| row.remaining_p80 / r);
}

fn total(rows: &[Row]) -> Row {
    let mut total = Row {
        kind: "total".into(),
        status: "empty".into(),
        ..Row::default()
    };
    let mut complete_rate = true;
    let mut sum_rate = 0.0;
    let mut complete_nps = true;
    let mut sum_nps = 0.0;
    for row in rows.iter().filter(|r| r.status == "ok") {
        total.status = "ok".into();
        total.not_ready += row.not_ready;
        total.workers_busy += row.workers_busy;
        total.workers_total += row.workers_total;
        total.trees_running += row.trees_running;
        total.trees_waiting += row.trees_waiting;
        total.trees_done += row.trees_done;
        total.trees_held += row.trees_held;
        total.positions_read += row.positions_read;
        total.positions_required += row.positions_required;
        total.remaining_visible += row.remaining_visible;
        total.remaining_mean += row.remaining_mean;
        total.remaining_p80 += row.remaining_p80;
        for (depth, count) in &row.depth_histogram {
            *total.depth_histogram.entry(depth.clone()).or_default() += count;
        }
        if let Some(nps) = row.nps_total {
            sum_nps += nps;
        } else {
            complete_nps = false;
        }
        if let Some(rate) = row.positions_per_hour {
            sum_rate += rate;
        } else {
            complete_rate = false;
        }
    }
    total.positions_per_hour =
        (complete_rate && rows.iter().any(|r| r.status == "ok")).then_some(sum_rate);
    total.nps_total = (complete_nps && total.status == "ok").then_some(sum_nps);
    total.nps_per_engine = total
        .nps_total
        .filter(|_| total.workers_total > 0)
        .map(|nps| nps / total.workers_total as f64);
    set_hours(&mut total);
    total
}

fn render(rows: &[Row], json: bool, mut out: impl Write) -> Result<()> {
    if !json {
        writeln!(
            out,
            "| 台 (state) | 状態 | engine 稼働/全 | 木 読み中/待ち/終わった (うち読めずに終わった)/保留 | 局面 読んだ/必要 | 残り 見えている分 | 残り 伸びを見込む 平均/80%点 | 1 時間に読む局面 | 1 engine の NPS | 合計 NPS | 手が空くまで 平均/80%点 (時間) | 深さ×木の数 |"
        )?;
        writeln!(out, "|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|")?;
    }
    let number = |n: Option<f64>| n.map_or_else(|| "-".into(), |v| format!("{v:.1}"));
    let nps = |n: Option<f64>| n.map_or_else(|| "-".into(), |v| format!("{:.2}M", v / 1e6));
    let escape = |s: &str| s.replace('|', "\\|").replace(['\n', '\r'], " ");
    for row in rows {
        if json {
            serde_json::to_writer(&mut out, row)?;
            writeln!(out)?;
            continue;
        }
        let status = row
            .reason
            .as_ref()
            .map_or_else(|| row.status.clone(), |reason| format!("{}: {reason}", row.status));
        let histogram = row
            .depth_histogram
            .iter()
            .map(|(d, n)| format!("{d}×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        writeln!(
            out,
            "| {} | {} | {}/{} | {}/{}/{} (うち読めず {})/{} | {}/{} | {} | {:.0}/{:.0} | {} | {} | {} | {}/{} | {} |",
            escape(row.state.as_deref().unwrap_or("合計")),
            escape(&status),
            row.workers_busy,
            row.workers_total,
            row.trees_running,
            row.trees_waiting,
            row.trees_done + row.not_ready,
            row.not_ready,
            row.trees_held,
            row.positions_read,
            row.positions_required,
            row.remaining_visible,
            row.remaining_mean,
            row.remaining_p80,
            row.positions_per_hour.map_or_else(|| "-".into(), |v| format!("{v:.0}")),
            nps(row.nps_per_engine),
            nps(row.nps_total),
            number(row.hours_until_idle_mean),
            number(row.hours_until_idle_p80),
            histogram
        )?;
    }
    Ok(())
}

pub(super) fn run(args: StatusArgs) -> Result<()> {
    ensure!(
        args.journal.is_empty() || args.journal.len() == args.state.len(),
        "--journal は --state と同数を同じ順序で指定してください"
    );
    ensure!(
        args.stale_hours.is_finite() && args.stale_hours >= 0.0,
        "--stale-hours は有限の非負数が必要です"
    );
    let now = SystemTime::now();
    let snapshots: Vec<_> = args
        .state
        .iter()
        .enumerate()
        .map(|(i, state)| {
            snapshot(state, args.journal.get(i).map(PathBuf::as_path), args.stale_hours, now)
        })
        .collect();
    let pool = pool(&snapshots);
    let mut rows: Vec<_> = args
        .state
        .iter()
        .zip(&snapshots)
        .map(|(path, snapshot)| row(path, snapshot, &pool))
        .collect();
    rows.push(total(&rows));
    render(&rows, args.json, std::io::stdout().lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;
    use std::time::Duration;

    fn target(
        id: &str,
        state: &str,
        generation: u32,
        read: usize,
        required: usize,
    ) -> TargetStatus {
        TargetStatus {
            id: id.into(),
            own: "b".into(),
            generation,
            priority: 100,
            hold: state == "HELD",
            state: state.into(),
            ready: state == "DONE",
            read,
            required,
            out: "out".into(),
        }
    }

    fn fixture(path: &Path, targets: Vec<TargetStatus>, depth: usize) {
        std::fs::create_dir_all(path).unwrap();
        let lines = targets
            .iter()
            .map(|tree| {
                json!({"id": tree.id, "root": "startpos", "own": "b", "depth": depth}).to_string()
                    + "\n"
            })
            .collect::<String>();
        std::fs::write(path.join(QUEUE), lines).unwrap();
        let status = json!({"run_id":"test", "processed_bytes":0, "processed_sha256":"", "all_ended":false,
            "queue_error":null, "targets":targets, "rejected":[]});
        std::fs::write(path.join(STATUS_FILE), status.to_string()).unwrap();
        std::fs::write(path.join("progress.json"), r#"{"workers":[{"key":"busy"},{"key":null}]}"#)
            .unwrap();
    }

    fn sample_pool(buckets: Vec<((usize, bool), Vec<usize>)>) -> Pool {
        let mut pool = Pool::new();
        for ((depth, generation), reads) in buckets {
            pool.entry((depth, None)).or_default().reads.extend(&reads);
            pool.entry((depth, Some(generation))).or_default().reads.extend(reads);
        }
        for samples in pool.values_mut() {
            samples.prepare();
        }
        pool
    }

    #[test]
    fn growth_bucket_depth_generation_threshold_and_fallbacks() {
        let tree = target("active", "RUNNING", 2, 30, 50);
        let pool = sample_pool(vec![
            ((3, true), (100..110).collect()),
            ((3, false), vec![500; 10]),
            ((4, true), vec![900; 20]),
        ]);
        assert_eq!(estimate(&tree, Some(3), &pool), (74.5, 78.0));
        let pool = sample_pool(vec![((3, true), vec![60; 5]), ((3, false), vec![100; 5])]);
        assert_eq!(estimate(&tree, Some(3), &pool), (50.0, 70.0));
        let pool = sample_pool(vec![((3, true), vec![49; 20]), ((3, false), vec![100; 9])]);
        assert_eq!(estimate(&tree, Some(3), &pool), (20.0, 20.0));
        assert_eq!(estimate(&tree, None, &pool), (20.0, 20.0));
        assert_eq!(estimate(&target("a", "RUNNING", 1, 60, 50), None, &pool), (0.0, 0.0));
    }

    #[test]
    fn pooled_states_counts_unknown_depth_and_stale_total() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        let mut targets = vec![
            target("active", "RUNNING", 2, 10, 20),
            target("wait", "QUEUED", 1, 0, 5),
            target("ended", "NOT_READY", 1, 200, 500),
            target("held", "HELD", 1, 0, 0),
        ];
        targets.extend((0..5).map(|i| target(&format!("done{i}"), "DONE", 2, 100, 100)));
        fixture(&a, targets, 3);
        fixture(
            &b,
            (5..10).map(|i| target(&format!("done{i}"), "DONE", 2, 100, 100)).collect(),
            3,
        );
        let now = SystemTime::now();
        let mut snapshots = vec![snapshot(&a, None, 2.0, now), snapshot(&b, None, 2.0, now)];
        let samples = pool(&snapshots);
        let first = snapshots[0].as_mut().unwrap();
        first.depths.remove(&("wait".into(), "b".into(), 1));
        first.rate = Some(100.0);
        let a_row = row(&a, &snapshots[0], &samples);
        assert_eq!((a_row.workers_busy, a_row.workers_total), (1, 2));
        assert_eq!(
            (a_row.trees_running, a_row.trees_waiting, a_row.trees_done, a_row.trees_held),
            (1, 1, 5, 1)
        );
        assert_eq!(
            (a_row.positions_read, a_row.positions_required, a_row.remaining_visible),
            (10, 25, 15)
        );
        assert_eq!(a_row.not_ready, 1);
        assert_eq!(a_row.remaining_mean, 95.0);
        assert_eq!(a_row.remaining_p80, 95.0);
        assert_eq!(a_row.hours_until_idle_mean, Some(0.95));
        assert_eq!(a_row.depth_histogram.values().sum::<usize>(), 2);
        assert_eq!(a_row.depth_histogram["不明"], 1);
        let stale = snapshot(&a, None, 2.0, now + Duration::from_secs(3 * 3600));
        let stale_row = row(&a, &stale, &samples);
        assert_eq!(stale_row.status, "stale");
        let total = total(&[a_row, stale_row]);
        assert_eq!(total.not_ready, 1);
        assert_eq!(total.remaining_mean, 95.0);
        assert_eq!(total.positions_per_hour, Some(100.0));
        assert_eq!(total.hours_until_idle_mean, Some(0.95));
    }

    #[test]
    fn queue_skips_invalid_rows_and_torn_tail_and_keeps_first_definition() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), vec![target("active", "RUNNING", 1, 0, 20)], 3);
        let queue = dir.path().join(QUEUE);
        let mut file = std::fs::OpenOptions::new().append(true).open(queue).unwrap();
        writeln!(file, "{{bad}}").unwrap();
        writeln!(file, "{}", json!({"id":"invalid", "root":"startpos", "own":"b", "depth":99}))
            .unwrap();
        writeln!(
            file,
            "{}",
            json!({"id":"active", "root":"startpos moves 7g7f", "own":"b", "depth":8})
        )
        .unwrap();
        writeln!(file, "{}", json!({"id":"later", "root":"startpos", "own":"b", "depth":4}))
            .unwrap();
        write!(file, "{}", json!({"id":"torn", "root":"startpos", "own":"b", "depth":5})).unwrap();
        assert_eq!(
            queue_depths(&dir.path().join(QUEUE)).unwrap(),
            BTreeMap::from([("active".into(), 3), ("later".into(), 4)])
        );
        let snapshot = snapshot(dir.path(), None, 2.0, SystemTime::now()).unwrap();
        assert_eq!(snapshot.depths, BTreeMap::from([(("active".into(), "b".into(), 1), 3)]));
        assert!(snapshot.notice.unwrap().contains("近似値"));
    }

    #[test]
    fn accepted_generation_depth_drives_histogram_and_done_estimates() {
        use super::super::targets::Defaults;
        use sha2::{Digest, Sha256};

        let dir = tempfile::tempdir().unwrap();
        let mut targets = vec![target("active", "RUNNING", 2, 30, 50)];
        targets.extend((0..10).map(|i| target(&format!("done{i}"), "DONE", 2, 100, 100)));
        fixture(dir.path(), targets, 3);
        let mut queue = std::fs::read(dir.path().join(QUEUE)).unwrap();
        queue.extend(
            String::from_utf8(queue.clone())
                .unwrap()
                .replace("\"depth\":3", "\"depth\":8")
                .bytes(),
        );
        let defaults = Defaults {
            multipv: 3,
            nodes: 100,
            opp_n: 1,
            stop: 300,
            rounds: 0,
            draw: 0,
        };
        let checkpoint = Checkpoint::new(dir.path().join("output"), "test".into());
        let (checkpoint, _) = checkpoint.apply(&queue, &defaults, &Sha256::new(), &[]);
        assert!(checkpoint.rejected.is_empty(), "{:?}", checkpoint.rejected);
        assert!(checkpoint.targets.iter().all(|t| t.generation == 2 && t.definition.depth == 8));
        std::fs::write(dir.path().join(QUEUE), queue).unwrap();
        std::fs::write(dir.path().join(CHECKPOINT), serde_json::to_vec(&checkpoint).unwrap())
            .unwrap();
        let now = SystemTime::now();
        let snapshots = vec![snapshot(dir.path(), None, 2.0, now)];
        let samples = pool(&snapshots);
        assert_eq!(samples[&(8, Some(true))].reads, vec![100; 10]);
        assert!(!samples.contains_key(&(3, None)));
        let current = row(dir.path(), &snapshots[0], &samples);
        assert_eq!(current.depth_histogram, BTreeMap::from([("8".into(), 1)]));
        assert_eq!((current.remaining_mean, current.remaining_p80), (70.0, 70.0));
        assert!(current.reason.is_none());

        std::fs::remove_file(dir.path().join(QUEUE)).unwrap();
        let mut different_generation = snapshot(dir.path(), None, 2.0, now).unwrap();
        different_generation.status.targets[0].generation = 1;
        let current = row(dir.path(), &Ok(different_generation), &samples);
        assert_eq!(current.depth_histogram, BTreeMap::from([("不明".into(), 1)]));
        assert_eq!(current.remaining_mean, 20.0);
        let mut different_own = snapshot(dir.path(), None, 2.0, now).unwrap();
        different_own.status.targets[0].own = "w".into();
        let current = row(dir.path(), &Ok(different_own), &samples);
        assert_eq!(current.depth_histogram, BTreeMap::from([("不明".into(), 1)]));
    }

    #[test]
    fn unreadable_checkpoint_falls_back_and_oversized_queue_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), vec![target("active", "RUNNING", 2, 0, 20)], 3);
        std::fs::write(dir.path().join(CHECKPOINT), b"{bad}").unwrap();
        let queue = dir.path().join(QUEUE);
        let valid = std::fs::read(&queue).unwrap();
        let mut bytes = vec![b'x'; MAX_LINE + CHUNK];
        bytes.push(b'\n');
        bytes.extend(valid);
        std::fs::write(&queue, bytes).unwrap();
        let now = SystemTime::now();
        let current = snapshot(dir.path(), None, 2.0, now);
        let current = row(dir.path(), &current, &Pool::new());
        assert_eq!(current.status, "ok");
        assert_eq!(current.depth_histogram, BTreeMap::from([("3".into(), 1)]));
        assert!(current.reason.unwrap().contains("近似値"));
        std::fs::remove_file(queue).unwrap();
        let current = snapshot(dir.path(), None, 2.0, now);
        let current = row(dir.path(), &current, &Pool::new());
        assert_eq!(current.status, "ok");
        assert_eq!(current.depth_histogram, BTreeMap::from([("不明".into(), 1)]));
        assert!(current.reason.unwrap().contains("targets.jsonl"));
    }

    #[test]
    fn markdown_uses_japanese_headers_integer_positions_and_tenths_of_hours() {
        let row = Row {
            status: "ok".into(),
            trees_done: 2,
            not_ready: 1,
            remaining_mean: 1234.56,
            remaining_p80: 2345.67,
            hours_until_idle_mean: Some(12.34),
            hours_until_idle_p80: Some(23.46),
            ..Row::default()
        };
        let mut table = Vec::new();
        render(&[row], false, &mut table).unwrap();
        let table = String::from_utf8(table).unwrap();
        assert!(table.contains("| 台 (state) | 状態 | engine 稼働/全 |"));
        assert!(table.contains("木 読み中/待ち/終わった (うち読めずに終わった)/保留"));
        assert!(table.contains("局面 読んだ/必要 | 残り 見えている分"));
        assert!(table.contains("残り 伸びを見込む 平均/80%点 | 1 時間に読む局面"));
        assert!(table.contains("手が空くまで 平均/80%点 (時間) | 深さ×木の数"));
        assert!(table.contains("0/0/3 (うち読めず 1)/0"));
        assert!(table.contains("| 1235/2346 | - | - | - | 12.3/23.5 |"));
    }

    #[test]
    fn done_samples_are_unique_by_id_and_own_and_use_largest_read() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        let b = dir.path().join("b");
        fixture(
            &a,
            (0..5).map(|i| target(&format!("done{i}"), "DONE", 1, 100, 100)).collect(),
            3,
        );
        fixture(
            &b,
            (0..5)
                .flat_map(|i| {
                    let black = target(&format!("done{i}"), "DONE", 2, 200, 200);
                    let mut white = black.clone();
                    white.own = "w".into();
                    [black, white]
                })
                .collect(),
            3,
        );
        let now = SystemTime::now();
        let snapshots = vec![snapshot(&a, None, 2.0, now), snapshot(&b, None, 2.0, now)];
        let samples = pool(&snapshots);
        assert_eq!(samples[&(3, None)].reads, vec![200; 10]);
        assert_eq!(samples[&(3, Some(true))].reads, vec![200; 10]);
        assert!(!samples.contains_key(&(3, Some(false))));
        assert_eq!(
            estimate(&target("active", "RUNNING", 2, 10, 20), Some(3), &samples),
            (190.0, 190.0)
        );
    }

    #[test]
    fn only_ended_or_stale_states_have_no_active_work() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), vec![target("ended", "NOT_READY", 1, 10, 100)], 3);
        let now = SystemTime::now();
        let mut current = snapshot(dir.path(), None, 2.0, now).unwrap();
        current.rate = Some(100.0);
        let active = row(dir.path(), &Ok(current), &Pool::new());
        assert_eq!(active.not_ready, 1);
        assert_eq!(active.trees_waiting, 0);
        assert_eq!(active.positions_read, 0);
        assert_eq!(active.positions_required, 0);
        assert_eq!(active.remaining_visible, 0);
        assert_eq!(active.remaining_mean, 0.0);
        assert_eq!(active.remaining_p80, 0.0);
        assert!(active.depth_histogram.is_empty());
        assert_eq!(active.hours_until_idle_mean, Some(0.0));
        assert_eq!(active.hours_until_idle_p80, Some(0.0));
        let mut table = Vec::new();
        render(&[active], false, &mut table).unwrap();
        assert!(String::from_utf8(table).unwrap().contains("0/0/1 (うち読めず 1)/0"));
        let stale = snapshot(dir.path(), None, 2.0, now + Duration::from_secs(3 * 3600));
        let stale = row(dir.path(), &stale, &Pool::new());
        assert_eq!(total(&[stale]).status, "empty");
        assert_eq!(total(&[]).status, "empty");
    }

    #[test]
    fn unreadable_state_and_stable_json_keys() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let row = row(&missing, &snapshot(&missing, None, 2.0, SystemTime::now()), &Pool::new());
        assert_eq!(row.status, "unreadable");
        assert!(row.reason.is_some());
        let rows = vec![row];
        let mut all = rows;
        all.push(total(&all));
        let mut output = Vec::new();
        render(&all, true, &mut output).unwrap();
        let values: Vec<serde_json::Value> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(values.len(), 2);
        assert_eq!(values[1]["kind"], "total");
        assert_eq!(values[1]["status"], "empty");
        let expected = [
            "kind",
            "state",
            "status",
            "reason",
            "workers_busy",
            "workers_total",
            "trees_running",
            "trees_waiting",
            "trees_done",
            "not_ready",
            "trees_held",
            "positions_read",
            "positions_required",
            "remaining_visible",
            "remaining_mean",
            "remaining_p80",
            "positions_per_hour",
            "nps_per_engine",
            "nps_total",
            "hours_until_idle_mean",
            "hours_until_idle_p80",
            "depth_histogram",
        ];
        for value in values {
            assert!(value["nps_per_engine"].is_null());
            assert!(value["nps_total"].is_null());
            let object = value.as_object().unwrap();
            assert_eq!(object.len(), expected.len());
            for key in expected {
                assert!(object.contains_key(key), "{key}");
            }
        }
        let mut table = Vec::new();
        render(&all, false, &mut table).unwrap();
        assert!(String::from_utf8(table).unwrap().contains("unreadable"));
    }

    #[test]
    fn unreadable_files_do_not_prevent_other_states_and_total_rates_sum() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let valid = dir.path().join("valid");
        fixture(&valid, vec![target("queued", "QUEUED", 1, 0, 100)], 2);
        let now = SystemTime::now();
        let snapshots = vec![
            snapshot(&missing, None, 2.0, now),
            snapshot(&valid, None, 2.0, now),
        ];
        let samples = pool(&snapshots);
        let mut rows = vec![
            row(&missing, &snapshots[0], &samples),
            row(&valid, &snapshots[1], &samples),
        ];
        assert_eq!(rows[0].status, "unreadable");
        assert_eq!(rows[1].status, "ok");
        assert_eq!(rows[1].trees_waiting, 1);
        assert_eq!(total(&rows).remaining_visible, 100);
        assert_eq!(total(&rows).positions_per_hour, None);
        rows[1].positions_per_hour = Some(50.0);
        rows.push(row(&valid, &snapshots[1], &samples));
        rows[2].positions_per_hour = Some(100.0);
        let summed = total(&rows);
        assert_eq!(summed.remaining_mean, 200.0);
        assert_eq!(summed.positions_per_hour, Some(150.0));
        assert_eq!(summed.hours_until_idle_p80, Some(200.0 / 150.0));
        std::fs::write(valid.join("progress.json"), b"invalid json").unwrap();
        assert!(snapshot(&valid, None, 2.0, now).is_err());
    }

    #[test]
    fn invalid_command_options_and_journal_durations() {
        let dir = tempfile::tempdir().unwrap();
        let args = |journal, stale_hours| StatusArgs {
            state: vec![dir.path().to_owned()],
            journal,
            stale_hours,
            json: true,
        };
        assert!(run(args(vec!["a".into(), "b".into()], 2.0)).is_err());
        assert!(run(args(vec![], -1.0)).is_err());
        assert!(run(args(vec![], f64::NAN)).is_err());
        assert_eq!(tail_rate(&mut Cursor::new(b"{bad}\n"), 1).unwrap().positions_per_hour, None);
        assert_eq!(
            tail_rate(&mut Cursor::new(journal_line("engine", -1.0, 0)), 1)
                .unwrap()
                .positions_per_hour,
            None
        );
        assert_eq!(
            tail_rate(&mut Cursor::new(journal_line("engine", 0.0, 0)), 1)
                .unwrap()
                .positions_per_hour,
            None
        );
    }

    fn journal_line(provenance: &str, seconds: f64, padding: usize) -> Vec<u8> {
        journal_line_with_nodes(provenance, seconds, padding, Some(100))
    }

    fn journal_line_with_nodes(
        provenance: &str,
        seconds: f64,
        padding: usize,
        nodes: Option<u64>,
    ) -> Vec<u8> {
        let mut record = json!({
            "kind":"book-tree-reading-v1", "provenance":provenance,
            "settings":{"fingerprint":"test", "version":"test", "engine_sha256":"test",
                "options":{}, "content_sha256":{}, "nodes":100, "multipv":1, "hash":1},
            "reading":{"tag":"test", "ply":1, "key":"test", "nodes_limit":100,
                "nodes":100, "sec":seconds, "threads":1, "hash":1, "draw":{},
                "multipv":[], "bestmove":"resign"},
            "padding":"x".repeat(padding)
        });
        if let Some(nodes) = nodes {
            record["reading"]["nodes"] = json!(nodes);
        } else {
            record["reading"].as_object_mut().unwrap().remove("nodes");
        }
        let mut bytes = record.to_string().into_bytes();
        bytes.push(b'\n');
        bytes
    }

    #[test]
    fn journal_tail_nps_median_skips_missing_nodes_and_nonpositive_seconds() {
        let mut bytes = journal_line_with_nodes("engine", 10.0, 0, Some(10_000_000));
        bytes.extend(journal_line_with_nodes("engine", 20.0, 0, Some(60_000_000)));
        bytes.extend(journal_line_with_nodes("engine", 30.0, 0, None));
        bytes.extend(journal_line_with_nodes("engine", 0.0, 0, Some(100_000_000)));
        bytes.extend(journal_line_with_nodes("engine", -1.0, 0, Some(100_000_000)));
        bytes.extend(journal_line_with_nodes("journal:other", 1.0, 0, Some(100_000_000)));
        let rates = tail_rate(&mut Cursor::new(&bytes), 2).unwrap();
        assert_eq!(rates.positions_per_hour, Some(480.0));
        assert_eq!(rates.nps_per_engine, Some(2_000_000.0));
        bytes.extend(journal_line_with_nodes("engine", 10.0, 0, Some(90_000_000)));
        assert_eq!(
            tail_rate(&mut Cursor::new(bytes), 2).unwrap().nps_per_engine,
            Some(3_000_000.0)
        );
        for (sec, nodes) in [(10.0, None), (0.0, Some(100)), (-1.0, Some(100))] {
            assert_eq!(
                tail_rate(&mut Cursor::new(journal_line_with_nodes("engine", sec, 0, nodes)), 2)
                    .unwrap()
                    .nps_per_engine,
                None
            );
        }
        let zero_nodes = journal_line_with_nodes("engine", 10.0, 0, Some(0));
        assert_eq!(tail_rate(&mut Cursor::new(zero_nodes), 2).unwrap().nps_per_engine, Some(0.0));
    }

    #[test]
    fn state_and_total_nps_use_all_workers_and_exclude_stale_states() {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), vec![], 3);
        let journal = dir.path().join("journal.jsonl");
        std::fs::write(&journal, journal_line_with_nodes("engine", 10.0, 0, Some(7_500_000)))
            .unwrap();
        let now = SystemTime::now();
        let first = snapshot(dir.path(), Some(&journal), 2.0, now);
        let first = row(dir.path(), &first, &Pool::new());
        assert_eq!(first.workers_busy, 1);
        assert_eq!(first.nps_per_engine, Some(750_000.0));
        assert_eq!(first.nps_total, Some(1_500_000.0));
        let second = Row {
            status: "ok".into(),
            workers_total: 4,
            nps_per_engine: Some(3_350_000.0),
            nps_total: Some(13_400_000.0),
            ..Row::default()
        };
        let stale = snapshot(dir.path(), Some(&journal), 2.0, now + Duration::from_secs(3 * 3600));
        let stale = row(dir.path(), &stale, &Pool::new());
        let mut rows = vec![first, second, stale];
        let summed = total(&rows);
        assert_eq!(summed.workers_total, 6);
        assert_eq!(summed.nps_total, Some(14_900_000.0));
        assert_eq!(summed.nps_per_engine, Some(14_900_000.0 / 6.0));
        rows.push(summed);
        let mut output = Vec::new();
        render(&rows, true, &mut output).unwrap();
        let values: Vec<serde_json::Value> = std::str::from_utf8(&output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(values[0]["nps_per_engine"], 750_000.0);
        assert_eq!(values[0]["nps_total"], 1_500_000.0);
        assert_eq!(values[3]["nps_total"], 14_900_000.0);
        assert_eq!(values[3]["nps_per_engine"], 14_900_000.0 / 6.0);
        output.clear();
        render(&rows, false, &mut output).unwrap();
        let table = std::str::from_utf8(&output).unwrap();
        assert!(table.contains("1 時間に読む局面 | 1 engine の NPS | 合計 NPS |"));
        assert!(table.contains("| 0.75M | 1.50M |"));
        assert!(table.contains("| 2.48M | 14.90M |"));
        rows[0].nps_total = None;
        assert_eq!(total(&rows[..3]).nps_total, None);
        assert_eq!(total(&[]).nps_per_engine, None);
        let zero_workers = Row {
            status: "ok".into(),
            nps_total: Some(0.0),
            ..Row::default()
        };
        assert_eq!(total(&[zero_workers]).nps_per_engine, None);
    }

    struct CountedReader {
        cursor: Cursor<Vec<u8>>,
        bytes_read: usize,
    }

    impl Read for CountedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let count = self.cursor.read(buffer)?;
            self.bytes_read += count;
            Ok(count)
        }
    }

    impl Seek for CountedReader {
        fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
            self.cursor.seek(position)
        }
    }

    #[test]
    fn journal_tail_crosses_chunks_and_uses_latest_400_engine_rows() {
        let mut bytes = vec![b'x'; CHUNK * 16];
        bytes.push(b'\n');
        for i in 0..450 {
            let sec = if i < 50 { 999.0 } else { 60.0 };
            bytes.extend(journal_line("engine", sec, 200));
            bytes.extend_from_slice(b"{bad}\n");
            bytes.extend(journal_line("engine", -1.0, 0));
            bytes.extend(journal_line("journal:other", 999.0, 0));
        }
        let length = bytes.len();
        let mut reader = CountedReader {
            cursor: Cursor::new(bytes),
            bytes_read: 0,
        };
        let rates = tail_rate(&mut reader, 2).unwrap();
        assert_eq!(rates.positions_per_hour, Some(120.0));
        assert_eq!(rates.nps_per_engine, Some(100.0 / 60.0));
        assert!(reader.bytes_read > CHUNK);
        assert!(reader.bytes_read <= CHUNK * 12);
        assert!(reader.bytes_read < length);

        let mut bytes = journal_line("engine", 1.0, 0);
        for _ in 0..SAMPLES {
            bytes.extend(journal_line_with_nodes("engine", 60.0, 0, None));
        }
        let rates = tail_rate(&mut Cursor::new(bytes), 2).unwrap();
        assert_eq!(rates.positions_per_hour, Some(120.0));
        assert_eq!(rates.nps_per_engine, None);
    }

    #[test]
    fn journal_tail_is_bounded_and_ignores_imports_and_partial_line() {
        let mut bytes = vec![b'x'; CHUNK * 16];
        bytes.push(b'\n');
        for _ in 0..450 {
            bytes.extend(journal_line("engine", 60.0, 0));
            bytes.extend(journal_line("journal:other", 999.0, 0));
        }
        bytes.extend_from_slice(b"{partial");
        let length = bytes.len() as u64;
        let mut cursor = Cursor::new(bytes);
        assert_eq!(tail_rate(&mut cursor, 2).unwrap().positions_per_hour, Some(120.0));
        assert!(cursor.position() >= length - (CHUNK * 8) as u64);
        assert_eq!(
            tail_rate(&mut Cursor::new(journal_line("engine", 120.0, 0)), 1)
                .unwrap()
                .positions_per_hour,
            Some(30.0)
        );
        let mut line = journal_line("engine", 120.0, 0);
        line.pop();
        assert_eq!(tail_rate(&mut Cursor::new(line), 1).unwrap().positions_per_hour, Some(30.0));
        assert_eq!(tail_rate(&mut Cursor::new(b""), 1).unwrap(), JournalRates::default());
    }
}
