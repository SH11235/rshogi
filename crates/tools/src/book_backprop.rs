//! YANEURAOU-DB2016 テキスト `.db` の評価値を negamax 逆伝播するライブラリ。
//!
//! `book_backprop` bin と `book_mine run` の双方から呼ぶ。

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use rshogi_core::position::Position;
use rshogi_core::types::{Move, Value};

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

impl PositionEntry {
    /// book_mine と同じく、全候補手が value=0 / depth=0 なら未探索。
    pub fn is_unexplored(&self) -> bool {
        self.moves.iter().all(|mv| mv.value == 0 && mv.depth == 0)
    }
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
    preserve: bool,
}

/// 逆伝播の追加オプション。既定値は `book_backprop` の従来挙動。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BackpropOptions {
    /// 非合法手と `none` 行を局面の best (伝播値と best の変化集計) から除く。
    /// 行自体は値を変えずに書き出す。
    pub skip_unusable_moves: bool,
    /// 未探索の子を評価値 0 と扱って親の探索値を上書きしないよう、全行 depth=0 の子への辺を除く。
    /// value は判定に使わず、候補手の無い局面は除外しない。
    /// 入力時に全行 value=0 / depth=0 の局面は、未探索マーカーを保つため出力も保持する。
    pub skip_unsearched_children: bool,
    /// 異なる手順で同一局面へ合流する合法手を補完し、手順間で候補手を共有する。
    pub add_transposition_moves: bool,
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
    checked: Vec<bool>,
    preserved: Vec<bool>,
    added_moves: Vec<Vec<BookMove>>,
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

fn add_transposition_moves(book: &BookDb, graph: &mut Graph, skipped: &[bool]) -> Result<()> {
    let index: BTreeMap<_, _> =
        graph.keys.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    for (node, key) in graph.keys.iter().enumerate() {
        if graph.preserved[node] {
            continue;
        }
        let entry = &book.entries[key];
        let mut pos = Position::new();
        pos.set_sfen(&entry.sfen)
            .map_err(|e| anyhow!("不正な SFEN: {}: {e}", entry.sfen))?;
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        let listed: BTreeSet<_> =
            entry.moves.iter().filter_map(|m| m.move_usi.as_deref()).collect();
        for &mv in legal.iter() {
            let usi = mv.to_usi();
            if listed.contains(usi.as_str()) {
                continue;
            }
            let gives_check = pos.gives_check(mv);
            pos.do_move(mv, gives_check);
            let child = pos.to_sfen();
            pos.undo_move(mv);
            let edge = index
                .get(strip_ply(&child))
                .map(|&to| Edge {
                    to,
                    via_flip: false,
                })
                .or_else(|| {
                    rshogi_book::flipped_key(&child).and_then(|flipped| {
                        index.get(strip_ply(&flipped)).map(|&to| Edge { to, via_flip: true })
                    })
                });
            let Some(edge) = edge.filter(|e| !skipped[e.to]) else {
                continue;
            };
            graph.flip_edges += usize::from(edge.via_flip);
            graph.adjacency[node].push(edge.to);
            graph.moves[node].push(MoveValue {
                old: 0,
                new: 0,
                edge: Some(edge),
                usable: true,
                preserve: false,
            });
            // 補完手には探索ラベルがない。depth=1 を出自の固定マーカーとして用い、
            // 未探索の 0/0 と区別する（伝播距離や探索した深さを表すものではない）。
            graph.added_moves[node].push(BookMove {
                move_usi: Some(usi),
                ponder_usi: None,
                value: 0,
                depth: 1,
                count: 0,
            });
        }
        graph.adjacency[node].sort_unstable();
        graph.adjacency[node].dedup();
    }
    Ok(())
}

const DISTANCE_MAX: u16 = 9999;
const PERPETUAL_CHECKED: u16 = 9998;
const PERPETUAL_CHECK: u16 = 9997;

/// 探索 depth とは独立した、手番側の評価値と葉までの距離。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValueDistance {
    value: i32,
    distance: u16,
}

impl ValueDistance {
    fn better(self, other: Self) -> bool {
        if self.value != other.value {
            return self.value > other.value;
        }
        if self.distance == PERPETUAL_CHECK {
            return false;
        }
        if other.distance == PERPETUAL_CHECK {
            return true;
        }
        if self.value >= 0 {
            self.distance < other.distance
        } else {
            self.distance > other.distance
        }
    }

    fn for_parent(self) -> Self {
        Self {
            value: -self.value,
            distance: (self.distance + 1).min(DISTANCE_MAX),
        }
    }
}

/// 王手されている候補から、2 手先に候補がないものを不動点まで除く。
/// 残った候補同士の間の局面も含め、DFS する部分グラフを得る。
fn extract_check_loop(graph: &Graph) -> Vec<bool> {
    let mut candidates = graph.checked.clone();
    loop {
        let mut changed = false;
        for node in 0..candidates.len() {
            if candidates[node]
                && !graph.adjacency[node]
                    .iter()
                    .any(|&child| graph.adjacency[child].iter().any(|&next| candidates[next]))
            {
                candidates[node] = false;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut check_loop = candidates.clone();
    for (node, &candidate) in candidates.iter().enumerate() {
        if candidate {
            for &child in &graph.adjacency[node] {
                if graph.adjacency[child].iter().any(|&next| candidates[next]) {
                    check_loop[child] = true;
                }
            }
        }
    }
    check_loop
}

struct SccEvaluator<'a> {
    graph: &'a Graph,
    params: BestParams,
    check_loop: Vec<bool>,
    input_lengths: Vec<usize>,
}

impl SccEvaluator<'_> {
    fn merged(&self, node: usize, idx: usize, mut value: ValueDistance) -> ValueDistance {
        // 補完手には min の上界となる探索ラベルがない。
        if idx < self.input_lengths[node] {
            value.value = self.params.merge.apply(self.graph.moves[node][idx].old, value.value);
        }
        value
    }

    fn move_value(&self, node: usize, idx: usize, best: &[ValueDistance]) -> ValueDistance {
        let mv = &self.graph.moves[node][idx];
        match mv.edge {
            Some(edge) => self.merged(node, idx, best[edge.to].for_parent()),
            None => ValueDistance {
                value: mv.old,
                distance: 0,
            },
        }
    }

    fn best(&self, node: usize, values: &[ValueDistance]) -> ValueDistance {
        self.graph.moves[node]
            .iter()
            .enumerate()
            .filter(|(_, mv)| self.params.counts(mv))
            .map(|(idx, _)| self.move_value(node, idx, values))
            .reduce(|a, b| if b.better(a) { b } else { a })
            .unwrap_or(ValueDistance {
                value: self.params.draw_value,
                distance: 0,
            })
    }

    // 深い定跡でも Windows のスレッドスタックを消費しない明示 DFS。
    // ループ以外への離脱は、SCC 反復の前回値と通常の negamax / merge で比較する。
    // ループ内で min(入力ラベル, 勝ち値) を取ると反則負けまで 0 に消えるため、
    // 継続辺は純 negamax で規則評価を伝え、既存ラベルとの merge は出力時に行う。
    fn check_best(&self, root: usize, values: &[ValueDistance]) -> ValueDistance {
        struct Frame {
            node: usize,
            next: usize,
            best: Option<ValueDistance>,
        }
        let mut trajectory = vec![false; values.len()];
        let mut stack = vec![Frame {
            node: root,
            next: 0,
            best: None,
        }];
        trajectory[root] = true;
        while let Some(frame) = stack.last_mut() {
            let node = frame.node;
            if frame.next == self.graph.moves[node].len() {
                let result = frame.best.unwrap_or(ValueDistance {
                    value: self.params.draw_value,
                    distance: 0,
                });
                trajectory[node] = false;
                stack.pop();
                let Some(parent) = stack.last_mut() else {
                    return result;
                };
                let candidate = result.for_parent();
                if parent.best.is_none_or(|best| candidate.better(best)) {
                    parent.best = Some(candidate);
                }
                continue;
            }
            let idx = frame.next;
            frame.next += 1;
            let mv = &self.graph.moves[node][idx];
            if !self.params.counts(mv) {
                continue;
            }
            let candidate = if let Some(edge) = mv.edge.filter(|e| self.check_loop[e.to]) {
                if trajectory[edge.to] {
                    if self.graph.checked[edge.to] {
                        ValueDistance {
                            value: -Value::MATE.raw(),
                            distance: PERPETUAL_CHECK,
                        }
                    } else {
                        ValueDistance {
                            value: Value::MATE.raw(),
                            distance: PERPETUAL_CHECKED,
                        }
                    }
                } else {
                    trajectory[edge.to] = true;
                    stack.push(Frame {
                        node: edge.to,
                        next: 0,
                        best: None,
                    });
                    continue;
                }
            } else {
                self.move_value(node, idx, values)
            };
            if frame.best.is_none_or(|best| candidate.better(best)) {
                frame.best = Some(candidate);
            }
        }
        unreachable!("DFS は root の結果を返す")
    }
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
    let preserved: Vec<_> = book
        .entries
        .values()
        .map(|entry| options.skip_unsearched_children && entry.is_unexplored())
        .collect();
    let mut checked = Vec::with_capacity(keys.len());
    let mut moves = Vec::with_capacity(keys.len());
    let mut adjacency_sets = vec![BTreeSet::new(); keys.len()];
    let mut flip_edges = 0;
    let mut illegal_moves = 0;

    for (node_idx, key) in keys.iter().enumerate() {
        let entry = book
            .entries
            .get(key)
            .ok_or_else(|| anyhow!("内部エラー: entry がありません: {key}"))?;
        let mut position = Position::new();
        position
            .set_sfen(&entry.sfen)
            .map_err(|e| anyhow!("不正な SFEN: {}: {e}", entry.sfen))?;
        checked.push(!position.checkers().is_empty());
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
            let preserve = edge.is_some_and(|edge| skipped_children[edge.to]);
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
                preserve,
            });
        }
        moves.push(move_values);
    }

    let adjacency = adjacency_sets.into_iter().map(|set| set.into_iter().collect()).collect();
    let mut graph = Graph {
        added_moves: vec![Vec::new(); keys.len()],
        checked,
        preserved,
        keys,
        moves,
        adjacency,
        flip_edges,
        illegal_moves,
    };
    if options.add_transposition_moves {
        add_transposition_moves(book, &mut graph, &skipped_children)?;
    }
    Ok(graph)
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
    let evaluator = SccEvaluator {
        graph,
        params,
        check_loop: extract_check_loop(graph),
        input_lengths: graph
            .keys
            .iter()
            .map(|key| {
                book.entries
                    .get(key)
                    .map(|entry| entry.moves.len())
                    .ok_or_else(|| anyhow!("局面がありません: {key}"))
            })
            .collect::<Result<_>>()?,
    };
    let mut node_best = vec![
        ValueDistance {
            value: draw_value,
            distance: DISTANCE_MAX
        };
        graph.keys.len()
    ];
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
                iterate_nontrivial_scc(&evaluator, &scc.comps[comp], &mut node_best, max_iters)?;
            stats.scc_iters.push(iters);
        } else {
            let node = scc.comps[comp][0];
            node_best[node] = if evaluator.check_loop[node] {
                evaluator.check_best(node, &node_best)
            } else {
                evaluator.best(node, &node_best)
            };
        }
    }

    // 補正は最終出力だけ。補正後の -1 をさらに親へ伝播させない。
    let outputs: Vec<Vec<_>> = graph
        .moves
        .iter()
        .enumerate()
        .map(|(node, moves)| {
            let mut output: Vec<_> = moves
                .iter()
                .enumerate()
                .map(|(idx, mv)| {
                    let mut vd = evaluator.move_value(node, idx, &node_best);
                    if mv.edge.is_some_and(|e| evaluator.check_loop[e.to] && graph.checked[e.to]) {
                        vd.distance = PERPETUAL_CHECK;
                    }
                    vd
                })
                .collect();
            let best = output
                .iter()
                .zip(moves)
                .filter(|(_, mv)| params.counts(mv))
                .map(|(&vd, _)| vd)
                .reduce(|a, b| if b.better(a) { b } else { a });
            for (vd, mv) in output.iter_mut().zip(moves) {
                if graph.preserved[node] || mv.preserve || !mv.usable {
                    vd.value = mv.old;
                } else if best
                    .is_some_and(|best| best.value == vd.value && best.distance != vd.distance)
                {
                    vd.value = vd.value.saturating_sub(1);
                }
            }
            output
        })
        .collect();
    for (node, moves) in graph.moves.iter_mut().enumerate() {
        let comp = scc.comp_of[node];
        for (mv, vd) in moves.iter_mut().zip(&outputs[node]) {
            mv.new = vd.value;
            if mv.new == draw_value
                && mv.edge.is_some_and(|e| scc.nontrivial[comp] && scc.comp_of[e.to] == comp)
            {
                stats.draw_moves += 1;
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
    evaluator: &SccEvaluator<'_>,
    nodes: &[usize],
    node_best: &mut [ValueDistance],
    max_iters: usize,
) -> Result<usize> {
    // 通常の循環は draw から始めるが、各辺の値を draw で clamp しない。
    for &node in nodes {
        node_best[node] = ValueDistance {
            value: evaluator.params.draw_value,
            distance: DISTANCE_MAX,
        };
    }
    for iter in 1..=max_iters {
        let prev = node_best.to_vec();
        let mut changed = false;
        for &node in nodes {
            let best = if evaluator.check_loop[node] {
                evaluator.check_best(node, &prev)
            } else {
                evaluator.best(node, &prev)
            };
            changed |= best != node_best[node];
            node_best[node] = best;
        }
        if !changed {
            return Ok(iter);
        }
    }
    bail!("SCC 値・距離反復が --max-iters ({max_iters}) に到達しました（未収束）");
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
        let node = graph.keys.binary_search(key).map_err(|_| anyhow!("局面がありません: {key}"))?;
        let rows: Vec<_> = entry.moves.iter().chain(&graph.added_moves[node]).collect();
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by(|&a, &b| {
            rows[b]
                .count
                .cmp(&rows[a].count)
                .then_with(|| move_sort_key(rows[a]).cmp(move_sort_key(rows[b])))
        });
        for idx in order {
            let book_move = rows[idx];
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

    fn solve_rows(rows: &[&[(i32, Option<usize>)]], max_iters: usize) -> Result<Graph> {
        let keys: Vec<_> = (0..rows.len()).map(|i| i.to_string()).collect();
        let book = BookDb {
            entries: keys
                .iter()
                .zip(rows)
                .map(|(key, rows)| {
                    (
                        key.clone(),
                        PositionEntry {
                            sfen: key.clone(),
                            moves: rows
                                .iter()
                                .map(|&(value, _)| BookMove {
                                    move_usi: None,
                                    ponder_usi: None,
                                    value,
                                    depth: 10,
                                    count: 1,
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
        };
        let mut graph = Graph {
            keys,
            moves: rows
                .iter()
                .map(|rows| {
                    rows.iter()
                        .map(|&(value, child)| MoveValue {
                            old: value,
                            new: value,
                            usable: true,
                            preserve: false,
                            edge: child.map(|to| Edge {
                                to,
                                via_flip: false,
                            }),
                        })
                        .collect()
                })
                .collect(),
            adjacency: rows
                .iter()
                .map(|rows| rows.iter().filter_map(|&(_, child)| child).collect())
                .collect(),
            flip_edges: 0,
            illegal_moves: 0,
            checked: vec![false; rows.len()],
            preserved: vec![false; rows.len()],
            added_moves: vec![vec![]; rows.len()],
        };
        propagate_values(&book, &mut graph, 0, max_iters, MergeMode::Replace)?;
        Ok(graph)
    }

    #[test]
    fn losing_side_cannot_claim_draw_when_winner_can_exit() {
        let graph = solve_rows(&[&[(0, Some(1))], &[(0, Some(0)), (100, None)]], 1000).unwrap();
        assert_eq!(graph.moves[0][0].new, -100);
        assert_eq!(graph.moves[1][0].new, 99); // 同値の遠回りを避ける。
        assert_eq!(graph.moves[1][1].new, 100);
    }

    #[test]
    fn distances_prefer_short_wins_and_long_losses_only_at_output() {
        for value in [-50, 0, 50] {
            let graph =
                solve_rows(&[&[(value, None), (0, Some(1))], &[(-value, None)]], 1000).unwrap();
            let expected = if value >= 0 {
                [value, value - 1]
            } else {
                [value - 1, value]
            };
            assert_eq!([graph.moves[0][0].new, graph.moves[0][1].new], expected);
        }
        let marker = ValueDistance {
            value: -50,
            distance: PERPETUAL_CHECK,
        };
        assert!(!marker.better(ValueDistance {
            value: -50,
            distance: 0
        }));
        assert!(
            ValueDistance {
                value: -50,
                distance: 0
            }
            .better(marker)
        );
        assert_eq!(
            ValueDistance {
                value: 1,
                distance: DISTANCE_MAX
            }
            .for_parent()
            .distance,
            DISTANCE_MAX
        );
        let graph = solve_rows(&[&[(0, Some(1))], &[(0, Some(0))]], 1).unwrap();
        assert_eq!(graph.moves[0][0].new, 0);
    }

    #[test]
    fn nonconvergence_is_an_error() {
        let error = solve_rows(&[&[(0, Some(1))], &[(0, Some(0)), (100, None)]], 1).unwrap_err();
        assert!(error.to_string().contains("未収束"));
        assert!(solve_rows(&[&[(0, Some(0)), (100, None)]], 0).is_err());
        // 非ゼロの draw 初期値による周期 2 の振動も、成功扱いしない。
        let graph = solve_rows(&[&[(0, Some(1))], &[(0, Some(0))]], 1).unwrap();
        let evaluator = SccEvaluator {
            graph: &graph,
            params: BestParams {
                draw_value: 1,
                merge: MergeMode::Replace,
                skip_unusable_moves: false,
            },
            check_loop: vec![false; 2],
            input_lengths: vec![1; 2],
        };
        let mut best = vec![
            ValueDistance {
                value: 1,
                distance: DISTANCE_MAX
            };
            2
        ];
        let error = iterate_nontrivial_scc(&evaluator, &[0, 1], &mut best, 10).unwrap_err();
        assert!(error.to_string().contains("未収束"));
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("in.db");
        let out = dir.path().join("out.db");
        std::fs::write(&input, include_str!("../tests/fixtures/book_backprop_check_in.db"))
            .unwrap();
        assert!(backprop_file(&input, &out, None, 0, 1, MergeMode::Replace).is_err());
        assert!(!out.exists());
    }

    #[test]
    fn perpetual_check_without_escape_is_loss_in_both_orientations() {
        let fixture = include_str!("../tests/fixtures/book_backprop_check_in.db");
        let pure: String = fixture
            .lines()
            .filter(|line| {
                !line.contains(" -200 ") && !line.contains(" -120 ") && !line.contains(" -80 ")
            })
            .map(|line| format!("{line}\n"))
            .collect();
        for flip in [false, true] {
            let input = if flip {
                pure.lines()
                    .map(|line| {
                        if let Some(sfen) = line.strip_prefix("sfen ") {
                            format!("sfen {}\n", rshogi_book::flipped_key(sfen).unwrap())
                        } else if line.starts_with('#') {
                            format!("{line}\n")
                        } else {
                            let (mv, rest) = line.split_once(' ').unwrap();
                            format!("{} {rest}\n", rshogi_book::flip_usi_move(mv).unwrap())
                        }
                    })
                    .collect()
            } else {
                pure.clone()
            };
            for merge in [MergeMode::Min, MergeMode::Replace] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("in.db");
                let out = dir.path().join("result.db");
                std::fs::write(&path, &input).unwrap();
                backprop_file(&path, &out, None, 0, 1000, merge).unwrap();
                for entry in read_book_db(&out).unwrap().entries.values() {
                    let mut pos = Position::new();
                    pos.set_sfen(&entry.sfen).unwrap();
                    let rule_value = if pos.checkers().is_empty() {
                        -Value::MATE.raw()
                    } else {
                        Value::MATE.raw()
                    };
                    assert_eq!(entry.moves[0].value, merge.apply(0, rule_value));
                }
            }
        }
    }

    #[test]
    fn unexplored_position_keeps_all_rows_even_with_searched_children() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f 3c3d 0 0 7\nsfen {child}\n3c3d none 50 12 3\n"
        );
        let output = backprop_text(
            &input,
            BackpropOptions {
                skip_unsearched_children: true,
                add_transposition_moves: true,
                ..BackpropOptions::default()
            },
        );
        assert!(output.contains("7g7f 3c3d 0 0 7\n"));
        assert!(!backprop_text(&input, BackpropOptions::default()).contains("7g7f 3c3d 0 0 7\n"));
    }

    #[test]
    fn tie_correction_keeps_unusable_and_skipped_child_rows_unchanged() {
        let searched = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        let unsearched = child_position_after_move(START, "2g2f").unwrap().to_sfen();
        let input = format!(
            "{BOOK_HEADER}\nsfen {START}\n7g7f none -50 9 7\n2g2f none -50 9 6\n5e5d none -50 9 5\nnone none -50 9 4\nsfen {searched}\n3c3d none 50 12 3\nsfen {unsearched}\n3c3d none 0 0 2\n"
        );
        for skip_unusable_moves in [false, true] {
            let output = backprop_text(
                &input,
                BackpropOptions {
                    skip_unsearched_children: true,
                    skip_unusable_moves,
                    ..BackpropOptions::default()
                },
            );
            for row in [
                "7g7f none -50 9 7",
                "2g2f none -50 9 6",
                "5e5d none -50 9 5",
                "none none -50 9 4",
            ] {
                assert!(output.contains(row), "{output}");
            }
        }
    }

    #[test]
    fn transposition_completion_is_opt_in_and_new_moves_have_no_min_label() {
        let child = child_position_after_move(START, "7g7f").unwrap().to_sfen();
        for value in [-50, 50] {
            let input = format!(
                "{BOOK_HEADER}\nsfen {START}\n2g2f none -100 10 7\nsfen {child}\n3c3d none {value} 12 3\n"
            );
            assert!(!backprop_text(&input, BackpropOptions::default()).contains("7g7f none"));
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("in.db");
            let out = dir.path().join("out.db");
            std::fs::write(&path, &input).unwrap();
            let options = BackpropOptions {
                add_transposition_moves: true,
                ..BackpropOptions::default()
            };
            for merge in [MergeMode::Min, MergeMode::Replace] {
                backprop_file_with(&path, &out, None, 0, 1000, merge, options).unwrap();
                assert!(
                    std::fs::read_to_string(&out)
                        .unwrap()
                        .contains(&format!("7g7f none {} 1 0\n", -value))
                );
            }
        }
    }

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
