//! 台帳の上位二手の差から一本道の区間を抽出する。
use super::{export::files, file_lock, journal, master_table, schema::Schema};
use crate::book_db::{apply_usi_move, child_position_after_move, position_from_sfen, strip_ply};
use anyhow::{Context, Result, ensure};
use clap::{Args, ValueEnum};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, BufWriter, Read, Seek, Write},
    path::PathBuf,
};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Side {
    B,
    W,
    Both,
}
impl Side {
    fn includes(self, key: &str) -> bool {
        matches!(self, Self::Both) || side(key) == if matches!(self, Self::B) { "b" } else { "w" }
    }
}
#[derive(Args, Debug)]
pub(super) struct OneWayArgs {
    #[arg(long)]
    master: PathBuf,
    #[arg(long)]
    schema: Option<PathBuf>,
    #[arg(long, required = true)]
    journal: Vec<PathBuf>,
    #[arg(long, value_enum, default_value = "both")]
    side: Side,
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(i32).range(0..))]
    gap: i32,
    #[arg(long, default_value_t = 3)]
    min_run: usize,
    #[arg(long)]
    moves: Vec<String>,
    #[arg(long, default_value_t = 100000)]
    max_rows: usize,
    #[arg(long, default_value_t = 10_000_000)]
    max_visits: usize,
    #[arg(long)]
    table: PathBuf,
}
#[derive(Default)]
struct Node {
    ply: u64,
    moves: BTreeMap<String, String>,
}
type Graph = BTreeMap<String, Node>;
type Reads = BTreeMap<String, (journal::Reading, String)>;
fn side(key: &str) -> &str {
    key.split_whitespace().nth(1).unwrap_or("")
}
fn gap(read: &journal::Reading) -> Option<i64> {
    let lines = read.lines();
    (lines.len() >= 2).then(|| i64::from(lines[0].cp) - i64::from(lines[1].cp))
}
fn black(key: &str, value: i32) -> i64 {
    i64::from(value) * if side(key) == "w" { -1 } else { 1 }
}
fn load_graph(args: &OneWayArgs) -> Result<Graph> {
    let schema = Schema::load(args.schema.as_deref())?;
    let mut graph = Graph::new();
    for row in master_table::read_rows(BufReader::new(File::open(&args.master)?))? {
        let (_, row) = row?;
        schema.validate_row(&row)?;
        let key = row.get("key");
        let ply: u64 = row.get("ply").parse()?;
        ensure!(ply > 0, "ply は正数が必要です");
        let child = child_position_after_move(&format!("{key} 1"), row.get("move"))?;
        let node = graph.entry(key.into()).or_insert_with(|| Node {
            ply,
            ..Node::default()
        });
        node.ply = node.ply.min(ply);
        if matches!(row.get("role"), "main" | "alt") {
            node.moves.insert(row.get("move").into(), strip_ply(&child.to_sfen()).into());
        }
    }
    Ok(graph)
}
#[derive(Default)]
struct Diagnostics {
    invalid: usize,
    conflicting: usize,
}
type Warnings = BTreeMap<String, Diagnostics>;
fn report_warnings(out: &mut impl Write, warnings: &Warnings) -> Result<()> {
    for (fp, counts) in warnings {
        writeln!(
            out,
            "one-way: fingerprint={fp} invalid={} conflicting={}",
            counts.invalid, counts.conflicting
        )?;
    }
    Ok(())
}
fn load_reads(
    paths: &[PathBuf],
    keys: &BTreeSet<String>,
    warnings: &mut Warnings,
) -> Result<Reads> {
    let mut reads = Reads::new();
    let mut validity = journal::Validity::default();
    let dir = tempfile::tempdir()?;
    let mut buckets = super::source_buckets::Buckets::new(dir.path(), 0)?;
    let mut staged = BufWriter::new(tempfile::tempfile_in(dir.path())?);
    let mut rows = 0;

    for path in paths {
        let file = File::open(path)?;
        file_lock::try_lock_shared(&file)?;
        let mut reader = BufReader::new(file);
        let mut bytes = Vec::new();
        let mut at = 0;
        loop {
            bytes.clear();
            if reader.read_until(b'\n', &mut bytes)? == 0 {
                break;
            }
            at += 1;
            if !bytes.ends_with(b"\n") {
                eprintln!("{}: 末尾の未完行を除外", path.display());
                break;
            }
            let rec = journal::parse_record(&bytes)
                .with_context(|| format!("{}:{at}", path.display()))?;
            if !keys.contains(&rec.reading.key) {
                continue;
            }
            if !validity.valid(&rec)? {
                warnings.entry(rec.settings.fingerprint).or_default().invalid += 1;
                continue;
            }
            buckets.push(&rec.reading, &rec.settings.fingerprint, rows, false)?;
            serde_json::to_writer(&mut staged, &(&rec.reading, &rec.settings.fingerprint))?;
            writeln!(staged)?;
            rows += 1;
        }
    }
    let mut marks = BufReader::new(buckets.finish(dir.path(), rows)?);
    let mut staged = staged.into_inner()?;
    staged.rewind()?;
    for line in BufReader::new(staged).lines() {
        let (reading, fp): (journal::Reading, String) = serde_json::from_str(&line?)?;
        let mut mark = [0];
        marks.read_exact(&mut mark)?;
        if mark[0] & 4 != 0 {
            warnings.entry(fp).or_default().conflicting += 1;
            continue;
        }
        if mark[0] & 2 != 0 || reading.lines().len() < 2 {
            continue;
        }
        let replace = reads.get(&reading.key).is_none_or(|(old, old_fp)| {
            journal::compare_readings(&reading, &fp, old, old_fp).is_gt()
        });
        if replace {
            reads.insert(reading.key.clone(), (reading, fp));
        }
    }
    Ok(reads)
}

struct Edge<'a> {
    key: &'a str,
    moves: [&'a str; 2],
}
struct Links<'a> {
    edges: Vec<Edge<'a>>,
    terminal: bool,
}
fn links<'a>(graph: &'a Graph, reads: &Reads, args: &OneWayArgs) -> BTreeMap<&'a str, Links<'a>> {
    let good = |key: &str| {
        args.side.includes(key)
            && reads
                .get(key)
                .is_some_and(|(r, _)| gap(r).is_some_and(|g| g >= i64::from(args.gap)))
    };
    let mut links = BTreeMap::new();
    for (key, node) in graph.iter().filter(|(key, _)| good(key)) {
        let mut edges = Vec::new();
        let mut terminal = false;
        let best = &reads[key].0.multipv[0].mv;
        if let Some((mv, child)) = node.moves.get_key_value(best)
            && let Some(opp) = graph.get(child)
        {
            for (reply, next) in &opp.moves {
                if graph.contains_key(next) && good(next) {
                    edges.push(Edge {
                        key: next,
                        moves: [mv, reply],
                    });
                } else {
                    terminal = true;
                }
            }
        }
        terminal |= edges.is_empty();
        links.insert(key.as_str(), Links { edges, terminal });
    }
    links
}
// 循環へ到達できる局面には無限大を残し、枝刈りで有効な経路を落とさない。
fn remaining_lengths<'a>(links: &BTreeMap<&'a str, Links<'a>>) -> BTreeMap<&'a str, usize> {
    let mut incoming: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut pending = BTreeMap::new();
    let mut lengths = BTreeMap::new();
    let mut ready = Vec::new();
    for (&key, link) in links {
        pending.insert(key, link.edges.len());
        lengths.insert(key, 1usize);
        if link.edges.is_empty() {
            ready.push(key);
        }
        for edge in &link.edges {
            incoming.entry(edge.key).or_default().push(key);
        }
    }
    while let Some(key) = ready.pop() {
        if let Some(parents) = incoming.get(key) {
            for parent in parents {
                let length = lengths[key].saturating_add(1);
                let old = lengths.get_mut(parent).expect("親局面");
                *old = (*old).max(length);
                let count = pending.get_mut(parent).expect("親局面");
                *count -= 1;
                if *count == 0 {
                    ready.push(parent);
                }
            }
        }
    }
    for (key, count) in pending {
        if count != 0 {
            lengths.insert(key, usize::MAX);
        }
    }
    lengths
}
fn intervals(
    out: &mut impl Write,
    graph: &Graph,
    reads: &Reads,
    args: &OneWayArgs,
) -> Result<Option<&'static str>> {
    writeln!(
        out,
        "side\tstart_ply\tend_ply\trun\tstart_key\tend_key\tstart_value_black\tend_value_black\tmin_gap\tmoves"
    )?;
    let links = links(graph, reads, args);
    let lengths = remaining_lengths(&links);
    let mut visits = 0usize;
    let incoming: BTreeSet<_> = links.values().flat_map(|l| &l.edges).map(|e| e.key).collect();
    let roots: Vec<_> = links
        .keys()
        .copied()
        .filter(|k| !incoming.contains(k))
        .chain(links.keys().copied().filter(|k| incoming.contains(k)))
        .collect();
    let mut covered = BTreeSet::new();
    let mut rows = 0;
    for root in roots {
        if covered.contains(root) || lengths[root] < args.min_run {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        let mut path = BTreeSet::from([root]);
        let mut moves = Vec::new();
        while let Some(&(key, index)) = stack.last() {
            covered.insert(key);
            let edges = &links[key].edges;
            if index == 0
                && (links[key].terminal || edges.iter().any(|e| path.contains(e.key)))
                && stack.len() >= args.min_run
            {
                if rows == args.max_rows {
                    return Ok(Some("max-rows"));
                }
                let first = &reads[root].0;
                let last = &reads[key].0;
                let min_gap = stack
                    .iter()
                    .filter_map(|(k, _)| gap(&reads[*k].0))
                    .min()
                    .context("区間が空です")?;
                writeln!(
                    out,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    side(root),
                    graph[root].ply,
                    graph[root]
                        .ply
                        .checked_add(moves.len() as u64)
                        .context("区間の ply が範囲外です")?,
                    stack.len(),
                    root,
                    key,
                    black(root, first.multipv[0].cp),
                    black(key, last.multipv[0].cp),
                    min_gap,
                    moves.join(" ")
                )?;
                rows += 1;
            }
            if index == edges.len() {
                stack.pop();
                path.remove(key);
                if !stack.is_empty() {
                    moves.truncate(moves.len() - 2);
                }
            } else {
                stack.last_mut().context("区間が空です")?.1 += 1;
                let edge = &edges[index];
                if visits.saturating_add(2) > args.max_visits {
                    return Ok(Some("max-visits"));
                }
                visits += 2;
                if stack.len().saturating_add(lengths[edge.key]) < args.min_run {
                    continue;
                }
                if path.insert(edge.key) {
                    stack.push((edge.key, 0));
                    moves.extend(edge.moves);
                }
            }
        }
    }
    Ok(None)
}
fn move_keys(lines: &[String]) -> Result<BTreeSet<String>> {
    let mut keys = BTreeSet::new();
    for line in lines {
        let mut pos = position_from_sfen(rshogi_core::position::SFEN_HIRATE)?;
        for mv in line.split_whitespace() {
            keys.insert(strip_ply(&pos.to_sfen()).into());
            apply_usi_move(&mut pos, mv)?;
        }
    }
    Ok(keys)
}
fn move_table(out: &mut impl Write, reads: &Reads, args: &OneWayArgs) -> Result<bool> {
    writeln!(
        out,
        "line\tply\tside\tkey\tbest_move\tbest_value_black\tsecond_move\tsecond_value_black\tgap\tone_way\tplayed_move\tplayed_best"
    )?;
    let mut rows = 0;
    for (line, moves) in args.moves.iter().enumerate() {
        let mut pos = position_from_sfen(rshogi_core::position::SFEN_HIRATE)?;
        for (ply, mv) in moves.split_whitespace().enumerate() {
            if rows == args.max_rows {
                return Ok(true);
            }
            let sfen = pos.to_sfen();
            let key = strip_ply(&sfen);
            let read = reads.get(key).map(|(r, _)| r);
            let pvs = read.map_or(&[][..], journal::Reading::lines);
            let best = pvs.first();
            let second = pvs.get(1);
            let diff = read.and_then(gap);
            let status = diff.map_or("unknown", |g| {
                if g >= i64::from(args.gap) {
                    "yes"
                } else {
                    "no"
                }
            });
            writeln!(
                out,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                line + 1,
                ply + 1,
                side(key),
                key,
                best.map_or("", |p| &p.mv),
                best.map_or(String::new(), |p| black(key, p.cp).to_string()),
                second.map_or("", |p| &p.mv),
                second.map_or(String::new(), |p| black(key, p.cp).to_string()),
                diff.map_or(String::new(), |g| g.to_string()),
                status,
                mv,
                best.map_or("unknown", |p| if p.mv == mv { "yes" } else { "no" })
            )?;
            apply_usi_move(&mut pos, mv)?;
            rows += 1;
        }
    }
    Ok(false)
}
pub(super) fn run(mut args: OneWayArgs) -> Result<()> {
    ensure!(
        args.min_run > 0 && args.max_rows > 0 && args.max_visits > 0,
        "min-run / max-rows / max-visits は正数が必要です"
    );
    args.journal =
        args.journal.iter().map(std::fs::canonicalize).collect::<std::io::Result<_>>()?;
    args.journal.sort();
    args.journal.dedup();
    let mut inputs = args.journal.clone();
    inputs.push(args.master.clone());
    if let Some(dir) = &args.schema {
        inputs.extend(super::schema::paths(dir));
    }
    files::guard(std::slice::from_ref(&args.table), &inputs, true)?;
    let graph = load_graph(&args)?;
    let keys = if args.moves.is_empty() {
        graph.keys().cloned().collect()
    } else {
        move_keys(&args.moves)?
    };
    let mut warnings = Warnings::new();
    let reads = load_reads(&args.journal, &keys, &mut warnings)?;
    files::write_generated(std::slice::from_ref(&args.table), &inputs, true, |outputs| {
        let truncated = if args.moves.is_empty() {
            intervals(&mut outputs[0], &graph, &reads, &args)?
        } else {
            move_table(&mut outputs[0], &reads, &args)?.then_some("max-rows")
        };
        if let Some(limit) = truncated {
            eprintln!("one-way: --{limit} の上限で打ち切りました");
        }
        Ok(())
    })?;
    report_warnings(&mut std::io::stderr().lock(), &warnings)
}
#[cfg(test)]
mod tests;
