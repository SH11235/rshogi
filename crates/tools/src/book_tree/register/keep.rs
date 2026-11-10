//! main を替える前に、元の main と木の 1 位を `master audit` と同じ伝播の値で比べる。

use super::Node;
use crate::book_backprop::{DrawValues, Graph, MoveValue};
use crate::book_tree::audit::{Fill, audit, audited_moves};
use crate::book_tree::master_table::{Row, positions_from_rows};
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
    pub line: i32,
    pub min_depth: usize,
    pub draws: DrawValues,
    pub fill: Fill,
}

fn mains(rows: &[Row]) -> BTreeMap<&str, &str> {
    rows.iter()
        .filter(|r| r.get("role") == "main")
        .map(|r| (r.get("key"), r.get("move")))
        .collect()
}

/// 手の値がいくつ先の局面の読みから来たか。最善の応手を辿り、読みの値で止まるまでの局面の数。補った相手の手番の
/// 局面で、着いた手の読みの値 (定跡に無い応手) が最善なら、その局面は数えない (値は手の読みそのもの)。
fn depth(graph: &Graph, master: &BTreeSet<&str>, start: MoveValue) -> usize {
    let mut depth = 0;
    let mut current = start;
    let mut seen = BTreeSet::new();
    while let Some(edge) = current.edge {
        if !seen.insert(edge.to) {
            break;
        }
        let Some(next) = graph.moves(edge.to).max_by_key(|m| m.new) else {
            break;
        };
        if next.edge.is_none() && !master.contains(graph.key(edge.to)) {
            break;
        }
        depth += 1;
        current = next;
    }
    depth
}

/// `before` から `after` (仮に登録した元データ) で main が替わった局面のうち、`after` を伝播した値で元の main が
/// 新しい main を `rule.line` 以上上回り、元の main の値が `rule.min_depth` 局面以上先の読みから来た局面。
pub(in crate::book_tree) fn kept_mains(
    before: &[Row],
    after: &[Row],
    nodes: &BTreeMap<String, Node>,
    rule: KeepRule,
) -> Result<BTreeMap<String, Kept>> {
    let mut kept = BTreeMap::new();
    if rule.line == 0 {
        return Ok(kept);
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
        return Ok(kept);
    }
    let book = positions_from_rows(after.iter().cloned().enumerate().map(|(i, r)| Ok((i + 2, r))))?;
    // 点検の表は使わないので、線を最大にして表を作らせない。
    let (_, _, audited) =
        audit(&book, rule.draws, i32::MAX, rule.fill, None).context("main を替える前の伝播")?;
    let index: BTreeMap<&str, usize> =
        book.0.keys().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
    let master: BTreeSet<&str> = book.0.keys().map(String::as_str).collect();
    for (key, old_move, new_move) in changed {
        let (Some(&i), Some(position)) = (index.get(key), book.0.get(key)) else {
            continue;
        };
        let known = &audited.known[i];
        let graph = audited.graph_for(key);
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
        if old_value - new_value < rule.line {
            continue;
        }
        let old_depth = depth(graph, &master, old);
        if old_depth >= rule.min_depth {
            let new_depth = depth(graph, &master, new);
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
    Ok(kept)
}
