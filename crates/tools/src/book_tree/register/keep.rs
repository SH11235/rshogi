//! main を替える前に、元の main と木の 1 位を `master audit` と同じ伝播の値で比べる。

use super::Node;
use crate::book_backprop::{DrawValues, Graph, MoveValue};
use crate::book_tree::audit::{Fill, audit_with_replies, audited_moves, opponent_replies};
use crate::book_tree::master_table::{MasterPosition, Row, positions_from_rows};
use crate::book_tree::switch::{SingleRead, SwitchRule, from_child};
use anyhow::{Context, Result};
use std::collections::{BTreeMap, BTreeSet};

/// main を残した局面の、仮に登録した元データを伝播した値 (手番の側から) と、その値がいくつ先の局面の読みから来たか。
#[derive(Debug, Clone, PartialEq)]
pub(in crate::book_tree) struct Kept {
    pub old_value: i32,
    pub old_depth: usize,
    pub new_value: i32,
    pub new_depth: usize,
}

impl Kept {
    pub fn gap(&self) -> i32 {
        self.old_value - self.new_value
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::book_tree) struct KeepRule {
    pub rule: SwitchRule,
    pub line: i32,
    pub min_depth: usize,
    pub draws: DrawValues,
    pub fill: Fill,
    /// 相手の手番の局面で、読みの値のある bench も伝播に使う (`master audit --opp-bench`)。
    pub opp_bench: bool,
}

fn mains(rows: &[Row]) -> BTreeMap<&str, &str> {
    rows.iter()
        .filter(|r| r.get("role") == "main")
        .map(|r| (r.get("key"), r.get("move")))
        .collect()
}

/// 手の値がいくつ先の局面の読みから来たか。最善の手を辿り、読みの値で止まるまでの局面の数。着いた手の読みの
/// 値を最後の手として残した局面 (`capped`) で、その値が最善なら、その局面は数えない (値は手の読みそのもの)。循環に
/// 入ったら循環の局面までを数える。局面ごとの数は辿り始めによらないので、辿った局面の数を覚えて使い回す。
struct Depths<'a> {
    graph: &'a Graph,
    capped: &'a BTreeSet<String>,
    memo: BTreeMap<usize, usize>,
}

impl Depths<'_> {
    fn of_move(&mut self, mv: MoveValue) -> usize {
        mv.edge.map_or(0, |edge| self.of_node(edge.to))
    }

    fn of_node(&mut self, start: usize) -> usize {
        let mut path: Vec<usize> = Vec::new();
        let mut on_path = BTreeMap::new();
        let mut node = start;
        let mut depth = loop {
            if let Some(&known) = self.memo.get(&node) {
                break known;
            }
            if let Some(&at) = on_path.get(&node) {
                let cycle = path.len() - at;
                for &v in &path[at..] {
                    self.memo.insert(v, cycle);
                }
                path.truncate(at);
                break cycle;
            }
            let count = self.graph.moves(node).count();
            let Some((i, best)) = self.graph.moves(node).enumerate().max_by_key(|(_, m)| m.new)
            else {
                self.memo.insert(node, 0);
                break 0;
            };
            let Some(edge) = best.edge else {
                let kept_read = self.capped.contains(self.graph.key(node)) && i + 1 == count;
                let own = usize::from(!kept_read);
                self.memo.insert(node, own);
                break own;
            };
            on_path.insert(node, path.len());
            path.push(node);
            node = edge.to;
        };
        for &v in path.iter().rev() {
            depth += 1;
            self.memo.insert(v, depth);
        }
        self.memo[&start]
    }
}

/// `before` から `after` (仮に登録した元データ) で main が替わった局面のうち、main を替えずに残す局面と、そのうち
/// 片方の値が 1 回の読みの局面。`SwitchRule::Line` では `after` を伝播した値で元の main が新しい main を
/// `rule.line` 以上上回り、元の main の値が `rule.min_depth` 局面以上先の読みから来た局面を残す。
/// `SwitchRule::Propagated` では、両方の値が手の先の局面から来て新しい main が上回る局面だけを替える。
pub(in crate::book_tree) fn kept_mains(
    before: &[Row],
    after: &[Row],
    nodes: &BTreeMap<String, Node>,
    rule: KeepRule,
) -> Result<(BTreeMap<String, Kept>, Vec<SingleRead>)> {
    kept_mains_in(before, after, nodes, rule, &BTreeMap::new(), || {
        positions_from_rows(after.iter().cloned().enumerate().map(|(i, r)| Ok((i + 2, r))))
    })
}

/// [`kept_mains`] と同じ。`before`・`after` は `nodes` の局面の行を全部含めばよく、伝播する `after` の全局面は
/// `book` で作る (main が替わった局面があるときだけ呼ぶ。record の行の数も返す)。`after` に無い局面の bench の手
/// ([`opponent_replies`]) は `rest_bench` で渡す。
pub(in crate::book_tree) fn kept_mains_in(
    before: &[Row],
    after: &[Row],
    nodes: &BTreeMap<String, Node>,
    rule: KeepRule,
    rest_bench: &BTreeMap<String, MasterPosition>,
    book: impl FnOnce() -> Result<(BTreeMap<String, MasterPosition>, usize)>,
) -> Result<(BTreeMap<String, Kept>, Vec<SingleRead>)> {
    let mut kept = BTreeMap::new();
    let mut singles = Vec::new();
    if rule.rule == SwitchRule::Line && rule.line == 0 {
        return Ok((kept, singles));
    }
    let old = mains(before);
    let new = mains(after);
    let changed: Vec<(&str, &str, &str)> = nodes
        .keys()
        .filter_map(|key| {
            let (o, n) = (old.get(key.as_str())?, new.get(key.as_str())?);
            (o != n).then_some((key.as_str(), *o, *n))
        })
        .collect();
    if changed.is_empty() {
        return Ok((kept, singles));
    }
    let book = book()?;
    // 点検の表は使わないので、線を最大にして表を作らせない。
    let replies = rule
        .opp_bench
        .then(|| -> Result<_> {
            let mut replies = opponent_replies(after, &["bench"])?;
            replies
                .extend(rest_bench.iter().map(|(key, position)| (key.clone(), position.clone())));
            Ok(replies)
        })
        .transpose()?;
    let (_, _, audited) =
        audit_with_replies(&book, rule.draws, i32::MAX, rule.fill, replies.as_ref())
            .context("main を替える前の伝播")?;
    let index: BTreeMap<&str, usize> =
        book.0.keys().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    // 相手の bench を使う伝播では、局面の手番ごとにグラフが分かれる。
    let mut depths = audited.graphs().map(|graph| Depths {
        graph,
        capped: &audited.capped,
        memo: BTreeMap::new(),
    });
    for (key, old_move, new_move) in changed {
        let (Some(&i), Some(position)) = (index.get(key), book.0.get(key)) else {
            continue;
        };
        let known = &audited.known[i];
        let white = usize::from(key.split(' ').nth(1) == Some("w"));
        let graph = audited.graphs()[white];
        let depths = &mut depths[white];
        let Some(node) = graph.find(key) else {
            continue;
        };
        let moves = audited_moves(graph, key, position, known)?;
        let in_graph: Vec<MoveValue> = graph.moves(node).collect();
        let value = |mv: &str| -> Option<(i32, MoveValue)> {
            let j = position.moves.iter().position(|m| m.mv == mv)?;
            let audited = moves[j].as_ref()?;
            // グラフには値の分かる手だけが元データの順で入っている。
            let g = known[..j].iter().filter(|&&k| k).count();
            // 同値の -1 は書き出しで 1 手を選ぶための印なので、比べるときは戻す。
            Some((audited.value + i32::from(audited.tie_lowered), in_graph[g]))
        };
        // 値の分からない手は比べられないので、従来どおり木の 1 位に替える。
        let (Some((old_value, old)), Some((new_value, new))) = (value(old_move), value(new_move))
        else {
            continue;
        };
        if rule.rule == SwitchRule::Propagated {
            let capped = |k: &str| audited.capped.contains(k);
            let (old_from, new_from) =
                (from_child(graph, capped, &old), from_child(graph, capped, &new));
            if old_from && new_from && new_value > old_value {
                continue;
            }
            if !(old_from && new_from) {
                let moves = [(old_from, old_move), (new_from, new_move)]
                    .into_iter()
                    .filter(|(from, _)| !from)
                    .map(|(_, mv)| mv.to_owned())
                    .collect();
                singles.push(SingleRead {
                    key: key.to_owned(),
                    ply: position.ply.clone(),
                    main: old_move.to_owned(),
                    main_value: old_value,
                    best: new_move.to_owned(),
                    best_value: new_value,
                    moves,
                    weight: None,
                });
            }
            let (old_depth, new_depth) = (depths.of_move(old), depths.of_move(new));
            kept.insert(
                key.to_owned(),
                Kept {
                    old_value,
                    old_depth,
                    new_value,
                    new_depth,
                },
            );
            continue;
        }
        if old_value - new_value < rule.line {
            continue;
        }
        let old_depth = depths.of_move(old);
        if old_depth >= rule.min_depth {
            let new_depth = depths.of_move(new);
            kept.insert(
                key.to_owned(),
                Kept {
                    old_value,
                    old_depth,
                    new_value,
                    new_depth,
                },
            );
        }
    }
    Ok((kept, singles))
}
