use super::super::targets::{MAX_PRIORITY, read_checked_lines};
use super::super::{master_table, model};
use super::*;

/// 台の番号と id ごとの受付行の priority・hold。
pub(super) type LineStates = BTreeMap<(usize, String), (u32, bool)>;

pub(super) struct Rerank {
    pub outputs: Vec<PathBuf>,
    pub payloads: Vec<Vec<u8>>,
    pub summary: String,
    /// 台と id ごとの更新後の受付行の priority・hold (変わらない行を含む)。
    pub lines: LineStates,
    /// 段の表で group を書き換えた受付行の、新しい group。
    pub groups: BTreeMap<(usize, String), String>,
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
    running: bool,
    /// 段の表を当てる前の priority (再採点と group 揃えの後)。
    rerank_priority: u32,
    /// 同じ根・同じ own の別の id の行 (`<台>:<id>`)。あれば hold を外さない。
    copy_of: Option<String>,
}

type Frontier = BTreeMap<String, BTreeMap<usize, u32>>;

struct BranchSearch<'a> {
    own: Vec<&'a str>,
    frontier: Frontier,
    visited: Vec<BTreeMap<String, u32>>,
    best: Vec<(f64, u32)>,
}

impl<'a> BranchSearch<'a> {
    fn new(roots: &[(&str, &'a str)], walk: &Walk) -> Self {
        let mut search = Self {
            own: roots.iter().map(|(_, own)| *own).collect(),
            frontier: BTreeMap::new(),
            visited: vec![BTreeMap::new(); roots.len()],
            best: vec![(0.0, 0); roots.len()],
        };
        for (tree, &(root, _)) in roots.iter().enumerate() {
            search.visited[tree].insert(root.into(), 0);
            search.frontier.entry(root.into()).or_default().insert(tree, 0);
        }
        search.finish_layer(0, walk);
        search
    }

    fn edge(&mut self, next: &mut Frontier, parent: &str, child: &str, role: &str) {
        for (&tree, &alts) in self.frontier.get(child).into_iter().flatten() {
            let own_turn = model::side(parent) == self.own[tree];
            if own_turn && !matches!(role, "main" | "alt") {
                continue;
            }
            let next_alts = alts + u32::from(own_turn && role != "main");
            // 幅優先なので、既訪問の経路は手数が同じか短い。alt 数が減る場合だけ再訪する。
            if self.visited[tree].get(parent).is_some_and(|&n| n <= next_alts) {
                continue;
            }
            self.visited[tree].insert(parent.into(), next_alts);
            next.entry(parent.into()).or_default().insert(tree, next_alts);
        }
    }

    fn finish_layer(&mut self, ply: u32, walk: &Walk) {
        self.frontier.retain(|key, trees| {
            trees.retain(|&tree, &mut alts| {
                let weight = walk
                    .reach
                    .get(&(self.own[tree].into(), key.clone()))
                    .copied()
                    .unwrap_or_default();
                if weight <= 0.0 {
                    return true;
                }
                let candidate = weight * 0.5f64.powi(alts as i32);
                let best = &mut self.best[tree];
                if candidate > best.0 || (candidate == best.0 && ply < best.1) {
                    *best = (candidate, ply);
                }
                false
            });
            !trees.is_empty()
        });
    }
}

fn branches(
    path: &std::path::Path,
    roots: &[(&str, &str)],
    walk: &Walk,
) -> Result<Vec<(f64, u32)>> {
    let mut search = BranchSearch::new(roots, walk);
    for ply in 1..=16 {
        if search.frontier.is_empty() {
            break;
        }
        let mut next = Frontier::new();
        for row in master_table::read_rows(BufReader::new(File::open(path)?))? {
            let (at, row) = row?;
            let parent = row.get("key");
            let child = model::child(parent, row.get("move"))
                .with_context(|| format!("元データの {at} 行目"))?;
            search.edge(&mut next, parent, &child, row.get("role"));
        }
        search.frontier = next;
        search.finish_layer(ply, walk);
    }
    Ok(search.best)
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
    let tiers = args.rerank_tiers.as_ref().map(|path| tiers::Tiers::load(path)).transpose()?;
    inputs.extend(args.rerank_tiers.iter().cloned());
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
            if tree.read > 0
                || b.progress.workers.iter().any(|w| {
                    w.key.as_ref().is_some_and(|k| b.queue[&tree.id].roots.contains(strip_ply(k)))
                })
            {
                started.insert((i, tree.id.as_str()));
            }
            locations.entry((&tree.id, &tree.own)).or_default().push((i, tree));
        }
    }
    // 新しい木は台の番号の代わりに boxes.len() を使い、居場所が 1 つの未着手の木として採点する。
    let new_index = boxes.len();
    let adds = plan::load(&args.add, inputs)?;
    let mut new_queue = BTreeMap::new();
    for q in adds {
        ensure!(
            boxes.iter().all(|b| !b.queue.contains_key(&q.line.id)),
            "add の id は受付済みです: {}",
            q.line.id
        );
        let id = q.line.id.clone();
        ensure!(new_queue.insert(id.clone(), q).is_none(), "add の id が重複しています: {id}");
    }
    let new_trees: Vec<_> = new_queue
        .values()
        .flat_map(|q| {
            q.line.sides().into_iter().map(|own| TreeProgress {
                id: q.line.id.clone(),
                own,
                priority: q.line.priority,
                hold: q.line.hold,
                finished: false,
                failed: false,
                read: 0,
                requested: 0,
            })
        })
        .collect();
    let queued_at = |i: usize, id: &str| -> &Queued {
        if i == new_index {
            &new_queue[id]
        } else {
            &boxes[i].queue[id]
        }
    };
    // 名前を変えて別の台へ移した木は id が違うので、居場所を id でまとめる棚上げの判定では
    // 見つからない。根の局面と own の側で照合し、写しのある行の hold は外さない。
    let mut copies: BTreeMap<(&str, String), Vec<(usize, &str)>> = BTreeMap::new();
    let all_lines = boxes
        .iter()
        .enumerate()
        .flat_map(|(i, b)| b.queue.values().map(move |q| (i, q)))
        .chain(new_queue.values().map(|q| (new_index, q)));
    for (i, q) in all_lines {
        for side in q.line.sides() {
            copies.entry((q.primary_root.as_str(), side)).or_default().push((i, &q.line.id));
        }
    }
    let box_name = |i: usize| boxes.get(i).map_or("add", |b| b.name.as_str());
    let walk_sides: BTreeSet<_> = args
        .walk_table
        .iter()
        .map(|text| assignment(text).map(|(side, _)| side))
        .collect::<Result<_>>()?;
    let mut targets = Vec::new();
    for locations in locations.into_values() {
        let shelved = locations.iter().all(|(_, tree)| tree.hold);
        if locations.iter().any(|(_, tree)| tree.finished || tree.failed) {
            continue;
        }
        for &(i, tree) in &locations {
            if tree.hold && (!shelved || i != locations[0].0) {
                continue;
            }
            ensure!(
                walk_sides.contains(tree.own.as_str()),
                "再採点の own={} に必要な --walk-table {}=... がありません",
                tree.own,
                tree.own
            );
            let queued = &boxes[i].queue[&tree.id];
            targets.push((i, tree, queued, shelved, parse_root(&queued.line.root)?));
        }
    }
    for tree in &new_trees {
        ensure!(
            walk_sides.contains(tree.own.as_str()),
            "add の own={} に必要な --walk-table {}=... がありません",
            tree.own,
            tree.own
        );
        let queued = &new_queue[&tree.id];
        // 居場所が 1 つなので、hold の行は既存の全台 hold の木と同じく棚上げとして扱う。
        targets.push((new_index, tree, queued, tree.hold, parse_root(&queued.line.root)?));
    }
    let roots: Vec<_> = targets
        .iter()
        .map(|(_, tree, _, _, root)| (root.key.as_str(), tree.own.as_str()))
        .collect();
    let weights = branches(master, &roots, walk)?;
    let mut rows = Vec::new();
    for ((i, tree, queued, shelved, root), (branch, branch_ply)) in targets.into_iter().zip(weights)
    {
        let key = (tree.own.clone(), root.key.clone());
        let reach = walk.reach.get(&key).copied().unwrap_or_default();
        let failing = walk.failing.get(&key).copied().unwrap_or_default();
        let boost = boosted.contains(&root.key);
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
        let running = tree.read > 0
            || boxes.get(i).is_some_and(|b| {
                b.progress
                    .workers
                    .iter()
                    .any(|w| w.key.as_ref().is_some_and(|k| queued.roots.contains(strip_ply(k))))
            });
        let hold = if running {
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
        let copy_of = (tree.hold && !hold)
            .then(|| {
                copies[&(queued.primary_root.as_str(), tree.own.clone())]
                    .iter()
                    .find(|(_, id)| *id != tree.id)
                    .map(|&(j, id)| format!("{}:{id}", box_name(j)))
            })
            .flatten();
        let hold = hold || copy_of.is_some();
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
            running,
            rerank_priority: priority,
            copy_of,
        });
    }
    let mut lines = LineStates::new();
    for row in &rows {
        let desired = (row.priority, row.hold);
        lines
            .entry((row.box_index, row.tree.id.clone()))
            .and_modify(|previous| {
                previous.0 = previous.0.min(desired.0);
                previous.1 &= desired.1;
            })
            .or_insert(desired);
    }
    for ((i, id), (_, hold)) in &mut lines {
        if started.contains(&(*i, id.as_str())) {
            *hold = queued_at(*i, id).line.hold;
        }
    }
    // 受付行に書く最終の hold で group の最小を取る。bw の片側や読み途中の規則で
    // 行ごとの hold と受付行の hold は食い違うことがある。
    let group_of = |(i, id): &(usize, String)| queued_at(*i, id).line.group.as_deref();
    let mut group_min: BTreeMap<&str, u32> = BTreeMap::new();
    for (key, &(priority, hold)) in &lines {
        if let Some(group) = group_of(key).filter(|_| !hold) {
            let min = group_min.entry(group).or_insert(priority);
            *min = (*min).min(priority);
        }
    }
    let aligned_min = |key: &(usize, String), hold: bool| {
        group_of(key).filter(|_| !hold).and_then(|g| group_min.get(g)).copied()
    };
    for (key, (priority, hold)) in &mut lines {
        if let Some(min) = aligned_min(key, *hold) {
            *priority = (*priority).min(min);
        }
    }
    let mut aligned = 0;
    for row in &mut rows {
        let key = (row.box_index, row.tree.id.clone());
        if let Some(min) = aligned_min(&key, lines[&key].1)
            && min < row.priority
        {
            row.priority = min;
            aligned += 1;
        }
    }
    // 段の表は再採点と group 揃えの後に当て、その値で確定する (段の後には group をそろえ直さない)。
    // hold のまま残る行は受付の priority に戻し、動いている木だけを段で並べ直す。
    let mut tier_of: BTreeMap<(usize, String), &str> = BTreeMap::new();
    let mut groups = BTreeMap::new();
    if let Some(tiers) = &tiers {
        let mut classes: BTreeMap<(usize, String), char> = BTreeMap::new();
        for row in &rows {
            let class = classes.entry((row.box_index, row.tree.id.clone())).or_insert(row.class);
            *class = (*class).min(row.class);
        }
        for (key, (priority, hold)) in &mut lines {
            let line = &queued_at(key.0, &key.1).line;
            if *hold {
                *priority = line.priority;
                continue;
            }
            if let Some((name, tiered, group)) =
                tiers.apply(&key.1, classes[key], line.priority, *priority)
            {
                *priority = tiered;
                tier_of.insert(key.clone(), name);
                if let Some(group) = group.filter(|g| line.group.as_deref() != Some(*g)) {
                    groups.insert(key.clone(), group.to_owned());
                }
            }
        }
    }
    for row in &mut rows {
        row.rerank_priority = row.priority;
        if tiers.is_some() {
            row.priority = lines[&(row.box_index, row.tree.id.clone())].0;
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
        lines,
        groups,
    };
    let mut counts = BTreeMap::from([('A', 0), ('B', 0), ('C', 0), ('D', 0)]);
    let (mut raised, mut lowered, mut released, mut held) = (0, 0, 0, 0);
    let kept_copies = rows.iter().filter(|row| row.copy_of.is_some()).count();
    let mut table = b"box\tid\town\tstate\tply\treach\tbranch\tbranch_ply\tfailing\tboost\tclass\told_priority\tnew_priority\told_hold\tnew_hold\tshelved\tread\troot\tgroup\ttier\trerank_priority\tcopy_of\n".to_vec();
    for row in &rows {
        let key = (row.box_index, row.tree.id.clone());
        let tree = row.tree;
        *counts.entry(row.class).or_default() += 1;
        raised += usize::from(row.priority < tree.priority);
        lowered += usize::from(row.priority > tree.priority);
        released += usize::from(tree.hold && !row.hold);
        held += usize::from(!tree.hold && row.hold);
        let state = if row.box_index == new_index {
            "new"
        } else if tree.hold {
            "held"
        } else if row.running {
            "running"
        } else {
            "queued"
        };
        writeln!(
            table,
            "{}\t{}\t{}\t{state}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            boxes.get(row.box_index).map_or("", |b| b.name.as_str()),
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
            row.queued.line.root,
            result
                .groups
                .get(&key)
                .map(String::as_str)
                .or(row.queued.line.group.as_deref())
                .unwrap_or_default(),
            tier_of.get(&key).copied().unwrap_or_default(),
            row.rerank_priority,
            row.copy_of.as_deref().unwrap_or_default()
        )?;
    }
    if let Some(path) = &args.rerank_table {
        result.outputs.push(path.clone());
        result.payloads.push(table);
    }
    if let Some(dir) = &args.rerank_out {
        for (i, b) in boxes.iter().enumerate() {
            let mut bytes = Vec::new();
            for ((_, id), &(priority, hold)) in
                result.lines.iter().filter(|((index, _), _)| *index == i)
            {
                let queued = &b.queue[id];
                let group = result.groups.get(&(i, id.clone()));
                if priority == queued.line.priority && hold == queued.line.hold && group.is_none() {
                    continue;
                }
                let mut raw = queued.raw.clone();
                raw["priority"] = priority.into();
                raw["hold"] = hold.into();
                if let Some(group) = group {
                    raw["group"] = group.as_str().into();
                }
                serde_json::to_writer(&mut bytes, &raw)?;
                writeln!(bytes)?;
            }
            result
                .outputs
                .push(dir.join(format!("{}_rerank_{}.jsonl", args.prefix, b.name)));
            result.payloads.push(bytes);
        }
        if !new_queue.is_empty() {
            // 台ごとの出力名は `_rerank_` で続くので、`_rerank-add` とは重ならない。
            let mut bytes = Vec::new();
            for (id, q) in &new_queue {
                let (priority, hold) = result.lines[&(new_index, id.clone())];
                let mut raw = q.raw.clone();
                raw["priority"] = priority.into();
                raw["hold"] = hold.into();
                if let Some(group) = result.groups.get(&(new_index, id.clone())) {
                    raw["group"] = group.as_str().into();
                }
                serde_json::to_writer(&mut bytes, &raw)?;
                writeln!(bytes)?;
            }
            result.outputs.push(dir.join(format!("{}_rerank-add.jsonl", args.prefix)));
            result.payloads.push(bytes);
        }
    }
    result.summary = format!(
        "再採点: A={} B={} C={} D={}\npriority: 上げる={raised} 下げる={lowered} (うち group 揃え={aligned})、hold: 外す={released} 付ける={held}",
        counts[&'A'], counts[&'B'], counts[&'C'], counts[&'D']
    );
    if kept_copies > 0 {
        result
            .summary
            .push_str(&format!("\n名前を変えた写しがあるので hold を外さない={kept_copies}"));
    }
    if tiers.is_some() {
        let mut per_tier: BTreeMap<&str, usize> = BTreeMap::new();
        for name in tier_of.values() {
            *per_tier.entry(name).or_default() += 1;
        }
        let listed: Vec<_> = per_tier.iter().map(|(name, n)| format!("{name}={n}")).collect();
        result.summary.push_str(&format!(
            "\n段 (受付行): {}、当たらない={}",
            listed.join(" "),
            result.lines.values().filter(|(_, hold)| !hold).count() - tier_of.len()
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

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
                let (_, row) = row?;
                let parent = row.get("key");
                let child = model::child(parent, row.get("move"))?;
                graph.0.entry(child).or_default().push(ReverseEdge {
                    parent: parent.into(),
                    main: row.get("role") == "main",
                    played: matches!(row.get("role"), "main" | "alt"),
                });
            }
            Ok(graph)
        }

        fn branch(&self, root: &str, own: &str, walk: &Walk) -> (f64, u32) {
            let mut search = BranchSearch::new(&[(root, own)], walk);
            for ply in 1..=16 {
                if search.frontier.is_empty() {
                    break;
                }
                let mut next = Frontier::new();
                for (child, edges) in &self.0 {
                    for edge in edges {
                        let role = if edge.main {
                            "main"
                        } else if edge.played {
                            "alt"
                        } else {
                            "record"
                        };
                        search.edge(&mut next, &edge.parent, child, role);
                    }
                }
                search.frontier = next;
                search.finish_layer(ply, walk);
            }
            search.best[0]
        }
    }

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
    fn streamed_branches_batch_roots_sides_and_stop_at_first_weight() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("master.tsv");
        let moves = [
            "7g7f", "3c3d", "2g2f", "8c8d", "2f2e", "8d8e", "6g6f", "4c4d", "5g5f", "5c5d", "4g4f",
            "6c6d", "3g3f", "7c7d", "1g1f", "9c9d", "9g9f",
        ];
        let mut keys = vec![HIRATE_KEY.to_owned()];
        let mut rows = Vec::new();
        for mv in moves {
            let key = keys.last().unwrap().clone();
            keys.push(model::child(&key, mv).unwrap());
            rows.push(master_table::Row::from_fields(&[
                ("key", key),
                ("move", mv.into()),
                ("role", "main".into()),
            ]));
        }
        rows.reverse();
        std::fs::write(&path, master_table::format_master(&rows)).unwrap();
        let mut walk = Walk::default();
        walk.reach.insert(("b".into(), keys[0].clone()), 1.0);
        walk.reach.insert(("w".into(), keys[0].clone()), 0.04);
        walk.reach.insert(("b".into(), keys[1].clone()), 0.01);
        let roots = [
            (keys[2].as_str(), "b"),
            (keys[2].as_str(), "w"),
            (keys[1].as_str(), "b"),
            (keys[16].as_str(), "w"),
            (keys[17].as_str(), "w"),
        ];
        let expected = vec![(0.01, 1), (0.04, 2), (0.01, 0), (0.04, 16), (0.0, 0)];
        assert_eq!(branches(&path, &roots, &walk).unwrap(), expected);
        rows.reverse();
        std::fs::write(&path, master_table::format_master(&rows)).unwrap();
        assert_eq!(branches(&path, &roots, &walk).unwrap(), expected);
    }

    #[test]
    fn branch_roles_sides_and_direct_reach() {
        for (roles, own, expected) in [
            (["alt", "record"], "b", 0.02),
            (["main", "record"], "b", 0.04),
            (["main", "bench"], "b", 0.04),
            (["bench", "main"], "w", 0.04),
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
    fn branch_longer_path_with_fewer_alts_wins() {
        let (mut g, keys) = graph(&["7g7f", "3c3d", "2g2f", "8c8d"], &["main"; 4]);
        g.0.entry(keys[4].clone()).or_default().insert(
            0,
            ReverseEdge {
                parent: keys[0].clone(),
                main: false,
                played: true,
            },
        );
        let mut walk = Walk::default();
        walk.reach.insert(("b".into(), keys[0].clone()), 0.04);
        assert_eq!(g.branch(&keys[4], "b", &walk), (0.04, 4));
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
