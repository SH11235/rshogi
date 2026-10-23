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

use crate::book_retrograde;

/// YANEURAOU-DB2016 テキスト定跡のヘッダ行。
pub const BOOK_HEADER: &str = "#YANEURAOU-DB2016 1.00";

/// 千日手の値に指定できる範囲。詰みの値 (32000) より内側に限り、値をずらしても桁あふれしないようにする。
const DRAW_VALUE_LIMIT: i32 = 30_000;

/// 伝播値と既存ラベル値の合成方法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MergeMode {
    /// 純 negamax。伝播値で置き換える。
    Replace,
    /// 自分の手は既存ラベル値と伝播値の小さい方を採る。相手の手は伝播値のまま。
    Min,
}

impl MergeMode {
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

/// 千日手の値。それぞれ手番の側から見た値で、先手を自分として解くときは `black`、後手なら `white` を使う。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DrawValues {
    pub black: i32,
    pub white: i32,
}

impl DrawValues {
    /// 先手・後手とも同じ値。
    pub fn both(value: i32) -> Self {
        Self {
            black: value,
            white: value,
        }
    }

    fn of(self, black: bool) -> i32 {
        if black { self.black } else { self.white }
    }
}

/// 局面の best 計算に共通の設定。
#[derive(Debug, Clone, Copy)]
struct BestParams {
    draws: DrawValues,
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

impl Graph {
    /// 局面 key が先手番か。
    fn black(&self, node: usize) -> bool {
        self.keys[node].split_whitespace().nth(1) != Some("w")
    }

    /// 入力の行の数。補完手は後ろに並び、探索ラベルを持たない。
    fn input_len(&self, node: usize) -> usize {
        self.moves[node].len() - self.added_moves[node].len()
    }
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
    pub depths: Vec<usize>,
    pub changed_nodes: Vec<NodeChange>,
    /// 自分・相手が王手をかけ続けられる循環の状態数 (先手・後手として解いた 2 回の合計)。
    pub check_core_states: usize,
    /// 同値手を選ぶと保証が破れるため、保証手だけを残した局面数。
    pub restricted_ties: usize,
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
    draws: DrawValues,
    merge: MergeMode,
) -> Result<PropagationStats> {
    backprop_file_with(book, out, report, draws, merge, BackpropOptions::default())
}

/// [`backprop_file`] に追加オプションを指定する版。
pub fn backprop_file_with(
    book: &Path,
    out: &Path,
    report: Option<&Path>,
    draws: DrawValues,
    merge: MergeMode,
    options: BackpropOptions,
) -> Result<PropagationStats> {
    rshogi_book::Book::from_path(book, true)
        .with_context(|| format!("定跡を rshogi-book で読めません: {}", book.display()))?;
    let db = read_book_db(book)?;
    let mut graph = build_graph_with(&db, options)?;
    let stats = propagate_values_with(&db, &mut graph, draws, merge, options)?;
    write_backprop_book(&db, &graph, out)?;
    if let Some(path) = report {
        write_report(&db, &graph, &stats, path, merge, draws)?;
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

/// 先手・後手をそれぞれ自分として後退解析し、候補手の値を逆伝播する。
pub fn propagate_values(
    book: &BookDb,
    graph: &mut Graph,
    draws: DrawValues,
    merge: MergeMode,
) -> Result<PropagationStats> {
    propagate_values_with(book, graph, draws, merge, BackpropOptions::default())
}

/// [`propagate_values`] に追加オプションを指定する版。
/// `skip_unsearched_children` はグラフ構築時に [`build_graph_with`] へ渡すこと。
pub fn propagate_values_with(
    book: &BookDb,
    graph: &mut Graph,
    draws: DrawValues,
    merge: MergeMode,
    options: BackpropOptions,
) -> Result<PropagationStats> {
    let params = BestParams {
        draws,
        merge,
        skip_unusable_moves: options.skip_unusable_moves,
    };
    for draw in [draws.black, draws.white] {
        if !(-DRAW_VALUE_LIMIT..=DRAW_VALUE_LIMIT).contains(&draw) {
            bail!(
                "千日手の値は -{DRAW_VALUE_LIMIT}..={DRAW_VALUE_LIMIT} で指定してください: {draw}"
            );
        }
    }
    let n = graph.keys.len();
    let mut results: Vec<Option<NodeResult>> = vec![None; n];
    let mut mirrored: Vec<Option<NodeResult>> = vec![None; n];
    let mut stats = PropagationStats::default();
    for black in [true, false] {
        let built = build_game(graph, params, black);
        let solution = book_retrograde::solve(&built.game)?;
        stats.check_core_states += solution.core_size[0] + solution.core_size[1];
        let draw = draws.of(black);
        for v in 0..n {
            let state = built.state[v][usize::from(!black)];
            if state == book_retrograde::NONE {
                continue;
            }
            let state = state as usize;
            let row = |j: u32| {
                let arc = built.game.arcs[built.game.start[state] + j as usize];
                if arc.to == book_retrograde::NONE {
                    (arc.leaf + draw, Some(0))
                } else {
                    let to = arc.to as usize;
                    (solution.values[to] + draw, solution.distance[to].map(|d| d + arc.length()))
                }
            };
            let index =
                |j: u32| built.arc_of[v].iter().position(|&a| a == j && a != book_retrograde::NONE);
            let result = NodeResult {
                value: solution.values[state] + draw,
                rows: built.arc_of[v]
                    .iter()
                    .map(|&j| (j != book_retrograde::NONE).then(|| row(j)))
                    .collect(),
                guaranteed: index(solution.guaranteed[state]),
                preferred: index(solution.preferred[state]),
            };
            // 反転キーで合流した向きは行を持たないので、保存されている向きの行で指せるかを後で確かめる。
            if graph.black(v) == black {
                results[v] = Some(result);
            } else {
                mirrored[v] = Some(result);
            }
        }
    }
    stats.restricted_ties = write_certified(graph, params, &results, &mirrored)?;

    let scc = build_scc_graph(&graph.adjacency);
    stats.nontrivial_sccs = scc.nontrivial.iter().filter(|&&v| v).count();
    stats.max_scc_size = scc.comps.iter().map(Vec::len).max().unwrap_or(0);
    let mut scc_depth = vec![0usize; scc.comps.len()];
    for &comp in scc.topo.iter().rev() {
        scc_depth[comp] =
            scc.edges[comp].iter().map(|&child| scc_depth[child] + 1).max().unwrap_or(0);
    }
    for (node_idx, key) in graph.keys.iter().enumerate() {
        let comp = scc.comp_of[node_idx];
        let draw = draws.of(graph.black(node_idx));
        let counted = || graph.moves[node_idx].iter().filter(|mv| params.counts(mv));
        let old_best = counted().map(|mv| mv.old).max().unwrap_or(draw);
        let new_best = counted().map(|mv| mv.new).max().unwrap_or(draw);
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
        stats.depths.push(scc_depth[comp]);
        for mv in &graph.moves[node_idx] {
            if mv.old != mv.new {
                stats.updated_moves += 1;
                stats.abs_deltas.push((mv.new - mv.old).abs());
            }
            if mv.new == draw
                && mv.edge.is_some_and(|e| scc.nontrivial[comp] && scc.comp_of[e.to] == comp)
            {
                stats.draw_moves += 1;
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

/// 解いた値を書き出し値にし、保証検査を通るまで危ない同値手を保証手だけに絞る。絞った局面数を返す。
fn write_certified(
    graph: &mut Graph,
    params: BestParams,
    results: &[Option<NodeResult>],
    mirrored: &[Option<NodeResult>],
) -> Result<usize> {
    let n = graph.keys.len();
    // 同値手は値を保つ手の中から選び、他は保存時だけ -1 する。選ぶと保証が破れる局面は保証手だけを残す。
    let mut restricted = vec![false; n];
    loop {
        assign_outputs(graph, results, &restricted);
        let conflicts = flip_conflicts(graph, params, mirrored);
        if conflicts > 0 {
            bail!(
                "反転キーで合流した {conflicts} 局面で、保存されている向きの行が反対の向きの値を保ちません。                 1 行では両方の向きを表せないので book は出力しません。千日手の値を先後で揃えてください"
            );
        }
        let violations = certify(graph, params);
        if violations.nodes.is_empty() {
            break;
        }
        let fresh: Vec<_> = violations.nodes.iter().copied().filter(|&v| !restricted[v]).collect();
        if fresh.is_empty() {
            bail!("{}", violations.message(graph));
        }
        for v in fresh {
            restricted[v] = true;
        }
    }
    let count = restricted.iter().filter(|&&r| r).count();
    if count > 0 {
        eprintln!("同値手を選ぶと保証が破れる {} 局面で、保証手以外の同値手を -1 しました", count);
    }
    Ok(count)
}

/// 反転キーで合流した向きで、書き出し値が最大の行のどれを指しても、その向きで解いた値を保つか。
/// 保たない局面の数を返す。千日手の値が先後で同じなら、両方の向きは同じ問題なので 0 になる。
fn flip_conflicts(graph: &Graph, params: BestParams, mirrored: &[Option<NodeResult>]) -> usize {
    (0..graph.keys.len())
        .filter(|&v| {
            let Some(mirror) = &mirrored[v] else {
                return false;
            };
            let moves = &graph.moves[v];
            let best = moves.iter().filter(|mv| params.counts(mv)).map(|mv| mv.new).max();
            moves.iter().enumerate().any(|(idx, mv)| {
                params.counts(mv)
                    && Some(mv.new) == best
                    && mirror.rows[idx].is_none_or(|(value, _)| value != mirror.value)
            })
        })
        .count()
}

/// 保存されている向きで、局面の手番の側を自分として解いた結果。値は手番の側から見た値。
#[derive(Debug, Clone)]
struct NodeResult {
    value: i32,
    /// 行ごとの値と出口までの手数。best の計算に数えない行は `None`。
    rows: Vec<Option<(i32, Option<u32>)>>,
    guaranteed: Option<usize>,
    preferred: Option<usize>,
}

/// 先手 (`black`) か後手を自分とした後退解析の問題と、局面・行との対応。
struct BuiltGame {
    game: book_retrograde::Game,
    /// `state[v][c]`: 局面 `v` を実際の手番 `c` (0 = 先手、1 = 後手) で指す頂点。届かない向きは `NONE`。
    state: Vec<[u32; 2]>,
    /// 局面の行ごとの手番号。best の計算に数えない行は `NONE`。
    arc_of: Vec<Vec<u32>>,
}

/// 千日手の値を自分の側の値に固定した 2 人零和の問題を作る。値は自分から見て千日手が 0 になるようにずらす。
///
/// 局面と実際の手番の組を頂点にする。反転キーで合流した辺は保存されている局面の手番と実際の手番が逆になるが、
/// 手を指すごとに実際の手番は必ず入れ替わる。`min` では、相手が book 外で咎められる (自分の手の探索ラベル)
/// ことを、自分の手と行き先の間に置いた相手の分岐 (ラベルの値の葉か、そのまま進むか) で表す。
fn build_game(graph: &Graph, params: BestParams, black: bool) -> BuiltGame {
    use book_retrograde::{Arc, NONE, Step};
    let n = graph.keys.len();
    let me = usize::from(!black);
    let draw = params.draws.of(black);
    let color = |v: usize| usize::from(!graph.black(v));
    let arc_of: Vec<Vec<u32>> = graph
        .moves
        .iter()
        .map(|moves| {
            let mut next = 0;
            moves
                .iter()
                .map(|mv| {
                    if params.counts(mv) {
                        next += 1;
                        next - 1
                    } else {
                        NONE
                    }
                })
                .collect()
        })
        .collect();
    let mut reachable = vec![[false; 2]; n];
    let mut stack: Vec<(usize, usize)> = (0..n).map(|v| (v, color(v))).collect();
    for &(v, c) in &stack {
        reachable[v][c] = true;
    }
    while let Some((v, c)) = stack.pop() {
        for mv in graph.moves[v].iter().filter(|mv| params.counts(mv)) {
            if let Some(edge) = mv.edge
                && !reachable[edge.to][1 - c]
            {
                reachable[edge.to][1 - c] = true;
                stack.push((edge.to, 1 - c));
            }
        }
    }
    let mut state = vec![[NONE; 2]; n];
    let mut order = Vec::new();
    for v in 0..n {
        for c in 0..2 {
            if reachable[v][c] {
                state[v][c] = order.len() as u32;
                order.push((v, c));
            }
        }
    }
    let mut own = Vec::with_capacity(order.len());
    let mut start = vec![0];
    let mut arcs = Vec::new();
    // `min` の分岐: (ラベルの値の葉、行き先の頂点)。
    let mut branches = Vec::new();
    for &(v, c) in &order {
        own.push(c == me);
        let input_len = graph.input_len(v);
        for (idx, mv) in graph.moves[v].iter().enumerate().filter(|(_, mv)| params.counts(mv)) {
            let mine = |x: i32| if c == me { x } else { -x };
            match mv.edge {
                None => arcs.push(Arc {
                    to: NONE,
                    leaf: mine(mv.old) - draw,
                    step: Step::Quiet,
                }),
                Some(edge) => {
                    let to = state[edge.to][1 - c];
                    let step = if graph.checked[edge.to] {
                        Step::Check
                    } else {
                        Step::Quiet
                    };
                    if params.merge == MergeMode::Min && c == me && idx < input_len {
                        let branch = (order.len() + branches.len()) as u32;
                        branches.push((mv.old - draw, to));
                        arcs.push(Arc {
                            to: branch,
                            leaf: 0,
                            step,
                        });
                    } else {
                        arcs.push(Arc { to, leaf: 0, step });
                    }
                }
            }
        }
        start.push(arcs.len());
    }
    for (label, to) in branches {
        own.push(false);
        arcs.push(Arc {
            to: NONE,
            leaf: label,
            step: Step::Quiet,
        });
        arcs.push(Arc {
            to,
            leaf: 0,
            step: Step::Pass,
        });
        start.push(arcs.len());
    }
    let mate = Value::MATE.raw();
    BuiltGame {
        game: book_retrograde::Game {
            own,
            start,
            arcs,
            win: [mate - draw, mate + draw],
        },
        state,
        arc_of,
    }
}

/// 解いた値を書き出す値にする。値が best と同じ行は、選ぶ手と、それと同じ手数で出口へ向かう手だけを残し、
/// 他は -1 する (保証手だけを残す局面では選ぶ手以外を全て -1)。補正は書き出しだけで親へ伝えない。
fn assign_outputs(graph: &mut Graph, results: &[Option<NodeResult>], restricted: &[bool]) {
    for (node, moves) in graph.moves.iter_mut().enumerate() {
        let preserved = graph.preserved[node];
        let result = results[node].as_ref();
        let chosen = result.and_then(|r| {
            if restricted[node] {
                r.guaranteed
            } else {
                r.preferred
            }
        });
        let chosen_distance = result.zip(chosen).and_then(|(r, c)| r.rows[c].and_then(|(_, d)| d));
        for (idx, mv) in moves.iter_mut().enumerate() {
            let row = result.and_then(|r| r.rows[idx]);
            mv.new = match row {
                Some((value, distance)) if !preserved && !mv.preserve && mv.usable => {
                    let best = result.is_some_and(|r| r.value == value);
                    let keep = chosen == Some(idx)
                        || (!restricted[node] && distance.is_some() && distance == chosen_distance);
                    if best && !keep {
                        // 最小の合法値 -MATE は据え置き、-INFINITE を生成しない。
                        value.saturating_sub(1).max(-Value::MATE.raw())
                    } else {
                        value
                    }
                }
                _ => mv.old,
            };
        }
    }
}

/// 保証検査で見つかった局面。
struct Violations {
    /// 保存されている向きで約束が破られる局面。
    nodes: BTreeSet<usize>,
    perpetual_check: BTreeSet<usize>,
    repetition: BTreeSet<usize>,
}

impl Violations {
    fn message(&self, graph: &Graph) -> String {
        let sample = |nodes: &BTreeSet<usize>| {
            nodes
                .iter()
                .take(5)
                .map(|&v| graph.keys[v].as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        };
        format!(
            "逆伝播した値の保証検査に失敗しました。book は出力しません: 連続王手で負ける循環へ持ち込まれる {} 局面 [{}]、千日手へ逃げられる {} 局面 [{}]",
            self.perpetual_check.len(),
            sample(&self.perpetual_check),
            self.repetition.len(),
            sample(&self.repetition)
        )
    }
}

/// 書き出す値で定跡を指したとき、循環によって値の約束が破られないかを、先手・後手をそれぞれ自分として検査する。
///
/// 自分は各局面で書き出し値が最大の手 (同値の -1 補正を受けなかった手) のどれを選んでもよく、
/// 相手は全手を選べるとして、次のどちらかの局面を返す。
/// - 詰まされる値でないのに、相手が自分の連続王手の循環 (反則負け) へ持ち込める。
/// - 自分の千日手の値より良い値なのに、相手が王手をやめながら続けられる循環 (千日手) へ持ち込める。
///
/// 値の整合は前提にせず、約束した局面から悪い循環へ届くかを到達可能性で調べる。
fn certify(graph: &Graph, params: BestParams) -> Violations {
    let mut violations = Violations {
        nodes: BTreeSet::new(),
        perpetual_check: BTreeSet::new(),
        repetition: BTreeSet::new(),
    };
    let mate = Value::MATE.raw();
    for black in [true, false] {
        let states = CertifyStates::new(graph, params, black);
        let draw = params.draws.of(black);
        let follows = |s: usize, idx: usize, _t: usize| states.follows(s, idx);

        // 自分の手が全て王手になる循環。強連結成分の状態はどれもその成分内の循環に乗る。
        let (comp, cyclic) = states.components(|s, idx, t| {
            states.follows(s, idx) && (states.is_opponent(s) || graph.checked[t / 2])
        });
        let seeds: Vec<bool> = comp.iter().map(|&c| cyclic[c as usize]).collect();
        drop(comp);
        let perpetual = states.violations(&states.reach(seeds, follows), |s| states.own(s) > -mate);

        // 相手が王手をしない手を含む循環。成分内の辺はどれもその成分内の循環に乗るので、
        // そうした相手の手が成分内にあれば、相手が王手を続けて負ける循環ではない。
        let (comp, _) = states.components(follows);
        let mut repeats = vec![false; comp.len()];
        for s in (0..states.len()).filter(|&s| states.is_opponent(s)) {
            if states.children(s).any(|(_, t)| !graph.checked[t / 2] && comp[s] == comp[t]) {
                repeats[comp[s] as usize] = true;
            }
        }
        let seeds: Vec<bool> = comp.iter().map(|&c| repeats[c as usize]).collect();
        drop(comp);
        let repetition = states.violations(&states.reach(seeds, follows), |s| states.own(s) > draw);

        violations.nodes.extend(perpetual.iter().chain(&repetition));
        violations.perpetual_check.extend(perpetual);
        violations.repetition.extend(repetition);
    }
    violations
}

/// [`certify`] が辿る、局面 `v` と実際の手番 `c` (0 = 先手、1 = 後手) を組にした状態 `2 * v + c` のグラフ。
///
/// 反転キーで合流した辺は保存されている局面の手番と実際の手番が逆になるので、実際の手番で自分か相手かを判定する。
struct CertifyStates<'a> {
    graph: &'a Graph,
    params: BestParams,
    /// 自分の手番の状態の偶奇。
    me: usize,
    values: Vec<i32>,
    /// 局面 `v` へ入る辺 `(親局面, 親の手番号)` は `reverse[reverse_start[v]..reverse_start[v + 1]]`。
    reverse_start: Vec<usize>,
    reverse: Vec<(u32, u32)>,
    /// 保存されている向きの局面から指して届く状態。反転キーで合流した向きはここに含まれるものだけが実在する。
    real: Vec<bool>,
}

impl<'a> CertifyStates<'a> {
    fn new(graph: &'a Graph, params: BestParams, black: bool) -> Self {
        let values = graph
            .moves
            .iter()
            .enumerate()
            .map(|(v, moves)| {
                moves
                    .iter()
                    .filter(|mv| params.counts(mv))
                    .map(|mv| mv.new)
                    .max()
                    .unwrap_or(params.draws.of(graph.black(v)))
            })
            .collect();
        let edges = || {
            graph.moves.iter().enumerate().flat_map(move |(node, moves)| {
                moves.iter().enumerate().filter_map(move |(idx, mv)| {
                    mv.edge.filter(|_| params.counts(mv)).map(|edge| (node, idx, edge.to))
                })
            })
        };
        let n = graph.keys.len();
        let mut reverse_start = vec![0; n + 1];
        for (_, _, to) in edges() {
            reverse_start[to + 1] += 1;
        }
        for v in 0..n {
            reverse_start[v + 1] += reverse_start[v];
        }
        let mut next = reverse_start.clone();
        let mut reverse = vec![(0, 0); reverse_start[n]];
        for (node, idx, to) in edges() {
            reverse[next[to]] = (node as u32, idx as u32);
            next[to] += 1;
        }
        let mut states = Self {
            graph,
            params,
            me: usize::from(!black),
            values,
            reverse_start,
            reverse,
            real: vec![false; 2 * n],
        };
        let mut stack: Vec<usize> = (0..n).map(|v| 2 * v + usize::from(!graph.black(v))).collect();
        for &s in &stack {
            states.real[s] = true;
        }
        while let Some(s) = stack.pop() {
            let children: Vec<usize> = states.children(s).map(|(_, t)| t).collect();
            for t in children {
                if !states.real[t] {
                    states.real[t] = true;
                    stack.push(t);
                }
            }
        }
        states
    }

    fn len(&self) -> usize {
        self.graph.keys.len() * 2
    }

    fn is_opponent(&self, s: usize) -> bool {
        s % 2 != self.me
    }

    /// 自分から見た状態の値。行の値は実際に手番の側から見た値として使われる。
    fn own(&self, s: usize) -> i32 {
        let value = self.values[s / 2];
        if self.is_opponent(s) { -value } else { value }
    }

    /// 自分は書き出し値が最大の手だけを、相手は全手を指しうる。
    /// 相手の探索ラベルが悪くても、book どおりに指す側は book 外の咎める手を指さないので相手の選択肢に残す。
    fn follows(&self, s: usize, idx: usize) -> bool {
        self.is_opponent(s) || self.graph.moves[s / 2][idx].new == self.values[s / 2]
    }

    fn children(&self, s: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let flip = 1 - s % 2;
        self.graph.moves[s / 2].iter().enumerate().filter_map(move |(idx, mv)| {
            mv.edge.filter(|_| self.params.counts(mv)).map(|edge| (idx, edge.to * 2 + flip))
        })
    }

    fn parents(&self, t: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let flip = 1 - t % 2;
        let v = t / 2;
        self.reverse[self.reverse_start[v]..self.reverse_start[v + 1]]
            .iter()
            .map(move |&(node, idx)| (node as usize * 2 + flip, idx as usize))
    }

    /// `allow(親, 手番号, 子)` を満たす辺だけで強連結成分を求め、成分番号と各成分が循環を持つかを返す。
    /// 辺は必ず手番を入れ替えるので自己ループはなく、2 状態以上の成分だけが循環を持つ。
    fn components(&self, allow: impl Fn(usize, usize, usize) -> bool) -> (Vec<u32>, Vec<bool>) {
        let n = self.len();
        let mut visited = vec![false; n];
        let mut order = Vec::with_capacity(n);
        let mut dfs = Vec::new();
        for root in 0..n {
            if visited[root] {
                continue;
            }
            visited[root] = true;
            dfs.push((root, self.children(root)));
            while let Some((s, children)) = dfs.last_mut() {
                let s = *s;
                match children.find(|&(idx, t)| !visited[t] && allow(s, idx, t)) {
                    Some((_, t)) => {
                        visited[t] = true;
                        dfs.push((t, self.children(t)));
                    }
                    None => {
                        order.push(s as u32);
                        dfs.pop();
                    }
                }
            }
        }
        drop(visited);
        let mut comp = vec![u32::MAX; n];
        let mut cyclic = Vec::new();
        let mut stack = Vec::new();
        for &root in order.iter().rev() {
            let root = root as usize;
            if comp[root] != u32::MAX {
                continue;
            }
            let id = cyclic.len() as u32;
            comp[root] = id;
            stack.push(root);
            let mut size = 0;
            while let Some(t) = stack.pop() {
                size += 1;
                for (s, idx) in self.parents(t) {
                    if comp[s] == u32::MAX && allow(s, idx, t) {
                        comp[s] = id;
                        stack.push(s);
                    }
                }
            }
            cyclic.push(size > 1);
        }
        (comp, cyclic)
    }

    /// `allow` を満たす辺を逆に辿り、`seeds` の状態へ届く状態を返す。
    fn reach(
        &self,
        mut seeds: Vec<bool>,
        allow: impl Fn(usize, usize, usize) -> bool,
    ) -> Vec<bool> {
        let mut stack: Vec<usize> = (0..seeds.len()).filter(|&s| seeds[s]).collect();
        while let Some(t) = stack.pop() {
            for (s, idx) in self.parents(t) {
                if !seeds[s] && allow(s, idx, t) {
                    seeds[s] = true;
                    stack.push(s);
                }
            }
        }
        seeds
    }

    /// 届く状態のうち、自分の手番で `promises` を満たす値を約束している局面を返す。
    /// 相手の手番の値は相手の約束で、`min` では相手側の上界なので自分の約束にはならない。
    /// 相手の手番から悪い循環へ届く道は自分の手番の局面を通るので、ここで見落としは出ない。
    /// 反転キーで合流した向きも、保存されている向きの行がその向きの値を保つと確かめてから数える。
    fn violations(&self, reached: &[bool], promises: impl Fn(usize) -> bool) -> BTreeSet<usize> {
        (0..self.len())
            .filter(|&s| !self.is_opponent(s) && self.real[s] && reached[s] && promises(s))
            .map(|s| s / 2)
            .collect()
    }
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
    draws: DrawValues,
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
    writeln!(writer, "- draw value: black {} / white {}", draws.black, draws.white)?;
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
    writeln!(writer, "- check-loop core states: {}", stats.check_core_states)?;
    writeln!(
        writer,
        "- tie moves restricted to the guaranteed move: {}",
        stats.restricted_ties
    )?;
    writeln!(writer)?;
    Ok(())
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

    #[test]
    fn review_quiet_cycle_with_check_branches_stays_draw() {
        let mut sfen = "5k3/9/6R2/9/9/9/6r2/9/5K3 b - 1".to_string();
        let mut input = format!("{BOOK_HEADER}\n");
        let mut quiet = Vec::new();
        for (mv, branch) in [
            ("3c5c", ["3c4c", "4a3a", "4c3c", "3a4a"]),
            ("3g5g", ["3g4g", "4i3i", "4g3g", "3i4i"]),
            ("5c3c", ["5c4c", "4a5a", "4c5c", "5a4a"]),
            ("5g3g", ["5g4g", "4i5i", "4g5g", "5i4i"]),
        ] {
            quiet.push((strip_ply(&sfen).to_string(), mv));
            input.push_str(&format!("sfen {sfen}\n{mv} none 0 10 1\n"));
            let mut check = sfen.clone();
            for checking_move in branch {
                input.push_str(&format!("sfen {check}\n{checking_move} none 0 10 1\n"));
                check = child_position_after_move(&check, checking_move).unwrap().to_sfen();
            }
            assert_eq!(strip_ply(&check), strip_ply(&sfen));
            sfen = child_position_after_move(&sfen, mv).unwrap().to_sfen();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("in.db");
        std::fs::write(&path, input).unwrap();
        let book = read_book_db(&path).unwrap();
        let mut graph = build_graph(&book).unwrap();
        assert_eq!(graph.illegal_moves, 0);
        propagate_values(&book, &mut graph, DrawValues::default(), MergeMode::Replace).unwrap();
        for (key, mv) in quiet {
            let node = graph.keys.iter().position(|k| k == &key).unwrap();
            let index = book.entries[&key]
                .moves
                .iter()
                .position(|m| m.move_usi.as_deref() == Some(mv))
                .unwrap();
            assert_eq!(graph.moves[node][index].new, 0, "{mv}: {key}");
        }
    }

    #[test]
    fn branching_check_loop_is_solved_without_a_visit_limit() {
        // 王手をかける側 (3k の局面) が 2 通りの王手で合流しながら離脱のない循環を回す。
        let mut rows = vec![Vec::new(); 62];
        for level in 0..20 {
            let n = level * 3;
            rows[n] = vec![(0, Some(n + 1)), (0, Some(n + 2))];
            rows[n + 1] = vec![(0, Some(n + 3))];
            rows[n + 2] = vec![(0, Some(n + 3))];
        }
        rows[60] = vec![(0, Some(61))];
        rows[61] = vec![(0, Some(0))];
        let refs: Vec<_> = rows.iter().map(Vec::as_slice).collect();
        let checked: Vec<_> = (0..62).map(|n| n % 3 != 0).collect();
        let black: Vec<_> = (0..62).map(|n| n % 3 == 0).collect();
        let (book, mut graph) = rows_graph(&refs, &checked, &black);
        propagate_values(&book, &mut graph, DrawValues::default(), MergeMode::Replace).unwrap();
        let mate = Value::MATE.raw();
        for (n, moves) in graph.moves.iter().enumerate() {
            let expected = if n % 3 == 0 { -mate } else { mate };
            assert!(moves.iter().all(|mv| mv.new == expected), "{n}");
        }
    }

    #[test]
    fn review_tie_correction_stays_inside_mate_boundary() {
        let graph = solve_rows(&[&[(-32000, None), (0, Some(1))], &[(32000, None)]]).unwrap();
        assert_eq!(graph.moves[0][0].new, -32000);
        assert_eq!(graph.moves[0][1].new, -32000);
    }

    /// 行 `(値, 子)` の局面を並べたグラフ。局面 key は `<番号> <手番>` で、`black[i]` が先手番。
    fn rows_graph(
        rows: &[&[(i32, Option<usize>)]],
        checked: &[bool],
        black: &[bool],
    ) -> (BookDb, Graph) {
        let keys: Vec<_> = (0..rows.len())
            .map(|i| format!("{i} {}", if black[i] { "b" } else { "w" }))
            .collect();
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
        let graph = Graph {
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
            checked: checked.to_vec(),
            preserved: vec![false; rows.len()],
            added_moves: vec![vec![]; rows.len()],
        };
        (book, graph)
    }

    /// 偶数番が先手番、奇数番が後手番の局面を並べて逆伝播する。
    fn solve_rows_with(
        rows: &[&[(i32, Option<usize>)]],
        draws: DrawValues,
        merge: MergeMode,
    ) -> Result<Graph> {
        let black: Vec<_> = (0..rows.len()).map(|i| i % 2 == 0).collect();
        let (book, mut graph) = rows_graph(rows, &vec![false; rows.len()], &black);
        propagate_values(&book, &mut graph, draws, merge)?;
        Ok(graph)
    }

    fn solve_rows(rows: &[&[(i32, Option<usize>)]]) -> Result<Graph> {
        solve_rows_with(rows, DrawValues::default(), MergeMode::Replace)
    }

    fn values(graph: &Graph) -> Vec<Vec<i32>> {
        graph
            .moves
            .iter()
            .map(|moves| moves.iter().map(|mv| mv.new).collect())
            .collect()
    }

    /// 書き出し値をそのまま与えたグラフに [`certify`] だけを適用する。偶数番が先手番。
    fn certify_rows(
        rows: &[&[(i32, Option<usize>)]],
        checked: &[bool],
        draws: DrawValues,
    ) -> Result<()> {
        let black: Vec<_> = (0..rows.len()).map(|i| i % 2 == 0).collect();
        let (_, graph) = rows_graph(rows, checked, &black);
        let violations = certify(
            &graph,
            BestParams {
                draws,
                merge: MergeMode::Replace,
                skip_unusable_moves: false,
            },
        );
        if violations.nodes.is_empty() {
            Ok(())
        } else {
            bail!("{}", violations.message(&graph))
        }
    }

    #[test]
    fn certify_rejects_positive_value_on_quiet_cycle() {
        let error = certify_rows(
            &[&[(50, Some(1))], &[(-50, Some(0))]],
            &[false; 2],
            DrawValues::default(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("千日手へ逃げられる 1 局面"), "{error}");
        certify_rows(&[&[(0, Some(1))], &[(0, Some(0))]], &[false; 2], DrawValues::default())
            .unwrap();
        // 相手が王手を続けるだけの循環なら、相手の反則負けなので勝ちを約束できる。
        let mate = Value::MATE.raw();
        certify_rows(
            &[&[(mate, Some(1))], &[(-mate, Some(0))]],
            &[true, false],
            DrawValues::default(),
        )
        .unwrap();
    }

    #[test]
    fn certify_rejects_own_perpetual_check_unless_already_mated() {
        let error = certify_rows(
            &[&[(-80, Some(1))], &[(80, Some(0))]],
            &[false, true],
            DrawValues::default(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("連続王手で負ける循環へ持ち込まれる 1 局面"),
            "{error}"
        );
        let mate = Value::MATE.raw();
        certify_rows(
            &[&[(-mate, Some(1))], &[(mate, Some(0))]],
            &[false, true],
            DrawValues::default(),
        )
        .unwrap();
    }

    #[test]
    fn certify_rejects_tied_move_into_unsafe_cycle() {
        // 同値の離脱があっても、補正されずに残った循環手を選べば千日手へ逃げられる。
        let rows: [&[_]; 2] = [&[(50, None), (50, Some(1))], &[(-50, Some(0))]];
        assert!(certify_rows(&rows, &[false; 2], DrawValues::default()).is_err());
        let rows: [&[_]; 2] = [&[(50, None), (49, Some(1))], &[(-50, Some(0))]];
        certify_rows(&rows, &[false; 2], DrawValues::default()).unwrap();
    }

    #[test]
    fn min_caps_only_own_moves_and_a_cycle_is_a_draw() {
        // 循環上の値 0 の局面で値の整合が切れていても、A の 50 は千日手にしかならない。
        // 先手 (A, C) の値は先手の手だけをラベルで抑え、後手 (B, D) の値は後手の手だけを抑えて求める。
        let rows: [&[_]; 4] = [
            &[(50, Some(1))],
            &[(-50, Some(2))],
            &[(0, Some(3))],
            &[(-50, Some(0))],
        ];
        let graph = solve_rows_with(&rows, DrawValues::default(), MergeMode::Min).unwrap();
        assert_eq!(values(&graph), [[0], [-50], [0], [-50]]);
        let graph = solve_rows(&rows).unwrap();
        assert_eq!(values(&graph), [[0], [0], [0], [0]]);
    }

    #[test]
    fn min_does_not_cap_opponent_moves_with_their_labels() {
        // 後手の 2 手目のラベル 20 は先手が book 外で咎める前提の値。先手は book どおりに指すので、
        // 先手から見た値では後手の手を抑えず、後手から見た値では後手自身の手を抑える。
        let rows: [&[_]; 3] = [
            &[(10, Some(1))],
            &[(10, None), (20, Some(2))],
            &[(-100, None)],
        ];
        let graph = solve_rows_with(&rows, DrawValues::default(), MergeMode::Min).unwrap();
        assert_eq!(values(&graph), [vec![-100], vec![10, 20], vec![-100]]);
    }

    #[test]
    fn draw_values_are_fixed_per_side() {
        // 先手が千日手を -70 とみると、後手は +30 で離脱せずに循環へ戻せば、先手に -50 の離脱を強いられる。
        let rows: [&[_]; 2] = [&[(-50, None), (0, Some(1))], &[(0, Some(0)), (30, None)]];
        let draws = DrawValues {
            black: -70,
            white: 0,
        };
        let graph = solve_rows_with(&rows, draws, MergeMode::Replace).unwrap();
        assert_eq!(values(&graph), [[-50, -51], [29, 30]]);
        // 両側とも 0 なら、後手が循環へ戻すと先手は千日手にできるので、後手は +30 で離脱する。
        let graph = solve_rows(&rows).unwrap();
        assert_eq!(values(&graph), [[-50, -30], [29, 30]]);
    }

    #[test]
    fn unsafe_tie_is_restricted_to_the_guaranteed_move() {
        // 同じ手数と見なした同値手が千日手の循環に入るなら、保証手 (葉) だけを残して循環手を -1 する。
        let (_, mut graph) = rows_graph(
            &[&[(50, None), (0, Some(1))], &[(0, Some(0))]],
            &[false; 2],
            &[true, false],
        );
        let results = [
            Some(NodeResult {
                value: 50,
                rows: vec![Some((50, Some(0))), Some((50, Some(0)))],
                guaranteed: Some(0),
                preferred: Some(0),
            }),
            Some(NodeResult {
                value: -50,
                rows: vec![Some((-50, Some(1)))],
                guaranteed: Some(0),
                preferred: Some(0),
            }),
        ];
        let params = BestParams {
            draws: DrawValues::default(),
            merge: MergeMode::Replace,
            skip_unusable_moves: false,
        };
        assert_eq!(write_certified(&mut graph, params, &results, &[None, None]).unwrap(), 1);
        assert_eq!(values(&graph), [vec![50, 49], vec![-50]]);
        // 保証手だけにしても破られる値はエラーにする。
        let mut broken = results.clone();
        broken[0].as_mut().unwrap().guaranteed = Some(1);
        broken[0].as_mut().unwrap().preferred = Some(1);
        assert!(write_certified(&mut graph, params, &broken, &[None, None]).is_err());
    }

    #[test]
    fn flipped_orientation_that_needs_another_move_is_rejected() {
        // B と C 以降は反転キーで合流し、A から進むと C は後手番になる。先手は千日手を -70 とみて C で -50 の葉を選ぶが、
        // 千日手を 0 とみる後手には循環を続ける方が良いので、保存されている 1 行では両方の向きを表せない。
        let rows: [&[_]; 6] = [
            &[(0, Some(1))],
            &[(0, Some(2))],
            &[(-50, None), (0, Some(3))],
            &[(0, Some(4))],
            &[(0, Some(5))],
            &[(0, Some(2))],
        ];
        let black = [false, false, true, false, true, false];
        let (book, mut graph) = rows_graph(&rows, &[false; 6], &black);
        let draws = DrawValues {
            black: -70,
            white: 0,
        };
        let error = propagate_values(&book, &mut graph, draws, MergeMode::Replace).unwrap_err();
        assert!(error.to_string().contains("反転キーで合流した"), "{error}");
        // 千日手の値が先後で同じなら、両方の向きは同じ問題なので通る。
        let (book, mut graph) = rows_graph(&rows, &[false; 6], &black);
        propagate_values(&book, &mut graph, DrawValues::both(-70), MergeMode::Replace).unwrap();
    }

    #[test]
    fn draw_values_outside_the_engine_range_are_rejected() {
        for draw in [i32::MIN, -30_001, 30_001, i32::MAX] {
            let error =
                solve_rows_with(&[&[(0, None)]], DrawValues::both(draw), MergeMode::Replace)
                    .unwrap_err();
            assert!(error.to_string().contains("千日手の値は"), "{error}");
        }
        solve_rows_with(&[&[(0, None)]], DrawValues::both(-30_000), MergeMode::Replace).unwrap();
    }

    #[test]
    fn mutual_continuous_check_is_rejected() {
        let black = [true, false];
        let (book, mut graph) =
            rows_graph(&[&[(0, Some(1))], &[(0, Some(0))]], &[true, true], &black);
        let error = propagate_values(&book, &mut graph, DrawValues::default(), MergeMode::Replace)
            .unwrap_err();
        assert!(error.to_string().contains("双方が王手をかけ続ける循環"), "{error}");
    }

    #[test]
    fn certify_compares_with_each_side_draw_value() {
        let rows: [&[_]; 2] = [&[(20, Some(1))], &[(-20, Some(0))]];
        let draws = |black, white| DrawValues { black, white };
        certify_rows(&rows, &[false; 2], draws(30, -10)).unwrap();
        // 先手は 20 を約束するが、千日手は先手に 10 しか与えない。
        assert!(certify_rows(&rows, &[false; 2], draws(10, -10)).is_err());
        // 後手は -20 を約束するが、千日手は後手に -10 を与えるので約束より良い。
        assert!(certify_rows(&rows, &[false; 2], draws(30, -30)).is_err());
    }

    #[test]
    fn losing_side_cannot_claim_draw_when_winner_can_exit() {
        let graph = solve_rows(&[&[(0, Some(1))], &[(0, Some(0)), (100, None)]]).unwrap();
        assert_eq!(graph.moves[0][0].new, -100);
        assert_eq!(graph.moves[1][0].new, 99); // 同値の遠回りを避ける。
        assert_eq!(graph.moves[1][1].new, 100);
    }

    #[test]
    fn distances_prefer_short_wins_and_long_losses_only_at_output() {
        for value in [-50, 0, 50] {
            let graph = solve_rows(&[&[(value, None), (0, Some(1))], &[(-value, None)]]).unwrap();
            let expected = if value >= 0 {
                [value, value - 1]
            } else {
                [value - 1, value]
            };
            assert_eq!([graph.moves[0][0].new, graph.moves[0][1].new], expected);
        }
        let graph = solve_rows(&[&[(0, Some(1))], &[(0, Some(0))]]).unwrap();
        assert_eq!(values(&graph), [[0], [0]]);
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
                backprop_file(&path, &out, None, DrawValues::default(), merge).unwrap();
                for entry in read_book_db(&out).unwrap().entries.values() {
                    let mut pos = Position::new();
                    pos.set_sfen(&entry.sfen).unwrap();
                    let rule_value = if pos.checkers().is_empty() {
                        -Value::MATE.raw()
                    } else {
                        Value::MATE.raw()
                    };
                    let expected = if merge == MergeMode::Min {
                        rule_value.min(0)
                    } else {
                        rule_value
                    };
                    assert_eq!(entry.moves[0].value, expected);
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
                backprop_file_with(&path, &out, None, DrawValues::default(), merge, options)
                    .unwrap();
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
                backprop_file_with(
                    &in_path,
                    &out_path,
                    None,
                    DrawValues::default(),
                    merge,
                    options,
                )
                .unwrap();
                let output = read_book_db(&out_path).unwrap();
                let value = output.entries[strip_ply(START)].moves[0].value;
                let expected = if excluded {
                    123
                } else {
                    if merge == MergeMode::Min {
                        (-best).min(123)
                    } else {
                        -best
                    }
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
        backprop_file_with(
            &in_path,
            &out_path,
            None,
            DrawValues::default(),
            MergeMode::Replace,
            options,
        )
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
