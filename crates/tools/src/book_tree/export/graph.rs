use super::super::{master_table::Row, reply_graph};
use crate::book_db::{position_from_sfen, strip_ply};
use anyhow::Result;
use rshogi_core::movegen::{MoveList, generate_legal_all};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) type Pair = (String, String);
pub(super) type Edges = BTreeMap<Pair, BTreeMap<String, String>>;
pub(super) fn continuations(rows: &[Row]) -> Result<Edges> {
    let keys: BTreeSet<_> = rows.iter().map(|r| r.get("key")).collect();
    let mut edges = Edges::new();
    for row in rows {
        let mut pos = position_from_sfen(&format!("{} 1", row.get("key")))?;
        reply_graph::apply_usi(&mut pos, row.get("move"))?;
        let mut legal = MoveList::new();
        generate_legal_all(&pos, &mut legal);
        let mut replies = BTreeMap::new();
        for &mv in legal.iter() {
            pos.do_move(mv, pos.gives_check(mv));
            let sfen = pos.to_sfen();
            let child = strip_ply(&sfen);
            if keys.contains(child) {
                replies.insert(mv.to_usi(), child.to_owned());
            }
            pos.undo_move(mv);
        }
        edges.insert((row.get("key").into(), row.get("move").into()), replies);
    }
    Ok(edges)
}
pub(super) fn depths(edges: &Edges, active: &BTreeSet<Pair>) -> BTreeMap<Pair, usize> {
    let keys: BTreeSet<_> = active.iter().map(|p| p.0.as_str()).collect();
    let index: BTreeMap<_, _> = keys.into_iter().enumerate().map(|(i, k)| (k, i)).collect();
    let mut graph = vec![Vec::new(); index.len()];
    for pair in active {
        for child in edges[pair].values() {
            if let Some(&i) = index.get(child.as_str()) {
                graph[index[pair.0.as_str()]].push(i as u32);
            }
        }
    }
    for children in &mut graph {
        children.sort_unstable();
        children.dedup();
    }
    let mut components = reply_graph::nontrivial_sccs(&graph);
    let mut component = vec![usize::MAX; index.len()];
    for (i, c) in components.iter().enumerate() {
        for &v in c {
            component[v as usize] = i;
        }
    }
    for (v, c) in component.iter_mut().enumerate() {
        if *c == usize::MAX {
            *c = components.len();
            components.push(vec![v as u32]);
        }
    }
    let mut dag = vec![BTreeSet::new(); components.len()];
    let mut parents = vec![BTreeSet::new(); components.len()];
    for (v, children) in graph.iter().enumerate() {
        for &w in children {
            let (a, b) = (component[v], component[w as usize]);
            if a != b {
                dag[a].insert(b);
                parents[b].insert(a);
            }
        }
    }
    let mut remaining: Vec<_> = dag.iter().map(BTreeSet::len).collect();
    let mut queue: VecDeque<_> =
        remaining.iter().enumerate().filter(|(_, n)| **n == 0).map(|(i, _)| i).collect();
    let mut longest = vec![0; components.len()];
    while let Some(i) = queue.pop_front() {
        longest[i] = 2 * (components[i].len() - 1)
            + dag[i].iter().map(|&c| 2 + longest[c]).max().unwrap_or(0);
        for &p in &parents[i] {
            remaining[p] -= 1;
            if remaining[p] == 0 {
                queue.push_back(p);
            }
        }
    }
    edges
        .iter()
        .map(|(pair, replies)| {
            let mut depth = 0;
            if active.contains(pair) {
                let source = component[index[pair.0.as_str()]];
                for child in replies.values() {
                    if child != &pair.0
                        && let Some(&v) = index.get(child.as_str())
                    {
                        let target = component[v];
                        depth = depth.max(if source == target {
                            longest[source]
                        } else {
                            2 + longest[target]
                        });
                    }
                }
            }
            (pair.clone(), depth)
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cycles_tail_records_and_self_loop() {
        let edges: Edges = [
            ("a", "b"),
            ("b", "a"),
            ("b", "c"),
            ("c", "d"),
            ("d", "d"),
            ("r", "a"),
        ]
        .into_iter()
        .map(|(a, b)| ((a.into(), b.into()), BTreeMap::from([("reply".into(), b.into())])))
        .collect();
        let active = edges.keys().filter(|p| p.0 != "r").cloned().collect();
        let depths = depths(&edges, &active);
        for (a, b, d) in [
            ("a", "b", 6),
            ("b", "a", 6),
            ("b", "c", 4),
            ("c", "d", 2),
            ("d", "d", 0),
            ("r", "a", 0),
        ] {
            assert_eq!(depths[&(a.into(), b.into())], d);
        }
    }
}
