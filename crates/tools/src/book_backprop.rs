//! YANEURAOU-DB2016 テキスト `.db` の評価値を negamax 逆伝播するライブラリ。
//!
//! `book_backprop` bin と `book_mine run` の双方から呼ぶ。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use rshogi_core::position::Position;
use rshogi_core::types::Move;

/// YANEURAOU-DB2016 テキスト定跡のヘッダ行。
pub const BOOK_HEADER: &str = "#YANEURAOU-DB2016 1.00";

/// 伝播値と既存ラベル値の合成方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MergeMode {
    /// 純 negamax。伝播値で置き換える。
    Replace,
    /// 既存ラベル値と伝播値の小さい方を採る。
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

/// ply 抜き key で集約した定跡。
#[derive(Debug, Clone)]
pub struct BookDb {
    pub entries: BTreeMap<String, PositionEntry>,
}

/// 1 局面分の定跡エントリ。`sfen` は同一 key 中の最小 ply 行。
#[derive(Debug, Clone)]
pub struct PositionEntry {
    pub sfen: String,
    pub moves: Vec<BookMove>,
}

/// 定跡の 1 候補手。
#[derive(Debug, Clone)]
pub struct BookMove {
    pub move_usi: Option<String>,
    pub ponder_usi: Option<String>,
    pub value: i32,
    pub depth: i32,
    pub count: u64,
}

/// book 内子局面への辺。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edge {
    pub to: usize,
    pub via_flip: bool,
}

/// 候補手ごとの伝播前後の値。
#[derive(Debug, Clone)]
pub struct MoveValue {
    pub old: i32,
    pub new: i32,
    pub edge: Option<Edge>,
    /// 合法な指し手を持つ行か。非合法手と `none` 行は `false`。
    pub usable: bool,
}

/// 逆伝播の追加オプション。既定値は `book_backprop` の従来挙動。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackpropOptions {
    /// 非合法手と `none` 行を局面の best (伝播値と best の変化集計) から除く。
    /// 行自体は値を変えずに書き出す。
    pub skip_unusable_moves: bool,
    /// 候補手が 1 件以上あり、全行の depth が 0 の子局面への辺を除く。
    /// value は判定に使わず、候補手の無い局面は除外しない。親の手は元の値を保つ。
    pub skip_unsearched_children: bool,
}

/// 局面の best 計算に共通の設定。
#[derive(Debug, Clone, Copy)]
struct BestParams {
    draw_value: i32,
    merge: MergeMode,
    skip_unusable_moves: bool,
}

impl BestParams {
    fn counts(self, mv: &MoveValue) -> bool {
        !self.skip_unusable_moves || mv.usable
    }
}

/// 局面を頂点、book 内子局面への候補手を辺とするグラフ。
#[derive(Debug, Clone)]
pub struct Graph {
    pub keys: Vec<String>,
    pub moves: Vec<Vec<MoveValue>>,
    pub adjacency: Vec<Vec<usize>>,
    pub flip_edges: usize,
    pub illegal_moves: usize,
}

#[derive(Debug, Clone)]
struct SccGraph {
    comp_of: Vec<usize>,
    comps: Vec<Vec<usize>>,
    edges: Vec<Vec<usize>>,
    topo: Vec<usize>,
    nontrivial: Vec<bool>,
}

/// 逆伝播の集計。
#[derive(Debug, Default, Clone)]
pub struct PropagationStats {
    pub updated_moves: usize,
    pub abs_deltas: Vec<i32>,
    pub nontrivial_sccs: usize,
    pub max_scc_size: usize,
    pub draw_moves: usize,
    pub scc_iters: Vec<usize>,
    pub depths: Vec<usize>,
    pub changed_nodes: Vec<NodeChange>,
}

/// best 値が変化した局面。
#[derive(Debug, Clone)]
pub struct NodeChange {
    pub sfen: String,
    pub old_best: i32,
    pub new_best: i32,
}

/// 定跡を読み、逆伝播した `.db` と任意の report を書き出す (`book_backprop` の処理本体)。
pub fn backprop_file(
    book: &Path,
    out: &Path,
    report: Option<&Path>,
    draw_value: i32,
    max_iters: usize,
    merge: MergeMode,
) -> Result<PropagationStats> {
    backprop_file_with(book, out, report, draw_value, max_iters, merge, BackpropOptions::default())
}

/// [`backprop_file`] に追加オプションを指定する版。
pub fn backprop_file_with(
    book: &Path,
    out: &Path,
    report: Option<&Path>,
    draw_value: i32,
    max_iters: usize,
    merge: MergeMode,
    options: BackpropOptions,
) -> Result<PropagationStats> {
    if max_iters == 0 {
        bail!("--max-iters は 1 以上を指定してください");
    }

    rshogi_book::Book::from_path(book, true)
        .with_context(|| format!("定跡を rshogi-book で読めません: {}", book.display()))?;
    let db = read_book_db(book)?;
    let mut graph = build_graph_with(&db, options)?;
    let stats = propagate_values_with(&db, &mut graph, draw_value, max_iters, merge, options)?;
    write_backprop_book(&db, &graph, out)?;
    if let Some(path) = report {
        write_report(&db, &graph, &stats, path, merge)?;
    }
    Ok(stats)
}

/// `.db` を ply 抜き key で読み込む。同一 key の行は最小 ply の sfen を代表にして手を連結する。
pub fn read_book_db(path: &Path) -> Result<BookDb> {
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

/// SFEN 末尾の ply フィールドを除く。
pub fn strip_ply(sfen: &str) -> &str {
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

/// book 内子局面への辺 (反転 key 合流を含む) を張ったグラフを作る。
pub fn build_graph(book: &BookDb) -> Result<Graph> {
    build_graph_with(book, BackpropOptions::default())
}

/// [`build_graph`] に追加オプションを指定する版。未探索の判定は入力時の depth を使う。
pub fn build_graph_with(book: &BookDb, options: BackpropOptions) -> Result<Graph> {
    let keys: Vec<String> = book.entries.keys().cloned().collect();
    let skipped_children: Vec<bool> = book
        .entries
        .values()
        .map(|entry| {
            options.skip_unsearched_children
                && !entry.moves.is_empty()
                && entry.moves.iter().all(|mv| mv.depth == 0)
        })
        .collect();
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
            let mut legal = false;
            let edge = if let Some(move_usi) = &book_move.move_usi {
                match child_position_after_move(&entry.sfen, move_usi) {
                    Ok(child) => {
                        legal = true;
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
                            flipped_to.map(|to| Edge { to, via_flip: true })
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
            let edge = edge.filter(|edge| !skipped_children[edge.to]);
            if let Some(edge) = edge {
                if edge.via_flip {
                    flip_edges += 1;
                }
                adjacency_sets[node_idx].insert(edge.to);
            }
            move_values.push(MoveValue {
                old: book_move.value,
                new: book_move.value,
                edge,
                usable: legal,
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

/// 親局面に USI 指し手を合法性検査付きで適用した子局面を返す。
pub fn child_position_after_move(parent_sfen: &str, move_usi: &str) -> Result<Position> {
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

/// SCC 縮約 DAG を葉側から処理して候補手の値を逆伝播する。
pub fn propagate_values(
    book: &BookDb,
    graph: &mut Graph,
    draw_value: i32,
    max_iters: usize,
    merge: MergeMode,
) -> Result<PropagationStats> {
    propagate_values_with(book, graph, draw_value, max_iters, merge, BackpropOptions::default())
}

/// [`propagate_values`] に追加オプションを指定する版。
/// `skip_unsearched_children` はグラフ構築時に [`build_graph_with`] へ渡すこと。
pub fn propagate_values_with(
    book: &BookDb,
    graph: &mut Graph,
    draw_value: i32,
    max_iters: usize,
    merge: MergeMode,
    options: BackpropOptions,
) -> Result<PropagationStats> {
    let params = BestParams {
        draw_value,
        merge,
        skip_unusable_moves: options.skip_unusable_moves,
    };
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
            let iters =
                iterate_nontrivial_scc(graph, &scc, comp, &mut node_best, max_iters, params)?;
            stats.scc_iters.push(iters);
        } else {
            let node = scc.comps[comp][0];
            node_best[node] = compute_node_best(graph, &scc, comp, node, &node_best, params);
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
        let counted = || graph.moves[node_idx].iter().filter(|mv| params.counts(mv));
        let old_best = counted().map(|mv| mv.old).max().unwrap_or(draw_value);
        let new_best = counted().map(|mv| mv.new).max().unwrap_or(draw_value);
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
    max_iters: usize,
    params: BestParams,
) -> Result<usize> {
    for &node in &scc.comps[comp] {
        node_best[node] = params.draw_value;
    }

    for iter in 1..=max_iters {
        let prev = node_best.to_vec();
        let mut changed = false;
        for &node in &scc.comps[comp] {
            let best = compute_node_best(graph, scc, comp, node, &prev, params);
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
    params: BestParams,
) -> i32 {
    let BestParams {
        draw_value, merge, ..
    } = params;
    graph.moves[node]
        .iter()
        .filter(|mv| params.counts(mv))
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

/// 逆伝播後の値で `.db` を書き出す。局面は key 昇順、手は count 降順 → USI 昇順。
pub fn write_backprop_book(book: &BookDb, graph: &Graph, out: &Path) -> Result<()> {
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

/// Markdown レポートを書き出す。
pub fn write_report(
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

    const START: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";

    fn check_child_propagation(child_rows: &str, flip: bool, unsearched: bool, best: i32) {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let child = if flip {
            rshogi_book::flipped_key(&child).unwrap()
        } else {
            child
        };
        let input =
            format!("{BOOK_HEADER}\nsfen {START}\n7g7f none 123 9 7\nsfen {child}\n{child_rows}");
        let dir = tempfile::tempdir().unwrap();
        let in_path = dir.path().join("in.db");
        let out_path = dir.path().join("out.db");
        std::fs::write(&in_path, &input).unwrap();
        let book = read_book_db(&in_path).unwrap();
        for skip in [false, true] {
            let options = BackpropOptions {
                skip_unsearched_children: skip,
                ..BackpropOptions::default()
            };
            let graph = build_graph_with(&book, options).unwrap();
            let parent = graph.keys.iter().position(|key| key == strip_ply(START)).unwrap();
            let excluded = skip && unsearched;
            assert_eq!(graph.moves[parent][0].edge.is_none(), excluded);
            assert_eq!(graph.adjacency[parent].is_empty(), excluded);
            assert_eq!(graph.flip_edges, usize::from(flip && !excluded));
            if let Some(edge) = graph.moves[parent][0].edge {
                assert_eq!(edge.via_flip, flip);
            }
            for merge in [MergeMode::Replace, MergeMode::Min] {
                backprop_file_with(&in_path, &out_path, None, 0, 1000, merge, options).unwrap();
                let output = read_book_db(&out_path).unwrap();
                let value = output.entries[strip_ply(START)].moves[0].value;
                let expected = if excluded {
                    123
                } else {
                    merge.apply(123, -best)
                };
                assert_eq!(value, expected, "skip={skip}, flip={flip}, merge={merge:?}");
            }
        }
    }

    #[test]
    fn skip_unsearched_children_preserves_parent_value() {
        check_child_propagation("3c3d none 0 0 3\n8c8d none 0 0 2\n", false, true, 0);
    }

    #[test]
    fn skip_unsearched_children_keeps_partly_searched_child() {
        check_child_propagation("3c3d none 50 10 3\n8c8d none 0 0 2\n", false, false, 50);
    }

    #[test]
    fn skip_unsearched_children_skips_flipped_child() {
        check_child_propagation("7g7f none 0 0 3\n2g2f none 0 0 2\n", true, true, 0);
    }

    #[test]
    fn skip_unsearched_children_keeps_empty_child() {
        check_child_propagation("", false, false, 0);
    }

    #[test]
    fn skip_unsearched_children_uses_depth_regardless_of_value() {
        check_child_propagation("3c3d none 50 0 3\n", false, true, 50);
    }

    fn backprop_text(input: &str, options: BackpropOptions) -> String {
        let dir = tempfile::tempdir().unwrap();
        let in_path = dir.path().join("in.db");
        let out_path = dir.path().join("out.db");
        std::fs::write(&in_path, input).unwrap();
        backprop_file_with(&in_path, &out_path, None, 0, 1000, MergeMode::Replace, options)
            .unwrap();
        std::fs::read_to_string(out_path).unwrap()
    }

    #[test]
    fn skip_unusable_moves_excludes_illegal_and_none_rows_from_best() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        // 子局面の非合法手 5e5d と none 行は value 0。best に入ると 7g7f が 0 になる。
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f none 0 1 1\nsfen {child}\n3c3d none -50 10 3\n5e5d none 0 0 1\nnone none 0 0 1\n"
        );

        let default = backprop_text(&input, BackpropOptions::default());
        assert!(default.contains("7g7f none 0 1 1\n"));

        let skipped = backprop_text(
            &input,
            BackpropOptions {
                skip_unusable_moves: true,
                ..BackpropOptions::default()
            },
        );
        assert!(skipped.contains("7g7f none 50 1 1\n"));
        // 除外した行も値を変えずに書き出す。
        assert!(skipped.contains("5e5d none 0 0 1\n"));
        assert!(skipped.contains("none none 0 0 1\n"));
    }
}
