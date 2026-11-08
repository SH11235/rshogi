//! 受付と進みの写しから、未着手の木の価値と締めまでの余裕を点検する。

use super::{
    export::files,
    reply_graph::HIRATE_KEY,
    targets::{Line, Receipt, parse_root},
};
use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result, ensure};
use chrono::{DateTime, FixedOffset, Utc};
use clap::{Args, ValueEnum};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::PathBuf,
};

#[derive(Args, Debug)]
pub(super) struct QueueReviewArgs {
    /// 台の名前と、enqueue/・fetch/ を置いた dir (NAME=DIR、繰り返し可)
    #[arg(long = "box", required = true)]
    box_specs: Vec<String>,
    /// worker あたりの局面/時 (NAME=速さ)
    #[arg(long)]
    rate: Vec<String>,
    #[arg(long, default_value_t = 1.44)]
    default_rate: f64,
    /// 台全体の局面/時 (NAME=速さ、worker 数による計算を上書き)
    #[arg(long)]
    box_rate: Vec<String>,
    /// 締めの時刻 (RFC3339)
    #[arg(long)]
    cutoff: DateTime<FixedOffset>,
    #[arg(long, default_value_t = 92)]
    tree_size: u64,
    /// master walk の表 (b=TSV / w=TSV、繰り返し可)
    #[arg(long)]
    walk_table: Vec<String>,
    /// 初期局面からの開始手順 (USI、空白区切り)
    #[arg(long)]
    start_moves: Option<String>,
    #[arg(long, default_value_t = 0.001)]
    value_floor: f64,
    #[arg(long)]
    table: PathBuf,
    #[arg(long)]
    boxes: PathBuf,
    #[arg(long)]
    hold_out: PathBuf,
    #[arg(long, default_value = "qrhold")]
    prefix: String,
    /// reach-low に加えて hold 行にする分類 (繰り返し可)
    #[arg(long, value_enum)]
    hold_class: Vec<Class>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum Class {
    Seed,
    BeyondOut,
    ReachHigh,
    ReachLow,
    ReachPartial,
    ExtraRoot,
    NotReached,
    PathMismatch,
}
impl Class {
    fn name(self) -> &'static str {
        match self {
            Self::Seed => "seed",
            Self::BeyondOut => "beyond-out",
            Self::ReachHigh => "reach-high",
            Self::ReachLow => "reach-low",
            Self::ReachPartial => "reach-partial",
            Self::ExtraRoot => "extra-root",
            Self::NotReached => "not-reached",
            Self::PathMismatch => "path-mismatch",
        }
    }
}

#[derive(Deserialize)]
struct Progress {
    trees: Vec<TreeProgress>,
    workers: Vec<WorkerProgress>,
}
#[derive(Deserialize)]
struct TreeProgress {
    id: String,
    own: String,
    priority: u32,
    hold: bool,
    finished: bool,
    read: u64,
    requested: u64,
}
#[derive(Deserialize)]
struct WorkerProgress {
    key: Option<String>,
}
struct Queued {
    line: Line,
    raw: Value,
    roots: BTreeSet<String>,
    primary_root: String,
}
struct BoxData {
    name: String,
    dir: PathBuf,
    queue: BTreeMap<String, Queued>,
    progress: Progress,
}
#[derive(Default)]
struct Walk {
    reach: BTreeMap<(String, String), f64>,
    out: BTreeMap<(String, String), f64>,
    stops: BTreeSet<(String, String)>,
}

fn assignment(text: &str) -> Result<(&str, &str)> {
    let (name, value) = text.split_once('=').context("NAME=値 の形が必要です")?;
    ensure!(!name.is_empty() && !value.is_empty(), "NAME=値 の形が必要です: {text}");
    Ok((name, value))
}
fn safe_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn nonnegative(value: f64) -> Result<f64> {
    ensure!(value.is_finite() && value >= 0.0, "有限の非負数が必要です: {value}");
    Ok(value)
}
fn rates(values: &[String], names: &BTreeSet<String>) -> Result<BTreeMap<String, f64>> {
    let mut result = BTreeMap::new();
    for text in values {
        let (name, value) = assignment(text)?;
        ensure!(names.contains(name), "未指定の台です: {name}");
        ensure!(
            result.insert(name.into(), nonnegative(value.parse()?)?).is_none(),
            "速さの指定が重複しています: {name}"
        );
    }
    Ok(result)
}

fn load_box(name: &str, dir: PathBuf, inputs: &mut Vec<PathBuf>) -> Result<BoxData> {
    let mut accepted = Vec::new();
    for entry in std::fs::read_dir(dir.join("enqueue"))? {
        let path = entry?.path();
        if path.extension().is_none_or(|ext| ext != "jsonl") {
            continue;
        }
        let appended = PathBuf::from(format!("{}.receipt", path.display()));
        let replaced = path.with_extension("receipt");
        ensure!(
            !(appended.try_exists()? && replaced.try_exists()?),
            "受領が二つあります: {}",
            path.display()
        );
        let receipt_path = if appended.try_exists()? {
            appended
        } else {
            replaced
        };
        if !receipt_path.try_exists()? {
            continue;
        }
        let receipt: Receipt = serde_json::from_reader(File::open(&receipt_path)?)
            .with_context(|| format!("受領が不正です: {}", receipt_path.display()))?;
        ensure!(receipt.start <= receipt.end, "受領の範囲が不正です: {}", receipt_path.display());
        inputs.extend([path.clone(), receipt_path]);
        accepted.push((receipt.start, path, receipt));
    }
    accepted.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    for pair in accepted.windows(2) {
        ensure!(
            pair[0].2.end <= pair[1].0 && pair[0].0 != pair[1].0,
            "受領の範囲が重複しています: {name}"
        );
    }
    let mut queue = BTreeMap::new();
    for (_, path, receipt) in accepted {
        let mut count = 0;
        for text in BufReader::new(File::open(&path)?).lines() {
            let text = text?;
            if text.trim().is_empty() {
                continue;
            }
            let raw: Value = serde_json::from_str(&text)?;
            let line: Line = serde_json::from_value(raw.clone())?;
            line.check().with_context(|| format!("{}: {}", path.display(), line.id))?;
            let roots = std::iter::once(&line.root)
                .chain(&line.roots)
                .map(|r| parse_root(r).map(|r| r.key))
                .collect::<Result<_>>()?;
            let primary_root = parse_root(&line.root)?.key;
            queue.insert(
                line.id.clone(),
                Queued {
                    line,
                    raw,
                    roots,
                    primary_root,
                },
            );
            count += 1;
        }
        ensure!(count == receipt.lines, "受領の行数と一致しません: {}", path.display());
    }
    let progress_path = dir.join("fetch/progress.json");
    let progress: Progress = serde_json::from_reader(File::open(&progress_path)?)
        .with_context(|| format!("進みのファイルが不正です: {}", progress_path.display()))?;
    inputs.push(progress_path);
    let mut ids = BTreeSet::new();
    for tree in &progress.trees {
        ensure!(
            ids.insert((&tree.id, &tree.own)),
            "progress の id が重複しています: {}",
            tree.id
        );
        let queued = queue
            .get(&tree.id)
            .with_context(|| format!("{name}: id={} の受付の行がありません", tree.id))?;
        ensure!(
            matches!(tree.own.as_str(), "b" | "w")
                && queued.line.own.contains(&tree.own)
                && tree.priority == queued.line.priority
                && tree.hold == queued.line.hold,
            "{name}: id={} の受付と progress が一致しません。写しを揃えてください",
            tree.id
        );
    }
    ensure!(
        ids.len() == queue.values().map(|q| q.line.sides().len()).sum::<usize>(),
        "{name}: progress に無い受付があります。写しを揃えてください"
    );
    Ok(BoxData {
        name: name.into(),
        dir,
        queue,
        progress,
    })
}

fn path_keys(moves: &[&str]) -> Result<Vec<String>> {
    let mut position = position_from_sfen(&format!("{HIRATE_KEY} 1"))?;
    let mut keys = vec![HIRATE_KEY.to_owned()];
    for mv in moves {
        apply_usi_move(&mut position, mv)?;
        keys.push(strip_ply(&position.to_sfen()).to_owned());
    }
    Ok(keys)
}

fn load_walk(args: &QueueReviewArgs, inputs: &mut Vec<PathBuf>) -> Result<Walk> {
    let start: Vec<_> =
        args.start_moves.as_deref().unwrap_or_default().split_whitespace().collect();
    path_keys(&start).context("--start-moves が不正です")?;
    let mut walk = Walk::default();
    for text in &args.walk_table {
        let (side, file) = assignment(text)?;
        ensure!(matches!(side, "b" | "w"), "walk-table の側は b / w です");
        let path = PathBuf::from(file);
        inputs.push(path.clone());
        let mut lines = BufReader::new(File::open(&path)?).lines();
        let header = lines.next().context("walk の表が空です")??;
        let columns: Vec<_> = header.split('\t').collect();
        let column = |name| {
            columns
                .iter()
                .position(|c| *c == name)
                .with_context(|| format!("walk の列がありません: {name}"))
        };
        let (moves, weight, stop) = (column("moves")?, column("weight")?, column("stop")?);
        let side_column = columns.iter().position(|c| *c == "side");
        for (n, text) in lines.enumerate() {
            let text = text?;
            if text.is_empty() {
                continue;
            }
            let cells: Vec<_> = text.split('\t').collect();
            ensure!(cells.len() == columns.len(), "{}:{}: 列数が不正です", path.display(), n + 2);
            if side_column.is_some_and(|i| cells[i] != side) {
                continue;
            }
            let weight = nonnegative(cells[weight].parse()?)?;
            let moves: Vec<_> = cells[moves].split_whitespace().collect();
            // walk の表は開始手順を含む。相対手順の表にも同じ開始局面を使う。
            let full: Vec<_> = if moves.starts_with(&start) {
                moves
            } else {
                start.iter().copied().chain(moves).collect()
            };
            let keys = path_keys(&full).with_context(|| format!("{}:{}", path.display(), n + 2))?;
            walk.stops.insert((side.into(), keys.last().context("空の手順です")?.clone()));
            if cells[stop] == "out" {
                *walk
                    .out
                    .entry((side.into(), keys.last().context("空の手順です")?.clone()))
                    .or_default() += weight;
            }
            for key in keys.into_iter().skip(start.len()).collect::<BTreeSet<_>>() {
                *walk.reach.entry((side.into(), key)).or_default() += weight;
            }
        }
    }
    ensure!(
        walk.reach.values().chain(walk.out.values()).all(|v| v.is_finite()),
        "到達の重みが大きすぎます"
    );
    Ok(walk)
}

fn memo_moves(memo: Option<&str>) -> Option<Vec<&str>> {
    let memo = memo?;
    let start = memo
        .match_indices("moves=")
        .find(|(i, _)| {
            *i == 0
                || !(memo.as_bytes()[i - 1].is_ascii_alphanumeric()
                    || memo.as_bytes()[i - 1] == b'_')
        })?
        .0
        + 6;
    let text = memo[start..].split([';', ',', '\n', '\r', '；']).next().unwrap_or_default();
    // memo は手順の後に説明の文が続くことがあるので、USI の手でない最初の語で止める。
    Some(text.split_whitespace().take_while(|token| is_usi_move(token)).collect())
}
fn is_usi_move(token: &str) -> bool {
    let b = token.as_bytes();
    let file = |c: u8| (b'1'..=b'9').contains(&c);
    let rank = |c: u8| (b'a'..=b'i').contains(&c);
    match b {
        [p, b'*', f, r] => b"PLNSGBR".contains(p) && file(*f) && rank(*r),
        [f1, r1, f2, r2] => file(*f1) && rank(*r1) && file(*f2) && rank(*r2),
        [f1, r1, f2, r2, b'+'] => file(*f1) && rank(*r1) && file(*f2) && rank(*r2),
        _ => false,
    }
}
enum MemoPath {
    Seed,
    Mismatch,
    Keys(Vec<String>),
}
fn memo_path(queued: &Queued, start: &[&str]) -> MemoPath {
    let Some(moves) = memo_moves(queued.line.memo.as_deref()) else {
        return MemoPath::Seed;
    };
    let full: Vec<_> = if moves.starts_with(start) {
        moves
    } else {
        start.iter().copied().chain(moves).collect()
    };
    match path_keys(&full) {
        Ok(keys) if keys.last() == Some(&queued.primary_root) => MemoPath::Keys(keys),
        _ => MemoPath::Mismatch,
    }
}
fn classify(
    queued: &Queued,
    root: &str,
    own: &str,
    walk: &Walk,
    floor: f64,
    path: &MemoPath,
) -> (Class, Option<f64>) {
    if root != queued.primary_root {
        return (Class::ExtraRoot, None);
    }
    let keys = match path {
        MemoPath::Seed => return (Class::Seed, None),
        MemoPath::Mismatch => return (Class::PathMismatch, None),
        MemoPath::Keys(keys) => keys,
    };
    let weight = |map: &BTreeMap<(String, String), f64>, key: &str| {
        map.get(&(own.to_owned(), key.to_owned())).copied()
    };
    if let Some(value) = keys.iter().find_map(|key| weight(&walk.out, key)) {
        return (Class::BeyondOut, Some(value));
    }
    match weight(&walk.reach, root) {
        Some(value) => (
            if value >= floor {
                Class::ReachHigh
            } else if walk.stops.contains(&(own.to_owned(), root.to_owned())) {
                Class::ReachLow
            } else {
                Class::ReachPartial
            },
            Some(value),
        ),
        None => (Class::NotReached, None),
    }
}

pub(super) fn run(args: QueueReviewArgs) -> Result<()> {
    run_at(args, Utc::now(), &mut std::io::stdout().lock())
}
fn run_at(args: QueueReviewArgs, now: DateTime<Utc>, stdout: &mut impl Write) -> Result<()> {
    nonnegative(args.default_rate)?;
    nonnegative(args.value_floor)?;
    ensure!(args.tree_size > 0, "tree-size は正数が必要です");
    ensure!(safe_name(&args.prefix), "prefix は英数字・_・- にしてください");
    let mut inputs = Vec::new();
    let mut boxes = Vec::new();
    let mut names = BTreeSet::new();
    for text in &args.box_specs {
        let (name, dir) = assignment(text)?;
        ensure!(
            safe_name(name) && names.insert(name.to_owned()),
            "台の名前は重複のない英数字・_・- にしてください: {name}"
        );
        boxes.push(load_box(name, PathBuf::from(dir), &mut inputs)?);
    }
    let rate = rates(&args.rate, &names)?;
    let box_rate = rates(&args.box_rate, &names)?;
    let walk = load_walk(&args, &mut inputs)?;
    let start: Vec<_> =
        args.start_moves.as_deref().unwrap_or_default().split_whitespace().collect();
    let hours =
        (args.cutoff.signed_duration_since(now).num_milliseconds() as f64 / 3_600_000.0).max(0.0);
    let mut duplicates: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
    for b in &boxes {
        for tree in b.progress.trees.iter().filter(|t| !t.finished && !t.hold) {
            for root in &b.queue[&tree.id].roots {
                duplicates
                    .entry((root.clone(), tree.own.clone()))
                    .or_default()
                    .insert((b.name.clone(), tree.id.clone()));
            }
        }
    }
    let mut outputs = vec![args.table.clone(), args.boxes.clone()];
    outputs.extend(
        boxes
            .iter()
            .map(|b| args.hold_out.join(format!("{}_{}.jsonl", args.prefix, b.name))),
    );
    let resolved: Vec<_> = outputs
        .iter()
        .map(|p| crate::common::dedup::canonicalize_maybe_new(p))
        .collect::<std::io::Result<_>>()?;
    for (i, path) in resolved.iter().enumerate() {
        ensure!(!resolved[..i].contains(path), "出力先が重複しています: {}", path.display());
        for b in &boxes {
            ensure!(
                !path.starts_with(std::fs::canonicalize(&b.dir)?),
                "台の dir 内には出力できません: {}",
                path.display()
            );
        }
    }
    files::write_generated(&outputs, &inputs, true, |temporary| {
        let (table, rest) = temporary.split_at_mut(1);
        let (summary, holds) = rest.split_at_mut(1);
        let mut table = BufWriter::new(table[0].as_file_mut());
        let mut summary = BufWriter::new(summary[0].as_file_mut());
        writeln!(table, "box\tid\town\tpriority\troot\tclass\treach_weight\tdup_of")?;
        writeln!(summary, "box\tworkers\trate\thours\tcapacity\tremaining\tratio\tunstarted")?;
        for (b, hold) in boxes.iter().zip(holds) {
            let mut hold = BufWriter::new(hold.as_file_mut());
            let mut paths = BTreeMap::new();
            let speed = box_rate.get(&b.name).copied().unwrap_or_else(|| {
                rate.get(&b.name).copied().unwrap_or(args.default_rate)
                    * b.progress.workers.len() as f64
            });
            let capacity = speed * hours;
            ensure!(speed.is_finite() && capacity.is_finite(), "速さ・読める量が大きすぎます");
            let mut remaining = 0u64;
            let mut unstarted = 0;
            let mut counts = BTreeMap::<Class, usize>::new();
            let mut hold_sides = BTreeMap::<String, usize>::new();
            for tree in b.progress.trees.iter().filter(|t| !t.finished && !t.hold) {
                remaining = remaining
                    .checked_add(args.tree_size.max(tree.requested).saturating_sub(tree.read))
                    .context("残りの量が大きすぎます")?;
                let queued = &b.queue[&tree.id];
                let running =
                    b.progress.workers.iter().any(|w| {
                        w.key.as_ref().is_some_and(|k| queued.roots.contains(strip_ply(k)))
                    });
                if tree.read != 0 || running {
                    continue;
                }
                unstarted += 1;
                let path = paths.entry(&tree.id).or_insert_with(|| memo_path(queued, &start));
                let mut hold_tree = true;
                let mut tree_classes = BTreeSet::new();
                for root in &queued.roots {
                    let (class, weight) =
                        classify(queued, root, &tree.own, &walk, args.value_floor, path);
                    tree_classes.insert(class);
                    hold_tree &= class == Class::ReachLow || args.hold_class.contains(&class);
                    let dup = duplicates[&(root.clone(), tree.own.clone())]
                        .iter()
                        .filter(|(name, id)| *name != b.name || *id != tree.id)
                        .take(8)
                        .map(|(name, id)| format!("{name}:{id}"))
                        .collect::<Vec<_>>()
                        .join(",");
                    let total = duplicates[&(root.clone(), tree.own.clone())].len() - 1;
                    let dup = if total == 0 {
                        dup
                    } else {
                        format!("{dup};total={total}")
                    };
                    writeln!(
                        table,
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        b.name,
                        tree.id,
                        tree.own,
                        tree.priority,
                        root,
                        class.name(),
                        weight.map(|w| w.to_string()).unwrap_or_default(),
                        dup
                    )?;
                }
                for class in tree_classes {
                    *counts.entry(class).or_default() += 1;
                }
                if hold_tree {
                    *hold_sides.entry(tree.id.clone()).or_default() += 1;
                }
            }
            for (id, count) in hold_sides {
                let queued = &b.queue[&id];
                // bw の片側だけを hold にすると、同じ受付行でもう片側まで止まる。
                if count == queued.line.sides().len() {
                    let mut raw = queued.raw.clone();
                    raw["hold"] = Value::Bool(true);
                    serde_json::to_writer(&mut hold, &raw)?;
                    writeln!(hold)?;
                }
            }
            let ratio = if capacity > 0.0 {
                remaining as f64 / capacity
            } else if remaining == 0 {
                0.0
            } else {
                f64::INFINITY
            };
            writeln!(
                summary,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                b.name,
                b.progress.workers.len(),
                speed,
                hours,
                capacity,
                remaining,
                ratio,
                unstarted
            )?;
            writeln!(
                stdout,
                "{}: 残り/読める量={ratio:.3} ({remaining}/{capacity:.3})、未着手={unstarted}",
                b.name
            )?;
            for (class, count) in counts {
                writeln!(stdout, "  {}: {count}", class.name())?;
            }
            hold.flush()?;
        }
        table.flush()?;
        summary.flush()?;
        Ok(())
    })
}

#[cfg(test)]
mod tests;
