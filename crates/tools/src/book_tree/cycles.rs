//! 自分の定跡の手と、相手の合法手で定跡に戻る手でできる千日手の循環を見つけ、自分の手番の手だけを
//! 外して (指さない手 = record にして) 循環を無くす手の組を選ぶ。

mod exact;

use super::audit::{Flips, evaluate_with, reject_same_file, report_direct_lookup};
use super::master_table::{
    MasterPosition, Row, format_master, parse_master, read_master, validate,
};
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
    #[arg(long, default_value_t = 70, allow_hyphen_values = true)]
    draw_value_white: i32,
    /// 定跡を直接の key だけで引く (対局で先後反転した局面の定跡を引かない) 前提で手の値を伝播する。反転した局面への
    /// 辺の値は使うが、局面の行が反転した向きの値も保つことは確かめず、反転キーで合流した局面では止めない
    #[arg(long)]
    direct_lookup: bool,
    /// --direct-lookup で確かめなかった局面 (値の一番高い手が反転した向きの値を保たない局面と、反転した向きの保証が
    /// 破れる局面) の一覧 (TSV: kind・key・保存されている向きの値・反転した向きの値) の出力先
    #[arg(long, value_name = "PATH", requires = "direct_lookup")]
    direct_lookup_table: Option<PathBuf>,
    /// 元データを書き換える (外す手を record に、main を替える局面の替わりの手を main にする)。
    /// 省くと一覧と要約だけを出す
    #[arg(long)]
    apply: bool,
    /// 外してよい手を、元データの since がこの値の行に限る (複数指定可)。省くと全部の手を外してよい
    #[arg(long, value_name = "TAG")]
    removable_since: Vec<String>,
    /// 書き換えるとき、外す手と替わりの main の行の note の末尾に付ける文
    #[arg(long, default_value = "千日手の循環を塞ぐため指さない手にした")]
    note: String,
    /// 外してよい手だけでは根から届く循環を塞げないとき、その循環へ入る手を、外してよい手でなくても
    /// (curated の手を除く) record にしてよくする範囲。循環の局面から自分の手で数えて何手前までか
    /// (相手の手は数えない)。0 なら外してよい手だけで塞ぐ。`--removable-since` を省くと全部の手を外してよいので
    /// 効かない
    #[arg(long, value_name = "N", default_value_t = 4)]
    entry_depth: usize,
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
    /// 採った外し方 (`plan_side` の A / B / C)。
    pub method: &'static str,
    /// C (最小) を循環ごとに解いた結果の数。
    pub exact: exact::ExactCounts,
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
    entry: EntryOutcome,
}

/// 外してよい手だけでは塞げないときに、循環へ入る手を外してよくした結果。
#[derive(Debug, Default)]
struct EntryOutcome {
    /// 外してよくした手 (key, 手の番号) と、循環の局面から自分の手で数えた段数。
    allowed: BTreeMap<(String, usize), usize>,
    /// 外してよくした段数の上限 (外してよい手だけで塞げたときは `None`)。
    depth: Option<usize>,
    /// `--entry-depth` の段数まで外してよくしても根から相手が循環に持ち込めたか。
    failed: bool,
}

pub(super) fn run(args: CyclesArgs) -> Result<()> {
    thread_pool(args.threads)?.install(|| run_from(args, HIRATE_KEY))
}

fn run_from(args: CyclesArgs, start: &str) -> Result<()> {
    ensure!(!args.note.contains(['\t', '\r', '\n']), "--note にタブや改行は使えません");
    reject_same_file(&args.table, &[("--master", &args.master)])?;
    if let Some(table) = &args.direct_lookup_table {
        reject_same_file(table, &[("--master", &args.master), ("--table", &args.table)])?;
    }
    let positions = read_master(&args.master)?.0;
    let draws = DrawValues {
        black: args.draw_value_black,
        white: args.draw_value_white,
    };
    let (results, values) = solve_all(&positions, &args, draws, start)?;
    let index = BookIndex::new(&positions)?;

    let mut table = String::from(
        "side\tkey\tply\tmove\trole\tvalue\teval\tnew_main\tnew_main_value\tentry_depth\n",
    );
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
            let entry_depth = r
                .entry
                .allowed
                .get(&(key.clone(), *i))
                .map_or_else(String::new, usize::to_string);
            writeln!(
                table,
                "{}\t{key}\t{}\t{}\t{role}\t{}\t{eval}\t{new_main}\t{new_main_value}\t{entry_depth}",
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
        let stuck: Vec<&str> = results
            .iter()
            .filter(|r| !r.plan.unbreakable.is_empty())
            .map(|r| r.side)
            .collect();
        ensure!(
            stuck.is_empty(),
            "{} の塞げない循環が残るので元データを書き換えません (一覧は書きました)",
            stuck.join("・")
        );
        // 行は書き換えるときだけ読む (元データを丸ごと 2 つ持たないため)。
        let text = std::fs::read_to_string(&args.master)
            .with_context(|| format!("{} を読めません", args.master.display()))?;
        let mut rows = parse_master(&text)?;
        let changed =
            rewrite_rows(&mut rows, &positions, results.iter().map(|r| &r.plan), &args.note)?;
        validate(&rows)?;
        write_atomic(&args.master, &format_master(&rows))?;
        println!("書き換えました: 行 {changed}");
    } else {
        println!("確認だけ (--apply なし)");
    }
    Ok(())
}

/// 元データの局面の番号・手の番号ごとの伝播の値。
type Values = Vec<Vec<Option<i32>>>;

/// 指定の側を全部解き、側ごとの結果と伝播の値を返す。
fn solve_all(
    positions: &BTreeMap<String, MasterPosition>,
    args: &CyclesArgs,
    draws: DrawValues,
    start: &str,
) -> Result<(Vec<SideResult>, Values)> {
    let (values, stats) = evaluate_with(positions, draws, Flips::joined(args.direct_lookup), None)?;
    report_direct_lookup(&stats, args.direct_lookup_table.as_deref())?;
    let index = BookIndex::new(positions)?;
    let tags = move_tags(&args.master, &args.removable_since, &index)?;
    // 先手と後手のグラフは互いの手に左右されない (相手の手番では合法手を全部見る) ので並べて解く。
    let results = args
        .side
        .sides()
        .par_iter()
        .map(|&side| solve_side(&index, &values, &tags, (side, start), args.entry_depth))
        .collect::<Result<_>>()?;
    Ok((results, values))
}

/// 1 つの側を解く。外した後も同じグラフの辺を絞って数える。
fn solve_side(
    index: &BookIndex,
    values: &[Vec<Option<i32>>],
    tags: &MoveTags,
    (side, start): (&'static str, &str),
    entry_depth: usize,
) -> Result<SideResult> {
    let graph = build(index, side, start, |_, p| selected_moves(p, Preset::Random, None))?;
    let before = count_on(&graph, |_| 0, |_, _| false);
    let mut allowed = allowed_edges(&graph, tags.removable.as_ref());
    let mut plan = plan_side(&graph, values, &allowed)?;
    let mut entry = EntryOutcome::default();
    if !plan.unbreakable.is_empty() && tags.removable.is_some() {
        let curated = |v: usize, i: usize| {
            graph.master(v).zip(graph.edges(v)[i].book_move()).is_some_and(|(m, j)| {
                tags.curated
                    .contains(&(index.key(m).to_owned(), index.position(m).moves[j].mv.clone()))
            })
        };
        let value = |v: usize, i: usize| values[graph.master(v)?][graph.edges(v)[i].book_move()?];
        match entry_moves(&graph, &allowed, entry_depth, &curated, &value) {
            Some((depth, extra)) => {
                for &(v, i, d) in &extra {
                    allowed[graph.edge_id(v, i)] = true;
                    let j = graph.edges(v)[i].book_move().context("内部エラー: 相手の手です")?;
                    entry.allowed.insert((graph.key(v)?, j), d);
                }
                entry.depth = Some(depth);
                plan = plan_side(&graph, values, &allowed)?;
                ensure!(
                    plan.unbreakable.is_empty(),
                    "内部エラー: 入る手を外してよくした後も {side} の塞げない循環が残りました"
                );
            }
            None => entry.failed = true,
        }
    }
    plan.new_main = new_mains(&graph, index, values, &plan.removed, &tags.curated)?;
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
        entry,
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
/// 外す候補を 3 通りで作り、それぞれ 3. と 4. を通して、根から届く循環に残る局面の少ない方、同じなら外す手の
/// 少ない方、main を外す数の少ない方 (同じなら A、B、C の順) を採る。外すのは `allowed[辺の番号]` が真の手だけ。
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
///   外せない自分の手は必ず残る手として段数に入れる (その局面は外せない手の行き先が全部抜けられるときだけ
///   抜けられる) ので、外す手は外してよい手だけになり、同じ性質が成り立つ。
/// - C (最小): 循環ごとに、循環を全部無くし各局面に手を残す組のうち、外す手の数が最小 (同じなら main の少ない)
///   組を整数計画で求める ([`exact`])。手前の入口を切って根から届かなくする組は考えない。上限に当たった循環と、
///   外せる手の無い循環が残る循環は A の組を使う。
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
pub(super) fn plan_side(
    graph: &ReplyGraph,
    values: &[Vec<Option<i32>>],
    allowed: &[bool],
) -> Result<SidePlan> {
    let value = |v: usize, i: usize| -> Option<i32> {
        let book_move = graph.edges(v)[i].book_move()?;
        values[graph.master(v)?][book_move]
    };
    let is_main = |v: usize, i: usize| graph.edges(v)[i].book_move() == Some(0);
    // 外したい順 (小さいほど先に外す)。局面は元データの番号 (key の順と同じ)、手は元データの手の文字列で比べる。
    let removal_order = |(v, i): (usize, usize)| {
        (is_main(v, i), value(v, i), graph.master(v), graph.move_text(v, i))
    };
    let components = graph.cycles(|_, _| true);
    let original: HashMap<usize, usize> = components
        .iter()
        .enumerate()
        .flat_map(|(c, nodes)| nodes.iter().map(move |&v| (v, c)))
        .collect();
    let finish =
        |removed: Vec<(usize, usize)>| finish_plan(graph, &original, removed, &value, &is_main);
    let by_component = cycle_removals(graph, &components, allowed, &removal_order);
    let exact = exact::exact_removals(graph, allowed, &components, &by_component);
    let by_rank = rank_removals(graph, &original, allowed);
    let plans = [
        SidePlan {
            method: "A (循環ごと)",
            ..finish(by_component.into_iter().flatten().collect())?
        },
        SidePlan {
            method: "B (抜けるまでの段数)",
            ..finish(by_rank)?
        },
        SidePlan {
            method: "C (最小)",
            ..finish(exact.removed)?
        },
    ];
    let score = |p: &SidePlan| {
        (
            p.unbreakable.iter().map(Vec::len).sum::<usize>(),
            p.removed.len(),
            p.removed.iter().filter(|(_, i)| *i == 0).count(),
        )
    };
    let best = plans.into_iter().min_by_key(score).expect("候補は 3 つ");
    Ok(SidePlan {
        exact: exact.counts,
        ..best
    })
}

/// 辺を使うかどうか (局面ごとに辺の順)。循環ごとに並べて解くとき、各循環は自分の局面の辺だけを読み書きする
/// (循環どうしは局面を共有しない) ので、読み書きの順は結果に影響しない。
type SharedActive = [AtomicBool];

/// A: 循環ごとに外す手 (`components` の順)。循環どうしは互いに影響しないので並べて解く。
fn cycle_removals<K: Ord>(
    graph: &ReplyGraph,
    components: &[Vec<usize>],
    allowed: &[bool],
    removal_order: &(impl Fn((usize, usize)) -> K + Sync),
) -> Vec<Vec<(usize, usize)>> {
    let active: Vec<AtomicBool> = (0..graph.edge_count()).map(|_| AtomicBool::new(true)).collect();
    components
        .par_iter()
        .map(|component| component_removals(graph, &active, allowed, component, removal_order))
        .collect()
}

/// 1 つの (元の) 循環の中で外す手。外した後に中に残る循環について繰り返す。
fn component_removals<K: Ord>(
    graph: &ReplyGraph,
    active: &SharedActive,
    allowed: &[bool],
    component: &[usize],
    removal_order: &impl Fn((usize, usize)) -> K,
) -> Vec<(usize, usize)> {
    let mut removed: Vec<(usize, usize)> = Vec::new();
    let mut spent = 0usize;
    let mut queue: Vec<Vec<usize>> = vec![component.to_vec()];
    while let Some(component) = queue.pop() {
        let candidates = removable(graph, active, allowed, &component);
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
            dfs_removals(graph, active, allowed, &component, removal_order)
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

/// B: 定跡を抜けるまでの段数で外す手 (外してよい手だけ)。
fn rank_removals(
    graph: &ReplyGraph,
    original: &HashMap<usize, usize>,
    allowed: &[bool],
) -> Vec<(usize, usize)> {
    let rank = attractor(graph, &Predecessors::new(graph), allowed);
    let mut removed = Vec::new();
    for v in (0..graph.len()).filter(|&v| graph.own[v]) {
        let Some(own_rank) = rank[v] else {
            continue;
        };
        for (i, e) in graph.edges(v).iter().enumerate() {
            if !allowed[graph.edge_id(v, i)] {
                continue;
            }
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

/// 逆向きの辺。頂点 `w` へ来る辺は `entries[start[w]..start[w + 1]]` の (前の局面, その局面での辺の番号)。
struct Predecessors {
    start: Vec<usize>,
    entries: Vec<(u32, u32)>,
}

impl Predecessors {
    fn new(graph: &ReplyGraph) -> Self {
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
        let mut entries = vec![(0u32, 0u32); graph.edge_count()];
        for v in 0..graph.len() {
            for (i, e) in graph.edges(v).iter().enumerate() {
                entries[fill[e.to()]] = (v as u32, i as u32);
                fill[e.to()] += 1;
            }
        }
        Self { start, entries }
    }

    fn of(&self, w: usize) -> &[(u32, u32)] {
        &self.entries[self.start[w]..self.start[w + 1]]
    }
}

/// 定跡を抜けるまでの段数 (抜けられない局面は `None`)。辺の無い局面が 0。自分の手番の局面は行き先の
/// 段数の最小 + 1、相手の手番の局面は全部の行き先が抜けられるとき最大 + 1。外せない自分の手
/// (`allowed` が偽) のある局面は、その手が必ず残るので、相手の手番と同じく外せない手の行き先が全部
/// 抜けられるとき最大 + 1 (外せない手の行き先は段数が小さいので、残しても循環にならない)。
fn attractor(
    graph: &ReplyGraph,
    predecessors: &Predecessors,
    allowed: &[bool],
) -> Vec<Option<u32>> {
    let is_forced = |v: usize, i: usize| graph.own[v] && !allowed[graph.edge_id(v, i)];
    // 外せない自分の手の数 (自分の手番の局面だけ。0 なら行き先の 1 つが抜けられれば抜けられる)。
    let forced: Vec<usize> = (0..graph.len())
        .map(|v| (0..graph.edges(v).len()).filter(|&i| is_forced(v, i)).count())
        .collect();
    let mut remaining: Vec<usize> = (0..graph.len())
        .map(|v| {
            if forced[v] > 0 {
                forced[v]
            } else {
                graph.edges(v).len()
            }
        })
        .collect();
    let mut rank: Vec<Option<u32>> = vec![None; graph.len()];
    let mut queue: VecDeque<usize> = (0..graph.len()).filter(|&v| remaining[v] == 0).collect();
    for &v in &queue {
        rank[v] = Some(0);
    }
    // 段数の小さい順に取り出すので、自分の手番は最初の行き先で最小、相手の手番は最後の行き先で最大になる。
    while let Some(v) = queue.pop_front() {
        let next = rank[v].expect("取り出す局面は段数がある") + 1;
        for &(p, i) in predecessors.of(v) {
            let (p, i) = (p as usize, i as usize);
            if rank[p].is_some() || (forced[p] > 0 && !is_forced(p, i)) {
                continue;
            }
            remaining[p] -= 1;
            if (graph.own[p] && forced[p] == 0) || remaining[p] == 0 {
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

/// 循環の中で外せる自分の手 (外してよい手で、循環の中の局面へ行き、その局面に 2 手以上残る手)。
fn removable(
    graph: &ReplyGraph,
    active: &SharedActive,
    allowed: &[bool],
    component: &[usize],
) -> Vec<(usize, usize)> {
    let inside: HashSet<usize> = component.iter().copied().collect();
    component
        .iter()
        .filter(|&&v| graph.own[v] && active_count(graph, active, v) >= 2)
        .flat_map(|&v| (0..graph.edges(v).len()).map(move |i| (v, i)))
        .filter(|&(v, i)| {
            allowed[graph.edge_id(v, i)]
                && is_active(graph, active, v, i)
                && inside.contains(&graph.edges(v)[i].to())
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
    allowed: &[bool],
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
                                && allowed[graph.edge_id(u, j)]
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

/// 循環へ入る手: (頂点, 辺の番号, 段数)。
type EntryMove = (usize, usize, usize);

/// 外してよい手だけでは根が抜けられないとき、根を抜けられるようにするために外してよくする、循環へ入る手。
/// (段数の上限, 入る手) を返す。
///
/// 段数は、手の行き先から抜けられない循環の局面まで自分の手を何手指して着くか + 1 (相手の手は数えない)。
/// 循環の局面へ相手の手で入る局面の手と循環の中の手が 1 段、その手前の局面の手が 2 段になる。
/// 候補は、抜けられない自分の手番の局面 (手が 2 つ以上) の、外してよい手でも curated の手でもない手のうち、
/// 抜けられない局面へ行く手。外してよい手が増えるほど抜けられる局面は増える (単調) ので、段数 1 から `max_depth` まで、その段数
/// までの候補を全部外してよくして根が抜けられる最初の段数を探す。その段数で、残したい順 (段数の深い順、main、
/// 値の高い順 (値の分からない手は後)、元データの順、手の順) に 1 つずつ外してよい手から戻し、戻すと根が
/// 抜けられなくなる手だけを外してよいままにする ([`keep_needed`])。どの 1 つを戻しても抜けられない組になる
/// (最小とは限らない)。
/// `max_depth` まで外してよくしても抜けられなければ `None`。
///
/// 手が 1 つしか無い局面は、その手を外せないので抜けられないままで、そこへ入る手が次の段の候補になる
/// (上へたどる)。外してよくした手を実際に外すかは、その後の [`plan_side`] が決める。
fn entry_moves(
    graph: &ReplyGraph,
    allowed: &[bool],
    max_depth: usize,
    curated: &impl Fn(usize, usize) -> bool,
    value: &impl Fn(usize, usize) -> Option<i32>,
) -> Option<(usize, Vec<EntryMove>)> {
    let predecessors = Predecessors::new(graph);
    let rank = attractor(graph, &predecessors, allowed);
    if rank[0].is_some() {
        return Some((0, Vec::new()));
    }
    let dist = own_move_distance(graph, &predecessors, &rank);
    let mut candidates: Vec<EntryMove> = Vec::new();
    // 手が 1 つの局面は、その手を外してよくしても抜けられるかが変わらない (行き先だけで決まる) ので候補にしない。
    for v in
        (0..graph.len()).filter(|&v| graph.own[v] && rank[v].is_none() && graph.edges(v).len() >= 2)
    {
        for (i, e) in graph.edges(v).iter().enumerate() {
            let depth = dist[e.to()].saturating_add(1);
            if e.book_move().is_some()
                && !allowed[graph.edge_id(v, i)]
                && rank[e.to()].is_none()
                && depth <= max_depth
                && !curated(v, i)
            {
                candidates.push((v, i, depth));
            }
        }
    }
    let mut extra = allowed.to_vec();
    let mut full_rank = Vec::new();
    let depth = (1..=max_depth).find(|&d| {
        for &(v, i, _) in candidates.iter().filter(|c| c.2 == d) {
            extra[graph.edge_id(v, i)] = true;
        }
        full_rank = attractor(graph, &predecessors, &extra);
        full_rank[0].is_some()
    })?;
    // 全部外してよくしても抜けられない局面の手は、外してよくするかどうかで段数がどこも変わらないので戻す。
    candidates.retain(|&(v, i, d)| {
        let keep = d <= depth && full_rank[v].is_some();
        extra[graph.edge_id(v, i)] = keep;
        keep
    });
    let is_main = |v: usize, i: usize| graph.edges(v)[i].book_move() == Some(0);
    candidates.sort_by_cached_key(|&(v, i, d)| {
        (
            std::cmp::Reverse(d),
            !is_main(v, i),
            value(v, i).is_none(),
            std::cmp::Reverse(value(v, i)),
            graph.master(v),
            graph.move_text(v, i).into_owned(),
        )
    });
    let mut needed = vec![false; candidates.len()];
    keep_needed(graph, &predecessors, &mut extra, &candidates, 0..candidates.len(), &mut needed);
    let kept = candidates.into_iter().zip(needed).filter_map(|(c, n)| n.then_some(c)).collect();
    Some((depth, kept))
}

/// `candidates[range]` を前から 1 つずつ外してよい手から戻し、戻すと根が抜けられなくなる手だけを外してよい
/// ままにする (`needed` に印を付ける)。抜けられる局面は外してよい手が増えるほど増える (単調) ので、まとめて
/// 戻しても根が抜けられれば 1 つずつ戻しても全部戻せる。まとめて戻せない範囲だけを半分に分けて調べ、要らない
/// 手が多いときの判定の回数を減らす (結果は 1 つずつ戻すのと同じ)。
fn keep_needed(
    graph: &ReplyGraph,
    predecessors: &Predecessors,
    extra: &mut [bool],
    candidates: &[EntryMove],
    range: std::ops::Range<usize>,
    needed: &mut [bool],
) {
    let set = |extra: &mut [bool], on: bool| {
        for &(v, i, _) in &candidates[range.clone()] {
            extra[graph.edge_id(v, i)] = on;
        }
    };
    set(extra, false);
    if range.is_empty() || attractor(graph, predecessors, extra)[0].is_some() {
        return;
    }
    set(extra, true);
    if range.len() == 1 {
        needed[range.start] = true;
        return;
    }
    let mid = range.start + range.len() / 2;
    keep_needed(graph, predecessors, extra, candidates, range.start..mid, needed);
    keep_needed(graph, predecessors, extra, candidates, mid..range.end, needed);
}

/// 局面から、抜けられない (`rank` が `None`) 循環の局面まで、自分の手を何手指して着くか (相手の手は数えない)。
/// 着かない局面は `usize::MAX`。
fn own_move_distance(
    graph: &ReplyGraph,
    predecessors: &Predecessors,
    rank: &[Option<u32>],
) -> Vec<usize> {
    let mut dist = vec![usize::MAX; graph.len()];
    let mut queue = VecDeque::new();
    for v in graph.cycles(|_, _| true).into_iter().flatten().filter(|&v| rank[v].is_none()) {
        dist[v] = 0;
        queue.push_back(v);
    }
    // 相手の手は 0、自分の手は 1 の重みの最短路 (0-1 BFS)。
    while let Some(v) = queue.pop_front() {
        for &(p, _) in predecessors.of(v) {
            let p = p as usize;
            let cost = usize::from(graph.own[p]);
            if dist[v] + cost < dist[p] {
                dist[p] = dist[v] + cost;
                if cost == 0 {
                    queue.push_front(p);
                } else {
                    queue.push_back(p);
                }
            }
        }
    }
    dist
}

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

/// 元データの行から読む手の印。
struct MoveTags {
    /// curated の行の (key, 手)。
    curated: HashSet<(String, String)>,
    /// 外してよい手。`--removable-since` を省くと `None` (全部の手)。
    removable: Option<MoveBits>,
}

/// 元データの手ごとの印 (局面の番号と手の番号で引く)。文字列を持たないので、印の付く行が多くても局面数と
/// 手数に比例する小さい量で済む。
struct MoveBits {
    /// 局面 `m` の手は `offsets[m]..offsets[m + 1]` 番目。
    offsets: Vec<u32>,
    bits: Vec<u64>,
}

impl MoveBits {
    fn new(index: &BookIndex) -> Result<Self> {
        let mut offsets = Vec::with_capacity(index.len() + 1);
        let mut total = 0u32;
        offsets.push(0);
        for m in 0..index.len() {
            total = u32::try_from(index.position(m).moves.len())
                .ok()
                .and_then(|n| total.checked_add(n))
                .context("元データの手が多すぎます")?;
            offsets.push(total);
        }
        Ok(Self {
            offsets,
            bits: vec![0; (total as usize).div_ceil(64)],
        })
    }

    fn set(&mut self, m: usize, j: usize) {
        let k = self.offsets[m] as usize + j;
        self.bits[k / 64] |= 1 << (k % 64);
    }

    fn get(&self, m: usize, j: usize) -> bool {
        let k = self.offsets[m] as usize + j;
        self.bits[k / 64] & (1 << (k % 64)) != 0
    }
}

/// 元データの行を 1 行ずつ読み、curated の行と、since が `since` のどれかの行の手に印を付ける。
fn move_tags(path: &std::path::Path, since: &[String], index: &BookIndex) -> Result<MoveTags> {
    let file =
        std::fs::File::open(path).with_context(|| format!("{} を読めません", path.display()))?;
    let mut tags = MoveTags {
        curated: HashSet::new(),
        removable: if since.is_empty() {
            None
        } else {
            Some(MoveBits::new(index)?)
        },
    };
    for line in std::io::BufReader::new(file).lines().skip(1) {
        let row = Row::split_line(&line?);
        let curated = row.get_optional("tier") == Some("curated");
        if curated {
            tags.curated.insert((row.get("key").to_owned(), row.get("move").to_owned()));
        }
        let new_row = row.get_optional("since").is_some_and(|s| since.iter().any(|t| t == s));
        if let Some(bits) = tags.removable.as_mut()
            && new_row
            // record の行は局面の手に無い (辺にもならない) ので印を付けない。
            && let Some(m) = index.find(row.get("key"))
            && let Some(j) = index.position(m).moves.iter().position(|x| x.mv == row.get("move"))
        {
            bits.set(m, j);
        }
    }
    Ok(tags)
}

/// 辺の番号ごとに、外してよい自分の手か。相手の手番の辺は偽。
fn allowed_edges(graph: &ReplyGraph, removable: Option<&MoveBits>) -> Vec<bool> {
    let mut allowed = vec![false; graph.edge_count()];
    for v in (0..graph.len()).filter(|&v| graph.own[v]) {
        let Some(master) = graph.master(v) else {
            continue;
        };
        for (i, e) in graph.edges(v).iter().enumerate() {
            if let Some(j) = e.book_move() {
                allowed[graph.edge_id(v, i)] = removable.is_none_or(|bits| bits.get(master, j));
            }
        }
    }
    allowed
}

/// 元データの行を書き換え、書き換えた行の数を返す。
fn rewrite_rows<'a>(
    rows: &mut [Row],
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
            !super::master_table::is_unplayed(row.get("role")),
            "内部エラー: 指さない行 (record・bench) です: {} {}",
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
    out(format!(
        "{side}: 最小 (C) の循環ごとの結果: 解けた {} / 循環の中では塞げない {} / 上限 {}",
        plan.exact.solved, plan.exact.infeasible, plan.exact.limit
    ));
    let entry = &r.entry;
    if let Some(depth) = entry.depth {
        let removed: Vec<&(String, usize)> =
            plan.removed.iter().filter(|m| entry.allowed.contains_key(m)).collect();
        out(format!(
            "{side}: 外してよい手だけでは塞げないので、循環へ入る手を {depth} 段まで外してよくした: \
             外してよくした手 {}、そのうち外す手 {} (main {})",
            entry.allowed.len(),
            removed.len(),
            removed.iter().filter(|(_, i)| *i == 0).count()
        ));
    }
    if entry.failed {
        out(format!(
            "{side}: 循環へ入る手を --entry-depth の段数まで外してよくしても塞げない"
        ));
    }
    for cycle in &plan.unbreakable {
        out(format!("{side}: 塞げない循環 (根から相手が持ち込める): {}", cycle.join(" | ")));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::super::audit::evaluate;
    use super::super::master_table::parse_master_rows;
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
        let mut plan = plan_side(&graph, &values, &vec![true; graph.edge_count()]).unwrap();
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
        let mut text = super::super::master_table::COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals, tier) in rows {
            writeln!(
                text,
                "{}",
                crate::book_tree::master_table::test_line(&[
                    ("key", key.to_string()),
                    ("ply", "1".into()),
                    ("move", mv.to_string()),
                    ("role", role.to_string()),
                    ("source", "read".into()),
                    ("evals", evals.to_string()),
                    ("tier", tier.to_string()),
                    ("since", "t".into()),
                    ("note", "n".into())
                ])
            )
            .unwrap();
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
            direct_lookup: false,
            direct_lookup_table: None,
            apply: true,
            note: "塞いだ".into(),
            removable_since: Vec::new(),
            entry_depth: 4,
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
    fn direct_lookup_solves_a_master_that_stops_at_a_flipped_position() {
        use crate::book_backprop::FlipFailure;
        use crate::book_tree::model::child;
        // S から玉だけの 4 手で S に戻る循環。T から指すと S の反転した局面に着く。S の -30 の手は、千日手を -70 と
        // みる先手は指し、+70 とみる後手は指さずに循環を選ぶので、S の 1 行では両方の向きを表せない。
        const S: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";
        const T: &str = "4k4/9/9/9/9/9/9/4K4/9 b -";
        let c1 = child(S, "5i4i").unwrap();
        let c2 = child(&c1, "5a4a").unwrap();
        let c3 = child(&c2, "4i5i").unwrap();
        let rows = [
            (S, "5i4i", "main", "0"),
            (S, "5i6i", "alt", "-30"),
            (c1.as_str(), "5a4a", "main", "0"),
            (c2.as_str(), "4i5i", "main", "0"),
            (c3.as_str(), "4a5a", "main", "0"),
            (T, "5h5i", "main", "0"),
        ];
        let mut text = super::super::master_table::COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals) in rows {
            let line = crate::book_tree::master_table::test_line(&[
                ("key", key.to_string()),
                ("ply", "1".into()),
                ("move", mv.to_string()),
                ("role", role.to_string()),
                ("source", "read".into()),
                ("evals", evals.to_string()),
                ("tier", "wide".into()),
                ("since", "t".into()),
                ("note", "n".into()),
            ]);
            writeln!(text, "{line}").unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let master = dir.path().join("moves.tsv");
        std::fs::write(&master, &text).unwrap();
        let args = |direct_lookup, apply| CyclesArgs {
            master: master.clone(),
            table: dir.path().join("cycles.tsv"),
            side: SideArg::B,
            draw_value_black: -70,
            draw_value_white: 70,
            direct_lookup,
            direct_lookup_table: direct_lookup.then(|| dir.path().join("flip.tsv")),
            apply,
            note: "塞いだ".into(),
            removable_since: Vec::new(),
            entry_depth: 4,
            threads: 0,
        };
        let error = run_from(args(false, true), S).unwrap_err();
        assert!(error.downcast_ref::<FlipFailure>().is_some(), "{error:#}");
        assert!(!dir.path().join("cycles.tsv").exists());
        assert_eq!(std::fs::read_to_string(&master).unwrap(), text);

        // S の手の値は保存されている向き (先手) の値で、循環は次に S で -30 の手を指せるので同値 (選ばれない方を -1)。
        // T の手は S の反転した向き (後手が循環を選ぶ) の値を使う。
        let positions = parse_master_rows(text.as_bytes()).unwrap().0;
        let draws = DrawValues {
            black: -70,
            white: 70,
        };
        let (values, stats) = evaluate_with(&positions, draws, Flips::DirectLookup, None).unwrap();
        let of = |key: &str| values[positions.keys().position(|k| k == key).unwrap()].clone();
        assert_eq!(of(S), [Some(-31), Some(-30)]);
        assert_eq!(of(T), [Some(-70)]);
        let report = stats.direct_lookup.unwrap();
        assert_eq!(
            (report.flip_moves, report.flip_positions, report.flip_best_positions),
            (1, 1, 1)
        );

        // 直接のキーだけの循環 (S から 4 手で S に戻る) を見つけ、S の main を外して -30 の手を main にする。
        let dry = args(true, false);
        let (results, _) = solve_all(&positions, &dry, draws, S).unwrap();
        let cycle = CycleCount {
            components: 1,
            positions: 4,
        };
        assert_eq!(results[0].before, [cycle; 2]);
        assert_eq!(results[0].plan.removed, [(S.to_owned(), 0)]);
        assert_eq!(results[0].plan.new_main, BTreeMap::from([(S.to_owned(), 1)]));
        assert_eq!(results[0].after, [CycleCount::default(); 2]);
        run_from(args(true, true), S).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cycles.tsv")).unwrap(),
            format!(
                "side\tkey\tply\tmove\trole\tvalue\teval\tnew_main\tnew_main_value\tentry_depth\nb\t{S}\t1\t5i4i\tmain\t-31\t0\t5i6i\t-30\t\n"
            )
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("flip.tsv")).unwrap(),
            format!("kind\tkey\tstored\tflipped\nvalue\t{S}\t-30\t70\n")
        );
        let rows = parse_master(&std::fs::read_to_string(&master).unwrap()).unwrap();
        let roles: Vec<(&str, &str)> = rows
            .iter()
            .filter(|r| r.get("key") == S)
            .map(|r| (r.get("move"), r.get("role")))
            .collect();
        assert_eq!(roles, [("5i4i", "record"), ("5i6i", "main")]);
        // 塞いだ後の元データに循環は残らない。
        let positions = read_master(&master).unwrap().0;
        let (results, _) = solve_all(&positions, &dry, draws, S).unwrap();
        assert_eq!(results[0].before, [CycleCount::default(); 2]);
        assert!(results[0].plan.removed.is_empty());
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
            direct_lookup: false,
            direct_lookup_table: None,
            apply: true,
            note: "塞いだ".into(),
            removable_since: Vec::new(),
            entry_depth: 4,
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
            format!("b\t{c}\t1\t4h5i\tmain\t30\t30\t4h3h\t20\t")
        );
    }

    /// (key, 手, role, evals, since, tier) の行で元データの表を書く。手数は key ごとに `plies` から引く (無ければ 1)。
    fn write_master(
        dir: &std::path::Path,
        rows: &[(&str, &str, &str, &str, &str, &str)],
        plies: &[(&str, u32)],
    ) -> PathBuf {
        let mut text = super::super::master_table::COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals, since, tier) in rows {
            let ply = plies.iter().find(|(k, _)| k == key).map_or(1, |&(_, p)| p);
            writeln!(
                text,
                "{}",
                crate::book_tree::master_table::test_line(&[
                    ("key", key.to_string()),
                    ("ply", ply.to_string()),
                    ("move", mv.to_string()),
                    ("role", role.to_string()),
                    ("source", "read".into()),
                    ("evals", evals.to_string()),
                    ("tier", tier.to_string()),
                    ("since", since.to_string()),
                    ("note", "n".into())
                ])
            )
            .unwrap();
        }
        let master = dir.join("moves.tsv");
        std::fs::write(&master, &text).unwrap();
        master
    }

    /// `--removable-since new --apply` で解き、成否、書き換え後の (key, 手, role) の一覧、一覧の行を返す。
    fn entry_run(
        rows: &[(&str, &str, &str, &str, &str, &str)],
        entry_depth: usize,
    ) -> (bool, Vec<(String, String, String)>, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let master = write_master(dir.path(), rows, &[]);
        let table = dir.path().join("table.tsv");
        let ok = run_from(
            CyclesArgs {
                master: master.clone(),
                table: table.clone(),
                side: SideArg::B,
                draw_value_black: 0,
                draw_value_white: 0,
                direct_lookup: false,
                direct_lookup_table: None,
                apply: true,
                note: "塞いだ".into(),
                removable_since: vec!["new".into()],
                entry_depth,
                threads: 0,
            },
            ROOT,
        )
        .is_ok();
        let text = std::fs::read_to_string(&master).unwrap();
        let after = parse_master(&text)
            .unwrap()
            .iter()
            .map(|r| (r.get("key").to_owned(), r.get("move").to_owned(), r.get("role").to_owned()))
            .collect();
        let table = std::fs::read_to_string(table)
            .unwrap()
            .lines()
            .skip(1)
            .map(str::to_owned)
            .collect();
        (ok, after, table)
    }

    fn row(key: &str, mv: &str, role: &str) -> (String, String, String) {
        (key.to_owned(), mv.to_owned(), role.to_owned())
    }

    #[test]
    fn records_old_entry_move_and_keeps_new_single_move_position() {
        // 根 (old の 2 手) の 5i4h → C (new の 1 手) 4h5i → 根の循環は、外してよい手 (C の 1 手) だけでは
        // 塞げない。C へ入る根の 5i4h を record にし、5i6h を main にする。C の行は残す。
        let c = c();
        let (ok, after, table) = entry_run(
            &[
                (ROOT, "5i4h", "main", "10", "old", "wide"),
                (ROOT, "5i6h", "alt", "0", "old", "wide"),
                (c.as_str(), "4h5i", "main", "30", "new", "wide"),
            ],
            4,
        );
        assert!(ok);
        assert_eq!(
            after,
            [
                row(ROOT, "5i4h", "record"),
                row(ROOT, "5i6h", "main"),
                row(&c, "4h5i", "main")
            ]
        );
        assert_eq!(table, [format!("b\t{ROOT}\t1\t5i4h\tmain\t10\t10\t5i6h\t0\t1")]);
    }

    #[test]
    fn climbs_past_single_move_position_up_to_entry_depth() {
        // E2 → X → Z → E2 の循環の手 (X・Z、new) は 1 手ずつ。手前の E も old の 1 手 (5h4h) だけなので、
        // さらに手前の根の 5i5h (2 段) を record にする。1 段までなら塞げずに止まり、元データを書き換えない。
        let e = after(ROOT, &["5i5h", "5a4a"]);
        let e2 = after(&e, &["5h4h"]);
        let x = after(&e2, &["4a5a"]);
        let z = after(&x, &["4h5i", "5a4a"]);
        let rows = [
            (ROOT, "5i5h", "main", "0", "old", "wide"),
            (ROOT, "5i6h", "alt", "0", "old", "wide"),
            (e.as_str(), "5h4h", "main", "30", "old", "wide"),
            (x.as_str(), "4h5i", "main", "0", "new", "wide"),
            (z.as_str(), "5i4h", "main", "0", "new", "wide"),
        ];
        let (ok, after, table) = entry_run(&rows, 2);
        assert!(ok);
        assert_eq!(after[..2], [row(ROOT, "5i5h", "record"), row(ROOT, "5i6h", "main")]);
        assert!(after[2..].iter().all(|(_, _, role)| role == "main"));
        assert_eq!(table.len(), 1);
        assert!(table[0].ends_with("\t2"), "{table:?}");
        let (ok, after, table) = entry_run(&rows, 1);
        assert!(!ok);
        assert!(
            after
                .iter()
                .all(|(_, mv, role)| role == if mv == "5i6h" { "alt" } else { "main" })
        );
        assert!(table.is_empty());
    }

    #[test]
    fn stops_when_entry_moves_cannot_break_the_cycle() {
        // 根の 5i4h が 1 手だけ、または curated なら、入る手を外せないので止まり、元データを書き換えない。
        let c = c();
        for root_rows in [
            vec![(ROOT, "5i4h", "main", "10", "old", "wide")],
            vec![
                (ROOT, "5i4h", "main", "10", "old", "curated"),
                (ROOT, "5i6h", "alt", "0", "old", "wide"),
            ],
        ] {
            let mut rows = root_rows.clone();
            rows.push((c.as_str(), "4h5i", "main", "30", "new", "wide"));
            let (ok, after, _) = entry_run(&rows, 4);
            assert!(!ok, "{root_rows:?}");
            assert!(after.iter().all(|(_, _, role)| role != "record"));
            assert_eq!(after.len(), rows.len());
        }
        // 0 段なら外してよい手だけで塞ぐので、塞げる元データでも止まる。
        let (ok, _, _) = entry_run(
            &[
                (ROOT, "5i4h", "main", "10", "old", "wide"),
                (ROOT, "5i6h", "alt", "0", "old", "wide"),
                (c.as_str(), "4h5i", "main", "30", "new", "wide"),
            ],
            0,
        );
        assert!(!ok);
    }

    #[test]
    fn apply_stops_on_unbreakable_cycle_without_removable_since() {
        let c = c();
        let rows = [
            (ROOT, "5i4h", "main", "10"),
            (c.as_str(), "4h5i", "main", "30"),
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
            direct_lookup: false,
            direct_lookup_table: None,
            apply: true,
            note: "塞いだ".into(),
            removable_since: Vec::new(),
            entry_depth: 4,
            threads: 0,
        };
        assert!(run_from(args, ROOT).is_err());
        assert_eq!(std::fs::read_to_string(&master).unwrap(), master_text(&rows));
        assert!(dir.path().join("cycles.tsv").exists());
    }

    /// 乱数の元データと乱数の外してよい手・curated の手で、入る手を外してよくしたとき、塞げるなら塞げて
    /// 各局面に手が残り、外してよくした手はどれを戻しても根が抜けられないこと、塞げないなら curated 以外を
    /// 全部外してよくしても根が抜けられないことを確かめる。
    #[test]
    fn entry_moves_agree_with_full_permission() {
        let (mut fixed, mut failed) = (0, 0);
        for seed in 0..400u64 {
            let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
            let (p, root) = random_king_book(&mut rng);
            let values = evaluate(&p, DrawValues::default()).unwrap();
            let index = BookIndex::new(&p).unwrap();
            let graph = graph_of(&index, &root);
            let own_edge = |e: usize| {
                let v = (0..graph.len()).rfind(|&v| graph.edge_id(v, 0) <= e).unwrap();
                graph.own[v]
            };
            let allowed: Vec<bool> = (0..graph.edge_count())
                .map(|e| own_edge(e) && rng.random_range(0..3) == 0)
                .collect();
            let curated_edges: Vec<bool> =
                (0..graph.edge_count()).map(|_| rng.random_range(0..5) == 0).collect();
            let curated = |v: usize, i: usize| curated_edges[graph.edge_id(v, i)];
            let value =
                |v: usize, i: usize| values[graph.master(v)?][graph.edges(v)[i].book_move()?];
            let plan = plan_side(&graph, &values, &allowed).unwrap();
            if plan.unbreakable.is_empty() {
                continue;
            }
            let predecessors = Predecessors::new(&graph);
            match entry_moves(&graph, &allowed, graph.len(), &curated, &value) {
                Some((_, extra)) => {
                    let mut expanded = allowed.clone();
                    for &(v, i, _) in &extra {
                        assert!(!curated(v, i) && !allowed[graph.edge_id(v, i)], "seed {seed}");
                        expanded[graph.edge_id(v, i)] = true;
                    }
                    for &(v, i, _) in &extra {
                        expanded[graph.edge_id(v, i)] = false;
                        assert!(
                            attractor(&graph, &predecessors, &expanded)[0].is_none(),
                            "seed {seed}"
                        );
                        expanded[graph.edge_id(v, i)] = true;
                    }
                    let mut plan = plan_side(&graph, &values, &expanded).unwrap();
                    assert!(plan.unbreakable.is_empty(), "seed {seed}");
                    plan.new_main =
                        new_mains(&graph, &index, &values, &plan.removed, &HashSet::new()).unwrap();
                    let after_positions = apply_plans(&p, [&plan].into_iter());
                    assert!(after_positions.values().all(|m| !m.moves.is_empty()), "seed {seed}");
                    assert_eq!(cycles_of(&after_positions, &root)[1].components, 0, "seed {seed}");
                    fixed += 1;
                }
                None => {
                    let all: Vec<bool> = (0..graph.len())
                        .flat_map(|v| (0..graph.edges(v).len()).map(move |i| (v, i)))
                        .map(|(v, i)| {
                            graph.own[v] && !curated(v, i) || allowed[graph.edge_id(v, i)]
                        })
                        .collect();
                    assert!(attractor(&graph, &predecessors, &all)[0].is_none(), "seed {seed}");
                    failed += 1;
                }
            }
        }
        assert!(fixed > 10 && failed > 0, "{fixed} {failed}");
    }

    #[test]
    fn removable_since_keeps_other_moves() {
        // C の main 4h5i (since=old) を外せば 1 手で塞げるが、外してよいのは since=new の手だけなので、根の alt
        // 5i4h (new) を外す。
        let c = c();
        let rows = [
            (ROOT, "5i6h", "main", "0", "old"),
            (ROOT, "5i4h", "alt", "50", "new"),
            (c.as_str(), "4h5i", "main", "10", "old"),
            (c.as_str(), "4h3h", "alt", "0", "old"),
        ];
        let mut text = super::super::master_table::COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals, since) in rows {
            writeln!(
                text,
                "{}",
                crate::book_tree::master_table::test_line(&[
                    ("key", key.to_string()),
                    ("ply", "1".into()),
                    ("move", mv.to_string()),
                    ("role", role.to_string()),
                    ("source", "read".into()),
                    ("evals", evals.to_string()),
                    ("tier", "wide".into()),
                    ("since", since.to_string()),
                    ("note", "n".into())
                ])
            )
            .unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let master = dir.path().join("moves.tsv");
        std::fs::write(&master, &text).unwrap();
        let args = |removable_since: Vec<String>, table: &str| CyclesArgs {
            master: master.clone(),
            table: dir.path().join(table),
            side: SideArg::B,
            draw_value_black: 0,
            draw_value_white: 0,
            direct_lookup: false,
            direct_lookup_table: None,
            apply: false,
            note: "塞いだ".into(),
            removable_since,
            entry_depth: 0,
            threads: 0,
        };
        let removed = |table: &str| -> Vec<String> {
            std::fs::read_to_string(dir.path().join(table))
                .unwrap()
                .lines()
                .skip(1)
                .map(|l| l.split('\t').nth(3).unwrap().to_owned())
                .collect()
        };
        run_from(args(vec!["new".into()], "new.tsv"), ROOT).unwrap();
        assert_eq!(removed("new.tsv"), ["5i4h"]);
        // old の手だけを外してよいなら C の 4h5i (alt を先に外す規則でも、根の main 5i6h は循環の外)。
        run_from(args(vec!["old".into()], "old.tsv"), ROOT).unwrap();
        assert_eq!(removed("old.tsv"), ["4h5i"]);
        // どちらでもない tag なら外せる手が無く、(入る手も外さなければ) 塞げない循環として残す。
        run_from(args(vec!["none".into()], "none.tsv"), ROOT).unwrap();
        assert!(removed("none.tsv").is_empty());
    }

    #[test]
    fn removable_since_cuts_entry_when_cycle_moves_are_kept() {
        // E2 → X → Y → Z → E2 の循環の手 (X・Z、old) は外せない。X には別の手もあるが old なので外せない。
        // 手前の局面 E の 5h4h (new) を外せば根から届かなくなる。
        let e = after(ROOT, &["5i5h", "5a4a"]);
        let e2 = after(&e, &["5h4h"]);
        let x = after(&e2, &["4a5a"]);
        let z = after(&x, &["4h5i", "5a4a"]);
        let rows = [
            (ROOT, "5i5h", "main", "0", "old"),
            (e.as_str(), "5h4h", "main", "30", "new"),
            (e.as_str(), "5h6h", "alt", "0", "new"),
            (x.as_str(), "4h5i", "main", "0", "old"),
            (x.as_str(), "4h3h", "alt", "0", "old"),
            (z.as_str(), "5i4h", "main", "0", "old"),
        ];
        let mut text = super::super::master_table::COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals, since) in rows {
            writeln!(
                text,
                "{}",
                crate::book_tree::master_table::test_line(&[
                    ("key", key.to_string()),
                    ("ply", "1".into()),
                    ("move", mv.to_string()),
                    ("role", role.to_string()),
                    ("source", "read".into()),
                    ("evals", evals.to_string()),
                    ("tier", "wide".into()),
                    ("since", since.to_string()),
                    ("note", "n".into())
                ])
            )
            .unwrap();
        }
        let p = parse_master_rows(text.as_bytes()).unwrap().0;
        let values = evaluate(&p, DrawValues::default()).unwrap();
        let index = BookIndex::new(&p).unwrap();
        let graph = graph_of(&index, ROOT);
        let allowed: Vec<bool> = (0..graph.len())
            .flat_map(|v| std::iter::repeat_n(v, graph.edges(v).len()))
            .map(|v| graph.own[v] && graph.master(v).is_some_and(|m| index.key(m) == e))
            .collect();
        let plan = plan_side(&graph, &values, &allowed).unwrap();
        assert_eq!(plan.removed, [(e.clone(), 0)]);
        assert!(plan.unbreakable.is_empty());
    }

    /// 乱数の元データで、C (循環ごとの整数計画) の外す手の数が、循環を全部無くし各局面に手を残す組の総当たりの
    /// 最小と一致することを確かめる。外してよい手も乱数で限る。
    #[test]
    fn exact_matches_brute_force_minimum() {
        let mut checked = 0;
        for seed in 0..300u64 {
            let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
            let (p, root) = random_king_book(&mut rng);
            let index = BookIndex::new(&p).unwrap();
            let graph = graph_of(&index, &root);
            let allowed: Vec<bool> =
                (0..graph.edge_count()).map(|_| rng.random_range(0..4) != 0).collect();
            let allowed: Vec<bool> = (0..graph.len())
                .flat_map(|v| {
                    let own = graph.own[v];
                    (0..graph.edges(v).len()).map(move |i| (v, i, own))
                })
                .map(|(v, i, own)| own && allowed[graph.edge_id(v, i)])
                .collect();
            for component in graph.cycles(|_, _| true) {
                let inside: HashSet<usize> = component.iter().copied().collect();
                let candidates: Vec<(usize, usize)> = component
                    .iter()
                    .flat_map(|&v| (0..graph.edges(v).len()).map(move |i| (v, i)))
                    .filter(|&(v, i)| {
                        allowed[graph.edge_id(v, i)] && inside.contains(&graph.edges(v)[i].to())
                    })
                    .collect();
                if candidates.len() > 14 {
                    continue;
                }
                // 部分集合を総当たりし、各局面に手が残り、循環の中に循環が残らない組の最小の数。
                let best = (0u32..1 << candidates.len())
                    .filter(|mask| {
                        let off = |v: usize, i: usize| {
                            candidates
                                .iter()
                                .position(|&c| c == (v, i))
                                .is_some_and(|k| mask & (1 << k) != 0)
                        };
                        component.iter().all(|&v| {
                            !graph.own[v] || (0..graph.edges(v).len()).any(|i| !off(v, i))
                        }) && {
                            // 根から届くかによらず、循環の中だけの辺で循環が残らないこと。
                            let local: HashMap<usize, u32> =
                                component.iter().enumerate().map(|(k, &v)| (v, k as u32)).collect();
                            let adjacency: Vec<Vec<u32>> = component
                                .iter()
                                .map(|&v| {
                                    (0..graph.edges(v).len())
                                        .filter(|&i| !off(v, i))
                                        .filter_map(|i| local.get(&graph.edges(v)[i].to()).copied())
                                        .collect()
                                })
                                .collect();
                            nontrivial_sccs(&adjacency).is_empty()
                        }
                    })
                    .map(u32::count_ones)
                    .min();
                match (best, exact::solve_component(&graph, &allowed, &component)) {
                    (Some(n), exact::Outcome::Solved(removed)) => {
                        assert_eq!(removed.len(), n as usize, "seed {seed}");
                        checked += 1;
                    }
                    (None, exact::Outcome::Infeasible) => {}
                    (best, outcome) => panic!("seed {seed}: {best:?} {outcome:?}"),
                }
            }
        }
        assert!(checked > 20, "{checked}");
    }

    /// 玉だけの局面 (先手玉は 4〜6 筋の 7〜9 段、後手玉は 4〜6 筋の 1〜3 段) の乱数の元データと根。
    fn random_king_book(
        rng: &mut rand_chacha::ChaCha8Rng,
    ) -> (BTreeMap<String, MasterPosition>, String) {
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
                .filter(|&(f, r)| f.abs_diff(bk.0) <= 1 && r.abs_diff(bk.1) <= 1 && (f, r) != bk)
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
        (parse_master_rows(master_text(&rows).as_bytes()).unwrap().0, root)
    }

    /// 乱数の元データで、各局面に 1 手残すこと、外した後の循環が塞げない循環と一致すること、1 手ずつ残す組を
    /// 総当たりして循環を無くせるなら塞げない循環が残らないことを確かめる。
    #[test]
    fn random_books_agree_with_exhaustive_search() {
        let mut solvable_cycles = 0;
        for seed in 0..400u64 {
            let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
            let (p, root) = random_king_book(&mut rng);
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
