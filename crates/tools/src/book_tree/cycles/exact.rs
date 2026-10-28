//! 循環 (元の強連結成分) ごとに、外す手の数が最小の組を整数計画で求める。
//!
//! 変数は循環の中の局面へ行く外せる自分の手 (外すなら 1)。制約は、自分の手番の局面に手が 1 つ以上残ることと、
//! 循環ごとに、その循環の外せる手を 1 つ以上外すこと。循環は全部を最初に並べず、解いた組で残る循環を見つける
//! たびに足す (足した制約は全部の循環の制約の一部なので、残る循環が無くなった解がそのまま最小になる)。

use super::super::reply_graph::{ReplyGraph, nontrivial_sccs};
use microlp::{
    ComparisonOp, OptimizationDirection, Problem, SolutionStatus, SolveOptions, SolveOutcome,
};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet, VecDeque};

/// 循環 1 つで、解き直しの全部を通して探す分枝の数の上限。時間でなく分枝の数で打ち切るので、結果は機械の
/// 速さによらない。microlp は未探索の分枝ごとに変数と制約の数に比例する基底を持つので、下の変数・制約の
/// 上限と合わせて 1 つの循環のメモリを抑える (未探索の分枝 × (変数 + 制約) が数千万要素まで)。
const NODE_BUDGET: u64 = 10_000;
/// 循環 1 つで、解き直しの全部を通して使う単体法のピボットの数の上限。解き直すたびに制約が増えて 1 回の
/// 整数計画が重くなるので、分枝の数とは別に時間を抑える (乱数で作った密な循環 (変数 188〜811) で、分枝かピボットの
/// 上限に当たるまで 6〜13 秒)。1 回の解く途中では止められないので、解き直しの前に確かめる。
const PIVOT_BUDGET: u64 = 300_000;
/// 変数 (循環の中の外せる手) の数の上限。超える循環は解かずに A の組を使う。
const MAX_VARS: usize = 1_000;
/// 足す循環の制約の数の上限。
const MAX_CUTS: usize = 2_000;
/// 解き直す回数の上限。
const MAX_ROUNDS: usize = 200;
/// 1 回に、残る循環 1 つあたり足す制約の数の上限 (始点を変えた最短の循環)。
const CUTS_PER_CYCLE: usize = 16;

/// 循環ごとの結果を元の循環の順に並べたもの。
pub(super) struct ExactRemovals {
    /// 外す手 (局面, 辺の番号)。厳密に解けなかった循環は `fallback` の手。
    pub removed: Vec<(usize, usize)>,
    pub counts: ExactCounts,
}

/// 循環の数を結果ごとに数えたもの。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in super::super) struct ExactCounts {
    /// 厳密に解けた。
    pub solved: usize,
    /// 循環の中の外せる手だけでは塞げない (外せる手が無い、手を残すと塞げない)。
    pub infeasible: usize,
    /// 分枝の数か解き直す回数の上限に当たった。
    pub limit: usize,
}

/// 1 つの循環の結果。
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Outcome {
    Solved(Vec<(usize, usize)>),
    Infeasible,
    Limit,
}

/// `allowed[辺の番号]` が真の自分の手だけを外す。厳密に解けない循環 (上限に当たる、外せる手の無い循環が残る、
/// 手を残せない) では、同じ循環の `fallback` を使う。
pub(super) fn exact_removals(
    graph: &ReplyGraph,
    allowed: &[bool],
    components: &[Vec<usize>],
    fallback: &[Vec<(usize, usize)>],
) -> ExactRemovals {
    let parts: Vec<Outcome> = components
        .par_iter()
        .map(|component| solve_component(graph, allowed, component))
        .collect();
    let mut counts = ExactCounts::default();
    let mut removed = Vec::new();
    for (part, fallback) in parts.into_iter().zip(fallback) {
        match part {
            Outcome::Solved(part) => {
                counts.solved += 1;
                removed.extend(part);
            }
            Outcome::Infeasible => {
                counts.infeasible += 1;
                removed.extend_from_slice(fallback);
            }
            Outcome::Limit => {
                counts.limit += 1;
                removed.extend_from_slice(fallback);
            }
        }
    }
    ExactRemovals { removed, counts }
}

/// 1 つの循環を解く。
pub(super) fn solve_component(
    graph: &ReplyGraph,
    allowed: &[bool],
    component: &[usize],
) -> Outcome {
    let local: HashMap<usize, u32> =
        component.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();
    // 循環の中の辺: (局面の局所番号ごとに) (辺の番号, 行き先の局所番号)。
    let inner: Vec<Vec<(usize, u32)>> = component
        .iter()
        .map(|&v| {
            graph
                .edges(v)
                .iter()
                .enumerate()
                .filter_map(|(i, e)| local.get(&e.to()).map(|&w| (i, w)))
                .collect()
        })
        .collect();
    // 変数: 循環の中の局面へ行く外せる自分の手。辺の全体の番号から変数の番号を引く。
    let mut vars: Vec<(usize, usize)> = Vec::new();
    let mut var_of: HashMap<usize, usize> = HashMap::new();
    for (u, &v) in component.iter().enumerate() {
        if !graph.own[v] {
            continue;
        }
        for &(i, _) in &inner[u] {
            if allowed[graph.edge_id(v, i)] {
                var_of.insert(graph.edge_id(v, i), vars.len());
                vars.push((v, i));
            }
        }
    }
    if vars.is_empty() {
        return Outcome::Infeasible;
    }
    if vars.len() > MAX_VARS {
        return Outcome::Limit;
    }
    // 手の数を先に、同じなら main を外す数を少なく (1 手の重さを main の数の合計より大きくする)。
    let weight = (vars.len() + 1) as f64;
    let cost: Vec<f64> = vars
        .iter()
        .map(|&(v, i)| weight + f64::from(u8::from(graph.edges(v)[i].book_move() == Some(0))))
        .collect();
    // 手を残す制約: 局面の手が全部変数なら、外すのは手の数 - 1 まで。
    let mut keep: Vec<Vec<usize>> = Vec::new();
    for &v in component.iter().filter(|&&v| graph.own[v]) {
        let mine: Vec<usize> = (0..graph.edges(v).len())
            .filter_map(|i| var_of.get(&graph.edge_id(v, i)).copied())
            .collect();
        if mine.len() == graph.edges(v).len() {
            keep.push(mine);
        }
    }
    let mut cuts: Vec<Vec<usize>> = Vec::new();
    let mut seen_cuts: HashSet<Vec<usize>> = HashSet::new();
    let mut budget = Budget {
        nodes: NODE_BUDGET,
        pivots: PIVOT_BUDGET,
    };
    for _ in 0..MAX_ROUNDS {
        if cuts.len() > MAX_CUTS || budget.pivots == 0 {
            return Outcome::Limit;
        }
        let chosen = match solve(&cost, &keep, &cuts, &mut budget) {
            Ok(chosen) => chosen,
            Err(outcome) => return outcome,
        };
        let removed: HashSet<usize> =
            chosen.iter().map(|&x| graph.edge_id(vars[x].0, vars[x].1)).collect();
        let live = |v: usize, i: usize| !removed.contains(&graph.edge_id(v, i));
        let adjacency: Vec<Vec<u32>> = component
            .iter()
            .zip(&inner)
            .map(|(&v, edges)| {
                edges.iter().filter(|&&(i, _)| live(v, i)).map(|&(_, w)| w).collect()
            })
            .collect();
        let remaining = nontrivial_sccs(&adjacency);
        if remaining.is_empty() {
            let mut result: Vec<(usize, usize)> = chosen.into_iter().map(|x| vars[x]).collect();
            result.sort_unstable();
            return Outcome::Solved(result);
        }
        let mut added = false;
        for scc in &remaining {
            let members: HashSet<u32> = scc.iter().copied().collect();
            for &start in scc.iter().take(CUTS_PER_CYCLE) {
                let cycle = shortest_cycle(&adjacency, &members, start)
                    .expect("強連結成分の頂点を通る循環がある");
                // 循環の辺 (局所番号の並び) を、外せる手の変数にする。
                let mut cut: Vec<usize> = Vec::new();
                for pair in cycle.windows(2) {
                    let (from, to) = (pair[0], pair[1]);
                    let v = component[from as usize];
                    for &(i, w) in &inner[from as usize] {
                        if w == to
                            && live(v, i)
                            && let Some(&x) = var_of.get(&graph.edge_id(v, i))
                        {
                            cut.push(x);
                        }
                    }
                }
                if cut.is_empty() {
                    return Outcome::Infeasible;
                }
                cut.sort_unstable();
                cut.dedup();
                if seen_cuts.insert(cut.clone()) {
                    cuts.push(cut);
                    added = true;
                }
            }
        }
        // 残る循環の制約は今の解を除くので必ず新しい。
        debug_assert!(added);
    }
    Outcome::Limit
}

/// `start` から出て `start` に戻る、`members` の中だけを通る最短の循環 (局所番号の列、最初と最後が `start`)。
/// 同じ 2 局面の間に辺が複数あれば全部を制約に入れるので、局面の列で表す。
fn shortest_cycle(adjacency: &[Vec<u32>], members: &HashSet<u32>, start: u32) -> Option<Vec<u32>> {
    let mut parent: HashMap<u32, u32> = HashMap::new();
    let mut queue = VecDeque::from([start]);
    while let Some(v) = queue.pop_front() {
        for &w in &adjacency[v as usize] {
            if !members.contains(&w) {
                continue;
            }
            if w == start {
                let mut path = vec![start, v];
                let mut u = v;
                while u != start {
                    u = parent[&u];
                    path.push(u);
                }
                path.reverse();
                return Some(path);
            }
            if let std::collections::hash_map::Entry::Vacant(e) = parent.entry(w) {
                e.insert(v);
                queue.push_back(w);
            }
        }
    }
    None
}

/// 循環 1 つで残っている計算量。
struct Budget {
    nodes: u64,
    pivots: u64,
}

/// 0-1 整数計画を解き、1 の変数の番号を返す。`budget` から探した分枝とピボットの数を引く。
fn solve(
    cost: &[f64],
    keep: &[Vec<usize>],
    cuts: &[Vec<usize>],
    budget: &mut Budget,
) -> Result<Vec<usize>, Outcome> {
    let mut problem = Problem::new(OptimizationDirection::Minimize);
    let vars: Vec<_> = cost.iter().map(|&c| problem.add_binary_var(c)).collect();
    for group in keep {
        let expr: Vec<_> = group.iter().map(|&x| (vars[x], 1.0)).collect();
        problem.add_constraint(expr.as_slice(), ComparisonOp::Le, (group.len() - 1) as f64);
    }
    for cut in cuts {
        let expr: Vec<_> = cut.iter().map(|&x| (vars[x], 1.0)).collect();
        problem.add_constraint(expr.as_slice(), ComparisonOp::Ge, 1.0);
    }
    let mut options = SolveOptions::default();
    options.node_limit = Some(budget.nodes);
    match problem.solve_with(options) {
        Ok(SolveOutcome::Solution(solution)) if solution.status() == SolutionStatus::Optimal => {
            let stats = solution.stats();
            budget.nodes = budget.nodes.saturating_sub(stats.nodes_solved);
            budget.pivots = budget.pivots.saturating_sub(stats.lp_iterations);
            Ok((0..cost.len()).filter(|&x| solution.var_value(vars[x]) > 0.5).collect())
        }
        Err(microlp::Error::Infeasible) => Err(Outcome::Infeasible),
        // 上限に当たった (最適と示せない)、または数値の問題で続けられない。
        _ => Err(Outcome::Limit),
    }
}
