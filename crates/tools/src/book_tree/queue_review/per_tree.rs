use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct Status {
    pub id: String,
    pub own: String,
    pub state: String,
    pub read: u64,
    pub required: u64,
}

impl Status {
    pub(super) fn active(&self) -> bool {
        matches!(self.state.as_str(), "RUNNING" | "QUEUED")
    }

    fn remaining(&self, tree_size: u64) -> u64 {
        tree_size.max(self.required).saturating_sub(self.read)
    }
}

pub(super) fn load(boxes: &[BoxData], inputs: &mut Vec<PathBuf>) -> Result<Vec<Vec<Status>>> {
    #[derive(Deserialize)]
    struct Snapshot {
        targets: Vec<Status>,
    }
    boxes
        .iter()
        .map(|b| {
            let path = b.dir.join("fetch/targets.status.json");
            let snapshot: Snapshot = serde_json::from_reader(
                File::open(&path).with_context(|| format!("{} を開けません", path.display()))?,
            )
            .with_context(|| format!("進みのファイルが不正です: {}", path.display()))?;
            let mut seen = BTreeSet::new();
            for status in &snapshot.targets {
                let queued = b.queue.get(&status.id).with_context(|| {
                    format!("{}: status の id={} に受付行がありません", b.name, status.id)
                })?;
                ensure!(
                    matches!(status.own.as_str(), "b" | "w")
                        && queued.line.sides().contains(&status.own),
                    "{}: id={} の受付と status の own が一致しません",
                    b.name,
                    status.id
                );
                ensure!(
                    seen.insert((&status.id, &status.own)),
                    "{}: status の id={}, own={} が重複しています",
                    b.name,
                    status.id,
                    status.own
                );
            }
            ensure!(
                seen.len() == b.queue.values().map(|q| q.line.sides().len()).sum::<usize>(),
                "{}: status に無い受付があります。写しを揃えてください",
                b.name
            );
            inputs.push(path);
            Ok(snapshot.targets)
        })
        .collect()
}

pub(super) fn ahead(
    b: &BoxData,
    statuses: &[Status],
    tree_size: u64,
    priority: u32,
) -> Result<u64> {
    statuses
        .iter()
        .filter(|s| s.active())
        .filter(|s| {
            let line = &b.queue[&s.id].line;
            !line.hold && line.priority <= priority
        })
        .try_fold(0u64, |sum, s| sum.checked_add(s.remaining(tree_size)))
        .context("帯の局面数が大きすぎます")
}

struct Entry {
    id: String,
    own: String,
    priority: u32,
    remaining: u64,
    source: &'static str,
}

fn before(b: &BoxData, statuses: &[Status], tree_size: u64) -> Vec<Entry> {
    statuses
        .iter()
        .filter(|s| s.active() && !b.queue[&s.id].line.hold)
        .map(|s| Entry {
            id: s.id.clone(),
            own: s.own.clone(),
            priority: b.queue[&s.id].line.priority,
            remaining: s.remaining(tree_size),
            source: "accepted",
        })
        .collect()
}

fn sent(rows: &[Value], statuses: &[Status], tree_size: u64) -> Result<Vec<Entry>> {
    let mut result = Vec::new();
    for raw in rows {
        let line: Line = serde_json::from_value(raw.clone())?;
        if line.hold {
            continue;
        }
        for own in line.sides() {
            let remaining = statuses
                .iter()
                .find(|s| s.id == line.id && s.own == own && s.active() && s.read > 0)
                .map_or(tree_size, |s| s.remaining(tree_size));
            result.push(Entry {
                id: line.id.clone(),
                own,
                priority: line.priority,
                remaining,
                source: "send",
            });
        }
    }
    Ok(result)
}

fn append(
    bytes: &mut Vec<u8>,
    phase: &str,
    b: &BoxData,
    metric: &BoxMetrics,
    mut entries: Vec<Entry>,
) -> Result<()> {
    entries.sort_by_key(|e| e.priority);
    let mut cumulative = 0u64;
    for entry in entries {
        cumulative = cumulative.checked_add(entry.remaining).context("帯の局面数が大きすぎます")?;
        let hours = if cumulative == 0 {
            0.0
        } else if metric.speed == 0.0 {
            f64::INFINITY
        } else {
            cumulative as f64 / metric.speed
        };
        writeln!(
            bytes,
            "{phase}\t{}\t{}\t{}\t{}\t{}\t{}\t{cumulative}\t{}\t{hours}\t{}\t{}",
            b.name,
            entry.id,
            entry.own,
            entry.priority,
            entry.source,
            entry.remaining,
            metric.speed,
            metric.hours,
            hours > metric.hours
        )?;
    }
    Ok(())
}

pub(super) fn table(
    args: &QueueReviewArgs,
    boxes: &[BoxData],
    metrics: &[BoxMetrics],
    statuses: &[Vec<Status>],
    plan: Option<&plan::Plan>,
    rerank: Option<&rerank::Rerank>,
) -> Result<Vec<u8>> {
    let mut bytes = b"phase\tbox\tid\town\tpriority\tsource\tremaining\tcumulative\trate\thours\tcutoff_hours\tover\n".to_vec();
    for ((b, metric), status) in boxes.iter().zip(metrics).zip(statuses) {
        append(&mut bytes, "before", b, metric, before(b, status, args.tree_size))?;
        let named = format!("{}_{}.jsonl", args.prefix, b.name);
        let reranked = format!("{}_rerank_{}.jsonl", args.prefix, b.name);
        let payloads = plan
            .into_iter()
            .flat_map(|p| p.outputs.iter().zip(&p.payloads))
            .chain(rerank.into_iter().flat_map(|r| r.outputs.iter().zip(&r.payloads)));
        let mut additions = Vec::new();
        for (path, payload) in payloads {
            if path.file_name().is_some_and(|n| n == named.as_str() || n == reranked.as_str()) {
                for line in payload.split(|&c| c == b'\n').filter(|line| !line.is_empty()) {
                    additions.push(serde_json::from_slice(line)?);
                }
            }
        }
        if plan.is_some() || (rerank.is_some() && args.rerank_out.is_some()) {
            let mut entries = before(b, status, args.tree_size);
            entries.extend(sent(&additions, status, args.tree_size)?);
            append(&mut bytes, "after-send", b, metric, entries)?;
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> (BoxData, Vec<Status>) {
        let mut queue = BTreeMap::new();
        let mut statuses = Vec::new();
        for (id, priority, hold, state, read, required) in [
            ("later", 20, false, "QUEUED", 0, 0),
            ("partial", 12, false, "RUNNING", 40, 180),
            ("tie", 12, false, "QUEUED", 0, 0),
            ("held", 11, true, "RUNNING", 0, 0),
            ("ended", 10, false, "NOT_READY", 40, 70),
        ] {
            let raw = json!({"id":id,"root":"startpos","own":"b","depth":0,"priority":priority,"hold":hold});
            queue.insert(
                id.into(),
                Queued {
                    line: serde_json::from_value(raw.clone()).unwrap(),
                    raw,
                    roots: BTreeSet::new(),
                    primary_root: String::new(),
                },
            );
            statuses.push(Status {
                id: id.into(),
                own: "b".into(),
                state: state.into(),
                read,
                required,
            });
        }
        (
            BoxData {
                name: "box".into(),
                dir: PathBuf::new(),
                queue,
                progress: Progress {
                    trees: Vec::new(),
                    workers: Vec::new(),
                },
                progress_missing: false,
            },
            statuses,
        )
    }

    #[test]
    fn exact_priority_ahead_excludes_held_and_ended() {
        let (b, statuses) = fixture();
        assert_eq!(ahead(&b, &statuses, 150, 11).unwrap(), 0);
        assert_eq!(ahead(&b, &statuses, 150, 12).unwrap(), 290);
        assert_eq!(ahead(&b, &statuses, 150, 19).unwrap(), 290);
        assert_eq!(ahead(&b, &statuses, 150, 20).unwrap(), 440);
        let metric = BoxMetrics {
            speed: 100.0,
            hours: 3.0,
            capacity: 300.0,
            remaining: 440.0,
        };
        let mut bytes = Vec::new();
        append(&mut bytes, "before", &b, &metric, before(&b, &statuses, 150)).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let rows: Vec<_> = text.lines().map(|s| s.split('\t').collect::<Vec<_>>()).collect();
        assert_eq!(rows.iter().map(|r| r[2]).collect::<Vec<_>>(), ["partial", "tie", "later"]);
        assert_eq!(rows.iter().map(|r| r[7]).collect::<Vec<_>>(), ["140", "290", "440"]);
        assert_eq!(rows.iter().map(|r| r[11]).collect::<Vec<_>>(), ["false", "false", "true"]);
        let mut entries = before(&b, &statuses, 150);
        entries.extend(
            sent(
                &[json!({"id":"new","root":"startpos","own":"b","depth":0,"priority":13})],
                &statuses,
                150,
            )
            .unwrap(),
        );
        let mut bytes = Vec::new();
        append(&mut bytes, "after-send", &b, &metric, entries).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let rows: Vec<_> = text.lines().map(|s| s.split('\t').collect::<Vec<_>>()).collect();
        assert_eq!(
            rows.iter().map(|r| r[2]).collect::<Vec<_>>(),
            ["partial", "tie", "new", "later"]
        );
        assert_eq!(rows.iter().map(|r| r[7]).collect::<Vec<_>>(), ["140", "290", "440", "590"]);
    }

    #[test]
    fn sent_partial_trees_use_remaining_and_skip_holds() {
        let statuses = vec![Status {
            id: "partial".into(),
            own: "b".into(),
            state: "RUNNING".into(),
            read: 40,
            required: 180,
        }];
        let rows = vec![
            json!({"id":"partial","root":"startpos","own":"b","depth":0,"priority":12}),
            json!({"id":"new","root":"startpos","own":"bw","depth":0,"priority":13}),
            json!({"id":"held","root":"startpos","own":"b","depth":0,"priority":14,"hold":true}),
        ];
        let entries = sent(&rows, &statuses, 150).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].remaining, 140);
        assert_eq!(entries[1].remaining, 150);
        assert_eq!(entries[2].remaining, 150);
        assert_eq!(entries[1].own, "b");
        assert_eq!(entries[2].own, "w");
    }
}
