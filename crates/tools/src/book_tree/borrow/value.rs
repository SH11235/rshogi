//! `book_mine borrow value`: 出口と確認点の読みを台帳から引き、部分木を後退解析で根まで伝播して
//! 定跡の元データに登録する。

use super::cut::master_value;
use super::{Kind, Meta, RootInfo, Subtree, read_cut, side, turn};
use crate::book_backprop::{
    BOOK_HEADER, BackpropOptions, DrawValues, Graph, MergeMode, propagate_values, read_graph,
};
use crate::book_tree::audit::{Fill, MasterValue, audit, audited_moves, master_values};
use crate::book_tree::journal::{Reading, read_source};
use crate::book_tree::master_table::{
    Row, format_master, parse_master, parse_master_rows, positions_from_rows, read_master,
};
use crate::book_tree::register::{self, Conflict, NodeMove, Options, merge};
use crate::common::io::write_atomic;
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};
use std::fmt::Write as _;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

/// 0 行の読みの値。`bestmove resign` は詰み (負け)、勝ち宣言は勝ち。
const MATE: i32 = 30000;

#[derive(Args, Debug)]
pub(super) struct ValueArgs {
    /// `borrow cut` の出力先
    #[arg(long)]
    cut: PathBuf,
    /// 読みの台帳 (tree serve の journal。serve が書いている間は複製を渡す)
    #[arg(long)]
    journal: PathBuf,
    /// 台帳の読み条件の指紋。省略時は台帳内の唯一の条件
    #[arg(long)]
    fingerprint: Option<String>,
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 語彙と読み条件の表を置いたディレクトリ
    #[arg(long)]
    schema: Option<PathBuf>,
    /// 結果の出力先 (まだ無い dir)
    #[arg(long)]
    out: PathBuf,
    /// 足す行の since 列
    #[arg(long)]
    since: String,
    /// 足す行の note 列の末尾に付ける文
    #[arg(long, default_value = "")]
    note: String,
    /// main からこの値未満の差の手を alt にする
    #[arg(long, default_value_t = 40)]
    alt_gap: i32,
    /// curated の行がある局面でも、main の source が置き換えてよいものなら main を伝播後の 1 位に替える
    #[arg(long)]
    replace_curated: bool,
    /// 確認点で、候補の外の手が候補の最善よりこの値以上良ければ印を付ける。登録後の点検の線も兼ねる
    #[arg(long, default_value_t = 40)]
    line: i32,
    /// 確認点で、候補の外の手があれば `--line` によらず一番良い手に印を付ける (足して伸ばす手を集めるとき)
    #[arg(long)]
    mark_all: bool,
    /// 先手を自分として解くときの千日手の値 (対局の DrawValueBlack に揃える)
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 後手を自分として解くときの千日手の値 (対局の DrawValueWhite に揃える)
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    draw_value_white: i32,
    /// 元データを書き換える。省くと出力先に結果を書くだけ
    #[arg(long)]
    apply: bool,
}

#[cfg(test)]
impl ValueArgs {
    pub(super) fn for_test(
        cut: PathBuf,
        journal: PathBuf,
        master: PathBuf,
        out: PathBuf,
        apply: bool,
    ) -> Self {
        Self {
            cut,
            journal,
            fingerprint: None,
            schema: None,
            master,
            out,
            since: "tag".into(),
            note: String::new(),
            alt_gap: 40,
            replace_curated: false,
            line: 40,
            mark_all: false,
            draw_value_black: -70,
            draw_value_white: 70,
            apply,
        }
    }
}

/// 伝播後の手の値 (手番の側から)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MoveValue {
    pub mv: String,
    pub value: i32,
    /// 部分木の手 (大きい定跡の手か --extra の手)。偽なら出口の読みの手。
    pub expanded: bool,
    pub book_value: Option<i32>,
    pub destination_level: Option<usize>,
    pub cond: String,
}

/// 確認点の印: 候補の外の手が候補の最善を線以上上回る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Mark {
    pub key: String,
    pub ply: u32,
    pub mv: String,
    pub cp: i32,
    pub best: String,
    pub best_value: i32,
}

/// 根 1 本の結果。`moves` は局面ごとに値の降順。
#[derive(Debug, Default)]
pub(super) struct RootValues {
    pub moves: BTreeMap<String, Vec<MoveValue>>,
    pub nodes: BTreeMap<String, u64>,
    pub marks: Vec<Mark>,
}

/// 部分木の読みの局面の読み。
pub(super) type ReadingsOf = BTreeMap<String, Reading>;

pub(super) struct ValueInput<'a> {
    pub readings: &'a ReadingsOf,
    pub master: &'a BTreeMap<String, MasterValue>,
    pub master_known: &'a BTreeMap<String, String>,
    pub conditions: &'a BTreeMap<String, String>,
}

// 全候補が同じ条件に由来すると証明できた局面だけ、その条件を親へ渡す。
fn verified_conditions<'a>(graph: &Graph, leaves: &[Vec<Option<&'a str>>]) -> Vec<Option<&'a str>> {
    let mut pending = vec![0; graph.len()];
    let mut parents = vec![Vec::new(); graph.len()];
    let mut queue = VecDeque::new();
    for (node, count) in pending.iter_mut().enumerate() {
        for m in graph.moves(node) {
            if let Some(edge) = m.edge {
                *count += 1;
                parents[edge.to].push(node);
            }
        }
        if *count == 0 {
            queue.push_back(node);
        }
    }
    let mut verified = vec![None; graph.len()];
    while let Some(node) = queue.pop_front() {
        let mut conditions = graph
            .moves(node)
            .zip(&leaves[node])
            .map(|(m, leaf)| m.edge.map_or(*leaf, |edge| verified[edge.to]));
        if let Some(Some(cond)) = conditions.next()
            && conditions.all(|other| other == Some(cond))
        {
            verified[node] = Some(cond);
        }
        for &parent in &parents[node] {
            pending[parent] -= 1;
            if pending[parent] == 0 {
                queue.push_back(parent);
            }
        }
    }
    verified
}

fn master_reading_conditions(
    rows: &[crate::book_tree::master_table::Row],
    positions: &BTreeMap<String, crate::book_tree::master_table::MasterPosition>,
    audited: &crate::book_tree::audit::AuditedBook,
) -> BTreeMap<String, String> {
    let conditions: BTreeMap<_, _> =
        rows.iter().map(|r| ((r.get("key"), r.get("move")), r.get("cond"))).collect();
    let mut leaves = vec![Vec::new(); audited.graph.len()];
    let mut same_condition = BTreeMap::new();
    for ((key, position), known) in positions.iter().zip(&audited.known) {
        let Some(node) = audited.graph.find(key) else {
            continue;
        };
        let matches: Vec<_> = position
            .moves
            .iter()
            .zip(known)
            .filter(|(_, known)| **known)
            .map(|(m, _)| {
                conditions
                    .get(&(key.as_str(), m.mv.as_str()))
                    .copied()
                    .filter(|cond| *cond != "unknown")
            })
            .collect();
        if let Some(Some(cond)) = matches.first()
            && matches.iter().all(|other| *other == Some(*cond))
        {
            same_condition.insert(key.clone(), (*cond).to_owned());
        }
        leaves[node] = matches;
    }
    let verified = verified_conditions(&audited.graph, &leaves);
    same_condition.retain(|key, cond| {
        audited
            .graph
            .find(key)
            .is_some_and(|node| verified[node] == Some(cond.as_str()))
    });
    same_condition
}

#[cfg(test)]
fn master_conditions(
    rows: &[crate::book_tree::master_table::Row],
    positions: &BTreeMap<String, crate::book_tree::master_table::MasterPosition>,
    audited: &crate::book_tree::audit::AuditedBook,
    cond: &str,
) -> BTreeSet<String> {
    master_reading_conditions(rows, positions, audited)
        .into_iter()
        .filter_map(|(key, found)| (found == cond).then_some(key))
        .collect()
}

/// `.db` に書いた 1 行。
struct Written<'a> {
    /// 指し手。`None` は指し手の無い値だけの行。
    mv: Option<&'a str>,
    /// 部分木の手か。
    expanded: bool,
    book_value: Option<i32>,
}

fn leaf_value(reading: &Reading) -> i32 {
    if reading.bestmove == "win" {
        reading.multipv.first().map_or(MATE, |p| p.cp)
    } else {
        -MATE
    }
}

/// 部分木を伝播する。出口は読みの手 (元データで止めた出口は元データの値)、途中の局面は部分木の手を持つ。
///
/// 確認点では、候補の外の手のうち一番良い手が候補の最善より `mark_line` 以上良ければ印を付ける。
/// `None` なら差によらず印を付ける。
pub(super) fn value_root(
    root: &RootInfo,
    tree: &Subtree,
    input: &ValueInput,
    draws: DrawValues,
    mark_line: Option<i32>,
    work: &Path,
) -> Result<RootValues> {
    let ValueInput {
        readings, master, ..
    } = input;
    let mut edges: BTreeMap<&str, Vec<&super::Edge>> = BTreeMap::new();
    for e in &tree.edges {
        edges.entry(e.key.as_str()).or_default().push(e);
    }
    let mut written: BTreeMap<&str, Vec<Written>> = BTreeMap::new();
    let db = work.join(format!("{}.db", root.id));
    let mut out = BufWriter::new(
        std::fs::File::create(&db).with_context(|| format!("{} を作れません", db.display()))?,
    );
    writeln!(out, "{BOOK_HEADER}")?;
    for (key, node) in &tree.nodes {
        writeln!(out, "sfen {key} {}", node.ply)?;
        let rows = written.entry(key.as_str()).or_default();
        match node.kind {
            Kind::Inner => {
                for e in edges.get(key.as_str()).into_iter().flatten() {
                    writeln!(out, "{} none 0 1 1", e.mv)?;
                    rows.push(Written {
                        mv: Some(&e.mv),
                        expanded: true,
                        book_value: e.book_value,
                    });
                }
            }
            Kind::Master if !node.read => {
                let value = master
                    .get(key)
                    .and_then(|v| master_value(v, turn(key, &root.own) == "own"))
                    .with_context(|| {
                        format!(
                            "{}: 元データで止めた局面の値が元データにありません: {key}",
                            root.id
                        )
                    })?;
                writeln!(out, "none none {value} 1 1")?;
                rows.push(Written {
                    mv: None,
                    expanded: false,
                    book_value: None,
                });
            }
            _ => {
                let reading = readings
                    .get(key)
                    .with_context(|| format!("{}: 読みがありません: {key}", root.id))?;
                if reading.lines().is_empty() {
                    writeln!(out, "none none {} 1 1", leaf_value(reading))?;
                    rows.push(Written {
                        mv: None,
                        expanded: false,
                        book_value: None,
                    });
                }
                for p in reading.lines() {
                    writeln!(out, "{} none {} 1 1", p.mv, p.cp)?;
                    rows.push(Written {
                        mv: Some(&p.mv),
                        expanded: false,
                        book_value: None,
                    });
                }
            }
        }
    }
    out.flush()?;
    drop(out);
    // 切り出しは先後反転した局面を合流させないので、伝播でも反転した局面へ辺を張らない。
    let options = BackpropOptions {
        skip_flipped_children: true,
        ..BackpropOptions::default()
    };
    let mut graph = read_graph(&db, options)?;
    ensure!(graph.illegal_moves == 0, "{}: 内部エラー: 非合法手があります", root.id);
    propagate_values(&mut graph, draws, MergeMode::Replace)?;

    let leaves: Vec<Vec<Option<&str>>> = (0..graph.len())
        .map(|node| {
            let key = graph.key(node);
            let n = &tree.nodes[key];
            let cond = if n.kind == Kind::Master && !n.read {
                input.master_known.get(key).map(String::as_str)
            } else if n.kind != Kind::Inner {
                input.conditions.get(key).map(String::as_str)
            } else {
                None
            };
            let cond = cond.filter(|cond| *cond != "unknown");
            graph.moves(node).map(|_| cond).collect()
        })
        .collect();
    let verified = verified_conditions(&graph, &leaves);
    let mut condition_nodes = BTreeMap::new();
    for key in tree.nodes.keys() {
        if let Some(reading) = readings.get(key)
            && let Some(cond) = input.conditions.get(key)
        {
            let count = condition_nodes.entry(cond).or_insert(0);
            *count = (*count).max(reading.nodes_limit);
        }
    }
    let mut parents = vec![Vec::new(); graph.len()];
    let mut backing_nodes = vec![0; graph.len()];
    let mut own_nodes = vec![0; graph.len()];
    let mut selected_children = vec![None; graph.len()];
    let mut queue = BinaryHeap::new();
    for (node, count) in backing_nodes.iter_mut().enumerate() {
        let key = graph.key(node);
        let n = &tree.nodes[key];
        own_nodes[node] = readings.get(key).map_or(0, |reading| reading.nodes_limit);
        *count = if own_nodes[node] > 0 {
            own_nodes[node]
        } else if n.kind == Kind::Master && !n.read {
            input
                .master_known
                .get(key)
                .and_then(|cond| condition_nodes.get(cond))
                .copied()
                .unwrap_or(0)
        } else {
            0
        };
        if *count > 0 {
            queue.push((*count, node));
        }
        for m in graph.moves(node) {
            if let Some(edge) = m.edge {
                parents[edge.to].push(node);
            }
        }
    }
    // 大きい上限から親へ渡し、循環や合流でも各辺を繰り返し辿らない。
    while let Some((count, node)) = queue.pop() {
        if count != backing_nodes[node] {
            continue;
        }
        for &parent in &parents[node] {
            if backing_nodes[parent] < count {
                backing_nodes[parent] = count;
                queue.push((count, parent));
            }
        }
    }
    let mut result = RootValues::default();
    for (key, rows) in written {
        let node = graph
            .find(key)
            .with_context(|| format!("内部エラー: 局面がありません: {key}"))?;
        let values: Vec<_> = graph.moves(node).collect();
        ensure!(values.len() == rows.len(), "内部エラー: 手の数が合いません: {key}");
        let mut moves: Vec<MoveValue> = rows
            .iter()
            .zip(values)
            .enumerate()
            .filter_map(|(i, (row, value))| {
                Some(MoveValue {
                    cond: value
                        .edge
                        .map_or(leaves[node][i], |edge| verified[edge.to])
                        .filter(|cond| !value.tie_lowered || verified[node] == Some(*cond))
                        .unwrap_or("unknown")
                        .to_owned(),
                    mv: row.mv?.to_owned(),
                    value: value.new,
                    destination_level: value.edge.map(|edge| tree.nodes[graph.key(edge.to)].level),
                    expanded: row.expanded,
                    book_value: row.book_value,
                })
            })
            .collect();
        moves.sort_by(|a, b| {
            b.value
                .cmp(&a.value)
                .then_with(|| b.expanded.cmp(&a.expanded))
                .then_with(|| b.book_value.cmp(&a.book_value))
                .then_with(|| a.mv.cmp(&b.mv))
        });
        if let Some(best) = moves.first() {
            selected_children[node] = rows
                .iter()
                .zip(graph.moves(node))
                .find(|(row, _)| row.mv == Some(best.mv.as_str()))
                .and_then(|(_, value)| value.edge.map(|edge| edge.to));
        }
        let n = &tree.nodes[key];
        if n.kind == Kind::Inner && n.read {
            let reading = readings
                .get(key)
                .with_context(|| format!("{}: 読みがありません: {key}", root.id))?;
            if let Some(best) = moves.first() {
                let outside = reading
                    .lines()
                    .iter()
                    .filter(|p| !moves.iter().any(|m| m.mv == p.mv))
                    .max_by_key(|p| p.cp);
                if let Some(p) = outside
                    && mark_line.is_none_or(|line| {
                        i64::from(p.cp) >= i64::from(best.value) + i64::from(line)
                    })
                {
                    result.marks.push(Mark {
                        key: key.to_owned(),
                        ply: n.ply,
                        mv: p.mv.clone(),
                        cp: p.cp,
                        best: best.mv.clone(),
                        best_value: best.value,
                    });
                }
            }
        }
        result.moves.insert(key.to_owned(), moves);
    }
    let counts = selected_backing_nodes(&selected_children, &own_nodes, &backing_nodes);
    for (node, count) in counts.into_iter().enumerate() {
        result.nodes.insert(graph.key(node).to_owned(), count);
    }
    Ok(result)
}

fn selected_backing_nodes(selected: &[Option<usize>], own: &[u64], fallback: &[u64]) -> Vec<u64> {
    let mut counts = own.to_vec();
    let mut state = vec![0; selected.len()];
    let mut path = Vec::new();
    for start in 0..selected.len() {
        let mut node = start;
        while state[node] == 0 && own[node] == 0 {
            state[node] = 1;
            path.push(node);
            if let Some(child) = selected[node] {
                node = child;
            } else {
                break;
            }
        }
        // 選択した手順が循環する場合も、そこから到達できる読みの上限を残す。
        let mut count = if counts[node] > 0 {
            counts[node]
        } else {
            fallback[node]
        };
        while let Some(parent) = path.pop() {
            if count == 0 {
                count = fallback[parent];
            }
            counts[parent] = count;
            state[parent] = 2;
        }
    }
    counts
}

/// `join_roots` で替わった数。
pub(super) struct Joined {
    /// 値が替わった手。
    pub moves: usize,
    /// 1 位の手が替わった局面。
    pub best: usize,
}

/// 登録する手の値を、全部の根の手を元データに入れて `master audit` と同じ伝播で解いた値に替える。
///
/// 根ごとの伝播では、途中の局面の手は部分木の手だけになる。相手の手番の局面は大きい定跡の上位の手しか
/// 持たないので、別の根がその局面を自分の手番として読んで見つけた手や、元データにある手が値に入らず、
/// 木の持ち主に甘い値になる。登録後の点検は全体で伝播するので、根ごとの 1 位を main にすると点検で
/// main が 1 位から負ける。ここで全体の値に揃えてから main を選ぶ。
///
/// 全部の手を入れるのは、alt にしない手も、全体で値が上がって 1 位になり得るため。値の替わった手は、
/// 部分木の何段先の値かが決まらないので value_kind を unknown にし、cond は局面の全候補が台帳の
/// 条件だけで決まると確かめられたときだけ残す。
pub(super) fn join_roots(
    rows: &[Row],
    nodes: &mut BTreeMap<String, register::Node>,
    options: &Options,
    draws: DrawValues,
    line: i32,
) -> Result<Joined> {
    let every_move = Options {
        alt_gap: i32::MAX,
        ..*options
    };
    let (all_rows, _, _) = register::register(rows.to_vec(), nodes, &every_move)?;
    // 行番号はファイルの行と同じ数え方 (見出しの次が 2)。
    let book =
        positions_from_rows(all_rows.iter().cloned().enumerate().map(|(i, r)| Ok((i + 2, r))))?;
    let (_, _, audited) = audit(&book, draws, line, Fill::Off, None)?;
    let verified = master_reading_conditions(&all_rows, &book.0, &audited);
    let index: BTreeMap<&str, usize> =
        book.0.keys().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    let mut joined = Joined { moves: 0, best: 0 };
    for (key, node) in nodes.iter_mut() {
        let (Some(position), Some(&i)) = (book.0.get(key), index.get(key.as_str())) else {
            continue;
        };
        let values = audited_moves(&audited.graph, key, position, &audited.known[i])?;
        let before = node.moves.first().map(|m| m.mv.clone());
        for m in &mut node.moves {
            let value = position
                .moves
                .iter()
                .zip(&values)
                .find(|(p, _)| p.mv == m.mv)
                .and_then(|(_, v)| v.as_ref());
            if let Some(v) = value
                && v.value != m.value
            {
                m.value = v.value;
                m.value_kind = "unknown".into();
                m.cond = verified.get(key).cloned().unwrap_or_else(|| "unknown".into());
                joined.moves += 1;
            }
        }
        // 同じ値の手は根ごとの順 (部分木の手を先) を保つ。
        node.moves.sort_by_key(|m| std::cmp::Reverse(m.value));
        if node.moves.first().map(|m| &m.mv) != before.as_ref() {
            joined.best += 1;
        }
    }
    Ok(joined)
}

/// 登録する局面: 部分木の局面のうち手番が `turn` (`own` / `opp`) のもので、元データで止めた出口と印の付いた
/// 局面を除く。印はどの根で付いても除く (同じ局面が別の根で確認点にならない段に合流していても登録しない)。
pub(super) fn register_nodes(
    index: usize,
    root: &RootInfo,
    turn_of: &str,
    tree: &Subtree,
    values: &RootValues,
    marked: &BTreeSet<&str>,
    max_plies: usize,
    readings: &ReadingsOf,
) -> BTreeMap<String, register::Node> {
    let mut nodes = BTreeMap::new();
    for (key, node) in &tree.nodes {
        // 元データの局面は元データの値と手を残す (読み直した出口でも替えない)。
        if turn(key, &root.own) != turn_of
            || node.kind == Kind::Master
            || marked.contains(key.as_str())
        {
            continue;
        }
        let moves = &values.moves[key];
        if moves.is_empty() {
            continue;
        }
        nodes.insert(
            key.clone(),
            register::Node {
                ply: node.ply.to_string(),
                remaining: max_plies.saturating_sub(node.level),
                nodes: readings.get(key).map_or_else(
                    || values.nodes.get(key).copied().unwrap_or(0),
                    |reading| reading.nodes_limit,
                ),
                moves: moves
                    .iter()
                    .map(|m| NodeMove {
                        cond: m.cond.clone(),
                        mv: m.mv.clone(),
                        value: m.value,
                        value_kind: if let Some(level) = m.destination_level {
                            format!("tree:{}", max_plies.saturating_sub(level).saturating_add(1))
                        } else {
                            "single".into()
                        },
                    })
                    .collect(),
                tree: index,
            },
        );
    }
    nodes
}

/// 全部の根の登録する局面を集め、根の間で食い違った局面の数と、相手の手番の局面を返す。
///
/// 自分の手番の局面は `nodes` に入れて main・alt を選ぶ。相手の手番の局面は別に返し、元データには指さない
/// `record` の手として残す (読んで確かめていない大きい定跡の手なので、逆の手番で対局するときに定跡として
/// 指さない)。同じ局面を別の根が自分の手番として持つときは、読んで確かめた候補を持つその根を使う。
pub(super) fn collect_nodes(
    nodes: &mut BTreeMap<String, register::Node>,
    roots: &[RootInfo],
    trees: &[Subtree],
    values: &[RootValues],
    marked: &BTreeSet<&str>,
    max_plies: usize,
    readings: &ReadingsOf,
) -> (usize, BTreeMap<String, register::Node>) {
    let mut disagreements = 0;
    let mut opponent = BTreeMap::new();
    for (turn_of, into) in [("own", &mut *nodes), ("opp", &mut opponent)] {
        for (index, ((root, tree), values)) in roots.iter().zip(trees).zip(values).enumerate() {
            let found =
                register_nodes(index, root, turn_of, tree, values, marked, max_plies, readings);
            disagreements += merge(into, found);
        }
    }
    opponent.retain(|key, _| !nodes.contains_key(key));
    (disagreements, opponent)
}

/// 自分の手番として登録する局面から、同じ出所の `tier=wide` の `record` (前の回に相手の手番として残した行) を外し、
/// 外した行の数を返す。
pub(super) fn drop_own_records(
    rows: &mut Vec<Row>,
    nodes: &BTreeMap<String, register::Node>,
    source: &str,
) -> usize {
    let before = rows.len();
    rows.retain(|r| {
        !(nodes.contains_key(r.get("key"))
            && r.get("role") == "record"
            && r.get("source") == source
            && r.get("tier") == "wide")
    });
    before - rows.len()
}

/// 相手の手番の局面の手を `record` の行で残し、足した行と値を更新した行の数を返す。元データに無い (局面, 手) は
/// 足し、同じ出所 (`options.source`) の record は値を今回のものに更新する。それ以外の既存の行には触らない。
pub(super) fn append_records(
    rows: &mut Vec<Row>,
    opponent: &BTreeMap<String, register::Node>,
    options: &Options,
) -> (usize, usize) {
    let index: BTreeMap<(String, String), usize> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| ((r.get("key").to_owned(), r.get("move").to_owned()), i))
        .collect();
    let (mut added, mut updated) = (0, 0);
    for (key, node) in opponent {
        let nodes = crate::book_tree::master_table::format_nodes(node.nodes);
        for m in &node.moves {
            let fields = [
                ("key", key.clone()),
                ("ply", node.ply.clone()),
                ("move", m.mv.clone()),
                ("role", "record".to_owned()),
                ("source", options.source.to_owned()),
                ("evals", m.value.to_string()),
                ("nodes", nodes.clone()),
                ("cond", m.cond.clone()),
                ("value_kind", m.value_kind.clone()),
                ("lineage", options.lineage.to_owned()),
                ("tier", "wide".to_owned()),
                ("since", options.since.to_owned()),
                ("note", options.note.to_owned()),
            ];
            match index.get(&(key.clone(), m.mv.clone())) {
                None => {
                    rows.push(Row::from_fields(&fields));
                    added += 1;
                }
                Some(&i)
                    if rows[i].get("role") == "record"
                        && rows[i].get("source") == options.source
                        && rows[i].get("tier") == "wide"
                        && fields[5..10].iter().any(|(c, v)| rows[i].get(c) != v) =>
                {
                    for (column, value) in &fields[5..10] {
                        rows[i].set(column, value.clone());
                    }
                    rows[i].set("since", options.since.to_owned());
                    updated += 1;
                }
                Some(_) => {}
            }
        }
    }
    (added, updated)
}

fn black(key: &str, value: i32) -> i32 {
    if side(key) == "w" { -value } else { value }
}

pub(super) fn run(args: ValueArgs) -> Result<()> {
    ensure!(!args.since.trim().is_empty(), "--since を指定してください");
    for text in [&args.since, &args.note] {
        ensure!(!text.contains(['\t', '\r', '\n']), "since・note にタブや改行は使えません");
    }
    ensure!(args.alt_gap >= 0, "--alt-gap は 0 以上にしてください");
    ensure!(args.line > 0, "--line は 1 以上にしてください");
    ensure!(
        std::fs::symlink_metadata(&args.out).is_err(),
        "{} は既にあります (--out はまだ無い dir にしてください)",
        args.out.display()
    );
    let (meta, trees) = read_cut(&args.cut)?;
    let wanted: BTreeSet<&str> = trees
        .iter()
        .flat_map(|t| t.nodes.iter().filter(|(_, n)| n.read).map(|(k, _)| k.as_str()))
        .collect();
    let schema = crate::book_tree::schema::Schema::load(args.schema.as_deref())?;
    let mut conditions = BTreeMap::new();
    let mut readings = ReadingsOf::new();
    // 読みが不要な木に、無関係な台帳の条件を割り当てない。
    if !wanted.is_empty() {
        let source =
            read_source(&args.journal, args.fingerprint.as_deref(), |key| wanted.contains(key))?;
        for (key, (reading, settings)) in source.selected(args.fingerprint.as_deref()) {
            conditions.insert(key.to_owned(), schema.condition(&settings.fingerprint)?);
            readings.insert(key.to_owned(), reading.clone());
        }
        println!("台帳の読み {} 局面", readings.len());
    }
    let missing: Vec<&str> =
        wanted.iter().copied().filter(|k| !readings.contains_key(*k)).collect();
    if !missing.is_empty() {
        bail!(
            "読みが足りません: {} / {} 局面 (例: {})",
            missing.len(),
            wanted.len(),
            missing.iter().take(3).copied().collect::<Vec<_>>().join(" | ")
        );
    }
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let work = tempfile::tempdir().context("一時ディレクトリを作れません")?;
    let master_rows = read_master(&args.master)?;
    let master = master_values(&master_rows, draws)?;
    let (_, before, audited) = audit(&master_rows, draws, args.line, Fill::Off, None)?;
    let text = std::fs::read_to_string(&args.master)
        .with_context(|| format!("{} を読めません", args.master.display()))?;
    let rows = parse_master(&text)?;
    for row in &rows {
        schema.validate_row(row)?;
    }
    let used_conditions: BTreeSet<_> = conditions.values().collect();
    let master_known = master_reading_conditions(&rows, &master_rows.0, &audited)
        .into_iter()
        .filter(|(_, cond)| used_conditions.contains(cond))
        .collect();
    drop(audited);
    let input = ValueInput {
        readings: &readings,
        master: &master,
        master_known: &master_known,
        conditions: &conditions,
    };

    let mark_line = (!args.mark_all).then_some(args.line);
    let all = meta
        .roots
        .iter()
        .zip(&trees)
        .map(|(root, tree)| value_root(root, tree, &input, draws, mark_line, work.path()))
        .collect::<Result<Vec<_>>>()?;
    let marked: BTreeSet<&str> =
        all.iter().flat_map(|v| v.marks.iter().map(|m| m.key.as_str())).collect();
    let mut nodes = BTreeMap::new();
    let (disagreements, opponent) = collect_nodes(
        &mut nodes,
        &meta.roots,
        &trees,
        &all,
        &marked,
        meta.params.max_plies,
        &readings,
    );
    let before_rows = rows.len();
    // ファイル名だけでは版が分からないので、内容の sha256 の先頭も付ける。
    let lineage = format!("book:{}@{}", meta.book.name, &meta.book.sha256[..8]);
    let options = Options {
        since: &args.since,
        note: &args.note,
        alt_gap: args.alt_gap,
        source: "ext",
        lineage: &lineage,
        schema: &schema,
        replace_curated: args.replace_curated,
        revive_bench: false,
    };
    // record の行も register の並べ替えと schema の検査を通す。前の回に相手の手番として残した record の局面を、
    // 今回は自分の手番として登録するなら、その record は外す (record の手は main・alt の候補から除かれるため)。
    let mut rows = rows;
    let records_dropped = drop_own_records(&mut rows, &nodes, options.source);
    let (records_added, records_updated) = append_records(&mut rows, &opponent, &options);
    let joined = join_roots(&rows, &mut nodes, &options, draws, args.line)?;
    let (rows, conflicts, summary) = register::register(rows, &nodes, &options)?;
    let registered = format_master(&rows);
    // 登録後の元データが点検を通ることを、書き換える前に確かめる。
    let (_, after, _) =
        audit(&parse_master_rows(registered.as_bytes())?, draws, args.line, Fill::Off, None)?;

    std::fs::create_dir_all(&args.out)?;
    write_outputs(&args.out, &meta, &trees, &all, &conflicts)?;
    for s in ["b", "w"] {
        println!(
            "{s}: main を足した局面 {} / alt を足した手 {}",
            summary.added_main.get(s).unwrap_or(&0),
            summary.added_alt.get(s).unwrap_or(&0)
        );
    }
    println!(
        "全体の伝播で値が替わった手 {} / 1 位が替わった局面 {}",
        joined.moves, joined.best
    );
    println!(
        "main を伝播後の 1 位に替えた局面 {} / 評価値を伝播後の値にした行 {} / record なので足さなかった手 {} / 食い違い {} / 根をまたいで 1 位が違った局面 {disagreements} / 印 {}",
        summary.replaced_main,
        summary.evals_updated,
        summary.record_skipped,
        summary.conflicts,
        all.iter().map(|v| v.marks.len()).sum::<usize>()
    );
    println!(
        "相手の手番の局面の record: 足した手 {records_added} / 値を更新した手 {records_updated} / 自分の手番として登録するので外した手 {records_dropped}"
    );
    let line_count =
        |s: &crate::book_tree::audit::AuditSummary| s.at_or_above_line[0] + s.at_or_above_line[1];
    println!(
        "点検 (main が 1 位から {} 以上負ける局面): 前 {} → 後 {}",
        args.line,
        line_count(&before),
        line_count(&after)
    );
    if args.apply {
        write_atomic(&args.master, &registered)?;
        println!("書き換えました: 行 {before_rows} -> {}", rows.len());
    } else {
        println!("確認だけ (--apply なし): 行 {before_rows} -> {}", rows.len());
    }
    Ok(())
}

fn write_outputs(
    out: &Path,
    meta: &Meta,
    trees: &[Subtree],
    all: &[RootValues],
    conflicts: &[Conflict],
) -> Result<()> {
    let mut values = BufWriter::new(std::fs::File::create(out.join("values.tsv"))?);
    writeln!(
        values,
        "id\tkey\tply\tlevel\tside\tturn\tkind\tmove\tvalue\tvalue_black\texpanded\tbook_value"
    )?;
    let mut roots = String::from(
        "id\town\tkey\tply\tkind\tbest\tbest_value\tbest_value_black\tsecond\tsecond_value\tmarks\n",
    );
    let mut marks = String::from("id\tkey\tply\tside\tmove\tcp\tbest\tbest_value\tgap\n");
    for ((root, tree), result) in meta.roots.iter().zip(trees).zip(all) {
        for (key, node) in &tree.nodes {
            for m in &result.moves[key] {
                writeln!(
                    values,
                    "{}\t{key}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    root.id,
                    node.ply,
                    node.level,
                    side(key),
                    turn(key, &root.own),
                    node.kind.as_str(),
                    m.mv,
                    m.value,
                    black(key, m.value),
                    u8::from(m.expanded),
                    m.book_value.map_or_else(String::new, |v| v.to_string())
                )?;
            }
        }
        let moves = &result.moves[&root.key];
        let cell = |i: usize| -> (String, String) {
            moves
                .get(i)
                .map_or((String::new(), String::new()), |m| (m.mv.clone(), m.value.to_string()))
        };
        let (best, best_value) = cell(0);
        let (second, second_value) = cell(1);
        let best_black = moves
            .first()
            .map_or_else(String::new, |m| black(&root.key, m.value).to_string());
        writeln!(
            roots,
            "{}\t{}\t{}\t{}\t{}\t{best}\t{best_value}\t{best_black}\t{second}\t{second_value}\t{}",
            root.id,
            root.own,
            root.key,
            root.ply,
            tree.nodes[&root.key].kind.as_str(),
            result.marks.len()
        )?;
        for m in &result.marks {
            writeln!(
                marks,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                root.id,
                m.key,
                m.ply,
                side(&m.key),
                m.mv,
                m.cp,
                m.best,
                m.best_value,
                m.cp - m.best_value
            )?;
        }
    }
    values.flush()?;
    write_atomic(&out.join("roots.tsv"), &roots)?;
    write_atomic(&out.join("marks.tsv"), &marks)?;
    let mut table = String::from(
        "key\tply\tside\tmain\tmain_source\tmain_tier\tmain_tree_value\ttree_best\ttree_value\troot\n",
    );
    for c in conflicts {
        writeln!(table, "{c}\t{}", meta.roots[c.tree].id)?;
    }
    write_atomic(&out.join("conflicts.tsv"), &table)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::book_tree::master_table::{positions_from_rows, test_row};

    #[test]
    fn master_conditions_check_exit_names_and_propagated_sources() {
        let root = "4k4/9/9/9/9/9/9/9/4K4 b -";
        let child = crate::book_tree::model::child(root, "5i4i").unwrap();
        let row = |key: &str, mv: &str, cond: &str| {
            test_row(&[
                ("key", key.into()),
                ("move", mv.into()),
                ("role", "main".into()),
                ("evals", "10".into()),
                ("cond", cond.into()),
                ("source", "read".into()),
            ])
        };
        for (parent_cond, child_cond, expected) in [
            ("ledger", "unknown", vec![]),
            ("ledger", "other", vec![]),
            ("ledger", "ledger", vec![root.to_owned(), child.clone()]),
            ("unknown", "ledger", vec![child.clone()]),
        ] {
            let rows = vec![
                row(root, "5i4i", parent_cond),
                row(&child, "5a4a", child_cond),
            ];
            let master =
                positions_from_rows(rows.iter().cloned().enumerate().map(|(i, r)| Ok((i + 2, r))))
                    .unwrap();
            let (_, _, book) = audit(&master, DrawValues::default(), 40, Fill::Off, None).unwrap();
            assert_eq!(
                master_conditions(&rows, &master.0, &book, "ledger"),
                expected.into_iter().collect()
            );
            assert!(master_conditions(&rows, &master.0, &book, "unknown").is_empty());
        }
    }

    #[test]
    fn backing_nodes_follow_selected_line_and_preserve_own_readings() {
        // 0 → 1 → 3 が最善手順。別の子 2 の大きい上限を選択した手順へ混ぜない。
        let selected = [Some(1), Some(3), None, None, Some(0)];
        let own = [0, 0, 900, 100, 2000];
        let fallback = [900, 100, 900, 100, 2000];
        assert_eq!(
            super::selected_backing_nodes(&selected, &own, &fallback),
            [100, 100, 900, 100, 2000]
        );
    }

    #[test]
    fn backing_nodes_fall_back_when_selected_subtree_has_no_reading() {
        let selected = [Some(1), None, None, Some(4), Some(3)];
        let own = [0, 0, 700, 0, 0];
        let fallback = [700, 0, 700, 700, 700];
        assert_eq!(
            super::selected_backing_nodes(&selected, &own, &fallback),
            [700, 0, 700, 700, 700]
        );
    }
}
