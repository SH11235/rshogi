use super::audit::reject_same_file;
use super::master_index::MasterIndex;
use super::master_table::{Row, mean_eval};
use super::model::{child, side};
#[cfg(test)]
use super::refutation::format_queue;
use super::reply_graph::HIRATE_KEY;
use super::schema::Schema;
use super::targets::{Line, MAX_DEPTH, MAX_PRIORITY, Opts, parse_root, read_checked_lines};
use super::walk::{check_tag, queue_id};
use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, BufWriter, Write};
use std::path::PathBuf;

mod disk;
use disk::{Record, Seen, Sorter, read_record};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Own {
    B,
    W,
}
impl Own {
    fn as_str(self) -> &'static str {
        match self {
            Self::B => "b",
            Self::W => "w",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Serialize, Deserialize)]
enum Class {
    Short,
    NovalueShallow,
    WeakShallow,
    Other,
}
impl Class {
    const ALL: [Self; 4] = [
        Self::Short,
        Self::NovalueShallow,
        Self::WeakShallow,
        Self::Other,
    ];

    fn as_str(self) -> &'static str {
        match self {
            Self::Short => "short",
            Self::NovalueShallow => "novalue-shallow",
            Self::WeakShallow => "weak-shallow",
            Self::Other => "other",
        }
    }
}

#[derive(Args, Debug)]
pub(super) struct ExitsArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 出口の一覧 (TSV)
    #[arg(long)]
    table: PathBuf,
    /// RAMU の側
    #[arg(long, value_enum)]
    side: Own,
    /// 初期局面から辿る手数の上限
    #[arg(long, default_value_t = 200)]
    max_ply: usize,
    #[arg(long, value_name = "USI")]
    start_moves: Option<String>,
    #[arg(long, allow_hyphen_values = true)]
    target_black: Option<i32>,
    #[arg(long, allow_hyphen_values = true)]
    target_white: Option<i32>,
    #[arg(long, default_value_t = 20)]
    shallow_ply: usize,
    /// tree enqueue --from に渡す JSON Lines
    #[arg(long)]
    enqueue_out: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        value_delimiter = ',',
        default_value = "short,novalue-shallow,weak-shallow"
    )]
    enqueue_class: Vec<Class>,
    #[arg(long, default_value_t = 10)]
    priority: u32,
    #[arg(long, default_value = "exits")]
    tag: String,
    #[arg(long, default_value_t = 6)]
    depth: usize,
    #[arg(long)]
    opp_n: Option<usize>,
    #[arg(long)]
    stop: Option<i32>,
    #[arg(long)]
    rounds: Option<usize>,
    #[arg(long, allow_hyphen_values = true)]
    draw_black: Option<i32>,
    #[arg(long, allow_hyphen_values = true)]
    draw_white: Option<i32>,
    /// 指定順に読み、id ごとの最後の行を使う
    #[arg(long)]
    exclude_queue: Vec<PathBuf>,
    #[arg(long, default_value_t = 6)]
    exclude_min_depth: usize,
    #[arg(long)]
    skip_covered_hold: bool,
}
impl ExitsArgs {
    fn opts(&self) -> Opts {
        Opts {
            opp_n: self.opp_n,
            stop: self.stop,
            rounds: self.rounds,
            draw_black: self.draw_black,
            draw_white: self.draw_white,
            ..Opts::default()
        }
    }

    fn check(&self) -> Result<()> {
        check_tag(&self.tag)?;
        self.opts().check()?;
        ensure!(self.depth <= MAX_DEPTH, "--depth は {MAX_DEPTH} 以下にしてください");
        ensure!(self.priority <= MAX_PRIORITY, "--priority は {MAX_PRIORITY} 以下にしてください");
        ensure!(
            !self.enqueue_class.is_empty()
                && self.enqueue_class.iter().collect::<BTreeSet<_>>().len()
                    == self.enqueue_class.len(),
            "--enqueue-class は重複のない class を指定してください"
        );
        let mut inputs = vec![("--master", self.master.as_path())];
        for path in &self.exclude_queue {
            inputs.push(("--exclude-queue", path.as_path()));
        }
        let schema_paths: Vec<_> =
            self.schema.iter().flat_map(|dir| super::schema::paths(dir)).collect();
        for path in &schema_paths {
            inputs.push(("--schema", path.as_path()));
        }
        reject_same_file(&self.table, &inputs)?;
        if let Some(out) = &self.enqueue_out {
            inputs.push(("--table", self.table.as_path()));
            reject_same_file(out, &inputs)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Entry {
    key: String,
    mv: String,
    value: Option<i64>,
    kind: String,
    source: String,
}
impl Entry {
    fn from_row(row: &Row) -> Result<Self> {
        let sign = if side(row.get("key")) == "b" { 1 } else { -1 };
        Ok(Self {
            key: row.get("key").to_owned(),
            mv: row.get("move").to_owned(),
            value: mean_eval(row.get("evals"))?.map(|v| i64::from(v) * sign),
            kind: row.get("value_kind").to_owned(),
            source: row.get("source").to_owned(),
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Exit {
    key: String,
    ply: usize,
    entry: Entry,
    #[cfg(test)]
    #[serde(skip)]
    entries: BTreeMap<(String, String), Entry>,
    class: Class,
    covered_by: BTreeSet<String>,
    covered_hold: BTreeSet<String>,
}

fn classify(entry: &Entry, ply: usize, args: &ExitsArgs) -> Class {
    let short = entry.value.is_some_and(|v| match args.side {
        Own::B => args.target_black.is_some_and(|t| v < i64::from(t)),
        Own::W => args.target_white.is_some_and(|t| v >= i64::from(t)),
    });
    if short {
        return Class::Short;
    }
    if ply <= args.shallow_ply {
        if entry.value.is_none() {
            return Class::NovalueShallow;
        }
        let weak = matches!(entry.kind.as_str(), "" | "single" | "unknown")
            || entry.kind.starts_with("mean:")
            || entry
                .kind
                .strip_prefix("tree:")
                .and_then(|n| n.parse::<u64>().ok())
                .is_some_and(|n| n < 6);
        if weak {
            return Class::WeakShallow;
        }
    }
    Class::Other
}

#[derive(Default)]
struct Coverage {
    active: BTreeSet<String>,
    held: BTreeSet<String>,
}

fn coverage(args: &ExitsArgs) -> Result<BTreeMap<String, Coverage>> {
    let mut latest = BTreeMap::new();
    for path in &args.exclude_queue {
        read_checked_lines(path, |line, _, _| {
            latest.insert(line.id.clone(), line);
            Ok(())
        })?;
    }
    let mut covered: BTreeMap<String, Coverage> = BTreeMap::new();
    for line in latest.into_values() {
        if line.depth < args.exclude_min_depth || !line.own.contains(args.side.as_str()) {
            continue;
        }
        for root in std::iter::once(&line.root).chain(&line.roots) {
            let c = covered.entry(parse_root(root)?.key).or_default();
            if line.hold {
                c.held.insert(line.id.clone());
            } else {
                c.active.insert(line.id.clone());
            }
        }
    }
    Ok(covered)
}

struct Traversal {
    visited: usize,
    reader: BufReader<File>,
    pending: Option<Record>,
}

impl Traversal {
    fn next_exit(&mut self) -> Result<Option<Exit>> {
        loop {
            let record = match self.pending.take() {
                Some(record) => Some(record),
                None => read_record(&mut self.reader)?,
            };
            let Some(record) = record else {
                return Ok(None);
            };
            if record.key[1] == "0" {
                return Ok(Some(serde_json::from_str(&record.value)?));
            }
        }
    }

    fn entries(&mut self, exit: &Exit, mut visit: impl FnMut(&Entry) -> Result<()>) -> Result<()> {
        let mut last: Option<Entry> = None;
        let mut found = false;
        while let Some(record) = read_record(&mut self.reader)? {
            if record.key[0] != exit.key {
                self.pending = Some(record);
                break;
            }
            let entry: Entry = serde_json::from_str(&record.value)?;
            if let Some(old) = &last
                && (old.key != entry.key || old.mv != entry.mv)
            {
                visit(old)?;
            }
            last = Some(entry);
            found = true;
        }
        if let Some(entry) = last {
            visit(&entry)?;
        }
        if !found && !exit.entry.mv.is_empty() {
            visit(&exit.entry)?;
        }
        Ok(())
    }
}

fn traverse(index: &mut MasterIndex, args: &ExitsArgs, initial: &str) -> Result<Traversal> {
    let mut start = initial.to_owned();
    let mut start_entry = Entry::default();
    let moves: Vec<_> =
        args.start_moves.as_deref().unwrap_or_default().split_whitespace().collect();
    ensure!(moves.len() <= args.max_ply, "--start-moves が --max-ply を超えています");
    for (i, mv) in moves.iter().enumerate() {
        start_entry = index
            .rows(&start)?
            .iter()
            .find(|r| r.get("move") == *mv)
            .map(Entry::from_row)
            .transpose()?
            .unwrap_or_else(|| Entry {
                key: start.clone(),
                mv: (*mv).to_owned(),
                ..Entry::default()
            });
        start = child(&start, mv).with_context(|| format!("--start-moves の {} 手目", i + 1))?;
    }
    let mut seen = Seen::new(&start)?;
    let mut visited = 1;
    let mut records = Sorter::new();
    let mut first = Sorter::new();
    first.push(vec![String::new(), start], serde_json::to_string(&start_entry)?)?;
    let mut layer = first.finish()?;
    let mut ply = moves.len();
    let mut sequence = 0u64;
    loop {
        let layer_start = sequence;
        let mut candidates = Sorter::new();
        while let Some(record) = read_record(&mut layer)? {
            let key = record.key[1].clone();
            let entry: Entry = serde_json::from_str(&record.value)?;
            let rows = index.rows(&key)?;
            if !rows.iter().any(|r| matches!(r.get("role"), "main" | "alt")) {
                let exit = Exit {
                    class: classify(&entry, ply, args),
                    key: key.clone(),
                    ply,
                    entry,
                    #[cfg(test)]
                    entries: BTreeMap::new(),
                    covered_by: BTreeSet::new(),
                    covered_hold: BTreeSet::new(),
                };
                records.push(vec![key, "0".into()], serde_json::to_string(&exit)?)?;
                continue;
            }
            if ply >= args.max_ply {
                continue;
            }
            for row in rows.iter().filter(|r| {
                r.get("role") == "main"
                    || (side(&key) != args.side.as_str() && r.get("role") == "alt")
            }) {
                let child = child(&key, row.get("move"))
                    .with_context(|| format!("元データの手: {key} {}", row.get("move")))?;
                let entry = Entry::from_row(row)?;
                sequence = sequence.checked_add(1).context("辿った辺の数が大きすぎます")?;
                records.push(
                    vec![
                        child.clone(),
                        "1".into(),
                        key.clone(),
                        entry.mv.clone(),
                        format!("{sequence:020}"),
                    ],
                    serde_json::to_string(&entry)?,
                )?;
                candidates
                    .push(vec![child, format!("{sequence:020}")], serde_json::to_string(&entry)?)?;
            }
        }
        if sequence == layer_start {
            break;
        }
        let (next, added) = seen.advance(candidates)?;
        visited += added;
        if added == 0 {
            break;
        }
        layer = next;
        ply += 1;
    }
    Ok(Traversal {
        visited,
        reader: records.finish()?,
        pending: None,
    })
}

fn cover(exit: &mut Exit, covered: &BTreeMap<String, Coverage>) {
    if let Some(c) = covered.get(&exit.key) {
        exit.covered_by = c.active.clone();
        if c.active.is_empty() {
            exit.covered_hold = c.held.clone();
        }
    }
}

fn value_text(value: Option<i64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

const HEADER: &str = "side\tply\tkey\tentry_key\tentry_move\tentry_value_black\tentry_value_kind\tentry_source\tentries\tclass\tcovered_by\tcovered_hold\n";

fn table_prefix(out: &mut impl Write, e: &Exit, own: &str) -> Result<()> {
    write!(
        out,
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
        own,
        e.ply,
        e.key,
        e.entry.key,
        e.entry.mv,
        value_text(e.entry.value),
        e.entry.kind,
        e.entry.source
    )?;
    Ok(())
}

fn table_entry(out: &mut impl Write, entry: &Entry, first: &mut bool) -> Result<()> {
    if !*first {
        write!(out, ",")?;
    }
    *first = false;
    write!(out, "{}:{}", entry.mv, value_text(entry.value))?;
    Ok(())
}

fn table_suffix(out: &mut impl Write, e: &Exit) -> Result<()> {
    writeln!(
        out,
        "\t{}\t{}\t{}",
        e.class.as_str(),
        e.covered_by.iter().cloned().collect::<Vec<_>>().join(","),
        e.covered_hold.iter().cloned().collect::<Vec<_>>().join(",")
    )?;
    Ok(())
}

#[cfg(test)]
fn table(exits: &[Exit], own: &str) -> String {
    let mut out = HEADER.as_bytes().to_vec();
    for e in exits {
        table_prefix(&mut out, e, own).unwrap();
        let mut first = true;
        for entry in e.entries.values() {
            table_entry(&mut out, entry, &mut first).unwrap();
        }
        table_suffix(&mut out, e).unwrap();
    }
    String::from_utf8(out).unwrap()
}

#[cfg(test)]
fn queue(exits: &[Exit], args: &ExitsArgs) -> Result<Vec<Line>> {
    let mut selected: Vec<_> = exits
        .iter()
        .filter_map(|e| {
            let rank = args.enqueue_class.iter().position(|c| *c == e.class)?;
            (e.covered_by.is_empty() && (!args.skip_covered_hold || e.covered_hold.is_empty()))
                .then_some((rank, e))
        })
        .collect();
    selected.sort_by(|(a, x), (b, y)| a.cmp(b).then(x.ply.cmp(&y.ply)).then(x.key.cmp(&y.key)));
    selected
        .iter()
        .enumerate()
        .map(|(rank, (_, e))| queue_line(e, rank, args))
        .collect()
}

fn queue_line(e: &Exit, rank: usize, args: &ExitsArgs) -> Result<Line> {
    let priority = args
        .priority
        .checked_add(u32::try_from(rank)?)
        .context("priority が大きすぎます")?;
    let root_ply = e.ply.checked_add(1).context("根の手数が大きすぎます")?;
    let line = Line {
        id: queue_id(&args.tag, root_ply, &e.key, args.side.as_str()),
        root: format!("sfen {} {root_ply}", e.key),
        roots: Vec::new(),
        own: args.side.as_str().to_owned(),
        depth: args.depth,
        priority,
        hold: false,
        game: Vec::new(),
        opts: args.opts(),
        memo: Some(format!(
            "exits class={} entry_value_black={} entry_value_kind={} entry_move={}",
            e.class.as_str(),
            value_text(e.entry.value),
            e.entry.kind,
            e.entry.mv
        )),
    };
    line.check()?;
    Ok(line)
}

fn output_temp(path: &std::path::Path) -> Result<tempfile::NamedTempFile> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    Ok(tempfile::NamedTempFile::new_in(parent)?)
}

pub(super) fn run(args: ExitsArgs) -> Result<()> {
    args.check()?;
    let mut index = MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?;
    let mut traversal = traverse(&mut index, &args, HIRATE_KEY)?;
    let covered = coverage(&args)?;
    let mut table = output_temp(&args.table)?;
    let mut queued = Sorter::new();
    let mut counts = [0usize; 4];
    let mut excluded = 0;
    let mut held = 0;
    {
        let mut out = BufWriter::new(table.as_file_mut());
        out.write_all(HEADER.as_bytes())?;
        while let Some(mut exit) = traversal.next_exit()? {
            cover(&mut exit, &covered);
            let class_index = Class::ALL
                .iter()
                .position(|c| *c == exit.class)
                .context("出口の分類がありません")?;
            counts[class_index] += 1;
            let skip = !exit.covered_by.is_empty()
                || (args.skip_covered_hold && !exit.covered_hold.is_empty());
            excluded += usize::from(skip);
            held += usize::from(!exit.covered_hold.is_empty());
            table_prefix(&mut out, &exit, args.side.as_str())?;
            let mut first = true;
            traversal.entries(&exit, |entry| table_entry(&mut out, entry, &mut first))?;
            table_suffix(&mut out, &exit)?;
            if args.enqueue_out.is_some()
                && !skip
                && let Some(rank) = args.enqueue_class.iter().position(|c| *c == exit.class)
            {
                queued.push(
                    vec![
                        rank.to_string(),
                        format!("{:020}", exit.ply),
                        exit.key.clone(),
                    ],
                    serde_json::to_string(&exit)?,
                )?;
            }
        }
        out.flush()?;
    }
    let mut queued_count = 0;
    let mut queue_temp = None;
    if let Some(path) = &args.enqueue_out {
        let mut temp = output_temp(path)?;
        {
            let mut out = BufWriter::new(temp.as_file_mut());
            let mut reader = queued.finish()?;
            while let Some(record) = read_record(&mut reader)? {
                let exit = serde_json::from_str(&record.value)?;
                let line = queue_line(&exit, queued_count, &args)?;
                serde_json::to_writer(&mut out, &line)?;
                out.write_all(b"\n")?;
                queued_count += 1;
            }
            out.flush()?;
        }
        queue_temp = Some(temp);
    }
    table.persist(&args.table)?;
    if let Some(temp) = queue_temp {
        temp.persist(args.enqueue_out.as_ref().context("キューの出力先がありません")?)?;
    }
    let classes = Class::ALL
        .iter()
        .zip(counts)
        .map(|(c, n)| format!("{}={n}", c.as_str()))
        .collect::<Vec<_>>()
        .join(" / ");
    println!(
        "{}: 辿った局面 {} / 出口 {} / {} / 除外 {excluded} / hold {held} / キューの行 {queued_count}",
        args.side.as_str(),
        traversal.visited,
        counts.iter().sum::<usize>(),
        classes
    );
    Ok(())
}

#[cfg(test)]
mod tests;
