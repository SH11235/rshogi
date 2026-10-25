//! 自分の定跡の手と、相手の合法手で定跡に戻る手でできる千日手の循環を見つけ、自分の手番の手だけを
//! 外して (指さない手 = record にして) 循環を無くす手の組を、決まった規則で選ぶ。

use super::audit::{MasterPosition, evaluate, read_master};
use super::register::{format_master, parse_master};
use super::reply_graph::{
    BookHashes, HIRATE_KEY, Preset, ReplyGraph, SideArg, build, nontrivial_sccs, selected_moves,
};
use crate::book_backprop::DrawValues;
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(super) struct CyclesArgs {
    /// 定跡の元データの表 (TSV)
    #[arg(long)]
    master: PathBuf,
    /// 外す手の一覧 (TSV) の出力先
    #[arg(long)]
    table: PathBuf,
    /// 循環を塞ぐ側。後手の千日手は大会で 0.6 点なので、既定は先手だけ
    #[arg(long, value_enum, default_value_t = SideArg::B)]
    side: SideArg,
    /// 手の値の伝播に使う、先手を自分として解くときの千日手の値
    #[arg(long, default_value_t = -70, allow_hyphen_values = true)]
    draw_value_black: i32,
    /// 手の値の伝播に使う、後手を自分として解くときの千日手の値
    #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
    draw_value_white: i32,
    /// 元データを書き換える (外す手を record に、main を替える局面の替わりの手を main にする)。
    /// 省くと一覧と要約だけを出す
    #[arg(long)]
    apply: bool,
    /// 書き換えるとき、外す手と替わりの main の行の note の末尾に付ける文
    #[arg(long, default_value = "千日手の循環を塞ぐため指さない手にした")]
    note: String,
}

/// 1 つの側で外す手と、main を替える局面。
#[derive(Debug, Default, PartialEq)]
pub(super) struct SidePlan {
    /// (key, 手の番号)。key 順・手の番号順。
    pub removed: Vec<(String, usize)>,
    /// main を外した局面の、替わりの main の手の番号。
    pub new_main: BTreeMap<String, usize>,
    /// 自分の手が 1 手ずつしか無く、塞げない循環 (局面の key の列)。
    pub unbreakable: Vec<Vec<String>>,
}

/// 循環の数と、循環に入る局面の数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct CycleCount {
    pub components: usize,
    pub positions: usize,
}

impl CycleCount {
    fn of(components: &[Vec<usize>]) -> Self {
        Self {
            components: components.len(),
            positions: components.iter().map(Vec::len).sum(),
        }
    }
}

pub(super) fn run(args: CyclesArgs) -> Result<()> {
    run_from(args, HIRATE_KEY)
}

fn run_from(args: CyclesArgs, start: &str) -> Result<()> {
    ensure!(!args.note.contains(['\t', '\r', '\n']), "--note にタブや改行は使えません");
    let parsed = read_master(&args.master)?;
    let positions = &parsed.0;
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let work = tempfile::tempdir().context("一時ディレクトリを作れません")?;
    let values = move_values(positions, draws, work.path())?;

    let hashes = BookHashes::new(positions)?;
    let mut plans = Vec::new();
    for &side in args.side.sides() {
        let before = count_cycles(positions, &hashes, side, start)?;
        let plan = plan_side(positions, &hashes, &values, side, start)?;
        plans.push((side, before, plan));
    }
    let after_positions = apply_plans(positions, plans.iter().map(|(_, _, p)| p));
    let mut table =
        String::from("side\tkey\tply\tmove\trole\tvalue\teval\tnew_main\tnew_main_value\n");
    for (side, before, plan) in &plans {
        for (key, i) in &plan.removed {
            let position = &positions[key];
            let m = &position.moves[*i];
            let role = if *i == 0 { "main" } else { "alt" };
            let value = |i: usize| values[key][i].map_or_else(String::new, |v| v.to_string());
            let (new_main, new_main_value) = match plan.new_main.get(key) {
                Some(&j) if *i == 0 => (position.moves[j].mv.clone(), value(j)),
                _ => (String::new(), String::new()),
            };
            let eval = m.eval.map_or_else(String::new, |v| v.to_string());
            writeln!(
                table,
                "{side}\t{key}\t{}\t{}\t{role}\t{}\t{eval}\t{new_main}\t{new_main_value}",
                position.ply,
                m.mv,
                value(*i)
            )?;
        }
        let after = count_cycles(&after_positions, &hashes, side, start)?;
        print!("{}", format_summary(side, before, &after, plan));
    }
    write_atomic(&args.table, &table)?;
    if args.apply {
        let text = std::fs::read_to_string(&args.master)
            .with_context(|| format!("{} を読めません", args.master.display()))?;
        let mut rows = parse_master(&text)?;
        let changed =
            rewrite_rows(&mut rows, positions, plans.iter().map(|(_, _, p)| p), &args.note)?;
        write_atomic(&args.master, &format_master(&rows))?;
        println!("書き換えました: 行 {changed}");
    } else {
        println!("確認だけ (--apply なし)");
    }
    Ok(())
}

/// 手ごとの伝播後の値 (手番の側から)。値の分からない手は `None`。
fn move_values(
    positions: &BTreeMap<String, MasterPosition>,
    draws: DrawValues,
    work: &std::path::Path,
) -> Result<HashMap<String, Vec<Option<i32>>>> {
    let evaluated = evaluate(positions, draws, work)?;
    Ok(positions
        .keys()
        .zip(evaluated.moves)
        .map(|(key, moves)| (key.clone(), moves.into_iter().map(|m| m.map(|m| m.value)).collect()))
        .collect())
}

/// default (main だけ) と random (main と alt) の循環。
pub(super) fn count_cycles(
    positions: &BTreeMap<String, MasterPosition>,
    hashes: &BookHashes,
    side: &str,
    start: &str,
) -> Result<[CycleCount; 2]> {
    let mut counts = [CycleCount::default(); 2];
    for (count, preset) in counts.iter_mut().zip([Preset::Default, Preset::Random]) {
        let graph = build(positions, hashes, side, start, |_, p| selected_moves(p, preset, None))?;
        *count = CycleCount::of(&graph.cycles(|_, _| true));
    }
    Ok(counts)
}

/// 循環の中の手を 1 手ずつ厳密に採点する上限 (候補の手の数 × 循環の中の辺の数)。超える大きい循環は
/// 深さ優先探索で塞ぐ。
const EXACT_SCORING_LIMIT: usize = 20_000_000;

/// 外す手の組を決める。main と alt の全部で作ったグラフで塞ぐので、main だけで指すときの循環も無くなる
/// (main だけのグラフは、main を替えた後も含めて、その部分グラフになる)。
///
/// 1. 循環 (強連結成分) ごとに、その中の自分の手のうち外せる手 (その局面に 2 手以上残る手) を外す。
///    小さい循環では、外した後に循環に残る局面の数の少ない手を 1 手選ぶ。大きい循環では、残したい手
///    (main、値の高い手) から先に辿る深さ優先探索で、循環を閉じるたびにその循環の中で外したい手を外す。
///    どちらも、外したい手の順は alt が先、値の低い手 (値の分からない手が先)、key 順、手の順。
///    外した後に循環の中に残る循環について、循環が無くなるか、外せる手が無くなるまで繰り返す。
/// 2. 外した手を、main・値の高い順に 1 手ずつ戻し、戻しても循環が出ない手は戻したままにする。
/// 3. main を外した局面は、残る手のうち値の一番高い手 (同じなら元データの順) を main にする。
///
/// 計算量: 1 手 (大きい循環では 1 回の探索) ごとに、その循環の中だけ強連結成分分解をし直す。小さい循環は
/// 1 手外すごとに候補の手の数 × 循環の大きさ、大きい循環と 2. は 1 回ごとに循環の大きさに比例する。
pub(super) fn plan_side(
    positions: &BTreeMap<String, MasterPosition>,
    hashes: &BookHashes,
    values: &HashMap<String, Vec<Option<i32>>>,
    side: &str,
    start: &str,
) -> Result<SidePlan> {
    plan_side_with(positions, hashes, values, (side, start), EXACT_SCORING_LIMIT)
}

/// [`plan_side`] の本体。`exact_limit` を超える循環は深さ優先探索で塞ぐ。
fn plan_side_with(
    positions: &BTreeMap<String, MasterPosition>,
    hashes: &BookHashes,
    values: &HashMap<String, Vec<Option<i32>>>,
    (side, start): (&str, &str),
    exact_limit: usize,
) -> Result<SidePlan> {
    let graph =
        build(positions, hashes, side, start, |_, p| selected_moves(p, Preset::Random, None))?;
    let value = |v: usize, i: usize| -> Option<i32> {
        let book_move = graph.edges[v][i].book_move?;
        values.get(&graph.keys[v]).and_then(|m| m[book_move])
    };
    let is_main = |v: usize, i: usize| graph.edges[v][i].book_move == Some(0);
    // 外したい順 (小さいほど先に外す)。
    let removal_order = |(v, i): (usize, usize)| {
        (
            is_main(v, i),
            value(v, i),
            graph.keys[v].as_str(),
            graph.edges[v][i].mv.as_str(),
        )
    };
    let original: HashMap<usize, usize> = graph
        .cycles(|_, _| true)
        .into_iter()
        .enumerate()
        .flat_map(|(c, nodes)| nodes.into_iter().map(move |v| (v, c)))
        .collect();
    let mut active: Vec<Vec<bool>> = graph.edges.iter().map(|e| vec![true; e.len()]).collect();
    let mut removed: Vec<(usize, usize)> = Vec::new();
    let mut queue: Vec<Vec<usize>> = graph.cycles(|_, _| true);
    while let Some(component) = queue.pop() {
        let candidates = removable(&graph, &active, &component);
        if candidates.is_empty() {
            continue;
        }
        let inner_edges: usize = component.iter().map(|&v| graph.edges[v].len()).sum();
        let mut chosen = if candidates.len().saturating_mul(inner_edges) <= exact_limit {
            vec![choose_removal(
                &graph,
                &active,
                &component,
                &candidates,
                &removal_order,
            )]
        } else {
            dfs_removals(&graph, &mut active, &component, &removal_order)
        };
        if chosen.is_empty() {
            chosen.push(*candidates.iter().min_by_key(|&&e| removal_order(e)).expect("候補"));
        }
        for (v, i) in chosen {
            active[v][i] = false;
            removed.push((v, i));
        }
        queue.extend(sub_cycles(&graph, &active, &component));
    }

    removed.sort_by(|&a, &b| {
        (
            !is_main(a.0, a.1),
            std::cmp::Reverse(value(a.0, a.1)),
            removal_order(a).2,
            removal_order(a).3,
        )
            .cmp(&(
                !is_main(b.0, b.1),
                std::cmp::Reverse(value(b.0, b.1)),
                removal_order(b).2,
                removal_order(b).3,
            ))
    });
    let mut reached = reachable(&graph, &active);
    removed.retain(|&(v, i)| {
        active[v][i] = true;
        let to = graph.edges[v][i].to;
        // 根から届かない局面の手は、戻しても届く範囲の循環を作らない (後で届くようになる手の側で調べる)。
        let added = if !reached[v] {
            Some(Vec::new())
        } else if reaches(&graph, &active, &original, to, v) {
            None
        } else {
            newly_reached(&graph, &active, &reached, to)
        };
        match added {
            Some(nodes) => {
                for u in nodes {
                    reached[u] = true;
                }
                false
            }
            None => {
                active[v][i] = false;
                true
            }
        }
    });

    let mut plan = SidePlan::default();
    for &(v, i) in &removed {
        let book_move = graph.edges[v][i].book_move.context("内部エラー: 相手の手を外しました")?;
        plan.removed.push((graph.keys[v].clone(), book_move));
        if book_move == 0 {
            let best = (0..graph.edges[v].len())
                .filter(|&j| active[v][j])
                .max_by(|&a, &b| {
                    value(v, a)
                        .cmp(&value(v, b))
                        .then_with(|| graph.edges[v][b].book_move.cmp(&graph.edges[v][a].book_move))
                })
                .context("内部エラー: 手の残らない局面です")?;
            let new_main = graph.edges[v][best].book_move.context("内部エラー: 相手の手です")?;
            plan.new_main.insert(graph.keys[v].clone(), new_main);
        }
    }
    plan.removed.sort();
    plan.unbreakable = graph
        .cycles(|v, i| active[v][i])
        .into_iter()
        .map(|c| c.into_iter().map(|v| graph.keys[v].clone()).collect())
        .collect();
    Ok(plan)
}

/// `component` の中の局面だけでできる循環 (今の辺で)。
fn sub_cycles(graph: &ReplyGraph, active: &[Vec<bool>], component: &[usize]) -> Vec<Vec<usize>> {
    let local: HashMap<usize, u32> =
        component.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();
    let adjacency: Vec<Vec<u32>> = component
        .iter()
        .map(|&v| {
            (0..graph.edges[v].len())
                .filter(|&i| active[v][i])
                .filter_map(|i| local.get(&graph.edges[v][i].to).copied())
                .collect()
        })
        .collect();
    nontrivial_sccs(&adjacency)
        .into_iter()
        .map(|c| c.into_iter().map(|i| component[i as usize]).collect())
        .collect()
}

/// 循環の中で外せる自分の手 (循環の中の局面へ行き、その局面に 2 手以上残る手)。
fn removable(graph: &ReplyGraph, active: &[Vec<bool>], component: &[usize]) -> Vec<(usize, usize)> {
    let inside: HashSet<usize> = component.iter().copied().collect();
    component
        .iter()
        .filter(|&&v| graph.own[v] && active[v].iter().filter(|&&a| a).count() >= 2)
        .flat_map(|&v| (0..graph.edges[v].len()).map(move |i| (v, i)))
        .filter(|&(v, i)| active[v][i] && inside.contains(&graph.edges[v][i].to))
        .collect()
}

/// 外した後に循環に残る局面の数の一番少ない手 (同じなら外したい順の先)。
fn choose_removal<K: Ord>(
    graph: &ReplyGraph,
    active: &[Vec<bool>],
    component: &[usize],
    candidates: &[(usize, usize)],
    removal_order: &impl Fn((usize, usize)) -> K,
) -> (usize, usize) {
    let local: HashMap<usize, u32> =
        component.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();
    let inner = |v: usize, i: usize| active[v][i] && local.contains_key(&graph.edges[v][i].to);
    *candidates
        .iter()
        .min_by_key(|&&(v, i)| {
            let adjacency: Vec<Vec<u32>> = component
                .iter()
                .map(|&u| {
                    (0..graph.edges[u].len())
                        .filter(|&j| inner(u, j) && (u, j) != (v, i))
                        .map(|j| local[&graph.edges[u][j].to])
                        .collect()
                })
                .collect();
            let cyclic: usize = nontrivial_sccs(&adjacency).iter().map(Vec::len).sum();
            (cyclic, removal_order((v, i)))
        })
        .expect("候補がある")
}

/// 残したい手から先に辿る深さ優先探索で、循環を閉じる辺を見つけるたびに、その循環の辺のうち外せる自分の手で
/// 外したい順の先の手を外す。外した手を返す (`active` も更新する)。
fn dfs_removals<K: Ord>(
    graph: &ReplyGraph,
    active: &mut [Vec<bool>],
    component: &[usize],
    removal_order: &impl Fn((usize, usize)) -> K,
) -> Vec<(usize, usize)> {
    const UNSEEN: usize = usize::MAX;
    const DONE: usize = usize::MAX - 1;
    let local: HashMap<usize, usize> = component.iter().enumerate().map(|(i, &v)| (v, i)).collect();
    // 局面ごとの状態。探索中ならスタックの深さ。
    let mut state = vec![UNSEEN; component.len()];
    let mut removed = Vec::new();
    for &root in component {
        if state[local[&root]] != UNSEEN {
            continue;
        }
        let mut stack: Vec<Frame> = Vec::new();
        let order = |v: usize, active: &[Vec<bool>]| -> Vec<usize> {
            let mut edges: Vec<usize> = (0..graph.edges[v].len())
                .filter(|&i| active[v][i] && local.contains_key(&graph.edges[v][i].to))
                .collect();
            edges.sort_by_key(|&i| std::cmp::Reverse(removal_order((v, i))));
            edges
        };
        state[local[&root]] = 0;
        stack.push((root, order(root, active), 0, None));
        while let Some(frame) = stack.last_mut() {
            let v = frame.0;
            let Some(&i) = frame.1.get(frame.2) else {
                state[local[&v]] = DONE;
                stack.pop();
                continue;
            };
            frame.2 += 1;
            if !active[v][i] {
                continue;
            }
            let w = graph.edges[v][i].to;
            match state[local[&w]] {
                UNSEEN => {
                    state[local[&w]] = stack.len();
                    let edges = order(w, active);
                    stack.push((w, edges, 0, Some((v, i))));
                }
                DONE => {}
                depth => {
                    let cycle = stack[depth + 1..].iter().filter_map(|f| f.3).chain([(v, i)]);
                    let target = cycle
                        .filter(|&(u, j)| {
                            graph.own[u]
                                && active[u][j]
                                && active[u].iter().filter(|&&a| a).count() >= 2
                        })
                        .min_by_key(|&e| removal_order(e));
                    if let Some((u, j)) = target {
                        active[u][j] = false;
                        removed.push((u, j));
                    }
                }
            }
        }
    }
    // 呼び出し側で外したことにするので、ここでは元に戻して返す。
    for &(u, j) in &removed {
        active[u][j] = true;
    }
    removed
}

/// 根から今の辺で届く局面。
fn reachable(graph: &ReplyGraph, active: &[Vec<bool>]) -> Vec<bool> {
    let mut reached = vec![false; graph.len()];
    reached[0] = true;
    let mut stack = vec![0];
    while let Some(v) = stack.pop() {
        for (i, e) in graph.edges[v].iter().enumerate() {
            if active[v][i] && !reached[e.to] {
                reached[e.to] = true;
                stack.push(e.to);
            }
        }
    }
    reached
}

/// `from` を通って新しく根から届く局面。その中に循環があれば `None`。
fn newly_reached(
    graph: &ReplyGraph,
    active: &[Vec<bool>],
    reached: &[bool],
    from: usize,
) -> Option<Vec<usize>> {
    if reached[from] {
        return Some(Vec::new());
    }
    let mut local: HashMap<usize, u32> = HashMap::from([(from, 0)]);
    let mut nodes = vec![from];
    let mut next = 0;
    while let Some(&v) = nodes.get(next) {
        next += 1;
        for (i, e) in graph.edges[v].iter().enumerate() {
            if active[v][i] && !reached[e.to] && !local.contains_key(&e.to) {
                local.insert(e.to, nodes.len() as u32);
                nodes.push(e.to);
            }
        }
    }
    let adjacency: Vec<Vec<u32>> = nodes
        .iter()
        .map(|&v| {
            (0..graph.edges[v].len())
                .filter(|&i| active[v][i])
                .filter_map(|i| local.get(&graph.edges[v][i].to).copied())
                .collect()
        })
        .collect();
    nontrivial_sccs(&adjacency).is_empty().then_some(nodes)
}

/// 深さ優先探索の 1 段: (局面, 辿る辺の順, 次の位置, その局面に来た辺)。
type Frame = (usize, Vec<usize>, usize, Option<(usize, usize)>);

/// `from` から `to` に、今の辺で行けるか。どちらも元のグラフの同じ循環の中の局面だけを通る。
fn reaches(
    graph: &ReplyGraph,
    active: &[Vec<bool>],
    original: &HashMap<usize, usize>,
    from: usize,
    to: usize,
) -> bool {
    let Some(&component) = original.get(&to) else {
        return false;
    };
    if original.get(&from) != Some(&component) {
        return false;
    }
    let mut seen = HashSet::from([from]);
    let mut stack = vec![from];
    while let Some(v) = stack.pop() {
        if v == to {
            return true;
        }
        for (i, e) in graph.edges[v].iter().enumerate() {
            if active[v][i] && original.get(&e.to) == Some(&component) && seen.insert(e.to) {
                stack.push(e.to);
            }
        }
    }
    false
}

/// 外す手を除き、main を替えた元データ (局面の手の並びだけ)。
pub(super) fn apply_plans<'a>(
    positions: &BTreeMap<String, MasterPosition>,
    plans: impl Iterator<Item = &'a SidePlan>,
) -> BTreeMap<String, MasterPosition> {
    let mut after = positions.clone();
    for plan in plans {
        let mut removed: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (key, i) in &plan.removed {
            removed.entry(key).or_default().push(*i);
        }
        for (key, indices) in removed {
            let original = &positions[key];
            let position = after.get_mut(key).expect("元データの局面");
            let mut moves: Vec<_> =
                (0..original.moves.len()).filter(|i| !indices.contains(i)).collect();
            if let Some(&j) = plan.new_main.get(key) {
                moves.retain(|&i| i != j);
                moves.insert(0, j);
            }
            position.moves = moves.into_iter().map(|i| original.moves[i].clone()).collect();
        }
    }
    after
}

/// 元データの行を書き換え、書き換えた行の数を返す。
fn rewrite_rows<'a>(
    rows: &mut [super::register::Row],
    positions: &BTreeMap<String, MasterPosition>,
    plans: impl Iterator<Item = &'a SidePlan>,
    note: &str,
) -> Result<usize> {
    let mut roles: HashMap<(String, String), &str> = HashMap::new();
    for plan in plans {
        for (key, i) in &plan.removed {
            roles.insert((key.clone(), positions[key].moves[*i].mv.clone()), "record");
        }
        for (key, &j) in &plan.new_main {
            roles.insert((key.clone(), positions[key].moves[j].mv.clone()), "main");
        }
    }
    let mut changed = 0;
    for row in rows.iter_mut() {
        let pair = (row.get("key").to_owned(), row.get("move").to_owned());
        let Some(&role) = roles.get(&pair) else {
            continue;
        };
        ensure!(
            row.get("role") != "record",
            "内部エラー: record の行です: {} {}",
            pair.0,
            pair.1
        );
        let suffix = if role == "main" {
            format!("。{note}: main を外したので残る手で値の一番高い手を main にした")
        } else {
            format!("。{note}")
        };
        let text = format!("{}{suffix}", row.get("note"));
        row.set("role", role.to_owned());
        row.set("note", text);
        changed += 1;
    }
    ensure!(changed == roles.len(), "内部エラー: 書き換える行が見つかりません");
    Ok(changed)
}

fn format_summary(
    side: &str,
    before: &[CycleCount; 2],
    after: &[CycleCount; 2],
    plan: &SidePlan,
) -> String {
    let show = |c: &CycleCount| format!("{} (局面 {})", c.components, c.positions);
    let mut text = String::new();
    let mut out = |s: String| {
        text.push_str(&s);
        text.push('\n');
    };
    out(format!(
        "{side}: 循環 (自明でない強連結成分) default {} / random {}",
        show(&before[0]),
        show(&before[1])
    ));
    out(format!(
        "{side}: 外す手 {} (main {} / alt {})、main を替える局面 {}",
        plan.removed.len(),
        plan.removed.iter().filter(|(_, i)| *i == 0).count(),
        plan.removed.iter().filter(|(_, i)| *i != 0).count(),
        plan.new_main.len()
    ));
    out(format!(
        "{side}: 外した後の循環 default {} / random {}",
        show(&after[0]),
        show(&after[1])
    ));
    for cycle in &plan.unbreakable {
        out(format!("{side}: 塞げない循環 (自分の手が 1 手ずつ): {}", cycle.join(" | ")));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::super::reply_graph::fixture::{ROOT, after, master_text, positions};
    use super::*;

    /// 根から 5i4h、相手 5a5b で C、C から 4h5i、相手 5b5a で根に戻る循環を作る。
    fn solve(rows: &[(&str, &str, &str, &str)]) -> (BTreeMap<String, MasterPosition>, SidePlan) {
        solve_with(rows, EXACT_SCORING_LIMIT)
    }

    fn solve_with(
        rows: &[(&str, &str, &str, &str)],
        exact_limit: usize,
    ) -> (BTreeMap<String, MasterPosition>, SidePlan) {
        let p = positions(rows);
        let work = tempfile::tempdir().unwrap();
        let values = move_values(&p, DrawValues::default(), work.path()).unwrap();
        let hashes = BookHashes::new(&p).unwrap();
        let plan = plan_side_with(&p, &hashes, &values, ("b", ROOT), exact_limit).unwrap();
        (p, plan)
    }

    fn c() -> String {
        after(ROOT, &["5i4h", "5a5b"])
    }

    #[test]
    fn removes_main_of_the_only_position_with_two_moves_and_promotes_best_remaining() {
        let c = c();
        let rows = [
            (ROOT, "5i4h", "main", "10"),
            (c.as_str(), "4h5i", "main", "30"),
            (c.as_str(), "4h3h", "alt", "20"),
            (c.as_str(), "4h4i", "alt", "25"),
        ];
        let (p, plan) = solve(&rows);
        // 根は 1 手しか無いので外せない。C の main を外し、残る手で値の高い 4h4i を main にする。
        assert_eq!(plan.removed, [(c.clone(), 0)]);
        assert_eq!(plan.new_main, BTreeMap::from([(c.clone(), 2)]));
        assert!(plan.unbreakable.is_empty());
        let after_positions = apply_plans(&p, [&plan].into_iter());
        assert_eq!(after_positions[&c].moves[0].mv, "4h4i");
        let none = CycleCount::default();
        assert_eq!(
            count_cycles(&p, &BookHashes::new(&p).unwrap(), "b", ROOT).unwrap(),
            [CycleCount {
                components: 1,
                positions: 4
            }; 2]
        );
        assert_eq!(
            count_cycles(&after_positions, &BookHashes::new(&p).unwrap(), "b", ROOT).unwrap(),
            [none; 2]
        );
    }

    #[test]
    fn prefers_alt_then_lower_value() {
        let c = c();
        // 根の alt 5i4h と C の main 4h5i のどちらでも塞げる。alt を外す。
        let (_, plan) = solve(&[
            (ROOT, "5i6h", "main", "0"),
            (ROOT, "5i4h", "alt", "50"),
            (c.as_str(), "4h5i", "main", "10"),
            (c.as_str(), "4h3h", "alt", "0"),
        ]);
        assert_eq!(plan.removed, [(ROOT.to_owned(), 1)]);
        assert!(plan.new_main.is_empty());
        // どちらも alt なら値の低い手を外す。
        let (_, plan) = solve(&[
            (ROOT, "5i6h", "main", "0"),
            (ROOT, "5i4h", "alt", "50"),
            (c.as_str(), "4h3h", "main", "0"),
            (c.as_str(), "4h5i", "alt", "40"),
        ]);
        assert_eq!(plan.removed, [(c.clone(), 1)]);
    }

    #[test]
    fn depth_first_path_breaks_cycles_and_keeps_one_move_per_position() {
        // 根と C の両方に 3 手ずつあり、根の 5i4h・5i5h と C の 4h5i・4h4i が循環に入る。
        let c = c();
        let rows = [
            (ROOT, "5i4h", "main", "10"),
            (ROOT, "5i5h", "alt", "30"),
            (ROOT, "5i6h", "alt", "0"),
            (c.as_str(), "4h5i", "main", "40"),
            (c.as_str(), "4h4i", "alt", "20"),
            (c.as_str(), "4h3h", "alt", "0"),
            (&after(ROOT, &["5i5h", "5a5b"]), "5h5i", "main", "0"),
            (&after(ROOT, &["5i5h", "5a5b"]), "5h4h", "alt", "0"),
        ];
        for limit in [0, EXACT_SCORING_LIMIT] {
            let (p, plan) = solve_with(&rows, limit);
            assert!(!plan.removed.is_empty());
            assert!(plan.unbreakable.is_empty());
            let after_positions = apply_plans(&p, [&plan].into_iter());
            assert!(after_positions.values().all(|m| !m.moves.is_empty()));
            let hashes = BookHashes::new(&p).unwrap();
            assert_eq!(
                count_cycles(&after_positions, &hashes, "b", ROOT).unwrap(),
                [CycleCount::default(); 2],
                "limit {limit}: {plan:?}"
            );
        }
    }

    #[test]
    fn reports_cycle_without_removable_move() {
        let c = c();
        let (_, plan) = solve(&[
            (ROOT, "5i4h", "main", "10"),
            (c.as_str(), "4h5i", "main", "30"),
        ]);
        assert!(plan.removed.is_empty());
        assert_eq!(plan.unbreakable.len(), 1);
        assert_eq!(plan.unbreakable[0].len(), 4);
    }

    #[test]
    fn apply_rewrites_roles_and_notes() {
        let c = c();
        let rows = [
            (ROOT, "5i4h", "main", "10"),
            (c.as_str(), "4h5i", "main", "30"),
            (c.as_str(), "4h3h", "alt", "20"),
        ];
        let dir = tempfile::tempdir().unwrap();
        let master = dir.path().join("moves.tsv");
        std::fs::write(&master, master_text(&rows)).unwrap();
        let args = CyclesArgs {
            master: master.clone(),
            table: dir.path().join("cycles.tsv"),
            side: SideArg::B,
            draw_value_black: 0,
            draw_value_white: 0,
            apply: true,
            note: "塞いだ".into(),
        };
        run_from(args, ROOT).unwrap();
        let rows = parse_master(&std::fs::read_to_string(&master).unwrap()).unwrap();
        let roles: Vec<(&str, &str, &str)> =
            rows.iter().map(|r| (r.get("move"), r.get("role"), r.get("note"))).collect();
        assert_eq!(
            roles,
            [
                ("5i4h", "main", "n"),
                ("4h5i", "record", "n。塞いだ"),
                (
                    "4h3h",
                    "main",
                    "n。塞いだ: main を外したので残る手で値の一番高い手を main にした"
                ),
            ]
        );
        let table = std::fs::read_to_string(dir.path().join("cycles.tsv")).unwrap();
        assert_eq!(
            table.lines().nth(1).unwrap(),
            format!("b\t{c}\t1\t4h5i\tmain\t30\t30\t4h3h\t20")
        );
    }
}
