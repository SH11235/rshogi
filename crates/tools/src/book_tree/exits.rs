use super::audit::reject_same_file;
use super::master_index::MasterIndex;
use super::master_table::{Row, mean_eval};
use super::model::{child, side};
use super::refutation::format_queue;
use super::reply_graph::HIRATE_KEY;
use super::schema::Schema;
use super::targets::{Line, MAX_DEPTH, MAX_PRIORITY, Opts, parse_root, read_checked_lines};
use super::walk::{check_tag, queue_id};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::PathBuf;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
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

#[derive(Clone, Debug, Default)]
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

#[derive(Debug)]
struct Exit {
    key: String,
    ply: usize,
    entry: Entry,
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

fn traverse(
    index: &mut MasterIndex,
    args: &ExitsArgs,
    initial: &str,
) -> Result<(usize, Vec<Exit>)> {
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
    let mut pending = VecDeque::from([(start.clone(), moves.len(), start_entry)]);
    let mut seen = BTreeSet::from([start]);
    let mut exits: BTreeMap<String, Exit> = BTreeMap::new();
    let mut incoming: BTreeMap<String, BTreeMap<(String, String), Entry>> = BTreeMap::new();
    while let Some((key, ply, entry)) = pending.pop_front() {
        let rows = index.rows(&key)?;
        if !rows.iter().any(|r| matches!(r.get("role"), "main" | "alt")) {
            exits.insert(
                key.clone(),
                Exit {
                    class: classify(&entry, ply, args),
                    key,
                    ply,
                    entry,
                    entries: BTreeMap::new(),
                    covered_by: BTreeSet::new(),
                    covered_hold: BTreeSet::new(),
                },
            );
            continue;
        }
        incoming.remove(&key);
        if ply >= args.max_ply {
            continue;
        }
        for row in rows.iter().filter(|r| {
            r.get("role") == "main" || (side(&key) != args.side.as_str() && r.get("role") == "alt")
        }) {
            let next = child(&key, row.get("move"))
                .with_context(|| format!("元データの手: {key} {}", row.get("move")))?;
            let entry = Entry::from_row(row)?;
            // 展開済みの非出口には入口を残さず、辺の総数に比例する保持を避ける。
            if !seen.contains(&next) || incoming.contains_key(&next) || exits.contains_key(&next) {
                incoming
                    .entry(next.clone())
                    .or_default()
                    .insert((key.clone(), entry.mv.clone()), entry.clone());
            }
            if seen.insert(next.clone()) {
                pending.push_back((next, ply + 1, entry));
            }
        }
    }
    let covered = coverage(args)?;
    for exit in exits.values_mut() {
        exit.entries = incoming.remove(&exit.key).unwrap_or_default();
        if exit.entries.is_empty() && !exit.entry.mv.is_empty() {
            exit.entries
                .insert((exit.entry.key.clone(), exit.entry.mv.clone()), exit.entry.clone());
        }
        if let Some(c) = covered.get(&exit.key) {
            exit.covered_by = c.active.clone();
            if c.active.is_empty() {
                exit.covered_hold = c.held.clone();
            }
        }
    }
    Ok((seen.len(), exits.into_values().collect()))
}

fn value_text(value: Option<i64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

fn table(exits: &[Exit], own: &str) -> String {
    let mut text = "side\tply\tkey\tentry_key\tentry_move\tentry_value_black\tentry_value_kind\tentry_source\tentries\tclass\tcovered_by\tcovered_hold\n".to_owned();
    for e in exits {
        let entries = e
            .entries
            .values()
            .map(|entry| format!("{}:{}", entry.mv, value_text(entry.value)))
            .collect::<Vec<_>>()
            .join(",");
        let fields = [
            own.to_owned(),
            e.ply.to_string(),
            e.key.clone(),
            e.entry.key.clone(),
            e.entry.mv.clone(),
            value_text(e.entry.value),
            e.entry.kind.clone(),
            e.entry.source.clone(),
            entries,
            e.class.as_str().to_owned(),
            e.covered_by.iter().cloned().collect::<Vec<_>>().join(","),
            e.covered_hold.iter().cloned().collect::<Vec<_>>().join(","),
        ];
        writeln!(text, "{}", fields.join("\t")).expect("String への書き込み");
    }
    text
}

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
        .map(|(rank, (_, e))| {
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
        })
        .collect()
}

pub(super) fn run(args: ExitsArgs) -> Result<()> {
    args.check()?;
    let mut index = MasterIndex::open(&args.master, &Schema::load(args.schema.as_deref())?)?;
    let (visited, exits) = traverse(&mut index, &args, HIRATE_KEY)?;
    let queued = if args.enqueue_out.is_some() {
        queue(&exits, &args)?
    } else {
        Vec::new()
    };
    let queue_text = format_queue(&queued)?;
    write_atomic(&args.table, &table(&exits, args.side.as_str()))?;
    if let Some(out) = &args.enqueue_out {
        write_atomic(out, &queue_text)?;
    }
    let counts = Class::ALL.map(|class| {
        format!("{}={}", class.as_str(), exits.iter().filter(|e| e.class == class).count())
    });
    let excluded = exits
        .iter()
        .filter(|e| {
            !e.covered_by.is_empty() || (args.skip_covered_hold && !e.covered_hold.is_empty())
        })
        .count();
    let held = exits.iter().filter(|e| !e.covered_hold.is_empty()).count();
    println!(
        "{}: 辿った局面 {visited} / 出口 {} / {} / 除外 {excluded} / hold {held} / キューの行 {}",
        args.side.as_str(),
        exits.len(),
        counts.join(" / "),
        queued.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests;
