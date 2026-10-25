//! 自分の定跡の手と、相手の合法手で定跡に戻る手でできる千日手の循環を見つけ、自分の手番の手だけを
//! 外して (指さない手 = record にして) 循環を無くす手の組を、決まった規則で選ぶ。

use super::audit::{MasterPosition, evaluate, read_master, reject_same_file};
use super::register::{COLUMNS, format_master, parse_master, validate};
use super::reply_graph::{
    BookIndex, HIRATE_KEY, Preset, ReplyGraph, SideArg, build, nontrivial_sccs, selected_moves,
    thread_pool,
};
use crate::book_backprop::DrawValues;
use crate::common::io::write_atomic;
use anyhow::{Context, Result, ensure};
use clap::Args;
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt::Write as _;
use std::io::BufRead as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

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
    /// スレッド数 (0 = 全コア)。結果はスレッド数によらない
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

/// 1 つの側で外す手と、main を替える局面。
#[derive(Debug, Default, PartialEq)]
pub(super) struct SidePlan {
    /// (key, 手の番号)。key 順・手の番号順。
    pub removed: Vec<(String, usize)>,
    /// main を外した局面の、替わりの main の手の番号。
    pub new_main: BTreeMap<String, usize>,
    /// 外した後も根から届く循環 (局面の key の列)。根から、自分がどの手を残しても相手が循環に持ち込める
    /// ときだけ残る。
    pub unbreakable: Vec<Vec<String>>,
    /// 採った外し方 (`plan_side` の A / B)。
    pub method: &'static str,
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

/// 1 つの側の結果。循環の数は default (main だけ)・random (main と alt) の順。
struct SideResult {
    side: &'static str,
    before: [CycleCount; 2],
    plan: SidePlan,
    after: [CycleCount; 2],
}

pub(super) fn run(args: CyclesArgs) -> Result<()> {
    thread_pool(args.threads)?.install(|| run_from(args, HIRATE_KEY))
}

fn run_from(args: CyclesArgs, start: &str) -> Result<()> {
    ensure!(!args.note.contains(['\t', '\r', '\n']), "--note にタブや改行は使えません");
    reject_same_file(&args.table, &[("--master", &args.master)])?;
    let parsed = read_master(&args.master)?;
    let positions = &parsed.0;
    let curated = curated_moves(&args.master)?;
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let values = evaluate(positions, draws)?;
    let index = BookIndex::new(positions)?;

    // 先手と後手のグラフは互いの手に左右されない (相手の手番では合法手を全部見る) ので並べて解く。
    let results: Vec<SideResult> = args
        .side
        .sides()
        .par_iter()
        .map(|&side| solve_side(&index, &values, &curated, (side, start)))
        .collect::<Result<_>>()?;

    let mut table =
        String::from("side\tkey\tply\tmove\trole\tvalue\teval\tnew_main\tnew_main_value\n");
    for r in &results {
        let plan = &r.plan;
        for (key, i) in &plan.removed {
            let position = &positions[key];
            let master = index.find(key).context("内部エラー: 元データに無い局面です")?;
            let m = &position.moves[*i];
            let role = if *i == 0 { "main" } else { "alt" };
            let value = |i: usize| values[master][i].map_or_else(String::new, |v| v.to_string());
            let (new_main, new_main_value) = match plan.new_main.get(key) {
                Some(&j) if *i == 0 => (position.moves[j].mv.clone(), value(j)),
                _ => (String::new(), String::new()),
            };
            let eval = m.eval.map_or_else(String::new, |v| v.to_string());
            writeln!(
                table,
                "{}\t{key}\t{}\t{}\t{role}\t{}\t{eval}\t{new_main}\t{new_main_value}",
                r.side,
                position.ply,
                m.mv,
                value(*i)
            )?;
        }
        print!("{}", format_summary(r));
    }
    write_atomic(&args.table, &table)?;
    if args.apply {
        // 行は書き換えるときだけ読む (元データを丸ごと 2 つ持たないため)。
        let text = std::fs::read_to_string(&args.master)
            .with_context(|| format!("{} を読めません", args.master.display()))?;
        let mut rows = parse_master(&text)?;
        let changed =
            rewrite_rows(&mut rows, positions, results.iter().map(|r| &r.plan), &args.note)?;
        validate(&rows)?;
        write_atomic(&args.master, &format_master(&rows))?;
        println!("書き換えました: 行 {changed}");
    } else {
        println!("確認だけ (--apply なし)");
    }
    Ok(())
}

/// 1 つの側を解く。外した後も同じグラフの辺を絞って数える。
fn solve_side(
    index: &BookIndex,
    values: &[Vec<Option<i32>>],
    curated: &HashSet<(String, String)>,
    (side, start): (&'static str, &str),
) -> Result<SideResult> {
    let graph = build(index, side, start, |_, p| selected_moves(p, Preset::Random, None))?;
    let before = count_on(&graph, |_| 0, |_, _| false);
    let mut plan = plan_side(&graph, values)?;
    plan.new_main = new_mains(&graph, index, values, &plan.removed, curated)?;
    // 同じグラフで、外した手を除き main を替えて数え直す。自分の手番の辺を減らすだけで相手の手番の辺は
    // 変わらないので、外した後の元データでグラフを作り直して数えるのと同じになる。
    let removed_at = |m: usize, i: usize| {
        let key = index.key(m);
        plan.removed.binary_search_by(|(k, j)| (k.as_str(), *j).cmp(&(key, i))).is_ok()
    };
    let after =
        count_on(&graph, |m| plan.new_main.get(index.key(m)).copied().unwrap_or(0), removed_at);
    Ok(SideResult {
        side,
        before,
        plan,
        after,
    })
}

/// random のグラフで、default (自分の手は main だけ) と random の循環を数える。`main` は元データの局面の番号
/// から main の手の番号を返し (書き出しと同じく、main が非合法ならその局面に default の辺は無い)、
/// `removed(局面の番号, 手の番号)` が真の自分の手は使わない。
fn count_on(
    graph: &ReplyGraph,
    main: impl Fn(usize) -> usize,
    removed: impl Fn(usize, usize) -> bool,
) -> [CycleCount; 2] {
    let book = |v: usize, i: usize| graph.master(v).zip(graph.edges(v)[i].book_move());
    let random =
        |v: usize, i: usize| !graph.own[v] || book(v, i).is_some_and(|(m, j)| !removed(m, j));
    let default =
        |v: usize, i: usize| !graph.own[v] || book(v, i).is_some_and(|(m, j)| j == main(m));
    [
        CycleCount::of(&graph.cycles(default)),
        CycleCount::of(&graph.cycles(random)),
    ]
}

/// 循環の中の手を 1 手ずつ厳密に採点する量の上限 (元の循環ごとに、候補の手の数 × 循環の中の辺の数の累計)。
/// 超えた循環は深さ優先探索で塞ぐ。
const EXACT_SCORING_LIMIT: usize = 20_000_000;

/// 外す手の組を決める。main と alt の全部で作ったグラフで塞ぐので、main だけで指すときの循環も無くなる
/// (main だけのグラフは、main を替えた後も含めて、その部分グラフになる)。
///
/// 外す候補を 2 通りで作り、それぞれ 3. と 4. を通して、根から届く循環に残る局面の少ない方、同じなら外す手の
/// 少ない方、main を外す数の少ない方 (同じなら A) を採る。
///
/// - A (循環ごと): 循環 (強連結成分) ごとに、その中の自分の手のうち外せる手 (その局面に 2 手以上残る手) を
///   外す。小さい循環では、外した後に循環に残る局面の数の少ない手を 1 手ずつ選び、大きい循環では、残したい手
///   から先に辿る深さ優先探索で、循環を閉じるたびにその循環の中の手を外す。どちらも、同じ条件なら alt を先に、
///   値の低い手 (値の分からない手が先) を先に外す。外した後に残る循環について繰り返す。手の少ない組になり
///   やすいが、手が 1 手ずつの局面だけでできた循環の手前の入口は切らないので、塞げる循環が残ることがある。
/// - B (抜けるまでの段数): 定跡を抜ける局面 (辺の無い局面) から逆に辿り、定跡を抜けられる局面と抜けるまでの
///   段数を求める (ゲームのアトラクタ)。自分の手番の局面は、抜けられる局面へ行く手が 1 つあれば抜けられ、
///   相手の手番の局面は、全部の手が抜けられる局面へ行けば抜けられる。抜けられる自分の手番の局面で、抜けられ
///   ない局面へ行く手と、同じ循環の中で段数の減らない局面へ行く手を外す。残る手は段数の減る手と循環に
///   関わらない手なので、根が抜けられる局面なら循環は必ず無くなり、各局面に段数の減る手が 1 手以上残る。
///
/// 3. 外した手を、main・値の高い順 (値の分からない手は後、同じなら key 順・手の順) に 1 手ずつ戻し、戻しても
///    根から届く循環ができない手は戻したままにする。
/// 4. main を外した局面は、残る手のうち値の一番高い手 (同じなら元データの順) を main にする。
///
/// B で根が抜けられない局面なら、相手が千日手に持ち込めるので、根から届く循環が残る (`unbreakable`)。
///
/// 計算量: B と 3. の準備はグラフの大きさに比例する。A は 1 手 (大きい循環では 1 回の探索) ごとに、その循環の
/// 中だけを分解し直す。厳密な採点は元の循環ごとに累計で上限を設ける。深さ優先探索は、循環を閉じる辺ごとに
/// 探索の経路をなぞるので、長い経路に循環を閉じる辺が多いと循環の大きさの 2 乗程度になり得る。3. は 1 手ごとに
/// その手の循環の中と、新しく根から届く局面を辿る (戻せた手で届いた局面は以後辿り直さない)。
pub(super) fn plan_side(graph: &ReplyGraph, values: &[Vec<Option<i32>>]) -> Result<SidePlan> {
    let value = |v: usize, i: usize| -> Option<i32> {
        let book_move = graph.edges(v)[i].book_move()?;
        values[graph.master(v)?][book_move]
    };
    let is_main = |v: usize, i: usize| graph.edges(v)[i].book_move() == Some(0);
    // 外したい順 (小さいほど先に外す)。局面は元データの番号 (key の順と同じ)、手は元データの手の文字列で比べる。
    let removal_order = |(v, i): (usize, usize)| {
        (is_main(v, i), value(v, i), graph.master(v), graph.move_text(v, i))
    };
    let original: HashMap<usize, usize> = graph
        .cycles(|_, _| true)
        .into_iter()
        .enumerate()
        .flat_map(|(c, nodes)| nodes.into_iter().map(move |v| (v, c)))
        .collect();
    let finish =
        |removed: Vec<(usize, usize)>| finish_plan(graph, &original, removed, &value, &is_main);
    let by_cycle = SidePlan {
        method: "A (循環ごと)",
        ..finish(cycle_removals(graph, &removal_order))?
    };
    let by_rank = SidePlan {
        method: "B (抜けるまでの段数)",
        ..finish(rank_removals(graph, &original))?
    };
    let score = |p: &SidePlan| {
        (
            p.unbreakable.iter().map(Vec::len).sum::<usize>(),
            p.removed.len(),
            p.removed.iter().filter(|(_, i)| *i == 0).count(),
        )
    };
    Ok(if score(&by_rank) < score(&by_cycle) {
        by_rank
    } else {
        by_cycle
    })
}

/// 辺を使うかどうか (局面ごとに辺の順)。循環ごとに並べて解くとき、各循環は自分の局面の辺だけを読み書きする
/// (循環どうしは局面を共有しない) ので、読み書きの順は結果に影響しない。
type SharedActive = [AtomicBool];

/// A: 循環ごとに外す手。循環どうしは互いに影響しないので並べて解く。
fn cycle_removals<K: Ord>(
    graph: &ReplyGraph,
    removal_order: &(impl Fn((usize, usize)) -> K + Sync),
) -> Vec<(usize, usize)> {
    let active: Vec<AtomicBool> = (0..graph.edge_count()).map(|_| AtomicBool::new(true)).collect();
    let parts: Vec<Vec<(usize, usize)>> = graph
        .cycles(|_, _| true)
        .par_iter()
        .map(|component| component_removals(graph, &active, component, removal_order))
        .collect();
    parts.into_iter().flatten().collect()
}

/// 1 つの (元の) 循環の中で外す手。外した後に中に残る循環について繰り返す。
fn component_removals<K: Ord>(
    graph: &ReplyGraph,
    active: &SharedActive,
    component: &[usize],
    removal_order: &impl Fn((usize, usize)) -> K,
) -> Vec<(usize, usize)> {
    let mut removed: Vec<(usize, usize)> = Vec::new();
    let mut spent = 0usize;
    let mut queue: Vec<Vec<usize>> = vec![component.to_vec()];
    while let Some(component) = queue.pop() {
        let candidates = removable(graph, active, &component);
        if candidates.is_empty() {
            continue;
        }
        let inner_edges: usize = component.iter().map(|&v| graph.edges(v).len()).sum();
        let cost = candidates.len().saturating_mul(inner_edges);
        let mut chosen = if spent.saturating_add(cost) <= EXACT_SCORING_LIMIT {
            spent += cost;
            vec![choose_removal(
                graph,
                active,
                &component,
                &candidates,
                removal_order,
            )]
        } else {
            dfs_removals(graph, active, &component, removal_order)
        };
        if chosen.is_empty() {
            chosen.push(*candidates.iter().min_by_key(|&&e| removal_order(e)).expect("候補"));
        }
        for (v, i) in chosen {
            set(graph, active, v, i, false);
            removed.push((v, i));
        }
        queue.extend(sub_cycles(graph, active, &component));
    }
    removed
}

fn is_active(graph: &ReplyGraph, active: &SharedActive, v: usize, i: usize) -> bool {
    active[graph.edge_id(v, i)].load(Ordering::Relaxed)
}

fn set(graph: &ReplyGraph, active: &SharedActive, v: usize, i: usize, value: bool) {
    active[graph.edge_id(v, i)].store(value, Ordering::Relaxed);
}

/// 局面に残っている (使う) 自分の手の数。
fn active_count(graph: &ReplyGraph, active: &SharedActive, v: usize) -> usize {
    (0..graph.edges(v).len()).filter(|&i| is_active(graph, active, v, i)).count()
}

/// B: 定跡を抜けるまでの段数で外す手。
fn rank_removals(graph: &ReplyGraph, original: &HashMap<usize, usize>) -> Vec<(usize, usize)> {
    let rank = attractor(graph);
    let mut removed = Vec::new();
    for v in (0..graph.len()).filter(|&v| graph.own[v]) {
        let Some(own_rank) = rank[v] else {
            continue;
        };
        for (i, e) in graph.edges(v).iter().enumerate() {
            let cycle_risk = match rank[e.to()] {
                None => true,
                Some(r) => {
                    r >= own_rank
                        && original.get(&v).is_some_and(|c| original.get(&e.to()) == Some(c))
                }
            };
            if cycle_risk {
                removed.push((v, i));
            }
        }
    }
    removed
}

/// 外す候補から戻せる手を戻し (3.)、計画にする (main の替わりは [`new_mains`] で決める)。
fn finish_plan(
    graph: &ReplyGraph,
    original: &HashMap<usize, usize>,
    mut removed: Vec<(usize, usize)>,
    value: &impl Fn(usize, usize) -> Option<i32>,
    is_main: &impl Fn(usize, usize) -> bool,
) -> Result<SidePlan> {
    let mut active = vec![true; graph.edge_count()];
    for &(v, i) in &removed {
        active[graph.edge_id(v, i)] = false;
    }
    // 戻したい順 (main、値の高い手、値の分からない手は後、key 順、手の順)。
    removed.sort_by(|&(av, ai), &(bv, bi)| {
        let order = |v: usize, i: usize| {
            (
                !is_main(v, i),
                value(v, i).is_none(),
                std::cmp::Reverse(value(v, i)),
                graph.master(v),
                graph.move_text(v, i),
            )
        };
        order(av, ai).cmp(&order(bv, bi))
    });
    let mut reached = reachable(graph, &active);
    removed.retain(|&(v, i)| {
        active[graph.edge_id(v, i)] = true;
        let to = graph.edges(v)[i].to();
        // 根から届かない局面の手は、戻しても届く範囲の循環を作らない (後で届くようになる手の側で調べる)。
        let added = if !reached[v] {
            Some(Vec::new())
        } else if reaches(graph, &active, original, to, v) {
            None
        } else {
            newly_reached(graph, &active, &reached, to)
        };
        match added {
            Some(nodes) => {
                for u in nodes {
                    reached[u] = true;
                }
                false
            }
            None => {
                active[graph.edge_id(v, i)] = false;
                true
            }
        }
    });

    let mut plan = SidePlan::default();
    for &(v, i) in &removed {
        let book_move =
            graph.edges(v)[i].book_move().context("内部エラー: 相手の手を外しました")?;
        plan.removed.push((graph.key(v)?, book_move));
    }
    plan.removed.sort();
    plan.unbreakable = graph
        .cycles(|v, i| active[graph.edge_id(v, i)])
        .into_iter()
        .map(|c| c.into_iter().map(|v| graph.key(v)).collect::<Result<_>>())
        .collect::<Result<_>>()?;
    Ok(plan)
}

/// main を外した局面の替わりの main (4.)。残る手 (外さない main・alt) のうち、curated の手が残っていれば
/// curated の手から選ぶ (curated の行だけの定跡にも main が 1 つ要るため)。その中で合法な手、値の高い手
/// (値の分からない手は後)、元データの順の先を選ぶ。
pub(super) fn new_mains(
    graph: &ReplyGraph,
    index: &BookIndex,
    values: &[Vec<Option<i32>>],
    removed: &[(String, usize)],
    curated: &HashSet<(String, String)>,
) -> Result<BTreeMap<String, usize>> {
    // 元データの番号 → グラフの頂点 (自分の手番の局面)。
    let node: HashMap<usize, usize> = (0..graph.len())
        .filter(|&v| graph.own[v])
        .filter_map(|v| Some((graph.master(v)?, v)))
        .collect();
    let mut result = BTreeMap::new();
    for (key, _) in removed.iter().filter(|(_, i)| *i == 0) {
        let master = index.find(key).context("内部エラー: 元データに無い局面です")?;
        let position = index.position(master);
        let gone: HashSet<usize> =
            removed.iter().filter(|(k, _)| k == key).map(|&(_, i)| i).collect();
        let legal: HashSet<usize> = node
            .get(&master)
            .map(|&v| graph.edges(v).iter().filter_map(|e| e.book_move()).collect())
            .unwrap_or_default();
        let remaining: Vec<usize> =
            (0..position.moves.len()).filter(|i| !gone.contains(i)).collect();
        let is_curated =
            |i: &usize| curated.contains(&(key.clone(), position.moves[*i].mv.clone()));
        let pool: Vec<usize> = if remaining.iter().any(is_curated) {
            remaining.iter().copied().filter(is_curated).collect()
        } else {
            remaining
        };
        let value = |i: usize| values[master][i];
        let best = pool
            .into_iter()
            .max_by_key(|&i| {
                (legal.contains(&i), value(i).is_some(), value(i), std::cmp::Reverse(i))
            })
            .context("内部エラー: 手の残らない局面です")?;
        result.insert(key.clone(), best);
    }
    Ok(result)
}

/// 定跡を抜けるまでの段数 (抜けられない局面は `None`)。辺の無い局面が 0。自分の手番の局面は行き先の
/// 段数の最小 + 1、相手の手番の局面は全部の行き先が抜けられるとき最大 + 1。
fn attractor(graph: &ReplyGraph) -> Vec<Option<u32>> {
    // 逆向きの辺を 1 本の配列に詰める (頂点 `w` の前の局面は `predecessors[start[w]..start[w + 1]]`)。
    let mut start = vec![0usize; graph.len() + 1];
    for v in 0..graph.len() {
        for e in graph.edges(v) {
            start[e.to() + 1] += 1;
        }
    }
    for w in 0..graph.len() {
        start[w + 1] += start[w];
    }
    let mut fill = start.clone();
    let mut predecessors = vec![0u32; graph.edge_count()];
    for v in 0..graph.len() {
        for e in graph.edges(v) {
            predecessors[fill[e.to()]] = v as u32;
            fill[e.to()] += 1;
        }
    }
    let mut remaining: Vec<usize> = (0..graph.len()).map(|v| graph.edges(v).len()).collect();
    let mut rank: Vec<Option<u32>> = vec![None; graph.len()];
    let mut queue: VecDeque<usize> = (0..graph.len()).filter(|&v| remaining[v] == 0).collect();
    for &v in &queue {
        rank[v] = Some(0);
    }
    // 段数の小さい順に取り出すので、自分の手番は最初の行き先で最小、相手の手番は最後の行き先で最大になる。
    while let Some(v) = queue.pop_front() {
        let next = rank[v].expect("取り出す局面は段数がある") + 1;
        for &p in &predecessors[start[v]..start[v + 1]] {
            let p = p as usize;
            if rank[p].is_some() {
                continue;
            }
            remaining[p] -= 1;
            if graph.own[p] || remaining[p] == 0 {
                rank[p] = Some(next);
                queue.push_back(p);
            }
        }
    }
    rank
}

/// `component` の中の局面だけでできる循環 (今の辺で)。
fn sub_cycles(graph: &ReplyGraph, active: &SharedActive, component: &[usize]) -> Vec<Vec<usize>> {
    let local: HashMap<usize, u32> =
        component.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();
    let adjacency: Vec<Vec<u32>> = component
        .iter()
        .map(|&v| {
            (0..graph.edges(v).len())
                .filter(|&i| is_active(graph, active, v, i))
                .filter_map(|i| local.get(&graph.edges(v)[i].to()).copied())
                .collect()
        })
        .collect();
    nontrivial_sccs(&adjacency)
        .into_iter()
        .map(|c| c.into_iter().map(|i| component[i as usize]).collect())
        .collect()
}

/// 循環の中で外せる自分の手 (循環の中の局面へ行き、その局面に 2 手以上残る手)。
fn removable(
    graph: &ReplyGraph,
    active: &SharedActive,
    component: &[usize],
) -> Vec<(usize, usize)> {
    let inside: HashSet<usize> = component.iter().copied().collect();
    component
        .iter()
        .filter(|&&v| graph.own[v] && active_count(graph, active, v) >= 2)
        .flat_map(|&v| (0..graph.edges(v).len()).map(move |i| (v, i)))
        .filter(|&(v, i)| {
            is_active(graph, active, v, i) && inside.contains(&graph.edges(v)[i].to())
        })
        .collect()
}

/// 外した後に循環に残る局面の数の一番少ない手 (同じなら外したい順の先)。
fn choose_removal<K: Ord>(
    graph: &ReplyGraph,
    active: &SharedActive,
    component: &[usize],
    candidates: &[(usize, usize)],
    removal_order: &impl Fn((usize, usize)) -> K,
) -> (usize, usize) {
    let local: HashMap<usize, u32> =
        component.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();
    let inner = |v: usize, i: usize| {
        is_active(graph, active, v, i) && local.contains_key(&graph.edges(v)[i].to())
    };
    *candidates
        .iter()
        .min_by_key(|&&(v, i)| {
            let adjacency: Vec<Vec<u32>> = component
                .iter()
                .map(|&u| {
                    (0..graph.edges(u).len())
                        .filter(|&j| inner(u, j) && (u, j) != (v, i))
                        .map(|j| local[&graph.edges(u)[j].to()])
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
    active: &SharedActive,
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
        let order = |v: usize, active: &SharedActive| -> Vec<usize> {
            let mut edges: Vec<usize> = (0..graph.edges(v).len())
                .filter(|&i| {
                    is_active(graph, active, v, i) && local.contains_key(&graph.edges(v)[i].to())
                })
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
            if !is_active(graph, active, v, i) {
                continue;
            }
            let w = graph.edges(v)[i].to();
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
                                && is_active(graph, active, u, j)
                                && active_count(graph, active, u) >= 2
                        })
                        .min_by_key(|&e| removal_order(e));
                    if let Some((u, j)) = target {
                        set(graph, active, u, j, false);
                        removed.push((u, j));
                    }
                }
            }
        }
    }
    // 呼び出し側で外したことにするので、ここでは元に戻して返す。
    for &(u, j) in &removed {
        set(graph, active, u, j, true);
    }
    removed
}

/// 深さ優先探索の 1 段: (局面, 辿る辺の順, 次の位置, その局面に来た辺)。
type Frame = (usize, Vec<usize>, usize, Option<(usize, usize)>);

/// 根から今の辺で届く局面。
fn reachable(graph: &ReplyGraph, active: &[bool]) -> Vec<bool> {
    let mut reached = vec![false; graph.len()];
    reached[0] = true;
    let mut stack = vec![0];
    while let Some(v) = stack.pop() {
        for (i, e) in graph.edges(v).iter().enumerate() {
            if active[graph.edge_id(v, i)] && !reached[e.to()] {
                reached[e.to()] = true;
                stack.push(e.to());
            }
        }
    }
    reached
}

/// `from` を通って新しく根から届く局面。その中に循環があれば `None`。
fn newly_reached(
    graph: &ReplyGraph,
    active: &[bool],
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
        for (i, e) in graph.edges(v).iter().enumerate() {
            if active[graph.edge_id(v, i)] && !reached[e.to()] && !local.contains_key(&e.to()) {
                local.insert(e.to(), nodes.len() as u32);
                nodes.push(e.to());
            }
        }
    }
    let adjacency: Vec<Vec<u32>> = nodes
        .iter()
        .map(|&v| {
            (0..graph.edges(v).len())
                .filter(|&i| active[graph.edge_id(v, i)])
                .filter_map(|i| local.get(&graph.edges(v)[i].to()).copied())
                .collect()
        })
        .collect();
    nontrivial_sccs(&adjacency).is_empty().then_some(nodes)
}

/// `from` から `to` に、今の辺で行けるか。どちらも元のグラフの同じ循環の中の局面だけを通る。
fn reaches(
    graph: &ReplyGraph,
    active: &[bool],
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
        for (i, e) in graph.edges(v).iter().enumerate() {
            if active[graph.edge_id(v, i)]
                && original.get(&e.to()) == Some(&component)
                && seen.insert(e.to())
            {
                stack.push(e.to());
            }
        }
    }
    false
}

/// 元データの curated の行の (key, 手)。行を 1 行ずつ読み、curated の行だけを持つ。
fn curated_moves(path: &std::path::Path) -> Result<HashSet<(String, String)>> {
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    let column = |name: &str| COLUMNS.iter().position(|c| *c == name).expect("既知の列");
    let (key, mv, tier) = (column("key"), column("move"), column("tier"));
    let mut curated = HashSet::new();
    for line in std::io::BufReader::new(file).lines().skip(1) {
        let line = line?;
        let cells: Vec<&str> = line.split('\t').collect();
        if cells.get(tier) == Some(&"curated") {
            curated.insert((cells[key].to_owned(), cells[mv].to_owned()));
        }
    }
    Ok(curated)
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

fn format_summary(r: &SideResult) -> String {
    let show = |c: &CycleCount| format!("{} (局面 {})", c.components, c.positions);
    let side = r.side;
    let plan = &r.plan;
    let mut text = String::new();
    let mut out = |s: String| {
        text.push_str(&s);
        text.push('\n');
    };
    out(format!(
        "{side}: 循環 (自明でない強連結成分) default {} / random {}",
        show(&r.before[0]),
        show(&r.before[1])
    ));
    out(format!(
        "{side}: 外す手 {} (main {} / alt {})、main を替える局面 {}、外し方 {}",
        plan.removed.len(),
        plan.removed.iter().filter(|(_, i)| *i == 0).count(),
        plan.removed.iter().filter(|(_, i)| *i != 0).count(),
        plan.new_main.len(),
        plan.method
    ));
    out(format!(
        "{side}: 外した後の循環 default {} / random {}",
        show(&r.after[0]),
        show(&r.after[1])
    ));
    for cycle in &plan.unbreakable {
        out(format!("{side}: 塞げない循環 (根から相手が持ち込める): {}", cycle.join(" | ")));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::super::audit::parse_master_rows;
    use super::super::reply_graph::fixture::{ROOT, after, master_text, positions};
    use super::*;
    use rand::{Rng, SeedableRng};

    /// 外す手を除き、main を替えた元データ (局面の手の並びだけ)。
    fn apply_plans<'a>(
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

    fn graph_of<'a>(index: &'a BookIndex<'a>, start: &str) -> ReplyGraph<'a> {
        build(index, "b", start, |_, m| selected_moves(m, Preset::Random, None)).unwrap()
    }

    /// default と random の循環の数。
    fn cycles_of(p: &BTreeMap<String, MasterPosition>, start: &str) -> [CycleCount; 2] {
        let index = BookIndex::new(p).unwrap();
        count_on(&graph_of(&index, start), |_| 0, |_, _| false)
    }

    fn solve_from(p: &BTreeMap<String, MasterPosition>, start: &str) -> SidePlan {
        let values = evaluate(p, DrawValues::default()).unwrap();
        let index = BookIndex::new(p).unwrap();
        let graph = graph_of(&index, start);
        let mut plan = plan_side(&graph, &values).unwrap();
        plan.new_main = new_mains(&graph, &index, &values, &plan.removed, &HashSet::new()).unwrap();
        plan
    }

    fn solve(rows: &[(&str, &str, &str, &str)]) -> (BTreeMap<String, MasterPosition>, SidePlan) {
        let p = positions(rows);
        let plan = solve_from(&p, ROOT);
        (p, plan)
    }

    /// 根から 5i4h、相手 5a5b で C、C から 4h5i、相手 5b5a で根に戻る循環を作る。
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
        assert_eq!(
            cycles_of(&p, ROOT),
            [CycleCount {
                components: 1,
                positions: 4
            }; 2]
        );
        assert_eq!(cycles_of(&after_positions, ROOT), [CycleCount::default(); 2]);
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
    fn cuts_entry_into_cycle_of_single_move_positions() {
        // E2 → X → Y → Z → E2 の循環は自分の手が 1 手ずつの局面 (X・Z) だけでできているが、手前の局面 E の
        // 5h4h で入る。その手を外して 5h6h を main にすれば根から届かなくなる。
        let e = after(ROOT, &["5i5h", "5a4a"]);
        let e2 = after(&e, &["5h4h"]);
        let x = after(&e2, &["4a5a"]);
        let z = after(&x, &["4h5i", "5a4a"]);
        let rows = [
            (ROOT, "5i5h", "main", "0"),
            (e.as_str(), "5h4h", "main", "30"),
            (e.as_str(), "5h6h", "alt", "0"),
            (x.as_str(), "4h5i", "main", "0"),
            (z.as_str(), "5i4h", "main", "0"),
        ];
        let (p, plan) = solve(&rows);
        assert_eq!(cycles_of(&p, ROOT)[1].components, 1);
        assert_eq!(plan.removed, [(e.clone(), 0)]);
        assert_eq!(plan.new_main, BTreeMap::from([(e.clone(), 1)]));
        assert!(plan.unbreakable.is_empty());
    }

    #[test]
    fn reports_cycle_the_opponent_can_force() {
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
    fn opponent_non_promotion_closes_a_cycle() {
        // 後手の飛車が敵陣の中で不成で動いて (2g2h、2h2g) 戻る循環。探索用の手の生成は飛車の不成を省くので、
        // それでは見つからない。
        let root = "4k4/9/9/9/9/9/7r1/9/K8 b -";
        let b = after(root, &["9i9h", "2g2h"]);
        let p = positions(&[
            (root, "9i9h", "main", "0"),
            (root, "9i8i", "alt", "0"),
            (b.as_str(), "9h9i", "main", "0"),
        ]);
        assert_eq!(cycles_of(&p, root)[1].components, 1);
        let plan = solve_from(&p, root);
        assert_eq!(plan.removed, [(root.to_owned(), 0)]);
        assert_eq!(plan.new_main, BTreeMap::from([(root.to_owned(), 1)]));
    }

    #[test]
    fn apply_keeps_one_curated_main() {
        // C の main (curated) を外すとき、値の高い wide の alt ではなく curated の alt を main にする。
        let c = c();
        let rows = [
            (ROOT, "5i4h", "main", "10", "wide"),
            (c.as_str(), "4h5i", "main", "30", "curated"),
            (c.as_str(), "4h3h", "alt", "20", "curated"),
            (c.as_str(), "4h4i", "alt", "90", "wide"),
        ];
        let mut text = super::super::register::COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals, tier) in rows {
            writeln!(text, "{key}\t1\t{mv}\t{role}\tread\t{evals}\t\t\t{tier}\tt\tn").unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let master = dir.path().join("moves.tsv");
        std::fs::write(&master, &text).unwrap();
        let args = CyclesArgs {
            master: master.clone(),
            table: dir.path().join("cycles.tsv"),
            side: SideArg::B,
            draw_value_black: 0,
            draw_value_white: 0,
            apply: true,
            note: "塞いだ".into(),
            threads: 0,
        };
        run_from(args, ROOT).unwrap();
        let rows = parse_master(&std::fs::read_to_string(&master).unwrap()).unwrap();
        let roles: Vec<(&str, &str)> =
            rows.iter().map(|r| (r.get("move"), r.get("role"))).collect();
        assert_eq!(
            roles,
            [
                ("5i4h", "main"),
                ("4h5i", "record"),
                ("4h3h", "main"),
                ("4h4i", "alt")
            ]
        );
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
        let args = |table: PathBuf| CyclesArgs {
            master: master.clone(),
            table,
            side: SideArg::B,
            draw_value_black: 0,
            draw_value_white: 0,
            apply: true,
            note: "塞いだ".into(),
            threads: 0,
        };
        // 一覧の出力先が元データと同じなら書かずに止める。
        assert!(run_from(args(master.clone()), ROOT).is_err());
        assert_eq!(std::fs::read_to_string(&master).unwrap(), master_text(&rows));
        run_from(args(dir.path().join("cycles.tsv")), ROOT).unwrap();
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

    /// 玉だけの局面 (先手玉は 4〜6 筋の 7〜9 段、後手玉は 4〜6 筋の 1〜3 段) の乱数の元データで、各局面に 1 手
    /// 残すこと、外した後の循環が塞げない循環と一致すること、1 手ずつ残す組を総当たりして循環を無くせるなら
    /// 塞げない循環が残らないことを確かめる。
    #[test]
    fn random_books_agree_with_exhaustive_search() {
        let sq = |(f, r): (u8, u8)| format!("{f}{}", (b'a' + r - 1) as char);
        let key = |bk: (u8, u8), wk: (u8, u8)| {
            let ranks: Vec<String> = (1..=9u8)
                .map(|r| {
                    let mut text = String::new();
                    let mut empty = 0;
                    for f in (1..=9u8).rev() {
                        let piece = if (f, r) == bk {
                            Some('K')
                        } else if (f, r) == wk {
                            Some('k')
                        } else {
                            None
                        };
                        match piece {
                            Some(c) => {
                                if empty > 0 {
                                    text.push_str(&empty.to_string());
                                    empty = 0;
                                }
                                text.push(c);
                            }
                            None => empty += 1,
                        }
                    }
                    if empty > 0 {
                        text.push_str(&empty.to_string());
                    }
                    text
                })
                .collect();
            format!("{} b -", ranks.join("/"))
        };
        let black: Vec<(u8, u8)> = (4..=6).flat_map(|f| (7..=9).map(move |r| (f, r))).collect();
        let white: Vec<(u8, u8)> = (4..=6).flat_map(|f| (1..=3).map(move |r| (f, r))).collect();
        let root = key((5, 9), (5, 1));
        let mut solvable_cycles = 0;
        for seed in 0..400u64 {
            let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
            let mut chosen = vec![((5u8, 9u8), (5u8, 1u8))];
            for _ in 0..rng.random_range(4..14) {
                let c = (black[rng.random_range(0..9)], white[rng.random_range(0..9)]);
                if !chosen.contains(&c) {
                    chosen.push(c);
                }
            }
            let mut rows: Vec<(String, String, String, String)> = Vec::new();
            for &(bk, wk) in &chosen {
                let mut targets: Vec<(u8, u8)> = black
                    .iter()
                    .copied()
                    .filter(|&(f, r)| {
                        f.abs_diff(bk.0) <= 1 && r.abs_diff(bk.1) <= 1 && (f, r) != bk
                    })
                    .collect();
                for j in 0..rng.random_range(1..=3.min(targets.len())) {
                    let t = targets.remove(rng.random_range(0..targets.len()));
                    rows.push((
                        key(bk, wk),
                        format!("{}{}", sq(bk), sq(t)),
                        if j == 0 { "main" } else { "alt" }.into(),
                        rng.random_range(-50..50).to_string(),
                    ));
                }
            }
            let rows: Vec<(&str, &str, &str, &str)> = rows
                .iter()
                .map(|(a, b, c, d)| (a.as_str(), b.as_str(), c.as_str(), d.as_str()))
                .collect();
            let p = parse_master_rows(master_text(&rows).as_bytes()).unwrap().0;
            let plan = solve_from(&p, &root);
            let after_positions = apply_plans(&p, [&plan].into_iter());
            assert!(after_positions.values().all(|m| !m.moves.is_empty()), "seed {seed}");
            let after = cycles_of(&after_positions, &root);
            assert_eq!(after[1].components, plan.unbreakable.len(), "seed {seed}");
            let index = BookIndex::new(&p).unwrap();
            let graph = graph_of(&index, &root);
            let filtered = count_on(
                &graph,
                |m| plan.new_main.get(index.key(m)).copied().unwrap_or(0),
                |m, i| {
                    plan.removed
                        .binary_search_by(|(k, j)| (k.as_str(), *j).cmp(&(index.key(m), i)))
                        .is_ok()
                },
            );
            assert_eq!(filtered, after, "seed {seed}");
            let multi: Vec<usize> = (0..graph.len())
                .filter(|&v| graph.own[v] && graph.edges(v).len() >= 2)
                .collect();
            // 2 手以上ある局面で残す 1 手の組を全部試す (各局面の手の番号を桁にした数え上げ)。
            let combinations: usize = multi.iter().map(|&v| graph.edges(v).len()).product();
            let solvable = (0..combinations).any(|mut n| {
                let keep: HashMap<usize, usize> = multi
                    .iter()
                    .map(|&v| {
                        let k = graph.edges(v).len();
                        let choice = n % k;
                        n /= k;
                        (v, choice)
                    })
                    .collect();
                graph.cycles(|v, i| keep.get(&v).is_none_or(|&c| c == i)).is_empty()
            });
            if solvable && !graph.cycles(|_, _| true).is_empty() {
                solvable_cycles += 1;
            }
            assert_eq!(plan.unbreakable.is_empty(), solvable, "seed {seed}: {plan:?}");
        }
        assert!(solvable_cycles > 0);
    }
}
