use super::rerank::LineStates;
use super::*;

#[derive(Default)]
struct Stats<'a> {
    trees: usize,
    held: usize,
    boxes: BTreeSet<&'a str>,
    priority: Option<(u32, u32)>,
    full: usize,
    part: usize,
    none: usize,
    remaining: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Reach {
    Full,
    Part,
    None,
}

/// 台ごとに priority の小さい順 (同点は progress の順 = serve の受付順) に残りを積み上げ、
/// 締めまでの読める量と比べて、読み切る (full)・途中まで (part)・手が付かない (none) に分ける。
fn simulate<'a>(
    boxes: &'a [BoxData],
    metrics: &[BoxMetrics],
    tree_size: u64,
    lines: Option<&LineStates>,
) -> Result<BTreeMap<String, Stats<'a>>> {
    let mut groups: BTreeMap<String, Stats> = BTreeMap::new();
    for (i, b) in boxes.iter().enumerate() {
        let mut active = Vec::new();
        for tree in b.progress.trees.iter().filter(|t| !t.finished && !t.failed) {
            let (priority, hold) = lines
                .and_then(|lines| lines.get(&(i, tree.id.clone())).copied())
                .unwrap_or((tree.priority, tree.hold));
            let group = b.queue[&tree.id].line.group.as_deref();
            if hold {
                if let Some(group) = group {
                    groups.entry(group.into()).or_default().held += 1;
                }
                continue;
            }
            active.push((priority, tree, group));
        }
        active.sort_by_key(|(priority, ..)| *priority);
        let capacity = metrics[i].capacity;
        let mut total = 0u64;
        for (priority, tree, group) in active {
            let remaining = tree.remaining(tree_size);
            let reach = if (total + remaining) as f64 <= capacity {
                Reach::Full
            } else if (total as f64) < capacity {
                Reach::Part
            } else {
                Reach::None
            };
            total = total.checked_add(remaining).context("残りの量が大きすぎます")?;
            let Some(group) = group else {
                continue;
            };
            let stats = groups.entry(group.into()).or_default();
            stats.trees += 1;
            stats.boxes.insert(&b.name);
            stats.priority = Some(
                stats
                    .priority
                    .map_or((priority, priority), |(lo, hi)| (lo.min(priority), hi.max(priority))),
            );
            match reach {
                Reach::Full => stats.full += 1,
                Reach::Part => stats.part += 1,
                Reach::None => stats.none += 1,
            }
            stats.remaining =
                stats.remaining.checked_add(remaining).context("残りの量が大きすぎます")?;
        }
    }
    Ok(groups)
}

/// group ごとの表の中身と標準出力の要約を作る。`lines` は再採点後の priority・hold。
pub(super) fn table(
    boxes: &[BoxData],
    metrics: &[BoxMetrics],
    tree_size: u64,
    lines: Option<&LineStates>,
) -> Result<(Vec<u8>, String)> {
    let mut bytes =
        b"group\tbasis\ttrees\theld\tboxes\tpriority_min\tpriority_max\tfull\tpart\tnone\tremaining\n"
            .to_vec();
    let mut summary = Vec::new();
    let bases = std::iter::once(("current", None)).chain(lines.map(|l| ("rerank", Some(l))));
    let mut tables = Vec::new();
    for (basis, lines) in bases {
        let groups = simulate(boxes, metrics, tree_size, lines)?;
        let (mut full, mut none, mut mixed) = (0, 0, 0);
        for stats in groups.values().filter(|s| s.trees > 0) {
            if stats.full == stats.trees {
                full += 1;
            } else if stats.none == stats.trees {
                none += 1;
            } else {
                mixed += 1;
            }
        }
        summary.push(format!(
            "group ({basis}): {} 組、全部読み切る={full} 手が付かない={none} 混在={mixed}",
            groups.len()
        ));
        tables.push((basis, groups));
    }
    let names: BTreeSet<&String> = tables.iter().flat_map(|(_, g)| g.keys()).collect();
    for name in names {
        for (basis, groups) in &tables {
            let empty = Stats::default();
            let s = groups.get(name).unwrap_or(&empty);
            let (lo, hi) = s.priority.map_or((String::new(), String::new()), |(lo, hi)| {
                (lo.to_string(), hi.to_string())
            });
            writeln!(
                bytes,
                "{name}\t{basis}\t{}\t{}\t{}\t{lo}\t{hi}\t{}\t{}\t{}\t{}",
                s.trees,
                s.held,
                s.boxes.iter().copied().collect::<Vec<_>>().join(","),
                s.full,
                s.part,
                s.none,
                s.remaining
            )?;
        }
    }
    Ok((bytes, summary.join("\n")))
}
