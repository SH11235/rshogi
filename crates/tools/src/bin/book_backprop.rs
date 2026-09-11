//! YANEURAOU-DB2016 テキスト `.db` の評価値を negamax 逆伝播するツール。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use rshogi_core::position::Position;
use rshogi_core::types::Move;

const BOOK_HEADER: &str = "#YANEURAOU-DB2016 1.00";

#[derive(Parser, Debug)]
#[command(about = "YANEURAOU-DB2016 テキスト定跡 .db の評価値を negamax 逆伝播する")]
struct Cli {
    #[arg(long)]
    book: PathBuf,
    #[arg(long)]
    out: PathBuf,
    #[arg(long, default_value_t = 0)]
    draw_value: i32,
    #[arg(long)]
    report: Option<PathBuf>,
    #[arg(long, default_value_t = 1000)]
    max_iters: usize,
    #[arg(long, value_enum, default_value_t = MergeMode::Min)]
    merge: MergeMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MergeMode {
    Replace,
    Min,
}

impl MergeMode {
    fn apply(self, old: i32, propagated: i32) -> i32 {
        match self {
            MergeMode::Replace => propagated,
            MergeMode::Min => old.min(propagated),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            MergeMode::Replace => "replace",
            MergeMode::Min => "min",
        }
    }
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

#[derive(Debug, Clone)]
struct BookMove {
    move_usi: Option<String>,
    ponder_usi: Option<String>,
    value: i32,
    depth: i32,
    count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Edge {
    to: usize,
    via_flip: bool,
}

#[derive(Debug, Clone)]
struct MoveValue {
    old: i32,
    new: i32,
    edge: Option<Edge>,
}

#[derive(Debug, Clone)]
struct Graph {
    keys: Vec<String>,
    moves: Vec<Vec<MoveValue>>,
    adjacency: Vec<Vec<usize>>,
    flip_edges: usize,
    illegal_moves: usize,
}

#[derive(Debug, Clone)]
struct SccGraph {
    comp_of: Vec<usize>,
    comps: Vec<Vec<usize>>,
    edges: Vec<Vec<usize>>,
    topo: Vec<usize>,
    nontrivial: Vec<bool>,
}

#[derive(Debug, Default, Clone)]
struct PropagationStats {
    updated_moves: usize,
    abs_deltas: Vec<i32>,
    nontrivial_sccs: usize,
    max_scc_size: usize,
    draw_moves: usize,
    scc_iters: Vec<usize>,
    depths: Vec<usize>,
    changed_nodes: Vec<NodeChange>,
}

#[derive(Debug, Clone)]
struct NodeChange {
    sfen: String,
    old_best: i32,
    new_best: i32,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.max_iters == 0 {
        bail!("--max-iters は 1 以上を指定してください");
    }

    rshogi_book::Book::from_path(&cli.book, true)
        .with_context(|| format!("定跡を rshogi-book で読めません: {}", cli.book.display()))?;
    let book = read_book_db(&cli.book)?;
    let mut graph = build_graph(&book)?;
    let stats = propagate_values(&book, &mut graph, cli.draw_value, cli.max_iters, cli.merge)?;
    write_backprop_book(&book, &graph, &cli.out)?;
    if let Some(path) = &cli.report {
        write_report(&book, &graph, &stats, path, cli.merge)?;
    }
    Ok(())
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
            let key = strip_ply(&sfen).to_string();
            entries
                .entry(key.clone())
                .and_modify(|entry| {
                    if ply_of(&sfen) < ply_of(&entry.sfen) {
                        entry.sfen = sfen.clone();
                    }
                })
                .or_insert(PositionEntry {
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

fn ply_of(sfen: &str) -> u32 {
    sfen.rsplit_once(' ')
        .and_then(|(_, tail)| tail.parse::<u32>().ok())
        .unwrap_or(u32::MAX)
}

fn build_graph(book: &BookDb) -> Result<Graph> {
    let keys: Vec<String> = book.entries.keys().cloned().collect();
    let node_index: BTreeMap<&str, usize> =
        keys.iter().enumerate().map(|(idx, key)| (key.as_str(), idx)).collect();
    let mut moves = Vec::with_capacity(keys.len());
    let mut adjacency_sets = vec![BTreeSet::new(); keys.len()];
    let mut flip_edges = 0;
    let mut illegal_moves = 0;

    for (node_idx, key) in keys.iter().enumerate() {
        let entry = book
            .entries
            .get(key)
            .ok_or_else(|| anyhow!("内部エラー: entry がありません: {key}"))?;
        let mut move_values = Vec::with_capacity(entry.moves.len());
        for book_move in &entry.moves {
            let edge = if let Some(move_usi) = &book_move.move_usi {
                match child_position_after_move(&entry.sfen, move_usi) {
                    Ok(child) => {
                        let child_sfen = child.to_sfen();
                        let child_key = strip_ply(&child_sfen);
                        if let Some(&to) = node_index.get(child_key) {
                            Some(Edge {
                                to,
                                via_flip: false,
                            })
                        } else {
                            let flipped = rshogi_book::flipped_key(&child_sfen);
                            let flipped_to = flipped
                                .as_deref()
                                .map(strip_ply)
                                .and_then(|key| node_index.get(key).copied());
                            flipped_to.map(|to| {
                                flip_edges += 1;
                                Edge { to, via_flip: true }
                            })
                        }
                    }
                    Err(err) => {
                        illegal_moves += 1;
                        eprintln!(
                            "警告: 非合法手を逆伝播から除外します: sfen={} move={} error={err:#}",
                            entry.sfen, move_usi
                        );
                        None
                    }
                }
            } else {
                None
            };
            if let Some(edge) = edge {
                adjacency_sets[node_idx].insert(edge.to);
            }
            move_values.push(MoveValue {
                old: book_move.value,
                new: book_move.value,
                edge,
            });
        }
        moves.push(move_values);
    }

    let adjacency = adjacency_sets.into_iter().map(|set| set.into_iter().collect()).collect();
    Ok(Graph {
        keys,
        moves,
        adjacency,
        flip_edges,
        illegal_moves,
    })
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

fn propagate_values(
    book: &BookDb,
    graph: &mut Graph,
    draw_value: i32,
    max_iters: usize,
    merge: MergeMode,
) -> Result<PropagationStats> {
    let scc = build_scc_graph(&graph.adjacency);
    let mut node_best = vec![draw_value; graph.keys.len()];
    let mut stats = PropagationStats {
        nontrivial_sccs: scc.nontrivial.iter().filter(|&&v| v).count(),
        max_scc_size: scc.comps.iter().map(Vec::len).max().unwrap_or(0),
        ..PropagationStats::default()
    };
    let mut scc_depth = vec![0usize; scc.comps.len()];

    for &comp in scc.topo.iter().rev() {
        let depth = scc.edges[comp].iter().map(|&child| scc_depth[child] + 1).max().unwrap_or(0);
        scc_depth[comp] = depth;

        if scc.nontrivial[comp] {
            let iters = iterate_nontrivial_scc(
                graph,
                &scc,
                comp,
                &mut node_best,
                draw_value,
                max_iters,
                merge,
            )?;
            stats.scc_iters.push(iters);
        } else {
            let node = scc.comps[comp][0];
            node_best[node] =
                compute_node_best(graph, &scc, comp, node, &node_best, draw_value, merge);
        }
    }

    for (node_idx, move_values) in graph.moves.iter_mut().enumerate() {
        let comp = scc.comp_of[node_idx];
        for mv in move_values {
            if let Some(edge) = mv.edge {
                let value = if scc.nontrivial[comp] && scc.comp_of[edge.to] == comp {
                    draw_value.max(-node_best[edge.to])
                } else {
                    -node_best[edge.to]
                };
                mv.new = merge.apply(mv.old, value);
                if scc.nontrivial[comp] && scc.comp_of[edge.to] == comp && mv.new == draw_value {
                    stats.draw_moves += 1;
                }
            }
        }
    }

    for (node_idx, key) in graph.keys.iter().enumerate() {
        let old_best = graph.moves[node_idx].iter().map(|mv| mv.old).max().unwrap_or(draw_value);
        let new_best = graph.moves[node_idx].iter().map(|mv| mv.new).max().unwrap_or(draw_value);
        if old_best != new_best {
            let entry = book
                .entries
                .get(key)
                .ok_or_else(|| anyhow!("内部エラー: entry がありません: {key}"))?;
            stats.changed_nodes.push(NodeChange {
                sfen: entry.sfen.clone(),
                old_best,
                new_best,
            });
        }
        stats.depths.push(scc_depth[scc.comp_of[node_idx]]);
        for mv in &graph.moves[node_idx] {
            if mv.old != mv.new {
                stats.updated_moves += 1;
                stats.abs_deltas.push((mv.new - mv.old).abs());
            }
        }
    }

    stats.changed_nodes.sort_by(|a, b| {
        (b.new_best - b.old_best)
            .abs()
            .cmp(&(a.new_best - a.old_best).abs())
            .then_with(|| a.sfen.cmp(&b.sfen))
    });
    Ok(stats)
}

fn iterate_nontrivial_scc(
    graph: &Graph,
    scc: &SccGraph,
    comp: usize,
    node_best: &mut [i32],
    draw_value: i32,
    max_iters: usize,
    merge: MergeMode,
) -> Result<usize> {
    for &node in &scc.comps[comp] {
        node_best[node] = draw_value;
    }

    for iter in 1..=max_iters {
        let prev = node_best.to_vec();
        let mut changed = false;
        for &node in &scc.comps[comp] {
            let best = compute_node_best(graph, scc, comp, node, &prev, draw_value, merge);
            if best != node_best[node] {
                changed = true;
                node_best[node] = best;
            }
        }
        if !changed {
            return Ok(iter);
        }
    }

    bail!("SCC 値反復が --max-iters ({max_iters}) に到達しました");
}

fn compute_node_best(
    graph: &Graph,
    scc: &SccGraph,
    comp: usize,
    node: usize,
    best_values: &[i32],
    draw_value: i32,
    merge: MergeMode,
) -> i32 {
    graph.moves[node]
        .iter()
        .map(|mv| match mv.edge {
            Some(edge) if scc.nontrivial[comp] && scc.comp_of[edge.to] == comp => {
                merge.apply(mv.old, draw_value.max(-best_values[edge.to]))
            }
            Some(edge) => merge.apply(mv.old, -best_values[edge.to]),
            None => mv.old,
        })
        .max()
        .unwrap_or(draw_value)
}

fn build_scc_graph(adjacency: &[Vec<usize>]) -> SccGraph {
    let n = adjacency.len();
    let mut visited = vec![false; n];
    let mut order = Vec::with_capacity(n);
    for start in 0..n {
        if visited[start] {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        visited[start] = true;
        while let Some((node, next_idx)) = stack.pop() {
            if next_idx < adjacency[node].len() {
                stack.push((node, next_idx + 1));
                let child = adjacency[node][next_idx];
                if !visited[child] {
                    visited[child] = true;
                    stack.push((child, 0));
                }
            } else {
                order.push(node);
            }
        }
    }

    let mut reverse = vec![Vec::new(); n];
    for (node, children) in adjacency.iter().enumerate() {
        for &child in children {
            reverse[child].push(node);
        }
    }
    for children in &mut reverse {
        children.sort_unstable();
    }

    let mut comp_of = vec![usize::MAX; n];
    let mut comps = Vec::<Vec<usize>>::new();
    for &start in order.iter().rev() {
        if comp_of[start] != usize::MAX {
            continue;
        }
        let comp_idx = comps.len();
        let mut nodes = Vec::new();
        let mut stack = vec![start];
        comp_of[start] = comp_idx;
        while let Some(node) = stack.pop() {
            nodes.push(node);
            for &parent in &reverse[node] {
                if comp_of[parent] == usize::MAX {
                    comp_of[parent] = comp_idx;
                    stack.push(parent);
                }
            }
        }
        nodes.sort_unstable();
        comps.push(nodes);
    }

    let mut edge_sets = vec![BTreeSet::new(); comps.len()];
    let mut self_loop = vec![false; comps.len()];
    for (node, children) in adjacency.iter().enumerate() {
        let from = comp_of[node];
        for &child in children {
            let to = comp_of[child];
            if from == to {
                self_loop[from] = true;
            } else {
                edge_sets[from].insert(to);
            }
        }
    }
    let edges: Vec<Vec<usize>> =
        edge_sets.into_iter().map(|set| set.into_iter().collect()).collect();
    let nontrivial: Vec<bool> = comps
        .iter()
        .enumerate()
        .map(|(idx, nodes)| nodes.len() > 1 || self_loop[idx])
        .collect();
    let topo = topo_order(&edges);

    SccGraph {
        comp_of,
        comps,
        edges,
        topo,
        nontrivial,
    }
}

fn topo_order(edges: &[Vec<usize>]) -> Vec<usize> {
    let mut indegree = vec![0usize; edges.len()];
    for children in edges {
        for &child in children {
            indegree[child] += 1;
        }
    }
    let mut queue: VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter_map(|(idx, &deg)| (deg == 0).then_some(idx))
        .collect();
    let mut order = Vec::with_capacity(edges.len());
    while let Some(node) = queue.pop_front() {
        order.push(node);
        for &child in &edges[node] {
            indegree[child] -= 1;
            if indegree[child] == 0 {
                queue.push_back(child);
            }
        }
    }
    order
}

fn write_backprop_book(book: &BookDb, graph: &Graph, out: &Path) -> Result<()> {
    let file =
        File::create(out).with_context(|| format!("出力を作成できません: {}", out.display()))?;
    let mut writer = BufWriter::new(file);
    writer.write_all(BOOK_HEADER.as_bytes())?;
    writer.write_all(b"\n")?;
    let value_by_key: BTreeMap<&str, &Vec<MoveValue>> = graph
        .keys
        .iter()
        .zip(graph.moves.iter())
        .map(|(key, values)| (key.as_str(), values))
        .collect();
    for (key, entry) in &book.entries {
        writeln!(writer, "sfen {}", entry.sfen)?;
        let values = value_by_key
            .get(key.as_str())
            .ok_or_else(|| anyhow!("内部エラー: move values がありません: {key}"))?;
        let mut order: Vec<usize> = (0..entry.moves.len()).collect();
        order.sort_by(|&a, &b| {
            entry.moves[b]
                .count
                .cmp(&entry.moves[a].count)
                .then_with(|| move_sort_key(&entry.moves[a]).cmp(move_sort_key(&entry.moves[b])))
        });
        for idx in order {
            let book_move = &entry.moves[idx];
            writeln!(
                writer,
                "{} {} {} {} {}",
                book_move.move_usi.as_deref().unwrap_or("none"),
                book_move.ponder_usi.as_deref().unwrap_or("none"),
                values[idx].new,
                book_move.depth,
                book_move.count
            )?;
        }
    }
    writer.flush()?;
    Ok(())
}

fn move_sort_key(book_move: &BookMove) -> &str {
    book_move.move_usi.as_deref().unwrap_or("none")
}

fn write_report(
    book: &BookDb,
    graph: &Graph,
    stats: &PropagationStats,
    path: &Path,
    merge: MergeMode,
) -> Result<()> {
    let file = File::create(path)
        .with_context(|| format!("report を作成できません: {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let total_moves: usize = graph.moves.iter().map(Vec::len).sum();
    let total_edges: usize = graph.moves.iter().flatten().filter(|mv| mv.edge.is_some()).count();
    let direct_edges = total_edges - graph.flip_edges;

    writeln!(writer, "# book_backprop report")?;
    writeln!(writer)?;
    writeln!(writer, "## Summary")?;
    writeln!(writer)?;
    writeln!(writer, "- merge mode: {}", merge.as_str())?;
    writeln!(writer, "- nodes: {}", book.entries.len())?;
    writeln!(writer, "- moves: {total_moves}")?;
    writeln!(writer, "- updated moves: {}", stats.updated_moves)?;
    writeln!(writer, "- in-book edges: {total_edges}")?;
    writeln!(writer, "- direct edges: {direct_edges}")?;
    writeln!(writer, "- flip merged edges: {}", graph.flip_edges)?;
    writeln!(writer, "- illegal moves kept: {}", graph.illegal_moves)?;
    writeln!(writer)?;
    write_delta_report(&mut writer, &stats.abs_deltas)?;
    write_depth_report(&mut writer, &stats.depths)?;
    write_scc_report(&mut writer, stats)?;
    write_top_changes(&mut writer, &stats.changed_nodes)?;
    writer.flush()?;
    Ok(())
}

fn write_delta_report(writer: &mut dyn Write, deltas: &[i32]) -> Result<()> {
    let mut sorted = deltas.to_vec();
    sorted.sort_unstable();
    writeln!(writer, "## Value deltas")?;
    writeln!(writer)?;
    if sorted.is_empty() {
        writeln!(writer, "- changed moves: 0")?;
        writeln!(writer)?;
        return Ok(());
    }
    writeln!(writer, "- changed moves: {}", sorted.len())?;
    writeln!(writer, "- p50: {}", percentile(&sorted, 50))?;
    writeln!(writer, "- p90: {}", percentile(&sorted, 90))?;
    writeln!(writer, "- max: {}", sorted.last().copied().unwrap_or(0))?;
    writeln!(writer)?;
    writeln!(writer, "| abs delta bucket | moves |")?;
    writeln!(writer, "|---|---:|")?;
    for (label, count) in delta_buckets(&sorted) {
        writeln!(writer, "| {label} | {count} |")?;
    }
    writeln!(writer)?;
    Ok(())
}

fn percentile(sorted: &[i32], pct: usize) -> i32 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) * pct).div_ceil(100);
    sorted[idx]
}

fn delta_buckets(sorted: &[i32]) -> Vec<(&'static str, usize)> {
    let ranges = [
        ("1-49", 1, 49),
        ("50-99", 50, 99),
        ("100-199", 100, 199),
        ("200-499", 200, 499),
        ("500+", 500, i32::MAX),
    ];
    ranges
        .into_iter()
        .map(|(label, lo, hi)| {
            let count = sorted.iter().filter(|&&v| lo <= v && v <= hi).count();
            (label, count)
        })
        .collect()
}

fn write_depth_report(writer: &mut dyn Write, depths: &[usize]) -> Result<()> {
    let mut counts = BTreeMap::<usize, usize>::new();
    for &depth in depths {
        *counts.entry(depth).or_default() += 1;
    }
    writeln!(writer, "## Propagation depth")?;
    writeln!(writer)?;
    writeln!(writer, "| depth | nodes |")?;
    writeln!(writer, "|---:|---:|")?;
    for (depth, count) in counts {
        writeln!(writer, "| {depth} | {count} |")?;
    }
    writeln!(writer)?;
    Ok(())
}

fn write_scc_report(writer: &mut dyn Write, stats: &PropagationStats) -> Result<()> {
    writeln!(writer, "## SCC")?;
    writeln!(writer)?;
    writeln!(writer, "- nontrivial SCCs: {}", stats.nontrivial_sccs)?;
    writeln!(writer, "- max SCC size: {}", stats.max_scc_size)?;
    writeln!(writer, "- draw-valued internal moves: {}", stats.draw_moves)?;
    if !stats.scc_iters.is_empty() {
        let mut iters = stats.scc_iters.clone();
        iters.sort_unstable();
        writeln!(writer, "- value iteration max iters: {}", iters.last().copied().unwrap_or(0))?;
        writeln!(writer, "- value iteration p90 iters: {}", percentile_usize(&iters, 90))?;
    }
    writeln!(writer)?;
    Ok(())
}

fn percentile_usize(sorted: &[usize], pct: usize) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) * pct).div_ceil(100);
    sorted[idx]
}

fn write_top_changes(writer: &mut dyn Write, changes: &[NodeChange]) -> Result<()> {
    writeln!(writer, "## Top changed nodes")?;
    writeln!(writer)?;
    writeln!(writer, "| rank | old best | new best | sfen |")?;
    writeln!(writer, "|---:|---:|---:|---|")?;
    for (idx, change) in changes.iter().take(20).enumerate() {
        writeln!(
            writer,
            "| {} | {} | {} | `{}` |",
            idx + 1,
            change.old_best,
            change.new_best,
            change.sfen
        )?;
    }
    writeln!(writer)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    const KINGS: &str = "4k4/9/9/9/9/9/9/9/4K4 b - 1";

    type FixtureMove<'a> = (&'a str, i32, i32, u64);
    type FixtureEntry<'a> = (&'a str, &'a [FixtureMove<'a>]);

    #[test]
    fn dead_piece_edges_are_excluded_even_when_child_exists() {
        // 不正着手の結果に相当する子SFENも配置し、辺を除外しないと値が変わる条件にする。
        for (parent, usi, child) in [
            ("4k4/9/9/9/9/9/9/9/4K4 b P 1", "P*4a", "4kP3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/9/9/9/9/9/9/4K4 b L 1", "L*4a", "4kL3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/9/9/9/9/9/9/4K4 b N 1", "N*4a", "4kN3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/9/9/9/9/9/9/4K4 b N 1", "N*4b", "4k4/5N3/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/5P3/9/9/9/9/9/9/4K4 b - 1", "4b4a", "4kP3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/5L3/9/9/9/9/9/9/4K4 b - 1", "4b4a", "4kL3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/4N4/9/9/9/9/9/4K4 b - 1", "5c4a", "4kN3/9/9/9/9/9/9/9/4K4 w - 2"),
            ("4k4/9/9/4N4/9/9/9/9/4K4 b - 1", "5d4b", "4k4/5N3/9/9/9/9/9/9/4K4 w - 2"),
        ] {
            for flip in [false, true] {
                let (parent, usi, child) = if flip {
                    (
                        rshogi_book::flipped_key(parent).unwrap(),
                        rshogi_book::flip_usi_move(usi).unwrap(),
                        rshogi_book::flipped_key(child).unwrap(),
                    )
                } else {
                    (parent.into(), usi.into(), child.into())
                };
                let input = line_book(&[
                    (&parent, &[(&usi, 123, 9, 7)]),
                    (&child, &[("none", 500, 1, 1)]),
                ]);
                let dir = tempdir().unwrap();
                let input_path = write_input(dir.path(), "in.db", &input);
                let book = read_book_db(&input_path).unwrap();
                for merge in [MergeMode::Min, MergeMode::Replace] {
                    let mut graph = build_graph(&book).unwrap();
                    assert_eq!(graph.illegal_moves, 1, "{parent}: {usi}");
                    let parent_idx =
                        graph.keys.iter().position(|key| key == strip_ply(&parent)).unwrap();
                    assert!(graph.moves[parent_idx][0].edge.is_none());
                    assert!(graph.adjacency[parent_idx].is_empty());
                    propagate_values(&book, &mut graph, 0, 1000, merge).unwrap();
                    assert_eq!(graph.moves[parent_idx][0].new, 123);
                    let out = dir.path().join("out.db");
                    write_backprop_book(&book, &graph, &out).unwrap();
                    assert!(
                        std::fs::read_to_string(out)
                            .unwrap()
                            .contains(&format!("{usi} none 123 9 7\n"))
                    );
                }
            }
        }
    }

    #[test]
    fn legal_non_promotion_still_propagates() {
        let parent = "4k4/9/9/5P3/9/9/9/9/4K4 b - 1";
        let child = after(parent, &["4d4c"]);
        let input = line_book(&[
            (parent, &[("4d4c", 123, 9, 7)]),
            (&child, &[("none", 500, 1, 1)]),
        ]);
        assert!(backprop_text(&input).contains("4d4c none -500 9 7\n"));
    }
    #[test]
    fn output_is_deterministic_byte_for_byte() {
        let input = line_book(&[
            (START, &[("7g7f", 0, 1, 10), ("2g2f", 0, 1, 10)]),
            (&after(START, &["7g7f"]), &[("3c3d", 15, 1, 1)]),
        ]);
        let dir = tempdir().unwrap();
        let in_path = write_input(dir.path(), "in.db", &input);
        let out1 = dir.path().join("a.db");
        let out2 = dir.path().join("b.db");

        run_backprop(&in_path, &out1, 0, 1000, MergeMode::Min).unwrap();
        run_backprop(&in_path, &out2, 0, 1000, MergeMode::Min).unwrap();

        assert_eq!(std::fs::read(&out1).unwrap(), std::fs::read(&out2).unwrap());
    }

    #[test]
    fn straight_line_propagates_with_negamax_signs() {
        let after_76 = after(START, &["7g7f"]);
        let after_76_34 = after(START, &["7g7f", "3c3d"]);
        let input = line_book(&[
            (START, &[("7g7f", 0, 1, 1)]),
            (&after_76, &[("3c3d", 0, 1, 1)]),
            (&after_76_34, &[("2g2f", 80, 1, 1)]),
        ]);
        let output = backprop_text_with_merge(&input, MergeMode::Replace);

        assert!(output.contains("sfen "));
        assert!(output.contains("7g7f none 80 1 1\n"));
        assert!(output.contains("3c3d none -80 1 1\n"));
        assert!(output.contains("2g2f none 80 1 1\n"));
    }

    #[test]
    fn transposed_child_updates_both_parents() {
        let p1 = after(START, &["7g7f", "3c3d"]);
        let p2 = after(START, &["2g2f", "3c3d"]);
        let child = after(START, &["7g7f", "3c3d", "2g2f"]);
        let input = line_book(&[
            (&p1, &[("2g2f", 0, 1, 4)]),
            (&p2, &[("7g7f", 0, 1, 4)]),
            (&child, &[("8c8d", 42, 1, 1)]),
        ]);
        let output = backprop_text(&input);

        assert!(output.contains("2g2f none -42 1 4\n"));
        assert!(output.contains("7g7f none -42 1 4\n"));
    }

    #[test]
    fn flipped_child_key_is_used_when_direct_key_misses() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let flipped = rshogi_book::flipped_key(&child).unwrap();
        let input = line_book(&[
            (START, &[("7g7f", 0, 1, 1)]),
            (&flipped, &[("7g7f", 33, 1, 1)]),
        ]);
        let output = backprop_text(&input);

        assert!(output.contains("sfen "));
        assert!(output.contains("7g7f none -33 1 1\n"));
    }

    #[test]
    fn min_merge_keeps_existing_value_when_propagation_would_raise_it() {
        let after_76 = after(START, &["7g7f"]);
        let input = line_book(&[
            (START, &[("7g7f", -81, 1, 1)]),
            (&after_76, &[("3c3d", 53, 1, 1)]),
        ]);
        let output = backprop_text(&input);

        assert!(output.contains("7g7f none -81 1 1\n"));
    }

    #[test]
    fn min_merge_lowers_existing_value_when_propagation_is_lower() {
        let after_76 = after(START, &["7g7f"]);
        let input = line_book(&[
            (START, &[("7g7f", 10, 1, 1)]),
            (&after_76, &[("3c3d", 50, 1, 1)]),
        ]);
        let output = backprop_text(&input);

        assert!(output.contains("7g7f none -50 1 1\n"));
    }

    #[test]
    fn cycle_uses_exit_above_draw_and_draw_when_exit_is_below_draw() {
        let above = cycle_book(20);
        let above_out = backprop_text(&above);
        assert!(above_out.contains("5a4a none 20 1 2\n"));
        assert!(above_out.contains("5a5b none 0 1 3\n"));

        let below = cycle_book(-20);
        let below_out = backprop_text(&below);
        assert!(below_out.contains("5a4a none -20 1 2\n"));
        assert!(below_out.contains("5a5b none 0 1 3\n"));
    }

    #[test]
    fn leaf_value_is_preserved() {
        let input = line_book(&[(START, &[("7g7f", 123, 9, 1)])]);
        let output = backprop_text(&input);

        assert!(output.contains("7g7f none 123 9 1\n"));
    }

    #[test]
    fn output_roundtrips_through_book_reader() {
        let input = line_book(&[
            (START, &[("7g7f", 0, 1, 1)]),
            (&after(START, &["7g7f"]), &[("3c3d", 15, 1, 1)]),
        ]);
        let dir = tempdir().unwrap();
        let in_path = write_input(dir.path(), "in.db", &input);
        let out_path = dir.path().join("out.db");

        run_backprop(&in_path, &out_path, 0, 1000, MergeMode::Min).unwrap();
        let book = rshogi_book::Book::from_path(&out_path, true).unwrap();

        assert_eq!(book.len(), 2);
    }

    fn run_backprop(
        input: &Path,
        output: &Path,
        draw_value: i32,
        max_iters: usize,
        merge: MergeMode,
    ) -> Result<()> {
        let book = read_book_db(input)?;
        let mut graph = build_graph(&book)?;
        propagate_values(&book, &mut graph, draw_value, max_iters, merge)?;
        write_backprop_book(&book, &graph, output)
    }

    fn backprop_text(input: &str) -> String {
        backprop_text_with_merge(input, MergeMode::Min)
    }

    fn backprop_text_with_merge(input: &str, merge: MergeMode) -> String {
        let dir = tempdir().unwrap();
        let in_path = write_input(dir.path(), "in.db", input);
        let out_path = dir.path().join("out.db");
        run_backprop(&in_path, &out_path, 0, 1000, merge).unwrap();
        std::fs::read_to_string(out_path).unwrap()
    }

    fn write_input(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    fn line_book(entries: &[FixtureEntry<'_>]) -> String {
        let mut out = String::from(BOOK_HEADER);
        out.push('\n');
        for (sfen, moves) in entries {
            out.push_str("sfen ");
            out.push_str(sfen);
            out.push('\n');
            for (move_usi, value, depth, count) in *moves {
                out.push_str(&format!("{move_usi} none {value} {depth} {count}\n"));
            }
        }
        out
    }

    fn after(start: &str, moves: &[&str]) -> String {
        let mut sfen = start.to_string();
        for move_usi in moves {
            sfen = child_position_after_move(&sfen, move_usi).unwrap().to_sfen();
        }
        sfen
    }

    fn cycle_book(exit_value: i32) -> String {
        let a = KINGS.to_string();
        let b = after(&a, &["5i5h"]);
        let c = after(&a, &["5i5h", "5a5b"]);
        let d = after(&a, &["5i5h", "5a5b", "5h5i"]);
        line_book(&[
            (&a, &[("5i5h", 0, 1, 3)]),
            (&b, &[("5a5b", 0, 1, 3), ("5a4a", exit_value, 1, 2)]),
            (&c, &[("5h5i", 0, 1, 3)]),
            (&d, &[("5b5a", 0, 1, 3)]),
        ])
    }
}
