//! 自分の定跡の手と、相手の (定跡に無い手を含む) 合法手のうち定跡の局面に戻る手を辺にしたグラフ。
//! 対局で RAMU が定跡を指し続けられる局面を、初期局面から辿る。先後反転した局面は引かない
//! (対局の設定 `FlippedBook=false` と同じ)。

use super::audit::MasterPosition;
use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
use anyhow::Result;
use clap::ValueEnum;
use rshogi_core::movegen::{MoveList, generate_legal};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

/// 初期局面の key。
pub(super) const HIRATE_KEY: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";

/// 自分の手番で指す手の範囲 (書き出しのプリセットと同じ)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum Preset {
    /// main だけ
    Default,
    /// main と alt
    Random,
}

/// 自分の側。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(super) enum SideArg {
    /// 先手
    B,
    /// 後手
    W,
    /// 先手と後手
    Both,
}

impl SideArg {
    pub fn sides(self) -> &'static [&'static str] {
        match self {
            SideArg::B => &["b"],
            SideArg::W => &["w"],
            SideArg::Both => &["b", "w"],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReplyEdge {
    pub mv: String,
    pub to: usize,
    /// 自分の手なら、元データの局面の手の番号 (`MasterPosition::moves` の添字)。
    pub book_move: Option<usize>,
}

/// 頂点 0 が根。頂点はすべて根から辿れる。
#[derive(Debug)]
pub(super) struct ReplyGraph {
    pub keys: Vec<String>,
    /// 自分の手番の局面か。
    pub own: Vec<bool>,
    pub edges: Vec<Vec<ReplyEdge>>,
    /// 自分の手のうち、非合法で辺にしなかった手の数。
    pub illegal_moves: usize,
}

impl ReplyGraph {
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// 根から辿れる自明でない強連結成分 (循環)。`active(頂点, 辺の番号)` が偽の辺は使わない。
    /// 成分の中は頂点の番号順、成分は先頭の頂点の番号順。
    pub fn cycles(&self, active: impl Fn(usize, usize) -> bool) -> Vec<Vec<usize>> {
        let mut reached = vec![false; self.len()];
        let mut order = Vec::new();
        let mut stack = vec![0];
        reached[0] = true;
        while let Some(v) = stack.pop() {
            order.push(v);
            for (i, e) in self.edges[v].iter().enumerate() {
                if active(v, i) && !reached[e.to] {
                    reached[e.to] = true;
                    stack.push(e.to);
                }
            }
        }
        order.sort_unstable();
        let local: HashMap<usize, u32> =
            order.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();
        let adjacency: Vec<Vec<u32>> = order
            .iter()
            .map(|&v| {
                self.edges[v]
                    .iter()
                    .enumerate()
                    .filter(|&(i, _)| active(v, i))
                    .map(|(_, e)| local[&e.to])
                    .collect()
            })
            .collect();
        nontrivial_sccs(&adjacency)
            .into_iter()
            .map(|c| c.into_iter().map(|i| order[i as usize]).collect())
            .collect()
    }
}

/// 自分の手番の局面で使う手の番号。`removed` の手は使わない。
pub(super) fn selected_moves(
    position: &MasterPosition,
    preset: Preset,
    removed: Option<&[bool]>,
) -> Vec<usize> {
    let limit = match preset {
        Preset::Default => 1,
        Preset::Random => position.moves.len(),
    };
    (0..position.moves.len())
        .filter(|&i| removed.is_none_or(|r| !r[i]))
        .take(limit)
        .collect()
}

/// 元データの局面のハッシュ値。相手の合法手の行き先が元データにあるかを、SFEN を作らずに当たりを付ける。
pub(super) struct BookHashes(HashSet<u64>);

impl BookHashes {
    pub fn new(positions: &BTreeMap<String, MasterPosition>) -> Result<Self> {
        positions
            .keys()
            .map(|key| Ok(position_from_sfen(&format!("{key} 1"))?.key()))
            .collect::<Result<_>>()
            .map(Self)
    }
}

/// `side` を自分として `start` から辿ったグラフを作る。自分の手番の局面では `select` の手を指し、
/// 相手の手番の局面では、合法手のうち元データの局面に戻る手を辺にする。`hashes` は `positions` の key から作る。
///
/// 計算量: 相手の手番の局面ごとに全合法手 (平均 80 前後) を 1 手ずつ進め、ハッシュ値が元データの局面と
/// 一致した手だけ SFEN を作って確かめる。
pub(super) fn build(
    positions: &BTreeMap<String, MasterPosition>,
    hashes: &BookHashes,
    side: &str,
    start: &str,
    mut select: impl FnMut(&str, &MasterPosition) -> Vec<usize>,
) -> Result<ReplyGraph> {
    let mut graph = ReplyGraph {
        keys: Vec::new(),
        own: Vec::new(),
        edges: Vec::new(),
        illegal_moves: 0,
    };
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut intern = |graph: &mut ReplyGraph, queue: &mut VecDeque<usize>, key: String| {
        if let Some(&v) = index.get(&key) {
            return v;
        }
        let v = graph.keys.len();
        graph.own.push(key.split(' ').nth(1) == Some(side));
        graph.keys.push(key.clone());
        graph.edges.push(Vec::new());
        index.insert(key, v);
        queue.push_back(v);
        v
    };
    let mut queue = VecDeque::new();
    intern(&mut graph, &mut queue, start.to_owned());
    while let Some(v) = queue.pop_front() {
        let key = graph.keys[v].clone();
        let mut pos = position_from_sfen(&format!("{key} 1"))?;
        let mut edges = Vec::new();
        if graph.own[v] {
            let Some(position) = positions.get(&key) else {
                continue;
            };
            for i in select(&key, position) {
                let mv = &position.moves[i].mv;
                let mut child = pos.clone();
                if apply_usi_move(&mut child, mv).is_err() {
                    graph.illegal_moves += 1;
                    continue;
                }
                let to = intern(&mut graph, &mut queue, strip_ply(&child.to_sfen()).to_owned());
                edges.push(ReplyEdge {
                    mv: mv.clone(),
                    to,
                    book_move: Some(i),
                });
            }
        } else {
            let mut list = MoveList::new();
            generate_legal(&pos, &mut list);
            for &mv in list.iter() {
                pos.do_move(mv, pos.gives_check(mv));
                let child =
                    hashes.0.contains(&pos.key()).then(|| strip_ply(&pos.to_sfen()).to_owned());
                pos.undo_move(mv);
                if let Some(child) = child.filter(|c| positions.contains_key(c)) {
                    let to = intern(&mut graph, &mut queue, child);
                    edges.push(ReplyEdge {
                        mv: mv.to_usi(),
                        to,
                        book_move: None,
                    });
                }
            }
        }
        graph.edges[v] = edges;
    }
    Ok(graph)
}

/// 自明でない強連結成分 (頂点 2 つ以上)。成分の中は番号順、成分は先頭の番号順。
pub(super) fn nontrivial_sccs(adjacency: &[Vec<u32>]) -> Vec<Vec<u32>> {
    const UNSEEN: u32 = u32::MAX;
    let n = adjacency.len();
    let mut index = vec![UNSEEN; n];
    let mut low = vec![0u32; n];
    let mut on_stack = vec![false; n];
    let mut stack: Vec<u32> = Vec::new();
    let mut components = Vec::new();
    let mut next = 0u32;
    // (頂点, 次に見る辺の番号) の明示的な再帰スタック。
    let mut calls: Vec<(u32, usize)> = Vec::new();
    for root in 0..n as u32 {
        if index[root as usize] != UNSEEN {
            continue;
        }
        calls.push((root, 0));
        index[root as usize] = next;
        low[root as usize] = next;
        next += 1;
        stack.push(root);
        on_stack[root as usize] = true;
        while let Some(&mut (v, ref mut i)) = calls.last_mut() {
            if let Some(&w) = adjacency[v as usize].get(*i) {
                *i += 1;
                if index[w as usize] == UNSEEN {
                    index[w as usize] = next;
                    low[w as usize] = next;
                    next += 1;
                    stack.push(w);
                    on_stack[w as usize] = true;
                    calls.push((w, 0));
                } else if on_stack[w as usize] {
                    low[v as usize] = low[v as usize].min(index[w as usize]);
                }
                continue;
            }
            calls.pop();
            if let Some(&(parent, _)) = calls.last() {
                low[parent as usize] = low[parent as usize].min(low[v as usize]);
            }
            if low[v as usize] == index[v as usize] {
                let mut component = Vec::new();
                loop {
                    let w = stack.pop().expect("Tarjan のスタック");
                    on_stack[w as usize] = false;
                    component.push(w);
                    if w == v {
                        break;
                    }
                }
                if component.len() >= 2 {
                    component.sort_unstable();
                    components.push(component);
                }
            }
        }
    }
    components.sort_unstable_by_key(|c| c[0]);
    components
}

/// テスト用の小さい元データ。
#[cfg(test)]
pub(super) mod fixture {
    use super::super::audit::{MasterPosition, parse_master_rows};
    use super::super::register::COLUMNS;
    use crate::book_db::{apply_usi_move, position_from_sfen, strip_ply};
    use std::collections::BTreeMap;
    use std::fmt::Write as _;

    /// 玉だけの局面。先手番。
    pub const ROOT: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";

    /// `key` から手を進めた局面の key。
    pub fn after(key: &str, moves: &[&str]) -> String {
        let mut pos = position_from_sfen(&format!("{key} 1")).unwrap();
        for mv in moves {
            apply_usi_move(&mut pos, mv).unwrap();
        }
        strip_ply(&pos.to_sfen()).to_owned()
    }

    /// (key, 手, role, evals) の行から元データの表を作る。
    pub fn master_text(rows: &[(&str, &str, &str, &str)]) -> String {
        let mut text = COLUMNS.join("\t");
        text.push('\n');
        for (key, mv, role, evals) in rows {
            writeln!(text, "{key}\t1\t{mv}\t{role}\tread\t{evals}\t\t\twide\tt\tn").unwrap();
        }
        text
    }

    pub fn positions(rows: &[(&str, &str, &str, &str)]) -> BTreeMap<String, MasterPosition> {
        parse_master_rows(master_text(rows).as_bytes()).unwrap().0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nontrivial_sccs_finds_cycles_only() {
        // 0 -> 1 -> 2 -> 0、2 -> 3、3 -> 4 -> 3、5 は孤立。
        let adjacency = vec![vec![1], vec![2], vec![0, 3], vec![4], vec![3], vec![]];
        assert_eq!(nontrivial_sccs(&adjacency), vec![vec![0, 1, 2], vec![3, 4]]);
        assert!(nontrivial_sccs(&[vec![1], vec![2], vec![]]).is_empty());
    }

    #[test]
    fn build_follows_own_book_moves_and_opponent_moves_back_into_book() {
        use fixture::{ROOT, after, positions};
        let a = after(ROOT, &["5i4h"]);
        let c = after(&a, &["5a5b"]);
        let p = positions(&[
            (ROOT, "5i4h", "main", "10"),
            (ROOT, "5i6h", "alt", "5"),
            (&c, "4h5i", "main", "30"),
        ]);
        let hashes = BookHashes::new(&p).unwrap();
        let graph =
            build(&p, &hashes, "b", ROOT, |_, m| selected_moves(m, Preset::Random, None)).unwrap();
        let moves = |key: &str| -> Vec<String> {
            let v = graph.keys.iter().position(|k| k == key).unwrap();
            graph.edges[v]
                .iter()
                .map(|e| format!("{}>{}", e.mv, graph.keys[e.to]))
                .collect()
        };
        assert_eq!(
            moves(ROOT),
            [
                format!("5i4h>{a}"),
                format!("5i6h>{}", after(ROOT, &["5i6h"]))
            ]
        );
        // 相手の手番では、合法手のうち元データの局面に戻る手だけが辺になる。
        assert_eq!(moves(&a), [format!("5a5b>{c}")]);
        // C から 4h5i、相手が 5b5a で根に戻る循環。
        assert_eq!(graph.cycles(|_, _| true).len(), 1);
        let default =
            build(&p, &hashes, "b", ROOT, |_, m| selected_moves(m, Preset::Default, None)).unwrap();
        assert_eq!(default.edges[0].len(), 1);
    }
}
