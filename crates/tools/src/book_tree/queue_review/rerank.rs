use super::super::targets::{MAX_PRIORITY, read_checked_lines};
use super::super::{master_table, model};
use super::*;
use std::collections::VecDeque;

pub(super) struct Rerank {
    pub outputs: Vec<PathBuf>,
    pub payloads: Vec<Vec<u8>>,
    pub summary: String,
}

struct Row<'a> {
    box_index: usize,
    tree: &'a TreeProgress,
    queued: &'a Queued,
    ply: u32,
    reach: f64,
    branch: f64,
    branch_ply: u32,
    failing: f64,
    boost: bool,
    class: char,
    priority: u32,
    hold: bool,
    shelved: bool,
}

struct ReverseEdge {
    parent: String,
    main: bool,
    played: bool,
}

#[derive(Default)]
struct BranchGraph(BTreeMap<String, Vec<ReverseEdge>>);

impl BranchGraph {
    fn load(path: &std::path::Path) -> Result<Self> {
        let mut graph = Self::default();
        for row in master_table::read_rows(BufReader::new(File::open(path)?))? {
            let (at, row) = row?;
            let parent = row.get("key");
            let child = model::child(parent, row.get("move"))
                .with_context(|| format!("元データの {at} 行目"))?;
            graph.0.entry(child).or_default().push(ReverseEdge {
                parent: parent.into(),
                main: row.get("role") == "main",
                played: matches!(row.get("role"), "main" | "alt"),
            });
        }
        Ok(graph)
    }

    fn branch(&self, root: &str, own: &str, walk: &Walk) -> (f64, u32) {
        let reach =
            |key: &str| walk.reach.get(&(own.into(), key.into())).copied().unwrap_or_default();
        if reach(root) > 0.0 {
            return (reach(root), 0);
        }
        let mut queue = VecDeque::from([(root, 0, 0u32)]);
        let mut visited = BTreeMap::from([(root, 0u32)]);
        let mut best = (0.0, 0);
        while let Some((key, ply, alts)) = queue.pop_front() {
            let weight = reach(key);
            if weight > 0.0 {
                let candidate = weight * 0.5f64.powi(alts as i32);
                if candidate > best.0 || (candidate == best.0 && ply < best.1) {
                    best = (candidate, ply);
                }
                continue;
            }
            if ply == 16 {
                continue;
            }
            for edge in self.0.get(key).into_iter().flatten() {
                let own_turn = model::side(&edge.parent) == own;
                if own_turn && !edge.played {
                    continue;
                }
                let next_alts = alts + u32::from(own_turn && !edge.main);
                // 幅優先なので、既訪問の経路は手数が同じか短い。減衰が改善する場合だけ再訪する。
                if visited.get(edge.parent.as_str()).is_some_and(|&n| n <= next_alts) {
                    continue;
                }
                visited.insert(edge.parent.as_str(), next_alts);
                queue.push_back((edge.parent.as_str(), ply + 1, next_alts));
            }
        }
        best
    }
}

pub(super) fn class(reach: f64, failing: f64, boost: bool, floor: f64) -> char {
    if failing >= 0.005 || boost {
        'A'
    } else if reach >= 0.005 {
        'B'
    } else if reach >= floor {
        'C'
    } else {
        'D'
    }
}

pub(super) fn build(
    args: &QueueReviewArgs,
    boxes: &[BoxData],
    walk: &Walk,
    inputs: &mut Vec<PathBuf>,
) -> Result<Rerank> {
    let master = args.master.as_ref().context("再採点には master が必要です")?;
    inputs.push(master.clone());
    let graph = BranchGraph::load(master)?;
    let mut boosted = BTreeSet::new();
    for path in &args.boost_queue {
        inputs.push(path.clone());
        read_checked_lines(path, |line, root, _| {
            boosted.insert(root.key);
            for root in &line.roots {
                boosted.insert(parse_root(root)?.key);
            }
            Ok(())
        })?;
    }
    let mut locations: BTreeMap<_, Vec<_>> = BTreeMap::new();
    let mut started = BTreeSet::new();
    for (i, b) in boxes.iter().enumerate() {
        for tree in &b.progress.trees {
            if tree.read > 0 {
                started.insert((i, tree.id.as_str()));
            }
            if tree.finished || tree.failed {
                continue;
            }
            locations.entry((&tree.id, &tree.own)).or_default().push((i, tree));
        }
    }
    let mut rows = Vec::new();
    for locations in locations.into_values() {
        let shelved = locations.iter().all(|(_, tree)| tree.hold);
        for &(i, tree) in &locations {
            if tree.hold && (!shelved || i != locations[0].0) {
                continue;
            }
            let queued = &boxes[i].queue[&tree.id];
            let root = parse_root(&queued.line.root)?;
            let key = (tree.own.clone(), root.key.clone());
            let reach = walk.reach.get(&key).copied().unwrap_or_default();
            let failing = walk.failing.get(&key).copied().unwrap_or_default();
            let boost = boosted.contains(&root.key);
            let (branch, branch_ply) = graph.branch(&root.key, &tree.own, walk);
            let effective = reach.max(branch);
            let class = class(effective, failing, boost, args.value_floor);
            let base = match class {
                'A' => Some(10),
                'B' => Some(40),
                'C' => Some(100),
                _ => None,
            };
            let priority = if tree.priority < args.rerank_keep_below {
                tree.priority
            } else {
                base.map_or(tree.priority, |base| {
                    tree.priority.min((base + (root.ply / 5).min(19)).min(MAX_PRIORITY))
                })
            };
            let hold = if tree.read > 0 {
                tree.hold
            } else if shelved && class != 'D' {
                false
            } else if !tree.hold
                && class == 'D'
                && effective == 0.0
                && tree.priority >= args.rerank_keep_below
            {
                true
            } else {
                tree.hold
            };
            rows.push(Row {
                box_index: i,
                tree,
                queued,
                ply: root.ply,
                reach,
                branch,
                branch_ply,
                failing,
                boost,
                class,
                priority,
                hold,
                shelved,
            });
        }
    }
    rows.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(a.ply.cmp(&b.ply))
            .then(a.tree.id.cmp(&b.tree.id))
            .then(a.tree.own.cmp(&b.tree.own))
            .then(a.box_index.cmp(&b.box_index))
    });
    let mut result = Rerank {
        outputs: Vec::new(),
        payloads: Vec::new(),
        summary: String::new(),
    };
    let mut counts = BTreeMap::from([('A', 0), ('B', 0), ('C', 0), ('D', 0)]);
    let (mut raised, mut lowered, mut released, mut held) = (0, 0, 0, 0);
    let mut table = b"box\tid\town\tstate\tply\treach\tbranch\tbranch_ply\tfailing\tboost\tclass\told_priority\tnew_priority\told_hold\tnew_hold\tshelved\tread\troot\n".to_vec();
    let mut updates: BTreeMap<(usize, &str), (u32, bool)> = BTreeMap::new();
    for row in &rows {
        let tree = row.tree;
        *counts.entry(row.class).or_default() += 1;
        raised += usize::from(row.priority < tree.priority);
        lowered += usize::from(row.priority > tree.priority);
        released += usize::from(tree.hold && !row.hold);
        held += usize::from(!tree.hold && row.hold);
        let state = if tree.hold {
            "held"
        } else if tree.read > 0 {
            "running"
        } else {
            "queued"
        };
        writeln!(
            table,
            "{}\t{}\t{}\t{state}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            boxes[row.box_index].name,
            tree.id,
            tree.own,
            row.ply,
            row.reach,
            row.branch,
            row.branch_ply,
            row.failing,
            row.boost,
            row.class,
            tree.priority,
            row.priority,
            tree.hold,
            row.hold,
            row.shelved,
            tree.read,
            row.queued.line.root
        )?;
        if args.rerank_out.is_some() {
            let desired = (row.priority, row.hold);
            updates
                .entry((row.box_index, &tree.id))
                .and_modify(|previous| {
                    previous.0 = previous.0.min(desired.0);
                    previous.1 &= desired.1;
                })
                .or_insert(desired);
        }
    }
    if let Some(path) = &args.rerank_table {
        result.outputs.push(path.clone());
        result.payloads.push(table);
    }
    if let Some(dir) = &args.rerank_out {
        for (i, b) in boxes.iter().enumerate() {
            let mut bytes = Vec::new();
            for ((_, id), &(priority, mut hold)) in
                updates.iter().filter(|((index, _), _)| *index == i)
            {
                let queued = &b.queue[*id];
                if started.contains(&(i, *id)) {
                    hold = queued.line.hold;
                }
                if priority == queued.line.priority && hold == queued.line.hold {
                    continue;
                }
                let mut raw = queued.raw.clone();
                raw["priority"] = priority.into();
                raw["hold"] = hold.into();
                serde_json::to_writer(&mut bytes, &raw)?;
                writeln!(bytes)?;
            }
            result
                .outputs
                .push(dir.join(format!("{}_rerank_{}.jsonl", args.prefix, b.name)));
            result.payloads.push(bytes);
        }
    }
    result.summary = format!(
        "再採点: A={} B={} C={} D={}\npriority: 上げる={raised} 下げる={lowered}、hold: 外す={released} 付ける={held}",
        counts[&'A'], counts[&'B'], counts[&'C'], counts[&'D']
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(moves: &[&str], roles: &[&str]) -> (BranchGraph, Vec<String>) {
        let mut keys = vec![HIRATE_KEY.to_owned()];
        let rows: Vec<_> = moves
            .iter()
            .zip(roles)
            .map(|(mv, role)| {
                let key = keys.last().unwrap().clone();
                keys.push(model::child(&key, mv).unwrap());
                master_table::Row::from_fields(&[
                    ("key", key),
                    ("move", (*mv).into()),
                    ("role", (*role).into()),
                ])
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.tsv");
        std::fs::write(&path, master_table::format_master(&rows)).unwrap();
        (BranchGraph::load(&path).unwrap(), keys)
    }

    #[test]
    fn branch_roles_sides_and_direct_reach() {
        for (roles, own, expected) in [
            (["alt", "record"], "b", 0.02),
            (["main", "record"], "b", 0.04),
            (["record", "main"], "b", 0.0),
            (["bench", "main"], "b", 0.0),
            (["record", "alt"], "w", 0.02),
            (["record", "record"], "w", 0.0),
        ] {
            let (g, keys) = graph(&["7g7f", "3c3d"], &roles);
            let mut walk = Walk::default();
            walk.reach.insert((own.into(), keys[0].clone()), 0.04);
            assert_eq!(
                g.branch(&keys[2], own, &walk),
                (expected, if expected > 0.0 { 2 } else { 0 })
            );
            walk.reach.insert((own.into(), keys[2].clone()), 0.01);
            assert_eq!(g.branch(&keys[2], own, &walk), (0.01, 0));
        }
    }

    #[test]
    fn branch_depth_limit_and_cycle() {
        let moves = [
            "7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e", "6g6f", "4c4d", "5g5f", "5c5d", "4g4f",
            "6c6d", "3g3f", "7c7d", "1g1f", "9c9d", "9g9f",
        ];
        let (mut g, keys) = graph(&moves, &["main"; 17]);
        let mut walk = Walk::default();
        walk.reach.insert(("b".into(), keys[0].clone()), 0.04);
        assert_eq!(g.branch(&keys[16], "b", &walk), (0.04, 16));
        assert_eq!(g.branch(&keys[17], "b", &walk), (0.0, 0));
        g.0.entry(keys[17].clone()).or_default().push(ReverseEdge {
            parent: keys[17].clone(),
            main: true,
            played: true,
        });
        assert_eq!(g.branch(&keys[17], "b", &walk), (0.0, 0));
    }
}
