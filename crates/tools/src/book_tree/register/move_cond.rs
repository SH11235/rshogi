//! 読みの条件が混ざる木で、手ごとの `cond` を決める。

use super::{Node, expanded};
use crate::book_tree::model::child;
use anyhow::Result;
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq)]
enum Known {
    None,
    One(usize),
    Mixed,
}

/// 手の値が依る読みの条件。
#[derive(Clone, Copy)]
struct Deps {
    known: Known,
    /// 条件を確かめられない読みにも依る。
    unsure: bool,
}

impl Deps {
    fn merge(&mut self, other: Self) {
        self.unsure |= other.unsure;
        self.known = match (self.known, other.known) {
            (Known::None, known) | (known, Known::None) => known,
            (Known::One(a), Known::One(b)) if a == b => Known::One(a),
            _ => Known::Mixed,
        };
    }

    fn name(self, names: &[&str]) -> String {
        match self.known {
            Known::Mixed => "mixed".into(),
            Known::One(id) if !self.unsure => names[id].into(),
            _ => "unknown".into(),
        }
    }
}

struct Position {
    /// この局面の読みの条件 (`names` の番号)。
    reading: Option<usize>,
    /// 値がどの読みから来たかを辿れない手がある。
    untraceable: bool,
    /// 手ごとの、同じ木にある行き先の局面。
    destinations: Vec<Option<usize>>,
}

/// 局面ごとに、その局面から同じ木の中を辿れる読みの条件をまとめる (局面自身の読みを含む)。
///
/// 伝播は行き先の全部の手の値から 1 位を選ぶので、辿れる読みを全部数える。循環の中の値は辿る手順で変わるので、
/// 循環に入る局面は確かめられない扱いにする。
fn reachable(positions: &[Position]) -> Vec<Deps> {
    let n = positions.len();
    let edges = |i: usize| positions[i].destinations.iter().flatten().copied();
    // 強連結成分を、行き先の成分から先に決まる順でまとめる (帰りがけ順の逆から逆向きの辺を辿る)。
    let mut seen = vec![false; n];
    let mut order = Vec::with_capacity(n);
    for root in 0..n {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        let mut stack = vec![(root, edges(root))];
        while let Some((i, rest)) = stack.last_mut() {
            match rest.next() {
                Some(j) if !seen[j] => {
                    seen[j] = true;
                    stack.push((j, edges(j)));
                }
                Some(_) => {}
                None => {
                    order.push(*i);
                    stack.pop();
                }
            }
        }
    }
    let mut reverse = vec![Vec::new(); n];
    for i in 0..n {
        for j in edges(i) {
            reverse[j].push(i);
        }
    }
    let mut component = vec![usize::MAX; n];
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for &root in order.iter().rev() {
        if component[root] != usize::MAX {
            continue;
        }
        component[root] = groups.len();
        let mut group = Vec::new();
        let mut stack = vec![root];
        while let Some(i) = stack.pop() {
            group.push(i);
            for &j in &reverse[i] {
                if component[j] == usize::MAX {
                    component[j] = groups.len();
                    stack.push(j);
                }
            }
        }
        groups.push(group);
    }
    let mut of_group = vec![
        Deps {
            known: Known::None,
            unsure: false,
        };
        groups.len()
    ];
    for (id, group) in groups.iter().enumerate().rev() {
        let mut all = of_group[id];
        for &i in group {
            all.merge(Deps {
                known: positions[i].reading.map_or(Known::None, Known::One),
                unsure: positions[i].reading.is_none() || positions[i].untraceable,
            });
            for j in edges(i) {
                if component[j] == id {
                    all.unsure = true;
                } else {
                    all.merge(of_group[component[j]]);
                }
            }
        }
        of_group[id] = all;
    }
    component.iter().map(|&id| of_group[id]).collect()
}

/// 1 本の木の手に `cond` を書く。`conds` は局面ごとの読みの条件名 (確かめられない局面は入れない)。
///
/// 1 回の読みの手はその局面の読みの条件。先を読んだ手は、行き先から辿れる読みの条件が全部同じならその名前、
/// 違う名前が 2 つ以上あれば `mixed`、確かめられない読みが混ざれば `unknown`。
pub(super) fn assign(
    nodes: &mut BTreeMap<String, Node>,
    conds: &BTreeMap<String, String>,
) -> Result<()> {
    let mut names: Vec<&str> = conds.values().map(String::as_str).collect();
    names.sort_unstable();
    names.dedup();
    let index: BTreeMap<&str, usize> =
        nodes.keys().enumerate().map(|(i, key)| (key.as_str(), i)).collect();
    let mut positions = Vec::with_capacity(nodes.len());
    for (key, node) in nodes.iter() {
        let mut untraceable = false;
        let mut destinations = Vec::with_capacity(node.moves.len());
        for mv in &node.moves {
            let destination = if expanded(mv) {
                index.get(child(key, &mv.mv)?.as_str()).copied()
            } else {
                None
            };
            untraceable |= destination.is_none() && mv.value_kind != "single";
            destinations.push(destination);
        }
        positions.push(Position {
            reading: conds.get(key).and_then(|name| names.binary_search(&name.as_str()).ok()),
            untraceable,
            destinations,
        });
    }
    let below = reachable(&positions);
    for (node, position) in nodes.values_mut().zip(&positions) {
        for (mv, &destination) in node.moves.iter_mut().zip(&position.destinations) {
            mv.cond = match (destination, position.reading) {
                (Some(destination), _) => below[destination].name(&names),
                (None, Some(reading)) if mv.value_kind == "single" => names[reading].into(),
                _ => "unknown".into(),
            };
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::parse_node_moves;
    use super::*;
    use std::collections::BTreeSet;

    const R: &str = "4k4/9/9/9/9/9/9/9/4K4 b -";

    fn cond<'a>(nodes: &'a BTreeMap<String, Node>, key: &str, mv: &str) -> &'a str {
        &nodes[key].moves.iter().find(|m| m.mv == mv).unwrap().cond
    }

    fn conds(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, name)| ((*key).to_owned(), (*name).to_owned()))
            .collect()
    }

    #[test]
    fn single_moves_take_the_position_and_expanded_moves_take_the_readings_below() {
        let w1 = child(R, "5i4i").unwrap();
        let w2 = child(R, "5i6i").unwrap();
        let x = child(&w1, "5a4a").unwrap();
        let lost = child(&w2, "5a6a").unwrap();
        let line = |key: &str, mv: &str, expanded: u8| {
            format!("{key}\t1\t0\t2\t100\t{mv}\t0\t{expanded}\t1\n")
        };
        let table = [
            "key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n".to_owned(),
            line(R, "5i4i", 1),
            line(R, "5i6i", 1),
            line(R, "5i5h", 0),
            line(&w1, "5a4a", 1),
            line(&w1, "5a6a", 0),
            line(&w2, "5a6a", 1),
            line(&x, "4i3i", 0),
        ]
        .concat();
        let known = BTreeSet::from([lost]);
        let parsed = parse_node_moves(&table, 0, Some(&known)).unwrap();
        assert_eq!(parsed[&w2].moves[0].value_kind, "unknown");

        let mut nodes = parsed.clone();
        assign(&mut nodes, &conds(&[(R, "a"), (&w1, "a"), (&x, "a"), (&w2, "b")])).unwrap();
        assert_eq!(cond(&nodes, R, "5i4i"), "a");
        assert_eq!(cond(&nodes, R, "5i5h"), "a");
        // 行き先の局面に、値の来た読みを辿れない手がある。
        assert_eq!(cond(&nodes, R, "5i6i"), "unknown");
        assert_eq!(cond(&nodes, &w2, "5a6a"), "unknown");

        let mut nodes = parsed.clone();
        assign(&mut nodes, &conds(&[(R, "a"), (&w1, "a"), (&x, "b")])).unwrap();
        assert_eq!(cond(&nodes, R, "5i4i"), "mixed");
        assert_eq!(cond(&nodes, &w1, "5a4a"), "b");
        assert_eq!(cond(&nodes, &w1, "5a6a"), "a");
        assert_eq!(cond(&nodes, &x, "4i3i"), "b");

        let mut nodes = parsed;
        assign(&mut nodes, &conds(&[(R, "a"), (&w1, "a")])).unwrap();
        assert_eq!(cond(&nodes, R, "5i4i"), "unknown");
        assert_eq!(cond(&nodes, &w1, "5a6a"), "a");
        assert_eq!(cond(&nodes, &x, "4i3i"), "unknown");
    }

    #[test]
    fn a_position_reached_by_two_lines_counts_for_both() {
        let w1 = child(R, "5i4i").unwrap();
        let w2 = child(R, "5i6i").unwrap();
        let x1 = child(&w1, "5a5b").unwrap();
        let x2 = child(&w2, "5a5b").unwrap();
        let y = child(&x1, "4i5i").unwrap();
        assert_eq!(child(&x2, "6i5i").unwrap(), y);
        let line = |key: &str, mv: &str, expanded: u8| {
            format!("{key}\t1\t0\t2\t100\t{mv}\t0\t{expanded}\t1\n")
        };
        let table = [
            "key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n".to_owned(),
            line(R, "5i4i", 1),
            line(R, "5i6i", 1),
            line(&w1, "5a5b", 1),
            line(&w2, "5a5b", 1),
            line(&x1, "4i5i", 1),
            line(&x2, "6i5i", 1),
            line(&y, "5b4b", 0),
        ]
        .concat();
        let mut nodes = parse_node_moves(&table, 0, Some(&BTreeSet::new())).unwrap();
        assign(
            &mut nodes,
            &conds(&[
                (R, "a"),
                (&w1, "a"),
                (&w2, "b"),
                (&x1, "a"),
                (&x2, "b"),
                (&y, "a"),
            ]),
        )
        .unwrap();
        assert_eq!(cond(&nodes, &y, "5b4b"), "a");
        assert_eq!(cond(&nodes, &x1, "4i5i"), "a");
        assert_eq!(cond(&nodes, &x2, "6i5i"), "a");
        assert_eq!(cond(&nodes, R, "5i4i"), "a");
        assert_eq!(cond(&nodes, &w2, "5a5b"), "mixed");
        assert_eq!(cond(&nodes, R, "5i6i"), "mixed");
    }

    #[test]
    fn moves_into_a_cycle_are_unknown_unless_two_conditions_are_seen() {
        let w = child(R, "5i4i").unwrap();
        let x = child(&w, "5a4a").unwrap();
        let y = child(&x, "4i5i").unwrap();
        assert_eq!(child(&y, "4a5a").unwrap(), R);
        let table = format!(
            "key\tply\tlevel\tremaining\tnodes\tmove\tvalue\texpanded\trank\n\
             {R}\t1\t0\t4\t100\t5i4i\t0\t1\t1\n\
             {w}\t2\t1\t3\t100\t5a4a\t0\t1\t1\n\
             {x}\t3\t2\t2\t100\t4i5i\t0\t1\t1\n\
             {x}\t3\t2\t2\t100\t4i3i\t0\t0\t2\n\
             {y}\t4\t3\t1\t100\t4a5a\t0\t1\t1\n"
        );
        let parsed = parse_node_moves(&table, 0, Some(&BTreeSet::new())).unwrap();
        let mut nodes = parsed.clone();
        assign(&mut nodes, &conds(&[(R, "a"), (&w, "a"), (&x, "a"), (&y, "a")])).unwrap();
        for (key, mv) in [
            (R, "5i4i"),
            (w.as_str(), "5a4a"),
            (x.as_str(), "4i5i"),
            (&y, "4a5a"),
        ] {
            assert_eq!(cond(&nodes, key, mv), "unknown", "{key} {mv}");
        }
        assert_eq!(cond(&nodes, &x, "4i3i"), "a");
        let mut nodes = parsed;
        assign(&mut nodes, &conds(&[(R, "a"), (&w, "a"), (&x, "b"), (&y, "a")])).unwrap();
        for (key, mv) in [
            (R, "5i4i"),
            (w.as_str(), "5a4a"),
            (x.as_str(), "4i5i"),
            (&y, "4a5a"),
        ] {
            assert_eq!(cond(&nodes, key, mv), "mixed", "{key} {mv}");
        }
    }
}
