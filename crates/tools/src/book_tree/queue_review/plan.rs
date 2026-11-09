use super::*;

pub(super) struct Plan {
    pub outputs: Vec<PathBuf>,
    pub payloads: Vec<Vec<u8>>,
    pub remaining: Vec<f64>,
    rows: Vec<Vec<Value>>,
    table: Vec<u8>,
}
impl Plan {
    fn emit(&mut self, destination: usize, raw: Value) {
        let rows = &mut self.rows[destination];
        if let Some(index) = rows.iter().position(|r| r["id"] == raw["id"]) {
            rows.remove(index);
        }
        rows.push(raw);
    }
    fn record(
        &mut self,
        action: &str,
        q: &Queued,
        from: &str,
        to: &str,
        reason: &str,
    ) -> Result<()> {
        writeln!(
            self.table,
            "{action}\t{}\t{}\t{}\t{from}\t{to}\t{reason}",
            q.line.id, q.line.own, q.line.priority
        )?;
        Ok(())
    }
}

pub(super) fn ratio(remaining: f64, capacity: f64) -> f64 {
    if capacity == 0.0 {
        f64::INFINITY
    } else {
        remaining / capacity
    }
}

fn load(paths: &[PathBuf], inputs: &mut Vec<PathBuf>) -> Result<Vec<Queued>> {
    let mut rows = Vec::new();
    for path in paths {
        inputs.push(path.clone());
        for (n, text) in BufReader::new(File::open(path)?).lines().enumerate() {
            let text = text?;
            if text.trim().is_empty() {
                continue;
            }
            let parse = || -> Result<Queued> {
                let raw: Value = serde_json::from_str(&text)?;
                let line: Line = serde_json::from_value(raw.clone())?;
                line.check()?;
                let roots = std::iter::once(&line.root)
                    .chain(&line.roots)
                    .map(|r| parse_root(r).map(|r| r.key))
                    .collect::<Result<_>>()?;
                let primary_root = parse_root(&line.root)?.key;
                Ok(Queued {
                    line,
                    raw,
                    roots,
                    primary_root,
                })
            };
            rows.push(parse().with_context(|| format!("{}:{}", path.display(), n + 1))?);
        }
    }
    Ok(rows)
}

fn updated(raw: &Value, update: Option<&Queued>, hold: bool) -> Value {
    let mut raw = raw.clone();
    raw["hold"] = Value::Bool(hold);
    if let Some(update) = update {
        raw["game"] = update.raw.get("game").cloned().unwrap_or_else(|| Value::Array(Vec::new()));
        raw["memo"] = update.raw.get("memo").cloned().unwrap_or(Value::Null);
    }
    raw
}

pub(super) fn build(
    args: &QueueReviewArgs,
    boxes: &[BoxData],
    metrics: &[BoxMetrics],
    walk: &Walk,
    inputs: &mut Vec<PathBuf>,
) -> Result<Plan> {
    nonnegative(args.max_ratio)?;
    let dir = args.plan_out.as_ref().context("plan-out が必要です")?;
    if let Some(hold_out) = &args.hold_out {
        ensure!(
            crate::common::dedup::canonicalize_maybe_new(dir)?
                != crate::common::dedup::canonicalize_maybe_new(hold_out)?,
            "plan-out と hold-out は別の dir にしてください"
        );
    }
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                let name = entry?.file_name();
                let name = name.to_string_lossy();
                ensure!(
                    !(name.starts_with(&format!("{}_", args.prefix)) && name.ends_with(".jsonl"))
                        && name != format!("{}_plan.tsv", args.prefix),
                    "計画の積み荷が既にあります: {name}"
                );
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut labels = vec![String::new(); boxes.len()];
    let mut specified = BTreeSet::new();
    for text in &args.condition {
        let (name, label) = assignment(text)?;
        let index = boxes
            .iter()
            .position(|b| b.name == name)
            .context("condition の台が未指定です")?;
        ensure!(specified.insert(index), "condition の台が重複しています: {name}");
        labels[index] = label.into();
    }
    let mut adds = load(&args.add, inputs)?;
    let updates = load(&args.update, inputs)?;
    let mut ids: BTreeSet<_> = boxes.iter().flat_map(|b| b.queue.keys().cloned()).collect();
    for q in &adds {
        ensure!(ids.insert(q.line.id.clone()), "add の id が重複しています: {}", q.line.id);
    }
    let mut updated_ids = BTreeSet::new();
    let mut plan = Plan {
        outputs: Vec::new(),
        payloads: Vec::new(),
        remaining: metrics.iter().map(|m| m.remaining).collect(),
        rows: vec![Vec::new(); boxes.len()],
        table: b"action\tid\town\tpriority\tfrom\tto\treason\n".to_vec(),
    };
    for q in &updates {
        let owners: Vec<_> = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                b.progress.trees.iter().any(|t| t.id == q.line.id && !t.hold && !t.failed)
            })
            .map(|(i, _)| i)
            .collect();
        ensure!(owners.len() <= 1, "update の読み手が複数あります: {}", q.line.id);
        let destinations: Vec<_> = if owners.is_empty() {
            boxes
                .iter()
                .enumerate()
                .filter(|(_, b)| b.queue.contains_key(&q.line.id))
                .map(|(i, _)| i)
                .collect()
        } else {
            owners
        };
        if destinations.is_empty() {
            plan.record("drop-update", q, "", "", "id-not-found")?;
        }
        for i in destinations {
            let original = &boxes[i].queue[&q.line.id];
            let held = original.line.hold;
            let raw = updated(&original.raw, Some(q), held);
            let check = || -> Result<()> {
                let line: Line = serde_json::from_value(raw.clone())?;
                line.check()?;
                Ok(())
            };
            check().with_context(|| {
                format!("update の送り先: 台={} id={}", boxes[i].name, q.line.id)
            })?;
            plan.emit(i, raw);
            if !held && updated_ids.insert((i, q.line.id.clone())) {
                plan.remaining[i] += args.update_size as f64;
            }
            plan.record(
                if held { "update-held" } else { "update" },
                original,
                &boxes[i].name,
                &boxes[i].name,
                "game-memo",
            )?;
        }
    }
    let mut root_ids: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for q in boxes.iter().flat_map(|b| b.queue.values()) {
        for root in &q.roots {
            root_ids.entry(root).or_default().insert(&q.line.id);
        }
    }
    let shared =
        |q: &Queued| q.roots.iter().any(|r| root_ids[r.as_str()].iter().any(|id| *id != q.line.id));
    let start: Vec<_> =
        args.start_moves.as_deref().unwrap_or_default().split_whitespace().collect();
    let mut order: Vec<_> = (0..boxes.len()).collect();
    order.sort_by_key(|&i| &boxes[i].name);
    let best = |remaining: &[f64], candidates: Vec<usize>, amount: f64| {
        candidates.into_iter().min_by(|&a, &b| {
            ratio(remaining[a] + amount, metrics[a].capacity)
                .total_cmp(&ratio(remaining[b] + amount, metrics[b].capacity))
                .then_with(|| boxes[a].name.cmp(&boxes[b].name))
        })
    };
    for &i in &order {
        let b = &boxes[i];
        let mut candidates = Vec::new();
        for q in b.queue.values() {
            let trees: Vec<_> = b.progress.trees.iter().filter(|t| t.id == q.line.id).collect();
            let running = b
                .progress
                .workers
                .iter()
                .any(|w| w.key.as_ref().is_some_and(|k| q.roots.contains(strip_ply(k))));
            if running || trees.iter().any(|t| t.finished || t.hold || t.read != 0) {
                continue;
            }
            let path = memo_path(q, &start);
            let low = trees.iter().all(|t| {
                q.roots.iter().all(|r| {
                    classify(q, r, &t.own, walk, args.value_floor, &path).0 == Class::ReachLow
                })
            });
            candidates.push((q, low));
        }
        candidates.sort_by(|(a, low_a), (b, low_b)| {
            low_b
                .cmp(low_a)
                .then_with(|| b.line.priority.cmp(&a.line.priority))
                .then_with(|| b.line.id.cmp(&a.line.id))
        });
        for (q, low) in candidates {
            if ratio(plan.remaining[i], metrics[i].capacity) <= args.max_ratio {
                break;
            }
            // bw は受付行を分割できないため、両側の見込み量を一緒に移す。
            let amount = b
                .progress
                .trees
                .iter()
                .filter(|t| t.id == q.line.id && !t.finished && !t.hold)
                .map(|t| t.remaining(args.tree_size) as f64)
                .sum::<f64>()
                + if updated_ids.contains(&(i, q.line.id.clone())) {
                    args.update_size as f64
                } else {
                    0.0
                };
            let destination = if low {
                None
            } else {
                let available = order
                    .iter()
                    .copied()
                    .filter(|&j| {
                        j != i
                            && !boxes[j].queue.contains_key(&q.line.id)
                            && !plan.rows[j].iter().any(|r| r["id"] == q.line.id)
                            && (labels[i] == labels[j] || !shared(q))
                            && ratio(plan.remaining[j] + amount, metrics[j].capacity)
                                <= args.max_ratio
                    })
                    .collect();
                best(&plan.remaining, available, amount)
            };
            if low || destination.is_some() || q.line.priority >= 100 {
                let source =
                    plan.rows[i].iter().find(|r| r["id"] == q.line.id).unwrap_or(&q.raw).clone();
                plan.emit(i, updated(&source, None, true));
                plan.remaining[i] = (plan.remaining[i] - amount).max(0.0);
                if let Some(j) = destination {
                    plan.emit(j, updated(&source, None, false));
                    plan.remaining[j] += amount;
                    plan.record("move", q, &b.name, &boxes[j].name, "capacity")?;
                } else {
                    plan.record(
                        if low { "hold-low" } else { "hold-over" },
                        q,
                        &b.name,
                        &b.name,
                        if low { "reach-low" } else { "no-capacity" },
                    )?;
                }
            } else {
                plan.record("keep-over", q, &b.name, &b.name, "priority-below-100")?;
            }
        }
    }
    let mut root_labels: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (i, b) in boxes.iter().enumerate() {
        let mut effective: BTreeMap<_, _> =
            b.queue.iter().map(|(id, q)| (id.as_str(), &q.raw)).collect();
        for raw in &plan.rows[i] {
            let id = raw["id"].as_str().context("計画行に id がありません")?;
            effective.insert(id, raw);
        }
        for raw in effective.into_values() {
            let line: Line = serde_json::from_value(raw.clone())?;
            if line.hold {
                continue;
            }
            for root in std::iter::once(&line.root).chain(&line.roots) {
                root_labels.entry(parse_root(root)?.key).or_default().insert(labels[i].clone());
            }
        }
    }
    adds.sort_by_key(|q| q.line.priority);
    for q in &adds {
        let allowed: Vec<_> = order
            .iter()
            .copied()
            .filter(|&i| {
                // 既に複数の条件の台で読まれている根は、どの条件に置いても共有できない読みが残るので縛らない
                q.roots.iter().all(|r| {
                    root_labels.get(r).is_none_or(|set| {
                        set.len() > 1 || set.iter().all(|label| label == &labels[i])
                    })
                })
            })
            .collect();
        ensure!(!allowed.is_empty(), "add の根に一致する条件の台がありません: {}", q.line.id);
        let amount = args.tree_size as f64 * q.line.sides().len() as f64;
        let available = allowed
            .iter()
            .copied()
            .filter(|&i| ratio(plan.remaining[i] + amount, metrics[i].capacity) <= args.max_ratio)
            .collect();
        let destination = best(&plan.remaining, available, amount);
        let held = destination.is_none();
        let i = destination
            .or_else(|| best(&plan.remaining, allowed, 0.0))
            .context("台がありません")?;
        plan.emit(i, updated(&q.raw, None, held));
        if !held {
            plan.remaining[i] += amount;
            for root in &q.roots {
                root_labels.entry(root.clone()).or_default().insert(labels[i].clone());
            }
        }
        plan.record(
            if held { "add-hold" } else { "add" },
            q,
            "",
            &boxes[i].name,
            if held { "no-capacity" } else { "capacity" },
        )?;
    }
    plan.outputs.push(dir.join(format!("{}_plan.tsv", args.prefix)));
    plan.payloads.push(std::mem::take(&mut plan.table));
    for &i in &order {
        if plan.rows[i].is_empty() {
            continue;
        }
        let mut bytes = Vec::new();
        for raw in &plan.rows[i] {
            serde_json::to_writer(&mut bytes, raw)?;
            writeln!(bytes)?;
        }
        plan.outputs.push(dir.join(format!("{}_{}.jsonl", args.prefix, boxes[i].name)));
        plan.payloads.push(bytes);
    }
    files::guard(&plan.outputs, inputs, true)?;
    Ok(plan)
}
